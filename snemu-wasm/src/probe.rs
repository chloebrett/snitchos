//! A differential probe: one fixed guest program, run to a fixed instret budget,
//! whose result must be **identical** on the 64-bit host and under wasm32.
//!
//! Why this exists rather than a plain "does it build" check: the browser target
//! is 32-bit, so every `usize` in the emulator narrows from 64 bits to 32. A
//! truncation there does not crash — it silently computes a different address and
//! executes the wrong instruction. The host test suite runs on a 64-bit target and
//! therefore cannot see that class at all. Running the *same* assertions on both
//! targets can.
//!
//! The instrument is self-checking. The register expectations below are derived
//! from the instruction encodings, not recorded from a run: if the hand-assembled
//! program were wrong, `x2` would not hold `1 << 40` and the host test would fail
//! before the wasm one ever ran.
//!
//! **What it found (2026-08-10).** Every architectural observation agrees across
//! the two targets — all registers, the UART bytes, and the retired instret are
//! identical. The emulator itself is width-clean on these paths.
//! [`Machine::state_hash`], however, is *not*: it differs between host and wasm32
//! for byte-identical machine state. [`hash_diagnostics`] isolates why — `SipHash`
//! and raw byte writes both agree across targets, and only `<[u8]>::hash`, which
//! length-prefixes with a **`usize`**, diverges. So the split below is not
//! squeamishness about a flaky value: [`check_portable`] asserts everything the two
//! targets must agree on, and the hash is pinned host-only because it is a
//! pointer-width-dependent digest by construction. snemu's own doc already says it
//! is "not a cross-toolchain-stable digest"; this measures the boundary of that.

use snemu::machine::Machine;
use snemu::mem::{Memory, RAM_BASE};

/// Where the probe stores its 64-bit test value — `RAM_BASE + 0x100`, i.e. a guest
/// physical address above 2^31, past the end of the 52-byte program.
const SCRATCH: u64 = RAM_BASE + 0x100;

/// The ns16550a transmit-holding register. Writing a byte here is what puts
/// characters in [`Machine::uart_output`], so the probe covers MMIO routing too.
const UART_THR: u64 = 0x1000_0000;

/// How many instructions to retire. The program's last instruction is a jump to
/// itself, so any budget past its length lands in the same state — the number is a
/// margin, not a tuned constant.
const BUDGET: u64 = 64;

/// The guest program, hand-assembled. Each line is `offset: encoding  mnemonic`.
///
/// It is deliberately not straight-line arithmetic: it builds a value wider than
/// 32 bits (`1 << 40`), builds a RAM address ≥ 2^31 *arithmetically* (rather than
/// via `lui`, whose RV64 sign-extension would make the intent harder to read),
/// round-trips that value through an 8-byte store/load, and writes a byte to MMIO.
/// Those are the paths where a narrowed `usize` would show up.
#[rustfmt::skip]
const PROGRAM: [u32; 13] = [
    0x0010_0093, //  0: addi x1, x0, 1
    0x0280_9113, //  4: slli x2, x1, 40       -> x2 = 1 << 40  (> 2^32)
    0x0010_0193, //  8: addi x3, x0, 1
    0x01F1_9193, // 12: slli x3, x3, 31       -> x3 = 0x8000_0000 (RAM_BASE)
    0x1001_8193, // 16: addi x3, x3, 0x100    -> x3 = SCRATCH
    0x0021_B023, // 20: sd   x2, 0(x3)        -> 8-byte store, high address
    0x0001_B203, // 24: ld   x4, 0(x3)        -> and back
    0x0010_0313, // 28: addi x6, x0, 1
    0x01C3_1313, // 32: slli x6, x6, 28       -> x6 = 0x1000_0000 (UART_THR)
    0x0410_0393, // 36: addi x7, x0, 0x41     -> 'A'
    0x0073_0023, // 40: sb   x7, 0(x6)        -> MMIO byte write
    0x0012_8293, // 44: addi x5, x5, 1
    0x0000_006F, // 48: jal  x0, 0            -> spin here forever
];

/// What one probe run observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Instructions retired. Equal to [`BUDGET`] unless stepping faulted early.
    pub instret: u64,
    /// `x0..=x7`. `x0` is included precisely because it must stay zero.
    pub regs: [u64; 8],
    /// Bytes the guest wrote to the UART.
    pub uart: Vec<u8>,
    /// The whole-machine hash — registers, CSRs and written RAM folded together.
    pub state_hash: u64,
    /// The error text if stepping faulted, so a wasm-only fault names itself
    /// instead of arriving as a mismatched hash. Discarding it would repeat a
    /// mistake this repo has already made once.
    pub fault: Option<String>,
}

