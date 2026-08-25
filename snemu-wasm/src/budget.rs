//! Running the guest for a bounded slice of time, and saying how it stopped.
//!
//! The page cannot call `step()` in a `while` loop: `step()` advances the guest by
//! *one* scheduler round, so a boot is tens of millions of them, and a loop that
//! long inside one `requestAnimationFrame` callback freezes the tab. Instead each
//! frame runs a bounded slice and reports back.
//!
//! Two things about that bound are easy to get wrong, and both are load-bearing:
//!
//! - **The unit is guest instret, not host step-calls.** One `step()` can retire
//!   many instructions — `HartEffect::Block(n)` advances the clock by a whole JIT
//!   block (`machine.rs:217`), and a collapsed `memset` charges its full length. The
//!   devlog records this exact confusion costing real debugging time ("sixty million
//!   steps scanned two hundred and forty-five million guest instructions").
//! - **A step can retire *nothing*.** With every hart parked on `wfi` and no armed
//!   timer, `step_round` returns `Ok(())` having moved neither the clock nor any
//!   PC (`machine.rs:234` — the idle fast-forward is guarded by `if let
//!   Some(deadline)`). Stepping again cannot change that: the comment there notes an
//!   idle hart with no armed timer can only be woken by an IPI, which cannot arrive
//!   while every hart idles. So it is a terminal state, and a budget loop that does
//!   not notice spins forever.

use snemu::machine::Machine;

/// How a bounded run stopped. The page branches on this: keep scheduling frames
/// while `Running`, stop when `Halted`, surface the reason when `Trapped`.
///
/// Every variant carries the instret actually retired by *this* slice — a delta,
/// not the machine's cumulative count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The slice used its whole budget and the guest has more to do.
    Running { instret: u64 },
    /// The guest can make no further progress: a step retired nothing, which for
    /// this emulator means every hart is parked with no way to be woken. Stepping
    /// again would do exactly as much, forever.
    Halted { instret: u64 },
    /// Stepping faulted. `reason` is snemu's own message, kept rather than reduced
    /// to a bool so the page can say *what* went wrong.
    Trapped { instret: u64, reason: String },
}

impl Status {
    /// The instret this slice retired, whatever the outcome.
    #[must_use]
    pub fn instret(&self) -> u64 {
        match *self {
            Status::Running { instret }
            | Status::Halted { instret }
            | Status::Trapped { instret, .. } => instret,
        }
    }
}

/// A slice's instret allowance, anchored to where the machine's cumulative counter
/// stood when the slice began.
///
/// The anchor is the entire point. `Machine::instret()` counts the machine's whole
/// life, so on any frame after the first it is already in the millions; comparing it
/// directly against a per-frame limit would report the budget exhausted before the
/// guest took a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    limit: u64,
    start: u64,
}

impl Budget {
    /// A budget of `limit` instructions, starting from cumulative `start`.
    #[must_use]
    pub fn new(limit: u64, start: u64) -> Self {
        Self { limit, start }
    }

    /// Instructions retired since the slice began.
    ///
    /// Saturating: the counter only ever climbs, so a `now` below the anchor is not
    /// reachable through `run`, and there is no useful meaning to invent for it.
    #[must_use]
    pub fn spent(&self, now: u64) -> u64 {
        now.saturating_sub(self.start)
    }

    /// Whether the slice has used its allowance.
    #[must_use]
    pub fn is_exhausted(&self, now: u64) -> bool {
        self.spent(now) >= self.limit
    }
}

