//! The discovered snapshot tree (design: `docs/snemu-itest-snapshot-tree-design.md`).
//!
//! Pure, host-testable core: the branch-key model that classifies scenarios by
//! their console-input history, and the stream truncation that lets an
//! observe-only scenario replay against a *shared* forward run without changing
//! its verdict.

use std::collections::BTreeMap;

use protocol::stream::OwnedFrame;

/// One console injection a scenario performed: the guest instret at which it was
/// fed, and the bytes. The `(instret, bytes)` pair is the atom of a
/// [`BranchKey`] — two scenarios coincide up to their first differing injection.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Injection {
    pub instret: u64,
    pub bytes: Vec<u8>,
}

/// A scenario's console-input history: the ordered sequence of injections it
/// performed. The empty key marks an **observe-only** scenario — one that only
/// watches the deterministic guest and never feeds it input, so it shares the
/// entire forward run with its siblings.
#[derive(Clone, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BranchKey {
    injections: Vec<Injection>,
    /// The scenario read UART console output (`wait_for_log`). Such a scenario
    /// depends on the live UART, which a shared *telemetry* stream can't reproduce,
    /// so it is not collapsible even with no input. `#[serde(default)]` so an older
    /// cache without the field still parses.
    #[serde(default)]
    reads_console: bool,
}

impl BranchKey {
    /// Record one console injection at guest `instret`.
    pub fn record(&mut self, instret: u64, bytes: &[u8]) {
        self.injections.push(Injection { instret, bytes: bytes.to_vec() });
    }

    /// Note that the scenario read UART console output — a dependency the shared
    /// telemetry stream can't serve, so it disqualifies the scenario from collapse.
    pub fn mark_console_read(&mut self) {
        self.reads_console = true;
    }

    /// Whether this scenario is a pure telemetry watcher — no input fed, no console
    /// output read. Only such scenarios can replay a shared forward run: their
    /// trajectory is a pure function of the shared boot snapshot, and every signal
    /// they assert on is in the telemetry frame stream.
    pub fn is_observe_only(&self) -> bool {
        self.injections.is_empty() && !self.reads_console
    }

    /// The instret of this scenario's first injection — the **fork point** at which
    /// it (and every sibling on the same workload with the same first-injection
    /// instret) diverges from the shared pre-injection execution. `None` for an
    /// observe-only scenario, which never injects. Scenarios sharing a
    /// `(workload, first_injection_instret)` coincide up to that instret (identical
    /// deterministic guest, no input yet), so one materialised node serves them all.
    pub fn first_injection_instret(&self) -> Option<u64> {
        self.injections.first().map(|i| i.instret)
    }
}

/// The prefix of a recorded `(emit_instret, frame)` stream a scenario budgeted to
/// `budget` guest instructions would see. A live scenario at budget `B` steps the
/// guest to instret `B` and drains every frame emitted up to that point, so its
/// frame set is exactly those with `emit_instret <= B`. Truncating the shared
/// forward run here reproduces that set — the guarantee that makes the zero-input
/// collapse verdict-preserving in *both* directions: a positive scenario still
/// sees its awaited frame, and a negative oracle is not tripped by a bad frame
/// emitted only past its window.
pub fn frames_within(stream: &[(u64, OwnedFrame)], budget: u64) -> Vec<OwnedFrame> {
    stream
        .iter()
        .take_while(|(instret, _)| *instret <= budget)
        .map(|(_, frame)| frame.clone())
        .collect()
}

/// Scenario name → its branch key, the persisted discovery output. A run under
/// `--share-snapshots` reads this to classify each scenario (empty key ⇒
/// collapsible observe-only) and writes it back with every branch key it observed
/// live this run. A `BTreeMap` so the on-disk JSON has a stable, diffable order.
pub type BranchKeyTable = BTreeMap<String, BranchKey>;

/// Serialize a branch-key table to pretty JSON — a diffable sibling of the packing
/// report. Infallible: the table is plain owned data.
pub fn serialize_branch_keys(table: &BranchKeyTable) -> String {
    serde_json::to_string_pretty(table).unwrap_or_else(|_| "{}".to_owned())
}