/// Which of snemu's optional accelerators to switch on.
///
/// They are all off in `Hart::new`, and each is documented as "a pure speedup proven
/// by the on↔off A/B" — the plain interpreter is the oracle. [`Speedups::OFF`] is
/// that oracle; [`Speedups::ON`] is what the browser runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speedups {
    /// Tier-1 decode cache.
    pub fetch_cache: bool,
    /// Tier-2 block JIT (Backend A — portable, so the browser gets it too).
    pub block_jit: bool,
    /// Software TLB over Sv39 translation.
    pub tlb: bool,
}

impl Speedups {
    /// The interpreter oracle: every accelerator off, as `Hart::new` leaves them.
    pub const OFF: Self = Self { fetch_cache: false, block_jit: false, tlb: false };
    /// What the browser build enables.
    pub const ON: Self = Self { fetch_cache: true, block_jit: true, tlb: true };

    /// Apply to a machine. Straight-line by design: this is the one place the
    /// browser's accelerator choice is written down, so it is worth being able to
    /// read it in a single glance.
    pub fn apply(self, machine: &mut Machine) {
        machine.set_fetch_cache(self.fetch_cache);
        machine.set_block_jit(self.block_jit);
        machine.set_tlb(self.tlb);
    }
}

/// Run [`PROGRAM`] for [`BUDGET`] instructions and report the result.
///
/// Deterministic by construction: no clock, no entropy, no host I/O. snemu's
/// clock is the instruction counter, which is why the browser gets determinism
/// for free.
#[must_use]
pub fn run() -> Probe {
    run_with(Speedups::OFF)
}

/// [`run`], with a chosen accelerator configuration.
#[must_use]
pub fn run_with(speedups: Speedups) -> Probe {
    let mut machine = Machine::new(Memory::new(64 * 1024), 1);
    speedups.apply(&mut machine);

    let mut image = Vec::with_capacity(PROGRAM.len() * 4);
    for word in PROGRAM {
        image.extend_from_slice(&word.to_le_bytes());
    }
    machine.write_ram(RAM_BASE, &image).expect("program fits in RAM");
    machine.set_pc(0, RAM_BASE);

    let mut fault = None;
    for _ in 0..BUDGET {
        if let Err(e) = machine.step() {
            fault = Some(format!("{e:?}"));
            break;
        }
    }

    let mut regs = [0u64; 8];
    for (i, slot) in regs.iter_mut().enumerate() {
        *slot = machine.reg(0, i);
    }

    Probe {
        instret: machine.instret(),
        regs,
        uart: machine.uart_output().to_vec(),
        state_hash: machine.state_hash(),
        fault,
    }
}

/// Everything the 64-bit host and wasm32 must agree on: the architectural result of
/// running the program. The single source of truth for both targets, so they cannot
/// drift apart by someone editing one and forgetting the other.
///
/// Deliberately excludes `state_hash` — see the module docs. Every assertion here is
/// derived from the instruction encodings, so this is a real oracle rather than a
/// recording of whatever the emulator happened to do.
///
/// # Panics
/// If the observed probe disagrees with the fixed expectation.
pub fn check_portable(p: &Probe) {
    assert_eq!(p.fault, None, "stepping faulted");
    assert_eq!(p.instret, BUDGET, "every budgeted instruction should retire");

    assert_eq!(p.regs[0], 0, "x0 is hardwired zero");
    assert_eq!(p.regs[1], 1, "x1 = 1");
    assert_eq!(p.regs[2], 1 << 40, "x2 = 1 << 40 — a value that does not fit in 32 bits");
    assert_eq!(p.regs[3], SCRATCH, "x3 = a guest address above 2^31");
    assert_eq!(p.regs[4], 1 << 40, "x4 = the same wide value, round-tripped through RAM");
    assert_eq!(p.regs[5], 1, "x5 = 1");
    assert_eq!(p.regs[6], UART_THR, "x6 = the UART base");
    assert_eq!(p.regs[7], 0x41, "x7 = 'A'");

    assert_eq!(p.uart, b"A", "the MMIO byte write reached the UART");
}

