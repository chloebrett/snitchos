//! A multi-hart machine: several [`Hart`]s sharing one [`Bus`] and a common
//! clock. The scheduler is deterministic round-robin — one instruction per
//! running hart per round — which keeps runs reproducible and lets a hart
//! spinning on a flag the other sets make progress every round.
//!
//! Memory is sequentially consistent: one shared `Bus`, an instruction is
//! indivisible, `aq`/`rl` are no-ops. Relaxed memory is a later milestone.

use crate::bus::Bus;
use crate::cpu::{Hart, HartEffect, StepError, memop_charge, service_sbi};
use crate::mem::Memory;

/// Calibration tripwire for the native-op collapse. With native ops OFF it watches
/// each `memset`/`memcpy` from its entry PC to its return, recording how many
/// instructions the interpreter *actually* retired next to what `memop_charge(len)`
/// would have charged. A run's `real` vs `charged` totals quantify the clock skew
/// the collapse introduces: if they diverge, the deterministic clock (and thus the
/// frame stream's timing) drifts when native ops are on, and `memop_charge` needs
/// recalibrating. Diagnostic only — enable with native ops off (a collapsed memop
/// never enters the interpreter, so the probe would not see it).
#[derive(Clone, Default)]
struct MemopProbe {
    /// Per-hart in-flight memop being timed. `None` when the hart is not currently
    /// inside a probed memop.
    inflight: Vec<Option<InFlight>>,
    invocations: u64,
    real: u64,
    charged: u64,
}

