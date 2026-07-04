//! A multi-hart machine: several [`Hart`]s sharing one [`Bus`] and a common
//! clock. The scheduler is deterministic round-robin — one instruction per
//! running hart per round — which keeps runs reproducible and lets a hart
//! spinning on a flag the other sets make progress every round.
//!
//! Memory is sequentially consistent: one shared `Bus`, an instruction is
//! indivisible, `aq`/`rl` are no-ops. Relaxed memory is a later milestone.

use crate::bus::Bus;
use crate::cpu::{Hart, StepError};
use crate::mem::Memory;

/// A machine with `hart_count` harts over one shared address space.
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
                self.harts[i].step(&mut self.bus)?;
                self.time += 1;
            }
        }
        Ok(())
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

    #[must_use]
    pub fn pc(&self, hart: usize) -> u64 {
        self.harts[hart].pc()
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
}