/// The host's `state_hash()` for this probe, pinned. Host-only by measurement, not
/// by assumption: wasm32 produces `10084449359911607788` for byte-identical machine
/// state, because the digest folds in `usize`-width lengths.
///
/// If a legitimate change to snemu's hashing or to [`PROGRAM`] moves it, re-pin it
/// **from a host run**.
pub const EXPECTED_STATE_HASH: u64 = 980_759_326_325_069_301;

/// Three `DefaultHasher` results that isolate *why* a hash can differ between a
/// 64-bit host and wasm32, rather than leaving it to inference:
///
/// - `[0]` a `u64` hashed directly — no `usize` anywhere.
/// - `[1]` a byte slice hashed via [`Hash`] — `<[u8]>::hash` length-prefixes with a
///   **`usize`**, which is 8 bytes on the host and 4 in the browser.
/// - `[2]` the same bytes via [`Hasher::write`] — raw, no length prefix.
///
/// If `[0]` and `[2]` agree across targets while `[1]` does not, the divergence is
/// the `usize` length prefix and nothing to do with `SipHash` or with snemu's
/// arithmetic. That is a measurement, not a guess.
#[must_use]
pub fn hash_diagnostics() -> [u64; 3] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let u64_only = {
        let mut h = DefaultHasher::new();
        0xDEAD_BEEF_u64.hash(&mut h);
        h.finish()
    };
    let slice_via_hash = {
        let mut h = DefaultHasher::new();
        [1u8, 2, 3].as_slice().hash(&mut h);
        h.finish()
    };
    let bytes_via_write = {
        let mut h = DefaultHasher::new();
        h.write(&[1u8, 2, 3]);
        h.finish()
    };
    [u64_only, slice_via_hash, bytes_via_write]
}

/// Host values for [`hash_diagnostics`], pinned so the wasm side can compare each
/// component independently.
pub const EXPECTED_HASH_DIAGNOSTICS: [u64; 3] =
    [2_170_039_440_619_509_621, 8_086_395_815_454_877_121, 6_984_003_033_159_075_747];

#[cfg(test)]
mod tests {
    use super::{Probe, check_portable, run, run_with};

    /// The 64-bit reference. This is the run that defines "correct"; the wasm test
    /// in `tests/wasm.rs` asserts the identical expectations.
    #[test]
    fn the_probe_matches_expectations_on_the_host() {
        check_portable(&run());
    }

    /// Pinned host-only, because the digest is pointer-width-dependent. Asserted
    /// here so a change to snemu's hashing or to the program still gets caught.
    #[test]
    fn the_state_hash_is_stable_on_the_host() {
        assert_eq!(run().state_hash, super::EXPECTED_STATE_HASH);
    }

    /// Each hashing component, asserted separately so a wasm failure names which
    /// one moved instead of collapsing three questions into one answer.
    #[test]
    fn hashing_a_u64_is_target_independent() {
        assert_eq!(super::hash_diagnostics()[0], super::EXPECTED_HASH_DIAGNOSTICS[0]);
    }

    #[test]
    fn hashing_a_slice_via_hash_is_target_independent() {
        assert_eq!(super::hash_diagnostics()[1], super::EXPECTED_HASH_DIAGNOSTICS[1]);
    }

    #[test]
    fn hashing_raw_bytes_via_write_is_target_independent() {
        assert_eq!(super::hash_diagnostics()[2], super::EXPECTED_HASH_DIAGNOSTICS[2]);
    }

    /// **The claim the browser build is about to rely on.**
    ///
    /// Every accelerator's doc says it is "a pure speedup proven by the on↔off A/B".
    /// Turning them on in the browser makes that claim load-bearing for a page whose
    /// determinism is an advertised property, so it is checked here rather than
    /// taken on trust: same registers, same UART bytes, same retired instret, same
    /// whole-machine hash, with them off and on.
    ///
    /// A failure here means an accelerator is not transparent, and the honest
    /// response is to turn it off, not to relax the assertion.
    #[test]
    fn the_speedups_change_nothing_but_speed() {
        let oracle = run_with(super::Speedups::OFF);
        let fast = run_with(super::Speedups::ON);

        assert_eq!(fast.regs, oracle.regs, "registers diverged");
        assert_eq!(fast.uart, oracle.uart, "device output diverged");
        assert_eq!(fast.instret, oracle.instret, "retired instruction count diverged");
        assert_eq!(fast.state_hash, oracle.state_hash, "machine state diverged");
        assert_eq!(fast.fault, oracle.fault, "one configuration faulted and the other did not");
    }

