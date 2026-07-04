//! A multi-hart machine: several [`Hart`]s sharing one [`Bus`] and a common
//! clock. The scheduler is deterministic round-robin — one instruction per
//! running hart per round — which keeps runs reproducible and lets a hart
//! spinning on a flag the other sets make progress every round.
//!
//! Memory is sequentially consistent: one shared `Bus`, an instruction is
//! indivisible, `aq`/`rl` are no-ops. Relaxed memory is a later milestone.

use crate::bus::Bus;
use crate::cpu::{Hart, HartEffect, StepError, service_sbi};
use crate::mem::Memory;

/// A machine with `hart_count` harts over one shared address space.
/// `Clone` is the snapshot primitive: snemu has no hidden state (no JIT cache,
/// no host threads), so a full machine snapshot is a deep copy of registers,
/// RAM, and device state — and restore is just keeping the clone. This is what
/// makes the boot-once/fork-per-workload harness possible.
#[derive(Clone)]
pub struct Machine {
    harts: Vec<Hart>,
    bus: Bus,
    /// The shared monotonic clock (the `rdtime` source): one tick per
    /// instruction executed by any hart.
    time: u64,
}

impl Machine {
    /// A machine over `mem` with `hart_count` harts. Hart 0 boots running; every
    /// secondary starts parked, waiting for an SBI `hart_start`.
    #[must_use]
    pub fn new(mem: Memory, hart_count: usize) -> Self {
        let mut harts: Vec<Hart> = (0..hart_count).map(|_| Hart::new()).collect();
        for hart in harts.iter_mut().skip(1) {
            hart.park();
        }
        Self {
            harts,
            bus: Bus::new(mem),
            time: 0,
        }
    }

    /// One scheduler round: step each running hart once, in id order, each
    /// observing the shared clock, which advances one tick per executed
    /// instruction.
    pub fn step(&mut self) -> Result<(), StepError> {
        for i in 0..self.harts.len() {
            if self.harts[i].is_running() {
                self.harts[i].set_cycle(self.time);
                if let HartEffect::Sbi(req) = self.harts[i].step(&mut self.bus)? {
                    service_sbi(&mut self.harts, i, &req);
                }
                self.time += 1;
            }
        }
        Ok(())
    }

    /// Step (round-robin) until the UART output contains `marker` — a stable
    /// boot checkpoint — or `max_steps` elapse. Returns the steps taken. This is
    /// the boot-once half of the snapshot/fork harness: run to the marker, then
    /// `clone()` the machine to snapshot it.
    pub fn run_until_uart(&mut self, marker: &[u8], max_steps: u64) -> Result<u64, String> {
        let mut steps = 0u64;
        let mut seen_len = 0usize;
        while steps < max_steps {
            let len = self.bus.uart_output().len();
            if len != seen_len {
                seen_len = len;
                if self.bus.uart_output().windows(marker.len()).any(|w| w == marker) {
                    return Ok(steps);
                }
            }
            self.step().map_err(|e| format!("fault before UART marker: {e:?}"))?;
            steps += 1;
        }
        Err(format!("UART marker not seen within {max_steps} steps"))
    }

    /// Overwrite guest RAM at `addr` — used to patch the DTB's `workload=`
    /// bootarg into a snapshot before resuming it (the per-workload fork).
    pub fn write_ram(&mut self, addr: u64, bytes: &[u8]) -> Result<(), String> {
        self.bus.write_ram(addr, bytes).map_err(|e| format!("write_ram: {e:?}"))
    }

    #[must_use]
    pub fn hart_count(&self) -> usize {
        self.harts.len()
    }

    #[must_use]
    pub fn is_running(&self, hart: usize) -> bool {
        self.harts[hart].is_running()
    }

    #[must_use]
    pub fn reg(&self, hart: usize, i: usize) -> u64 {
        self.harts[hart].reg(i)
    }

    pub fn set_reg(&mut self, hart: usize, i: usize, value: u64) {
        self.harts[hart].set_reg(i, value);
    }

    pub fn set_pc(&mut self, hart: usize, pc: u64) {
        self.harts[hart].set_pc(pc);
    }

    #[must_use]
    pub fn pc(&self, hart: usize) -> u64 {
        self.harts[hart].pc()
    }

    #[must_use]
    pub fn satp(&self, hart: usize) -> u64 {
        self.harts[hart].satp()
    }

    #[must_use]
    pub fn uart_output(&self) -> &[u8] {
        self.bus.uart_output()
    }

