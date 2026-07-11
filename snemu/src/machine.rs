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
    /// Opt-in exact instret profiler: `PC → instructions retired at that PC`,
    /// accumulated across all harts. `None` (the default) so the normal
    /// boot/fork path pays nothing; the `snemu-profile` tool enables it after
    /// forking a snapshot, so the histogram covers only the scenario's own run.
    /// Every retired instruction is counted (no sampling) — deterministic, like
    /// the instret clock itself.
    profile: Option<std::collections::HashMap<u64, u64>>,
    /// Native-op helpers (tier-0.5 of the JIT): when a hart's PC hits `memset_pc` /
    /// `memcpy_pc`, execute the op natively on guest RAM and charge the instret the
    /// interpreter would have retired, instead of grinding the store loop one
    /// instruction at a time. This is the shared "PC → native block" dispatch a JIT
    /// generalises (compiled blocks replace these hand-written ones). Resolved from
    /// the kernel ELF symbols; `None` if stripped. Off unless [`set_native_ops`].
    memset_pc: Option<u64>,
    memcpy_pc: Option<u64>,
    native_ops: bool,
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
            profile: None,
            memset_pc: None,
            memcpy_pc: None,
            native_ops: false,
        }
    }

    /// Register the guest entry PCs of `memset`/`memcpy` (resolved from the kernel
    /// ELF) so the native-op helper can intercept them. Called by the loader.
    pub fn set_native_op_pcs(&mut self, memset_pc: Option<u64>, memcpy_pc: Option<u64>) {
        self.memset_pc = memset_pc;
        self.memcpy_pc = memcpy_pc;
    }

    /// Enable/disable the native-op helper (off by default). A/B this against the
    /// pure interpreter to confirm it changes only speed, never the frame stream —
    /// the same fidelity discipline as `idle_skip`.
    pub fn set_native_ops(&mut self, on: bool) {
        self.native_ops = on;
    }

    /// Start (or reset) exact instret profiling on this machine. The histogram
    /// begins empty; every subsequently retired instruction is attributed to the
    /// PC it executed at. Enable *after* forking a booted snapshot to profile only
    /// the scenario's run.
    pub fn enable_profiling(&mut self) {
        self.profile = Some(std::collections::HashMap::new());
    }

    /// Take the accumulated `PC → instret` histogram, leaving profiling off.
    /// `None` if profiling was never enabled.
    #[must_use]
    pub fn take_profile(&mut self) -> Option<std::collections::HashMap<u64, u64>> {
        self.profile.take()
    }

    /// Advance the machine. Normally this is one scheduler round (each running hart
    /// steps once). When the native-op helper is on and a running hart sits at a
    /// `memset`/`memcpy` entry, this instead collapses that whole op — see
    /// [`collapse_memop`](Self::collapse_memop) — which subsumes many rounds into
    /// one call. Instret (the shared clock) is accounted identically either way.
    pub fn step(&mut self) -> Result<(), StepError> {
        // Native-op fast path (tier-0.5 JIT): if a running hart is at a memop entry,
        // collapse the op AND advance every other hart by the charged instret, so
        // cross-hart interleaving is reproduced exactly (a running peer retires the
        // same instructions it would across the collapsed rounds). Handled at the
        // top — before any hart steps — so at collapse time no hart has moved this
        // round and the peer catch-up is symmetric. Declines (fault) fall through to
        // a normal round so the interpreter traps.
        if self.native_ops
            && let Some(i) = self.hart_at_memop()
        {
            let entry_pc = self.harts[i].pc();
            if let Some(charged) =
                self.harts[i].try_native_memop(&mut self.bus, self.memset_pc, self.memcpy_pc)
            {
                if let Some(p) = self.profile.as_mut() {
                    *p.entry(entry_pc).or_insert(0) += charged;
                }
                return self.collapse_memop(i, charged);
            }
        }
        self.step_round()
    }

    /// One scheduler round: step each running hart once, in id order, each
    /// observing the shared clock, which advances one tick per executed
    /// instruction.
    fn step_round(&mut self) -> Result<(), StepError> {
        let mut retired = false;
        for i in 0..self.harts.len() {
            // A stopped (parked secondary) hart never runs; only running and idle
            // harts are visited — idle ones so a pending timer/IPI can wake them.
            if self.harts[i].is_stopped() {
                continue;
            }
            self.harts[i].set_cycle(self.time);
            // PC of the instruction about to execute — captured before `step`
            // advances it, so the profiler attributes the retired instruction to
            // where it ran.
            let pc = self.harts[i].pc();

            match self.harts[i].step(&mut self.bus)? {
                HartEffect::Sbi(req) => {
                    service_sbi(&mut self.harts, i, &req);
                    self.time += 1;
                    retired = true;
                    if let Some(p) = self.profile.as_mut() {
                        *p.entry(pc).or_insert(0) += 1;
                    }
                }
                HartEffect::None => {
                    self.time += 1;
                    retired = true;
                    if let Some(p) = self.profile.as_mut() {
                        *p.entry(pc).or_insert(0) += 1;
                    }
                }
                // A parked (wfi) hart retired nothing — don't tick the clock for it.
                HartEffect::Idle => {}
            }
        }
        // Every running hart is parked on wfi: nothing will advance the shared
        // clock toward a timer, so jump it to the earliest armed deadline. That
        // makes the timer pending, and the next round wakes the hart — collapsing
        // the idle wait from millions of steps to one. A stopped hart never runs;
        // an idle hart with no armed timer can only be woken by an IPI, which
        // can't arrive while every hart idles, so it contributes no deadline.
        if !retired && let Some(deadline) = self.earliest_wake_deadline() {
            self.time = self.time.max(deadline);
        }
        Ok(())
    }

    /// The lowest-id running hart whose PC is a `memset`/`memcpy` entry, or `None`.
    /// Idle/stopped harts are skipped — only a hart about to *execute* the op is a
    /// collapse candidate. Cheap: a scan of `hart_count` PCs against two values.
    fn hart_at_memop(&self) -> Option<usize> {
        (0..self.harts.len()).find(|&i| {
            self.harts[i].is_running()
                && (Some(self.harts[i].pc()) == self.memset_pc
                    || Some(self.harts[i].pc()) == self.memcpy_pc)
        })
    }

    /// Reproduce the `charged` scheduler rounds that hart `i`'s memop collapses,
    /// with hart `i`'s stores already applied natively by `try_native_memop`. Each
    /// round advances the shared clock by hart `i`'s one store, then steps every
    /// other non-stopped hart once against that clock — exactly the round-robin the
    /// interpreter would run, so a running peer retires the same instructions and an
    /// idle peer's timer fires at the same tick. The collapse is only *observably*
    /// exact when no peer touches the memop's byte range mid-op (private in the
    /// kernel's call sites); the `snemu-itest` on↔off byte-identical A/B is the proof.
    fn collapse_memop(&mut self, i: usize, charged: u64) -> Result<(), StepError> {
        let base = self.time;
        // Quiet-span fast path: if every other hart is idle/stopped and none has a
        // timer armed to fire within the span, no peer retires or wakes — the whole
        // collapse is just hart `i`'s `charged` ticks. Preserves post-7's O(1)
        // single-runner collapse (the common boot / all-idle-peer case).
        if self.no_peer_activity_within(i, base + charged) {
            self.time = base + charged;
            return Ok(());
        }
        for _ in 0..charged {
            self.time += 1; // hart i's store this round
            for j in 0..self.harts.len() {
                if j == i || self.harts[j].is_stopped() {
                    continue;
                }
                self.harts[j].set_cycle(self.time);
                let pc = self.harts[j].pc();
                match self.harts[j].step(&mut self.bus)? {
                    HartEffect::Sbi(req) => {
                        service_sbi(&mut self.harts, j, &req);
                        self.time += 1;
                        if let Some(p) = self.profile.as_mut() {
                            *p.entry(pc).or_insert(0) += 1;
                        }
                    }
                    HartEffect::None => {
                        self.time += 1;
                        if let Some(p) = self.profile.as_mut() {
                            *p.entry(pc).or_insert(0) += 1;
                        }
                    }
                    HartEffect::Idle => {}
                }
            }
        }
        Ok(())
    }

    /// Whether no hart other than `i` will retire or wake before the clock reaches
    /// `end`: every other hart is idle or stopped, and no idle peer has a timer
    /// armed to fire at or before `end`. When true a collapse can jump straight to
    /// `end` without stepping any peer (nothing would have moved).
    fn no_peer_activity_within(&self, i: usize, end: u64) -> bool {
        self.harts.iter().enumerate().all(|(j, h)| {
            j == i
                || h.is_stopped()
                || (h.is_idle() && h.wake_deadline().is_none_or(|d| d > end))
        })
    }

    /// The soonest clock value at which any idle hart's armed timer would wake it.
    /// `None` if no idle hart has a deadline (nothing to fast-forward to).
    fn earliest_wake_deadline(&self) -> Option<u64> {
        self.harts
            .iter()
            .filter(|h| h.is_idle())
            .filter_map(Hart::wake_deadline)
            .min()
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

    /// Inject host console input for the guest to read through the UART receive
    /// buffer. The interactive audit harness calls this when a scenario reaches
    /// its "ready to read" marker, then keeps stepping so the guest drains it.
    pub fn push_console_input(&mut self, bytes: &[u8]) {
        self.bus.push_console_input(bytes);
    }

    #[must_use]
    pub fn virtio_tx_output(&self) -> &[u8] {
        self.bus.virtio_tx_output()
    }

    /// Total guest instructions retired across all harts — the shared clock is
    /// advanced once per executed instruction, so it *is* the aggregate instret.
    /// The measurement spine's headline counter: deterministic for a given
    /// program+seed, so MIPS (`instret / wall_clock`) is the honest, engine-
    /// independent speed number. See `plans/snemu-milestone-4-measurement.md`.
    #[must_use]
    pub fn instret(&self) -> u64 {
        self.time
    }

    /// Total supervisor timer interrupts delivered across all harts (diagnostic).
    #[must_use]
    pub fn timer_fires(&self) -> u64 {
        self.harts.iter().map(Hart::timer_fires).sum()
    }

    /// Enable or disable the Tier-1 decode cache (M5) on **every** hart. Off by
    /// default (the pure interpreter is the oracle); snemu exposes this as a flag
    /// so a run with it on can be proven identical to one with it off — same
    /// instret, same telemetry, only faster.
    pub fn set_decode_cache(&mut self, on: bool) {
        for hart in &mut self.harts {
            hart.set_decode_cache(on);
        }
    }

    /// Enable or disable `wfi` idle-skip on **every** hart (on by default). Off
    /// restores bare nop-`wfi` and disables the clock fast-forward — the A/B
    /// baseline proving idle-skip changes only speed, not the telemetry stream.
    pub fn set_idle_skip(&mut self, on: bool) {
        for hart in &mut self.harts {
            hart.set_idle_skip(on);
        }
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
    fn profiler_attributes_each_retired_instruction_to_its_pc() {
        // Three sequential addis; profiling counts one instret at each PC.
        let program = &[0x02a0_0093, 0x0070_0113, 0x0010_0193];
        let mut m = machine_with(program, 1);
        m.enable_profiling();
        for _ in 0..3 {
            m.step().unwrap();
        }
        let profile = m.take_profile().expect("profiling was enabled");
        assert_eq!(profile.get(&RAM_BASE), Some(&1));
        assert_eq!(profile.get(&(RAM_BASE + 4)), Some(&1));
        assert_eq!(profile.get(&(RAM_BASE + 8)), Some(&1));
        // Total attributed == instructions retired.
        assert_eq!(profile.values().sum::<u64>(), 3);
    }

    #[test]
    fn profiling_is_off_by_default() {
        let mut m = machine_with(&[0x02a0_0093], 1);
        m.step().unwrap();
        assert!(m.take_profile().is_none());
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
    fn an_all_idle_machine_fast_forwards_to_the_earliest_armed_timer() {
        // Both harts parked on wfi with timers armed at different deadlines.
        // Nothing advances the shared clock, so the machine must jump it to the
        // *earliest* deadline — and wake exactly that hart, not the later one.
        let mut m = machine_with(&[0x0000_0013], 2); // nop; harts are parked below
        m.harts[0].arm_idle_timer(1200);
        m.harts[1].arm_idle_timer(500);

        m.step().unwrap(); // no hart retires → jump to min(1200, 500)
        assert_eq!(m.instret(), 500, "clock jumped to the earliest deadline");

        m.step().unwrap(); // clock now 500: hart 1's timer fires, hart 0 waits
        assert!(m.is_running(1), "the earliest-deadline hart woke");
        assert!(!m.is_running(0), "the later-deadline hart is still parked");
    }

    #[test]
    fn a_running_hart_advances_the_clock_and_wakes_an_idle_peer_without_a_jump() {
        // Hart 0 spins in a self-loop (always retiring, advancing the shared
        // clock one tick per round); hart 1 idles with a timer 3 ticks out. The
        // clock must reach 3 by real execution — no fast-forward while a hart
        // runs — and hart 1 wakes exactly when it does.
        const SELF_LOOP: u32 = 0x0000_006f; // jal x0, 0
        let mut m = machine_with(&[SELF_LOOP], 2);
        m.harts[1].arm_idle_timer(3);

        // Rounds 1-2 take the clock to 2 (hart 0 retires one tick each); hart 1
        // observes 1 then 2, both under its deadline, so it stays parked.
        for _ in 0..2 {
            m.step().unwrap();
            assert!(!m.is_running(1), "idle until the clock reaches the deadline");
        }
        assert_eq!(m.pc(0), RAM_BASE, "hart 0 kept spinning on its self-loop");

        // Round 3: hart 0 advances the clock to 3, and hart 1 — checked later the
        // same round against the shared clock — sees its deadline and wakes. No
        // fast-forward jump happened; real execution carried the clock there.
        m.step().unwrap();
        assert!(m.is_running(1), "idle peer woke when the running hart's clock hit 3");
        assert_eq!(m.instret(), 4, "clock advanced by execution (3 hart-0 + 1 wake), not a jump");
    }

    #[test]
    fn a_native_memop_advances_a_running_peer_by_the_charged_instret() {
        // The lockstep-preserving collapse: hart 0 hits a memset entry while hart 1
        // runs — collapsing hart 0's whole memset into one shot must ALSO advance
        // hart 1 by exactly the charged instret, so the shared clock counts both
        // harts' retirements just as a round-by-round interpreter run would. This is
        // what keeps cross-hart interleaving faithful with the helper on.
        const SELF_LOOP: u32 = 0x0000_006f; // jal x0, 0
        let after_memset = RAM_BASE + 4;
        let peer_pc = RAM_BASE + 0x100;
        let mut mem = Memory::new(0x2000);
        mem.write_u32(after_memset, SELF_LOOP).unwrap(); // hart 0 parks here post-memset
        mem.write_u32(peer_pc, SELF_LOOP).unwrap(); // hart 1 spins here, retiring 1/round
        let mut m = Machine::new(mem, 2);
        m.set_native_op_pcs(Some(RAM_BASE), None); // memset entry PC = RAM_BASE
        m.set_native_ops(true);

        // Hart 0 at the memset entry: memset(dst, val=0, len=8), ra = after_memset.
        m.harts[0].set_pc(RAM_BASE);
        m.harts[0].set_reg(1, after_memset); // ra
        m.harts[0].set_reg(10, RAM_BASE + 0x800); // a0 = dst (disjoint from code)
        m.harts[0].set_reg(11, 0); // a1 = val
        m.harts[0].set_reg(12, 8); // a2 = len
        // Hart 1 running its self-loop.
        m.harts[1].start(peer_pc, 1, 0);

        let charged = 11; // memop_charge(8) = 8 + (8/8)*3 + 0
        m.step().unwrap();

        assert_eq!(m.pc(0), after_memset, "hart 0 returned from the collapsed memset");
        assert_eq!(m.pc(1), peer_pc, "hart 1 kept spinning on its self-loop");
        assert_eq!(
            m.instret(),
            charged * 2,
            "clock counts hart 0's charged stores AND hart 1's catch-up retirements"
        );
    }

    #[test]
    fn a_lone_hart_collapses_a_memop_to_the_charged_instret_in_one_step() {
        // No peer to interleave → the quiet-span fast path: one `Machine::step`
        // applies the whole memop and jumps the clock by exactly the charged instret
        // (here a 4 KiB fill), instead of grinding the store loop one tick at a time.
        let after_memset = RAM_BASE + 4;
        let mut mem = Memory::new(0x4000);
        mem.write_u32(after_memset, 0x0000_006f).unwrap();
        let mut m = Machine::new(mem, 1);
        m.set_native_op_pcs(Some(RAM_BASE), None);
        m.set_native_ops(true);

        m.harts[0].set_pc(RAM_BASE);
        m.harts[0].set_reg(1, after_memset); // ra
        m.harts[0].set_reg(10, RAM_BASE + 0x1000); // dst
        m.harts[0].set_reg(11, 0); // val
        m.harts[0].set_reg(12, 0x1000); // len = 4096

        m.step().unwrap();

        let charged = 8 + (0x1000 / 8) * 3; // memop_charge(4096) = 8 + 512*3 = 1544
        assert_eq!(m.instret(), charged, "clock jumped by the charged instret in one step");
        assert_eq!(m.pc(0), after_memset, "returned from the memset");
        assert_eq!(m.reg(0, 10), RAM_BASE + 0x1000, "a0 = dst per the memop ABI");
    }

    #[test]
    fn a_native_memop_wakes_an_idle_peer_whose_timer_fires_within_the_span() {
        // Hart 0 collapses a memset; hart 1 idles with a timer armed to fire on the
        // last tick of the collapsed span. The interleave must advance the shared
        // clock through the span so hart 1's timer fires at exactly its deadline —
        // a bare `time += charged` jump would skip past the wake.
        let after_memset = RAM_BASE + 4;
        let mut mem = Memory::new(0x2000);
        mem.write_u32(after_memset, 0x0000_006f).unwrap(); // hart 0 self-loops post-memset
        let mut m = Machine::new(mem, 2);
        m.set_native_op_pcs(Some(RAM_BASE), None);
        m.set_native_ops(true);

        m.harts[0].set_pc(RAM_BASE);
        m.harts[0].set_reg(1, after_memset); // ra
        m.harts[0].set_reg(10, RAM_BASE + 0x800); // dst
        m.harts[0].set_reg(11, 0); // val
        m.harts[0].set_reg(12, 8); // len → charged = 11
        let charged = 11;
        m.harts[1].arm_idle_timer(charged); // fires on the span's final tick

        assert!(!m.is_running(1), "hart 1 idle before the collapse");
        m.step().unwrap();

        assert!(m.is_running(1), "hart 1's timer fired within the collapsed span");
        assert_eq!(m.pc(0), after_memset, "hart 0 returned from the collapsed memset");
    }

    #[test]
    fn instret_counts_every_retired_instruction_deterministically() {
        // The measurement spine's headline counter: guest instructions retired.
        // One hart over three independent instructions retires exactly three,
        // and a fresh machine over the same program retires the same count —
        // the determinism that makes cross-engine MIPS comparison honest.
        let program = &[0x02a0_0093, 0x0070_0113, 0x0010_0193];
        let mut m = machine_with(program, 1);
        assert_eq!(m.instret(), 0, "a fresh machine has retired nothing");
        m.step().unwrap();
        m.step().unwrap();
        m.step().unwrap();
        assert_eq!(m.instret(), 3);

        let mut again = machine_with(program, 1);
        for _ in 0..3 {
            again.step().unwrap();
        }
        assert_eq!(again.instret(), m.instret(), "same program → same instret");
    }

    #[test]
    fn instret_aggregates_across_running_harts() {
        // With two harts running, one scheduler round retires one instruction
        // per running hart — the aggregate the MIPS number is built on. Hart 1
        // starts parked, so the first round retires only hart 0's instruction.
        let mut m = machine_with(&[0x02a0_0093], 2); // addi x1, x0, 42
        m.step().unwrap();
        assert_eq!(m.instret(), 1, "only the running hart retired");
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