/// Run `machine` for at most `limit` guest instructions and report how it stopped.
///
/// The budget is checked *before* each step, so a zero limit retires nothing.
///
/// `limit` is a floor for stopping, not a ceiling on work: a step is atomic, so a
/// slice overshoots by up to whatever the last step retired (a JIT block, or a
/// collapsed `memset`, can be thousands). Callers sizing a frame budget should treat
/// it as approximate.
///
/// **Bounded by construction.** The iteration count is capped at `limit`, which is
/// not arbitrary defensive padding but a restatement of the loop's own invariant:
/// every iteration either retires at least one instruction or returns `Halted`, so a
/// correct run can never reach the cap. Writing it as a `for` rather than a `loop`
/// costs nothing and makes "this cannot spin" a property of the shape instead of a
/// property of the arithmetic being right. That matters more here than it usually
/// would: this runs inside a `requestAnimationFrame` callback, where an infinite loop
/// is not a hang you can interrupt — it is a tab you have to kill.
pub fn run(machine: &mut Machine, limit: u64) -> Status {
    let budget = Budget::new(limit, machine.instret());

    for _ in 0..limit {
        let before = machine.instret();
        if budget.is_exhausted(before) {
            return Status::Running { instret: budget.spent(before) };
        }

        if let Err(e) = machine.step() {
            return Status::Trapped {
                instret: budget.spent(machine.instret()),
                reason: format!("{e:?}"),
            };
        }

        let after = machine.instret();
        if after == before {
            return Status::Halted { instret: budget.spent(after) };
        }
    }

    Status::Running { instret: budget.spent(machine.instret()) }
}

#[cfg(test)]
mod tests {
    use super::{Budget, Status, run};
    use snemu::machine::Machine;
    use snemu::mem::{Memory, RAM_BASE};

    /// `wfi` — park the hart. With no timer armed this machine can never wake.
    const WFI: u32 = 0x1050_0073;
    /// `jal x0, 0` — branch to self. Retires an instruction every round, forever.
    const SPIN: u32 = 0x0000_006F;
    /// All-zero is not a valid RV64 encoding; the decoder rejects it.
    const ILLEGAL: u32 = 0x0000_0000;

    fn machine_running(program: &[u32]) -> Machine {
        let mut m = Machine::new(Memory::new(64 * 1024), 1);
        let mut image = Vec::new();
        for w in program {
            image.extend_from_slice(&w.to_le_bytes());
        }
        m.write_ram(RAM_BASE, &image).expect("program fits");
        m.set_pc(0, RAM_BASE);
        m
    }

    // --- The pure budget arithmetic -------------------------------------------

    /// `Machine::instret()` is cumulative over the machine's whole life, so on the
    /// second frame it starts in the millions. A budget that compared it directly
    /// against the limit would be exhausted before taking a single step — the same
    /// absolute-vs-delta trap `Cursor` exists to avoid on the output buffers.
    #[test]
    fn a_budget_measures_a_delta_not_an_absolute_instret() {
        let b = Budget::new(1_000, 5_000_000);
        assert_eq!(b.spent(5_000_400), 400);
        assert!(!b.is_exhausted(5_000_400), "400 of 1000 spent — not exhausted");
    }

    #[test]
    fn a_budget_is_exhausted_once_the_limit_is_reached() {
        let b = Budget::new(1_000, 0);
        assert!(!b.is_exhausted(999), "just under");
        assert!(b.is_exhausted(1_000), "exactly at the limit is exhausted");
        assert!(b.is_exhausted(1_001), "and past it");
    }

    /// A zero budget must retire nothing at all, rather than the one step an
    /// `is_exhausted`-after-stepping loop would sneak through.
    #[test]
    fn a_zero_budget_is_exhausted_immediately() {
        assert!(Budget::new(0, 42).is_exhausted(42));
    }

    // --- Driving a real machine -----------------------------------------------

    /// A guest with work left to do exhausts its slice and reports how much it ran.
    #[test]
    fn a_guest_that_keeps_running_reports_running_with_the_instret_spent() {
        let mut m = machine_running(&[SPIN]);
        assert_eq!(run(&mut m, 50), Status::Running { instret: 50 });
    }

    /// Successive slices accumulate: the second call runs a fresh 50, rather than
    /// finding the budget already spent by the first.
    #[test]
    fn successive_slices_each_get_a_full_budget() {
        let mut m = machine_running(&[SPIN]);
        run(&mut m, 50);
        assert_eq!(run(&mut m, 50), Status::Running { instret: 50 });
        assert_eq!(m.instret(), 100, "the machine advanced across both slices");
    }