    /// **The A/B pair cannot, on its own, prove the speedups are switched on.**
    ///
    /// Every other test here asserts that `ON` and `OFF` agree — which an `apply`
    /// that did nothing would satisfy perfectly. Mutation testing found exactly that
    /// (`replace Speedups::apply with ()` survived), and the consequence is not
    /// cosmetic: the browser would silently drop from 38.9 MIPS back to the 11 MIPS
    /// plain interpreter, and every test would stay green.
    ///
    /// So this asserts the opposite direction, using the one difference that *is*
    /// deterministic: with the block JIT on, a compiled block retires its whole
    /// length in a single `step()`. Without it, a step retires exactly one
    /// instruction, always.
    #[test]
    fn the_speedups_are_actually_applied() {
        use snemu::machine::Machine;
        use snemu::mem::{Memory, RAM_BASE};

        // A five-instruction loop: long enough to form a block worth compiling.
        let loop_body: [u32; 5] = [
            0x0011_8193, // addi x3, x3, 1
            0x0012_0213, // addi x4, x4, 1
            0x0012_8293, // addi x5, x5, 1
            0x0013_0313, // addi x6, x6, 1
            0xFF1F_F06F, // jal  x0, -16
        ];

        let biggest_step = |speedups: super::Speedups| {
            let mut m = Machine::new(Memory::new(64 * 1024), 1);
            speedups.apply(&mut m);
            let mut image = Vec::new();
            for w in loop_body {
                image.extend_from_slice(&w.to_le_bytes());
            }
            m.write_ram(RAM_BASE, &image).expect("fits");
            m.set_pc(0, RAM_BASE);

            let mut biggest = 0;
            for _ in 0..40 {
                let before = m.instret();
                m.step().expect("steps");
                biggest = biggest.max(m.instret() - before);
            }
            biggest
        };

        assert_eq!(biggest_step(super::Speedups::OFF), 1, "the interpreter retires one at a time");
        assert!(
            biggest_step(super::Speedups::ON) > 1,
            "with the block JIT on, a step should retire a whole block — if this fails, \
             the accelerators are not reaching the machine"
        );
    }

    /// Each accelerator alone, so a failure names which one is not transparent
    /// rather than only that the combination is not.
    #[test]
    fn each_speedup_is_independently_transparent() {
        let oracle = run_with(super::Speedups::OFF);
        for (label, cfg) in [
            ("fetch cache", super::Speedups { fetch_cache: true, ..super::Speedups::OFF }),
            ("block jit", super::Speedups { block_jit: true, ..super::Speedups::OFF }),
            ("tlb", super::Speedups { tlb: true, ..super::Speedups::OFF }),
        ] {
            let got = run_with(cfg);
            assert_eq!(got.state_hash, oracle.state_hash, "{label} changed machine state");
            assert_eq!(got.instret, oracle.instret, "{label} changed retired instret");
        }
    }

    /// The oracle must be able to *fail*.
    ///
    /// [`check_portable`] lives in `src/` rather than in a test module because the
    /// host test and the wasm32 test in `tests/wasm.rs` compile separately and must
    /// share one set of expectations — that sharing is what stops the two targets
    /// drifting apart. The cost is that the oracle is production code, so hollowing
    /// it out to a no-op would leave every test that calls it passing vacuously.
    /// Mutation testing found exactly that (`replace check_portable with ()`
    /// survived). This is the negative control that kills it: a probe with one wrong
    /// register must be rejected.
    #[test]
    #[should_panic(expected = "x2 = 1 << 40")]
    fn the_oracle_rejects_a_probe_that_disagrees() {
        let mut wrong = run();
        wrong.regs[2] = 0xDEAD;
        check_portable(&wrong);
    }

    /// And it must notice a *missing* observation, not just a wrong one — the shape
    /// a silently-empty device buffer would take.
    #[test]
    #[should_panic(expected = "MMIO byte write")]
    fn the_oracle_rejects_a_probe_with_no_uart_output() {
        let mut wrong = run();
        wrong.uart.clear();
        check_portable(&wrong);
    }

    /// Guards the instrument itself: a probe whose observations were all zero, or
    /// whose program never ran, would satisfy a weaker assertion set silently.
    #[test]
    fn the_probe_actually_executed_something() {
        let p: Probe = run();
        assert!(p.instret > 0, "no instructions retired");
        assert!(p.state_hash != 0, "state hash looks unpopulated");
        assert!(!p.uart.is_empty(), "guest produced no UART output");
    }
}