/// A memop currently being timed on one hart: its return address, the `len` it was
/// called with, and the instructions retired so far since its entry.
#[derive(Clone)]
struct InFlight {
    return_pc: u64,
    len: u64,
    count: u64,
}

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
    /// The memop calibration probe (see [`MemopProbe`]); `None` unless enabled.
    memop_probe: Option<MemopProbe>,
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
            memop_probe: None,
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

    /// Enable the memop calibration probe (see [`MemopProbe`]). Run with native ops
    /// **off**: the probe measures the interpreter's real per-memop retired count
    /// against `memop_charge`, which is only observable when memops are interpreted
    /// rather than collapsed.
    pub fn enable_memop_probe(&mut self) {
        self.memop_probe = Some(MemopProbe {
            inflight: vec![None; self.harts.len()],
            ..MemopProbe::default()
        });
    }

    /// The probe's `(invocations, real_retired, charged)` totals, or `None` if the
    /// probe was never enabled. `real == charged` means `memop_charge` is faithful
    /// and the collapse introduces no clock skew; `charged < real` means the clock
    /// runs fast (short-charges memops) when native ops are on.
    #[must_use]
    pub fn memop_probe_report(&self) -> Option<(u64, u64, u64)> {
        self.memop_probe
            .as_ref()
            .map(|p| (p.invocations, p.real, p.charged))
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
            self.probe_memop_entry(i, pc);

            match self.harts[i].step(&mut self.bus)? {
                HartEffect::Sbi(req) => {
                    service_sbi(&mut self.harts, i, &req);
                    self.time += 1;
                    retired = true;
                    self.probe_memop_retire(i);
                    if let Some(p) = self.profile.as_mut() {
                        *p.entry(pc).or_insert(0) += 1;
                    }
                }
                HartEffect::None => {
                    self.time += 1;
                    retired = true;
                    self.probe_memop_retire(i);
                    if let Some(p) = self.profile.as_mut() {
                        *p.entry(pc).or_insert(0) += 1;
                    }
                }
                // A block JIT block retired `n` instructions in one step — advance
                // the shared clock by all of them (the single-tick case, generalised).
                HartEffect::Block(n) => {
                    self.time += n;
                    retired = true;
                    if let Some(p) = self.profile.as_mut() {
                        *p.entry(pc).or_insert(0) += n;
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

    /// Probe hook: if hart `i` is at a memop entry and not already inside one, start
    /// timing the op (remember its return address + `len`). No-op unless the probe
    /// is enabled. Runs with native ops off, so a memop reaches the interpreter and
    /// its real retired count is observable.
    fn probe_memop_entry(&mut self, i: usize, pc: u64) {
        if Some(pc) != self.memset_pc && Some(pc) != self.memcpy_pc {
            return;
        }
        let return_pc = self.harts[i].reg(1);
        let len = self.harts[i].reg(12);
        if let Some(probe) = self.memop_probe.as_mut()
            && probe.inflight[i].is_none()
        {
            probe.inflight[i] = Some(InFlight { return_pc, len, count: 0 });
        }
    }

    /// Probe hook: hart `i` retired one instruction. Count it toward any in-flight
    /// memop; when the hart returns to the op's `ra`, commit its real retired count
    /// and what `memop_charge` would have charged.
    fn probe_memop_retire(&mut self, i: usize) {
        let new_pc = self.harts[i].pc();
        let Some(probe) = self.memop_probe.as_mut() else {
            return;
        };
        let Some(inflight) = probe.inflight[i].as_mut() else {
            return;
        };
        inflight.count += 1;
        let (done, len, count) = (new_pc == inflight.return_pc, inflight.len, inflight.count);
        if done {
            probe.inflight[i] = None;
            probe.invocations += 1;
            probe.real += count;
            probe.charged += memop_charge(len);
        }
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
                    HartEffect::Block(n) => {
                        self.time += n;
                        if let Some(p) = self.profile.as_mut() {
                            *p.entry(pc).or_insert(0) += n;
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

    /// Make `etc/ramfb` exist in the guest's fw_cfg directory — the snemu
    /// equivalent of passing `-device ramfb` to real QEMU. Off by default;
    /// call before booting a machine that should find it.
    pub fn enable_fwcfg_ramfb(&mut self) {
        self.bus.fwcfg_enable_ramfb();
    }

    /// Extract the captured `etc/ramfb` framebuffer's raw pixel bytes from
    /// guest RAM, alongside its `(width, height, stride)` — `None` if no DMA
    /// write has completed (`etc/ramfb` was never enabled, or the guest
    /// hasn't presented yet). Shared by [`Self::dump_framebuffer`] and
    /// [`Self::framebuffer_pixels`] so the RAM-extraction logic lives once;
    /// each just applies a different pure conversion
    /// (`framebuffer::render_ppm` vs `framebuffer::to_minifb_buffer`).
    fn read_framebuffer(&self) -> Option<(Vec<u8>, u32, u32, u32)> {
        let cfg = self.bus.fwcfg_ramfb_cfg()?;
        let ram = self.bus.ram();
        let len = u64::from(cfg.stride) * u64::from(cfg.height);
        let pixels: Vec<u8> = (0..len).map(|i| ram.read_u8(cfg.addr + i).unwrap_or(0)).collect();
        Some((pixels, cfg.width, cfg.height, cfg.stride))
    }

    /// Render the captured `etc/ramfb` framebuffer as a binary PPM (P6)
    /// image — `None` if nothing was ever captured (see
    /// [`Self::read_framebuffer`]). The `--dump-framebuffer` CLI flag's whole
    /// implementation; pixel-format conversion itself is the pure,
    /// host-tested `framebuffer::render_ppm`.
    #[must_use]
    pub fn dump_framebuffer(&self) -> Option<Vec<u8>> {
        let (pixels, width, height, stride) = self.read_framebuffer()?;
        Some(crate::framebuffer::render_ppm(&pixels, width, height, stride))
    }

    /// Render the captured `etc/ramfb` framebuffer as a `minifb` pixel
    /// buffer (one `u32` per pixel, `0x00RRGGBB`) plus its `(width, height)`
    /// — `None` if nothing was ever captured. The `--window` CLI flag's
    /// live-display counterpart to [`Self::dump_framebuffer`]; pixel-format
    /// conversion is the pure, host-tested `framebuffer::to_minifb_buffer`.
    #[must_use]
    pub fn framebuffer_pixels(&self) -> Option<(Vec<u32>, u32, u32)> {
        let (pixels, width, height, stride) = self.read_framebuffer()?;
        Some((crate::framebuffer::to_minifb_buffer(&pixels, width, height, stride), width, height))
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

    /// A content hash of the machine's guest-visible state: the shared clock, every
    /// hart's architectural registers, and guest RAM + device output. Because the
    /// guest is a closed deterministic system, this is a pure function of
    /// `(initial state, harness input)` — so two runs fed the same input to the same
    /// instret hash **equal**, and an unequal hash at a claimed shared fork point is
    /// a determinism leak (a hidden entropy source, or a mis-share). That makes the
    /// snapshot tree's sharing self-verifying. Excludes performance toggles (caches,
    /// idle-skip, native-ops), which must not change the hash. Cost is O(written RAM
    /// + harts), paid only at the few fork points, not per step.
    ///
    /// The value is stable within a build (a fixed-key `DefaultHasher`), enough to
    /// key a cache and to compare two states in the same process; it is not a
    /// cross-toolchain-stable digest.
    #[must_use]
    pub fn state_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.time.hash(&mut h);
        for hart in &self.harts {
            hart.hash_state(&mut h);
        }
        self.bus.hash_state(&mut h);
        h.finish()
    }

    /// Guest RAM footprint so far — the highest byte the guest has written (past the
    /// ELF/DTB load). Used to right-size the machine: the smallest RAM that still fits.
    #[must_use]
    pub fn ram_high_water(&self) -> u64 {
        self.bus.ram().high_water()
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

    /// Enable or disable the Tier-2 block JIT (M6) on **every** hart. Off by default
    /// (the interpreter is the oracle); the on↔off A/B proves it changes only speed.
    pub fn set_block_jit(&mut self, on: bool) {
        for hart in &mut self.harts {
            hart.set_block_jit(on);
        }
    }

    /// Select **Backend B** (native AArch64 codegen) vs **Backend A** (the reified-`Op`
    /// interpreter) for the block JIT on **every** hart. Off by default — A is the
    /// oracle and the browser backend; B falls back to A per-block, so the on↔off A/B
    /// must stay byte-identical. No effect unless the block JIT is also on and the host
    /// supports native emission (aarch64/macos today).
    pub fn set_native_jit(&mut self, on: bool) {
        for hart in &mut self.harts {
            hart.set_native_jit(on);
        }
    }

    /// Enable or disable the software TLB (translation cache) on **every** hart. Off by
    /// default (the walk-every-access oracle); a pure speedup, on↔off byte-identical.
    pub fn set_tlb(&mut self, on: bool) {
        for hart in &mut self.harts {
            hart.set_tlb(on);
        }
    }

    /// Enable or disable block-executor register caching (M6 increment 4) on **every**
    /// hart. On by default; the on↔off A/B isolates the caching's speed effect.
    pub fn set_register_cache(&mut self, on: bool) {
        for hart in &mut self.harts {
            hart.set_register_cache(on);
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
    fn state_hash_is_equal_for_equal_state_and_changes_as_the_guest_runs() {
        let program = &[0x02a0_0093, 0x0070_0113, 0x0010_0193];
        let mut a = machine_with(program, 1);
        let mut b = machine_with(program, 1);

        // Identical fresh machines hash equal — the hash is a pure function of state.
        assert_eq!(a.state_hash(), b.state_hash());

        // Stepping the guest changes the state, and so the hash.
        let before = a.state_hash();
        a.step().unwrap();
        assert_ne!(a.state_hash(), before, "a retired instruction must change the hash");

        // Determinism (the self-audit): two independent runs to the same point hash
        // equal. Unequal here would be a determinism leak — exactly what the snapshot
        // tree's fork-point check relies on to confirm a share is sound.
        b.step().unwrap();
        assert_eq!(a.state_hash(), b.state_hash(), "same input to the same point ⇒ same state");

        // A clone reproduces the hash; diverging one register breaks it.
        let mut c = a.clone();
        assert_eq!(a.state_hash(), c.state_hash(), "a clone is the same state");
        c.set_reg(0, 5, 0xdead);
        assert_ne!(a.state_hash(), c.state_hash(), "a changed register changes the hash");
    }

    #[test]
    fn state_hash_reflects_guest_ram_writes() {
        let mut a = machine_with(&[], 1);
        let b = machine_with(&[], 1);
        assert_eq!(a.state_hash(), b.state_hash());
        a.write_ram(RAM_BASE + 0x40, &[1, 2, 3, 4]).unwrap();
        assert_ne!(a.state_hash(), b.state_hash(), "a guest RAM write changes the hash");
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

        let charged = 27; // memop_charge(8) = 24 + (8/8)*3 + 0
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
    fn the_memop_probe_measures_real_retired_against_the_charge() {
        // The calibration tripwire: with native ops OFF, the probe watches each
        // memop from its entry PC to its return (`ra`) and records how many
        // instructions the interpreter *actually* retired, alongside what
        // `memop_charge(len)` would have charged — so a run can quantify the clock
        // skew the native-op collapse introduces (charged != real => the clock
        // drifts). Here a 3-instruction stub stands in for the kernel's memset.
        const NOP: u32 = 0x0000_0013; // addi x0, x0, 0
        const RET: u32 = 0x0000_8067; // jalr x0, 0(x1)
        let ret_target = RAM_BASE + 0x100;
        let mut mem = Memory::new(0x2000);
        mem.write_u32(RAM_BASE, NOP).unwrap();
        mem.write_u32(RAM_BASE + 4, NOP).unwrap();
        mem.write_u32(RAM_BASE + 8, RET).unwrap();
        mem.write_u32(ret_target, 0x0000_006f).unwrap(); // self-loop after return
        let mut m = Machine::new(mem, 1);
        m.set_native_op_pcs(Some(RAM_BASE), None); // memset entry = RAM_BASE
        m.enable_memop_probe(); // native ops stay OFF — the probe measures the real loop

        m.harts[0].set_reg(1, ret_target); // ra
        m.harts[0].set_reg(12, 8); // a2 = len → charged = memop_charge(8) = 27
        for _ in 0..3 {
            m.step().unwrap(); // run the 3-instruction stub to its return
        }

        let (invocations, real, charged) =
            m.memop_probe_report().expect("probe was enabled");
        assert_eq!(invocations, 1, "one memop observed");
        assert_eq!(real, 3, "the interpreter retired the stub's 3 instructions");
        assert_eq!(charged, 27, "memop_charge(8) would have charged 27");
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

        let charged = 24 + (0x1000 / 8) * 3; // memop_charge(4096) = 24 + 512*3 = 1560
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
        m.harts[0].set_reg(12, 8); // len → charged = 27
        let charged = 27;
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