    /// The anti-freeze property, and the reason this module exists. Every hart
    /// parked on `wfi` with no armed timer retires nothing per step; the run must
    /// notice and stop rather than spin the browser's animation frame forever.
    #[test]
    fn a_guest_that_can_never_progress_reports_halted_instead_of_spinning() {
        let mut m = machine_running(&[WFI]);
        let status = run(&mut m, 1_000_000);
        assert!(
            matches!(status, Status::Halted { .. }),
            "a wedged guest must halt the slice, got {status:?}"
        );
    }

    /// Halting reports the instret actually spent, not the budget — the `wfi`
    /// itself retires, then the machine wedges.
    #[test]
    fn halting_reports_what_was_spent_not_the_whole_budget() {
        let mut m = machine_running(&[WFI]);
        let Status::Halted { instret } = run(&mut m, 1_000_000) else {
            panic!("expected Halted");
        };
        assert!(instret < 1_000_000, "spent {instret}, which is the whole budget");
    }

    /// A fault carries its reason to the page. Keeping the text is deliberate: this
    /// repo has already been bitten once by an `.is_err()` that discarded snemu's
    /// "unmodelled instruction" message and left the caller blaming itself.
    #[test]
    fn a_faulting_guest_reports_trapped_carrying_the_reason() {
        let mut m = machine_running(&[ILLEGAL]);
        let Status::Trapped { reason, .. } = run(&mut m, 100) else {
            panic!("expected Trapped");
        };
        assert!(!reason.is_empty(), "the fault must name itself");
    }

    /// The accessor the `#[wasm_bindgen]` shell will read, since an enum with fields
    /// cannot cross the JS boundary directly. Tested rather than assumed: mutation
    /// testing found it returning a constant with nothing to notice.
    #[test]
    fn the_instret_accessor_reports_each_variants_own_count() {
        assert_eq!(Status::Running { instret: 7 }.instret(), 7);
        assert_eq!(Status::Halted { instret: 9 }.instret(), 9);
        assert_eq!(
            Status::Trapped { instret: 11, reason: "boom".into() }.instret(),
            11
        );
    }

    /// **The unit is guest instret, not host step-calls** — the distinction the
    /// devlog records losing real time to, and the one this whole module turns on.
    ///
    /// With the block JIT on, a compiled block retires its whole length in a single
    /// `step()`: measured here at 5 instructions per step after warm-up. So an
    /// implementation that counted *steps* would run ~5x past its allowance. The
    /// assertion is deliberately loose at the top end — a slice may overshoot by one
    /// step, and pinning the exact number would be pinning snemu's block-formation
    /// policy rather than this module's behaviour.
    #[test]
    fn the_budget_counts_instructions_not_steps() {
        let loop_body = [
            0x0011_8193u32, // addi x3, x3, 1
            0x0012_0213,    // addi x4, x4, 1
            0x0012_8293,    // addi x5, x5, 1
            0x0013_0313,    // addi x6, x6, 1
            0xFF1F_F06F,    // jal  x0, -16   (back to the top)
        ];
        let mut m = machine_running(&loop_body);
        m.set_block_jit(true);

        let spent = run(&mut m, 12).instret();
        assert!(spent >= 12, "should run its budget out, spent {spent}");
        assert!(
            spent <= 20,
            "spent {spent} for a budget of 12 — that is step-counting, not instret"
        );
    }

    /// A zero budget steps the guest not at all — the property a page would rely on
    /// to pause cleanly.
    #[test]
    fn a_zero_budget_retires_nothing() {
        let mut m = machine_running(&[SPIN]);
        assert_eq!(run(&mut m, 0), Status::Running { instret: 0 });
        assert_eq!(m.instret(), 0, "the guest must not have moved");
    }
}