    #[must_use]
    pub fn virtio_tx_output(&self) -> &[u8] {
        self.bus.virtio_tx_output()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::RAM_BASE;

    /// A machine of `harts` harts with `program` loaded at the RAM base.
    fn machine_with(program: &[u32], harts: usize) -> Machine {
        let mut mem = Memory::new(0x1000);
        for (i, &word) in program.iter().enumerate() {
            mem.write_u32(RAM_BASE + (i as u64) * 4, word).unwrap();
        }
        Machine::new(mem, harts)
    }

    #[test]
    fn a_clone_is_an_independent_deterministic_snapshot() {
        // addi x1=42, x2=7, x3=1 — three independent instructions.
        let program = &[0x02a0_0093, 0x0070_0113, 0x0010_0193];
        let mut m = machine_with(program, 1);
        m.step().unwrap(); // x1 = 42
        let snapshot = m.clone();
        m.step().unwrap(); // original advances: x2 = 7

        // The snapshot is independent — the original's step didn't touch it.
        assert_eq!(snapshot.reg(0, 1), 42);
        assert_eq!(snapshot.reg(0, 2), 0);
        assert_eq!(m.reg(0, 2), 7);

        // ...and deterministic — resuming the snapshot reproduces the original.
        let mut resumed = snapshot.clone();
        resumed.step().unwrap();
        assert_eq!(resumed.reg(0, 2), 7);
    }

    #[test]
    fn boot_hart_runs_while_the_secondary_stays_parked() {
        // Both harts share the same code, but only hart 0 boots running.
        let mut m = machine_with(&[0x02a0_0093], 2); // addi x1, x0, 42
        assert!(m.is_running(0));
        assert!(!m.is_running(1));

        m.step().unwrap();

        assert_eq!(m.reg(0, 1), 42); // hart 0 executed the instruction
        assert_eq!(m.pc(0), RAM_BASE + 4);
        assert_eq!(m.reg(1, 1), 0); // hart 1 never ran
        assert_eq!(m.pc(1), RAM_BASE);
    }

    #[test]
    fn a_single_hart_machine_still_runs_hart_zero() {
        let mut m = machine_with(&[0x02a0_0093], 1);
        assert_eq!(m.hart_count(), 1);
        m.step().unwrap();
        assert_eq!(m.reg(0, 1), 42);
    }

    #[test]
    fn hart_start_wakes_a_parked_secondary_at_the_entry() {
        const ECALL: u32 = 0x0000_0073;
        const SELF_LOOP: u32 = 0x0000_006f; // jal x0, 0 — hart 1 idles here
        const EID_HSM: u64 = 0x0048_534D;
        let entry = RAM_BASE + 0x40;

        let mut mem = Memory::new(0x1000);
        mem.write_u32(RAM_BASE, ECALL).unwrap(); // hart 0 issues the SBI call
        mem.write_u32(entry, SELF_LOOP).unwrap(); // hart 1's entry point
        let mut m = Machine::new(mem, 2);
        // sbi_hart_start(hartid=1, start_addr=entry, opaque=0x1234).
        m.set_reg(0, 17, EID_HSM); // a7 = EID
        m.set_reg(0, 16, 0); // a6 = FID 0
        m.set_reg(0, 10, 1); // a0 = target hartid
        m.set_reg(0, 11, entry); // a1 = start address
        m.set_reg(0, 12, 0x1234); // a2 = opaque
        assert!(!m.is_running(1));

        m.step().unwrap();

        assert_eq!(m.reg(0, 10), 0); // SBI_SUCCESS returned to the caller
        assert!(m.is_running(1)); // secondary woken
        assert_eq!(m.pc(1), entry); // ...running its self-loop at the entry
        assert_eq!(m.reg(1, 10), 1); // a0 = hartid
        assert_eq!(m.reg(1, 11), 0x1234); // a1 = opaque
    }

    #[test]
    fn hart_start_on_an_unknown_hart_id_errors() {
        const ECALL: u32 = 0x0000_0073;
        const EID_HSM: u64 = 0x0048_534D;
        let mut m = machine_with(&[ECALL], 2);
        m.set_reg(0, 17, EID_HSM);
        m.set_reg(0, 16, 0);
        m.set_reg(0, 10, 5); // no hart 5 exists
        m.set_reg(0, 11, RAM_BASE);
        m.step().unwrap();
        assert_eq!(m.reg(0, 10) as i64, -3); // SBI_ERR_INVALID_PARAM
    }
}