/// Parse a branch-key table from JSON, or `None` if the text is not a valid table
/// (a corrupt/absent cache is not fatal — the run just falls back to discovering
/// every scenario live this pass).
pub fn parse_branch_keys(json: &str) -> Option<BranchKeyTable> {
    serde_json::from_str(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::stream::OwnedFrame;

    #[test]
    fn a_scenario_that_injects_no_input_is_observe_only() {
        let key = BranchKey::default();
        assert!(key.is_observe_only(), "an empty branch key is observe-only");
    }

    #[test]
    fn a_scenario_that_injects_input_is_not_observe_only() {
        let mut key = BranchKey::default();
        key.record(5_000, b"x");
        assert!(
            !key.is_observe_only(),
            "any recorded injection makes a scenario interactive"
        );
    }

    #[test]
    fn an_observe_only_key_has_no_fork_instret() {
        assert_eq!(BranchKey::default().first_injection_instret(), None);
    }

    #[test]
    fn the_fork_instret_is_the_first_injection() {
        // Two scenarios sharing a workload coincide up to their first injection —
        // that instret is where the shared pre-injection node is materialised and
        // the scenarios fork. Later injections don't move the fork point.
        let mut key = BranchKey::default();
        key.record(9_913_396, b":load primes.st\n");
        key.record(26_570_989, b"quit\n");
        assert_eq!(key.first_injection_instret(), Some(9_913_396));
    }

    #[test]
    fn a_scenario_that_reads_the_console_is_not_observe_only() {
        // A `wait_for_log` scenario watches UART console output, not the telemetry
        // stream — the shared frame stream can't serve it, so it must not collapse
        // even though it injects no input.
        let mut key = BranchKey::default();
        key.mark_console_read();
        assert!(
            !key.is_observe_only(),
            "reading console output makes a scenario un-collapsible"
        );
    }

    /// A recorded stream, tagged by emit instret. `Dropped { count }` stands in
    /// for a frame; its count is a positional tag so truncation is checkable
    /// without frame `PartialEq` gymnastics.
    fn tagged_stream() -> Vec<(u64, OwnedFrame)> {
        vec![
            (100, OwnedFrame::Dropped { count: 1 }),
            (200, OwnedFrame::Dropped { count: 2 }),
            (300, OwnedFrame::Dropped { count: 3 }),
        ]
    }

    fn counts(frames: &[OwnedFrame]) -> Vec<u32> {
        frames
            .iter()
            .map(|f| match f {
                OwnedFrame::Dropped { count } => *count,
                other => panic!("unexpected frame: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn frames_within_keeps_frames_emitted_at_or_before_the_budget() {
        // A live scenario at budget B sees exactly the frames the guest emits
        // within instret [0, B]. Truncating the shared stream to B must
        // reproduce that set — no more (which could trip a negative oracle), no
        // less (which could fail a positive one).
        let within = frames_within(&tagged_stream(), 200);
        assert_eq!(counts(&within), vec![1, 2], "frame at instret 300 is past the budget");
    }

    #[test]
    fn frames_within_a_budget_below_every_frame_is_empty() {
        let within = frames_within(&tagged_stream(), 50);
        assert!(within.is_empty(), "no frame is emitted by instret 50");
    }

    #[test]
    fn frames_within_a_budget_past_every_frame_keeps_them_all() {
        let within = frames_within(&tagged_stream(), 10_000);
        assert_eq!(counts(&within), vec![1, 2, 3]);
    }

    #[test]
    fn a_branch_key_table_round_trips_through_json() {
        // Discovery writes each scenario's branch key one run; a later run reads
        // them back to decide what to collapse. The persisted form must survive the
        // round trip exactly — a mis-parsed key would mis-classify a scenario.
        let mut table = BranchKeyTable::default();
        table.insert("observer".to_owned(), BranchKey::default());
        let mut interactive = BranchKey::default();
        interactive.record(1_234, b"\n");
        interactive.record(5_678, b"quit\n");
        table.insert("repl".to_owned(), interactive);

        let json = serialize_branch_keys(&table);
        let parsed = parse_branch_keys(&json).expect("valid JSON round-trips");
        assert_eq!(parsed, table);
        assert!(parsed["observer"].is_observe_only());
        assert!(!parsed["repl"].is_observe_only());
    }

    #[test]
    fn parsing_garbage_is_an_error_not_a_panic() {
        assert!(parse_branch_keys("not json").is_none());
    }
}
