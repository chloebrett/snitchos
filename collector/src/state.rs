//! Stateful frame observer.
//!
//! As frames stream in, `State` accumulates the kernel's view of the
//! world: timebase, name table, metric types, currently-open spans,
//! latest counter/gauge values. When a `SpanEnd` matches a `SpanStart`,
//! `handle` returns a `CompletedSpan` ready for export to Tempo.

use std::collections::HashMap;
use std::time::SystemTime;

use protocol::{Frame, MetricKind, SpanId, StringId};

/// Injectable source of host wall-clock nanoseconds since epoch.
pub trait WallClock: Send {
    fn now_ns(&self) -> u128;
}

/// Production impl — reads `SystemTime::now()`.
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    #[cfg_attr(test, mutants::skip)] // reads the real wall clock — not unit-testable
    fn now_ns(&self) -> u128 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    }
}

/// Test double — returns the pinned value supplied at construction.
#[cfg(test)]
pub(crate) struct FakeWallClock(pub u128);

#[cfg(test)]
impl WallClock for FakeWallClock {
    fn now_ns(&self) -> u128 {
        self.0
    }
}

/// A span completed by matching a `SpanEnd` to a remembered `SpanStart`.
/// Carries enough info to build an OTLP span at export time.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedSpan {
    pub name: String,
    pub span_id: u64,
    pub parent_span_id: u64,
    /// Nanoseconds since UNIX epoch — anchored to host wall-clock at
    /// the first frame of the session.
    pub start_time_ns: u128,
    pub end_time_ns: u128,
    /// Task that opened this span (0 = main/boot task). Mapped to a
    /// human-readable name via the State's `thread_names` table; the
    /// export path materialises it as a `thread.name` OTLP attribute.
    pub task_id: u32,
    /// Cached thread name at `SpanEnd` time. `None` if no
    /// `ThreadRegister` for this `task_id` arrived before `SpanEnd`.
    pub thread_name: Option<String>,
    /// Hart the span opened on (from `SpanStart.hart_id`). The export
    /// path surfaces it as the `host.cpu_id` OTLP attribute so Tempo
    /// can slice traces by CPU.
    pub hart_id: u8,
}

/// Wall-clock + tick anchor for the current kernel session. Set on
/// `Hello`; reset if a new `Hello` arrives (kernel restart).
struct SessionAnchor {
    /// Host wall-clock nanos since epoch at the moment we received
    /// (well, processed) the first frame of this session.
    wallclock_ns: u128,
    /// The kernel-side `t` value we treat as `wallclock_ns`. Frames
    /// with `t < first_t` (pre-init burst) land slightly *before*
    /// `wallclock_ns` in real time — documented quirk.
    first_t: u64,
}

/// Open span: `SpanStart` seen, `SpanEnd` not yet.
struct OpenSpan {
    parent: SpanId,
    name_id: StringId,
    start_t: u64,
    task_id: u32,
    hart_id: u8,
}

/// A single histogram metric — buckets of observations.
///
/// Boundaries are inclusive upper bounds; `bucket[i]` counts the
/// observations whose value is `<= boundaries[i]` and `> boundaries[i-1]`.
/// On Prometheus exposition, we convert to cumulative counts as the
/// format expects.
#[derive(Debug, Default)]
pub struct Histogram {
    /// Counts in each bucket (non-cumulative).
    pub buckets: Vec<u64>,
    /// Observations exceeding the highest boundary (the `+Inf` bucket).
    pub inf_count: u64,
    /// Sum of all observed values.
    pub sum: u64,
    /// Total observations.
    pub count: u64,
}

impl Histogram {
    pub fn observe(&mut self, value: u64, boundaries: &[u64]) {
        if self.buckets.len() != boundaries.len() {
            self.buckets = vec![0; boundaries.len()];
        }
        let idx = boundaries.iter().position(|&b| value <= b);
        match idx {
            Some(i) => self.buckets[i] += 1,
            None => self.inf_count += 1,
        }
        self.sum = self.sum.saturating_add(value);
        self.count += 1;
    }
}

pub struct State {
    clock: Box<dyn WallClock>,
    timebase_hz: u64,
    anchor: Option<SessionAnchor>,
    strings: HashMap<u32, String>,
    metric_kinds: HashMap<u32, MetricKind>,
    open_spans: HashMap<u64, OpenSpan>,
    /// `task_id` → human-readable thread name. Populated by
    /// `ThreadRegister`; consulted at `SpanEnd` to tag the completed
    /// span with its `thread.name`.
    thread_names: HashMap<u32, String>,
    /// Last-seen value per counter/gauge metric, keyed by
    /// `(name_id, hart_id)` so same-named metrics from different harts
    /// don't clobber each other. Histograms go in `histograms` instead.
    pub metric_values: HashMap<(u32, u8), i64>,
    /// Histogram state per metric, keyed by `(name_id, hart_id)`.
    pub histograms: HashMap<(u32, u8), Histogram>,
    /// Have we seen the warning-about-missing-Hello yet? Avoids
    /// spamming once per frame.
    warned_no_hello: bool,
}

impl State {
    pub fn new(clock: impl WallClock + 'static) -> Self {
        Self {
            clock: Box::new(clock),
            timebase_hz: 0,
            anchor: None,
            strings: HashMap::new(),
            metric_kinds: HashMap::new(),
            open_spans: HashMap::new(),
            thread_names: HashMap::new(),
            metric_values: HashMap::new(),
            histograms: HashMap::new(),
            warned_no_hello: false,
        }
    }

    /// Default bucket boundaries for histogram observations. Exponential
    /// from 100 ticks up to 1 million ticks, which spans the realistic
    /// range for IRQ duration (typically hundreds of ticks) up to
    /// "something is very wrong" territory.
    pub const HISTOGRAM_BOUNDS: &'static [u64] = &[
        100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 100_000, 1_000_000,
    ];

    /// Observe a frame. Returns a `CompletedSpan` if this frame closed
    /// out a previously-open span.
    pub fn handle(&mut self, frame: &Frame<'_>) -> Option<CompletedSpan> {
        // Hello is the contract: it must arrive first. Without it we
        // can't anchor timestamps — every span we export would land
        // at Unix epoch 0 (1970-01-01) and Grafana's recent-time view
        // would never find them. Drop frames silently after warning
        // once; the user almost certainly started the collector after
        // the kernel was already running.
        if self.anchor.is_none() && !matches!(frame, Frame::Hello { .. }) {
            if !self.warned_no_hello {
                eprintln!(
                    "collector: WARNING — received {frame:?} before Hello. \
                     Dropping. Stop QEMU and restart the kernel after the \
                     collector connects (use `cargo xtask boot` first, then \
                     `cargo xtask collect`).",
                );
                self.warned_no_hello = true;
            }
            return None;
        }

        match frame {
            Frame::Hello {
                timebase_hz,
                protocol_version: _,
            } => {
                self.reset_session();
                self.timebase_hz = *timebase_hz;
                self.anchor = Some(SessionAnchor {
                    wallclock_ns: self.clock.now_ns(),
                    first_t: 0,
                });
                None
            }
            Frame::StringRegister { id, value } => {
                self.strings.insert(id.0, (*value).to_string());
                None
            }
            Frame::MetricRegister { name_id, kind } => {
                self.metric_kinds.insert(name_id.0, *kind);
                None
            }
            Frame::SpanStart {
                id,
                parent,
                name_id,
                t,
                task_id,
                hart_id,
            } => {
                self.advance_anchor(*t);
                self.open_spans.insert(
                    id.0,
                    OpenSpan {
                        parent: *parent,
                        name_id: *name_id,
                        start_t: *t,
                        task_id: *task_id,
                        hart_id: *hart_id,
                    },
                );
                None
            }
            Frame::SpanEnd { id, t } => {
                self.advance_anchor(*t);
                let open = self.open_spans.remove(&id.0)?;
                let name = self
                    .strings
                    .get(&open.name_id.0)
                    .cloned()
                    .unwrap_or_else(|| format!("<unknown name_id={}>", open.name_id.0));
                let thread_name = self.thread_names.get(&open.task_id).cloned();
                Some(CompletedSpan {
                    name,
                    span_id: id.0,
                    parent_span_id: open.parent.0,
                    start_time_ns: self.tick_to_wall_ns(open.start_t),
                    end_time_ns: self.tick_to_wall_ns(*t),
                    task_id: open.task_id,
                    thread_name,
                    hart_id: open.hart_id,
                })
            }
            // Kept distinct from the `Dropped` arm despite the identical
            // body: `Event` is a reserved wire slot with no emitter yet
            // (first one ~v0.8 — see protocol's `Frame::Event` doc),
            // parked pending OTLP span-event wiring, whereas `Dropped`
            // genuinely has nothing to export.
            #[allow(clippy::match_same_arms, reason = "distinct intent; see comment")]
            Frame::Event { .. } => None, // reserved: OTLP span-event, no emitter yet
            Frame::Metric { name_id, value, t, hart_id } => {
                self.advance_anchor(*t);
                // Route histogram-kind metrics to the histogram table;
                // counters/gauges to the value table. Keyed by
                // (name_id, hart_id) — the metric kind, however, is a
                // per-name property (MetricRegister carries no hart_id).
                let key = (name_id.0, *hart_id);
                match self.metric_kinds.get(&name_id.0).copied() {
                    Some(MetricKind::Histogram) => {
                        let hist = self.histograms.entry(key).or_default();
                        let v = (*value).max(0) as u64;
                        hist.observe(v, Self::HISTOGRAM_BOUNDS);
                    }
                    _ => {
                        self.metric_values.insert(key, *value);
                    }
                }
                None
            }
            Frame::Dropped { count: _ } => None,
            Frame::ThreadRegister { id, name } => {
                self.thread_names.insert(*id, (*name).to_string());
                None
            }
            Frame::ContextSwitch { t, .. } => {
                // Not yet wired to OTLP — surfaced as scheduler
                // metrics + a future trace-event API. Advance the
                // anchor so any timestamp-based downstream logic sees
                // continued progress.
                self.advance_anchor(*t);
                None
            }
            Frame::HartRegister { .. } => {
                // v0.6: the hart is observable on both telemetry kinds —
                // spans carry `host.cpu_id` from `SpanStart.hart_id` (see
                // otlp::span_attributes), and metrics carry a `hart="N"`
                // Prometheus label keyed off `Metric.hart_id` (see
                // prom::format_metrics). `HartRegister`'s `role`
                // (Boot/Worker) is the one bit still unsurfaced; numeric
                // ids remain valid in the meantime.
                None
            }
            Frame::CapEvent { t, .. } => {
                // v0.7b: the authority event is on the wire (the itest reads
                // it directly off the socket). Host-side reconstruction of the
                // capability derivation tree from these events — and the
                // Grafana node-graph view — is v0.8, once transfer/attenuation
                // produce real parent→child edges. Advance the anchor for
                // timestamp continuity in the meantime.
                self.advance_anchor(*t);
                None
            }
            Frame::SyscallRefused { t, .. } => {
                // The refusal event is on the wire (the itest reads it directly).
                // Surfacing it as a Prometheus `syscall_refused_total{reason}`
                // counter / OTLP span-event is a follow-on; for now just keep
                // the timeline anchored.
                self.advance_anchor(*t);
                None
            }
            Frame::Log { t, .. } => {
                // Userspace stdout line. On the wire (the itest reads it
                // directly); surfacing it via Loki / an OTLP log record is a
                // follow-on. Keep the timeline anchored.
                self.advance_anchor(*t);
                None
            }
        }
    }

    /// Lookup the kind for a given metric. Returns `None` if no
    /// `MetricRegister` has been seen for this id yet.
    pub fn metric_kind(&self, name_id: u32) -> Option<MetricKind> {
        self.metric_kinds.get(&name_id).copied()
    }

    /// Lookup the name string for a given id.
    pub fn name(&self, id: u32) -> Option<&str> {
        self.strings.get(&id).map(String::as_str)
    }

    fn reset_session(&mut self) {
        self.strings.clear();
        self.metric_kinds.clear();
        self.open_spans.clear();
        self.metric_values.clear();
        self.histograms.clear();
        self.warned_no_hello = false;
    }

    /// Update `first_t` if we're seeing the smallest `t` yet — pre-init
    /// spans may arrive after Hello with `t < hello.t`.
    fn advance_anchor(&mut self, t: u64) {
        if let Some(anchor) = self.anchor.as_mut()
            && (anchor.first_t == 0 || t < anchor.first_t) {
                anchor.first_t = t;
            }
    }

    /// Convert a kernel-side tick value to host wall-clock nanoseconds
    /// since epoch, using the session anchor + timebase.
    fn tick_to_wall_ns(&self, t: u64) -> u128 {
        let Some(anchor) = self.anchor.as_ref() else {
            return 0;
        };
        ticks_to_wall_ns(t, anchor.first_t, self.timebase_hz, anchor.wallclock_ns)
    }
}

/// Pure tick-to-nanosecond conversion. `wallclock_ns` is the host wall-clock
/// at `first_t`; returns the wall-clock nanoseconds corresponding to `t`.
fn ticks_to_wall_ns(t: u64, first_t: u64, timebase_hz: u64, wallclock_ns: u128) -> u128 {
    if timebase_hz == 0 {
        return wallclock_ns;
    }
    let delta_ns =
        u128::from(t.saturating_sub(first_t)) * 1_000_000_000 / u128::from(timebase_hz);
    wallclock_ns + delta_ns
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_sets_timebase() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        assert_eq!(s.timebase_hz, 10_000_000);
        assert!(s.anchor.is_some());
    }

    /// Helper: build a State pre-anchored by sending Hello.
    fn anchored_state() -> State {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        s
    }

    #[test]
    fn string_register_inserts() {
        let mut s = anchored_state();
        s.handle(&Frame::StringRegister {
            id: StringId(3),
            value: "kernel.boot",
        });
        assert_eq!(s.name(3), Some("kernel.boot"));
    }

    #[test]
    fn metric_register_inserts_kind() {
        let mut s = anchored_state();
        s.handle(&Frame::MetricRegister {
            name_id: StringId(7),
            kind: MetricKind::Counter,
        });
        assert_eq!(s.metric_kind(7), Some(MetricKind::Counter));
    }

    #[test]
    fn frames_before_hello_are_dropped() {
        let mut s = State::new(FakeWallClock(0));
        // No Hello yet — these should be ignored.
        s.handle(&Frame::StringRegister {
            id: StringId(0),
            value: "should-be-ignored",
        });
        s.handle(&Frame::Metric {
            name_id: StringId(0),
            value: 42,
            t: 100,
            hart_id: 0,
        });
        assert!(s.name(0).is_none());
        assert!(s.metric_values.is_empty());
    }

    #[test]
    fn span_end_yields_completed_span() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        s.handle(&Frame::StringRegister {
            id: StringId(1),
            value: "kernel.boot",
        });
        s.handle(&Frame::SpanStart {
            id: SpanId(1),
            parent: SpanId(0),
            name_id: StringId(1),
            t: 100,
            task_id: 0,
            hart_id: 0,
        });

        // 1ms later at 10MHz = 10_000 ticks.
        let result = s.handle(&Frame::SpanEnd {
            id: SpanId(1),
            t: 10_100,
        });

        let span = result.expect("SpanEnd should yield a CompletedSpan");
        assert_eq!(span.name, "kernel.boot");
        assert_eq!(span.span_id, 1);
        assert_eq!(span.parent_span_id, 0);
        // 1ms duration in nanos.
        let duration_ns = span.end_time_ns - span.start_time_ns;
        assert_eq!(duration_ns, 1_000_000);
    }

    #[test]
    fn completed_span_carries_originating_hart_id() {
        // v0.6: the wire stamps each SpanStart with the hart it opened
        // on. The collector must carry that through to the CompletedSpan
        // so the export path can surface it as `host.cpu_id`.
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        s.handle(&Frame::StringRegister {
            id: StringId(1),
            value: "task_b.tick",
        });
        s.handle(&Frame::SpanStart {
            id: SpanId(1),
            parent: SpanId(0),
            name_id: StringId(1),
            t: 100,
            task_id: 3,
            hart_id: 1,
        });

        let span = s
            .handle(&Frame::SpanEnd { id: SpanId(1), t: 200 })
            .expect("SpanEnd should yield a CompletedSpan");
        assert_eq!(span.hart_id, 1);
    }

    #[test]
    fn unmatched_span_end_returns_none() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        let result = s.handle(&Frame::SpanEnd {
            id: SpanId(99),
            t: 100,
        });
        assert!(result.is_none());
    }

    #[test]
    fn metric_updates_value() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        s.handle(&Frame::MetricRegister {
            name_id: StringId(5),
            kind: MetricKind::Counter,
        });
        s.handle(&Frame::Metric {
            name_id: StringId(5),
            value: 42,
            t: 100,
            hart_id: 0,
        });
        assert_eq!(s.metric_values.get(&(5, 0)), Some(&42));
    }

    #[test]
    fn same_named_metric_from_two_harts_stays_distinct() {
        // The whole point of `hart_id` on `Metric`: two harts emitting
        // the same counter name must not clobber each other. State keys
        // by (name_id, hart_id), so both values survive side by side.
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        s.handle(&Frame::MetricRegister {
            name_id: StringId(5),
            kind: MetricKind::Counter,
        });
        s.handle(&Frame::Metric { name_id: StringId(5), value: 42, t: 100, hart_id: 0 });
        s.handle(&Frame::Metric { name_id: StringId(5), value: 99, t: 100, hart_id: 1 });
        assert_eq!(s.metric_values.get(&(5, 0)), Some(&42));
        assert_eq!(s.metric_values.get(&(5, 1)), Some(&99));
    }

    #[test]
    fn second_hello_resets_session_state() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "x" });
        s.handle(&Frame::MetricRegister { name_id: StringId(2), kind: MetricKind::Counter });
        s.handle(&Frame::Metric { name_id: StringId(2), value: 42, t: 100, hart_id: 0 });

        // Kernel restarts — second Hello must clear all per-session state.
        s.handle(&Frame::Hello { timebase_hz: 20_000_000, protocol_version: 1 });

        assert!(s.name(1).is_none(), "string table should be cleared on Hello");
        assert!(s.metric_kind(2).is_none(), "metric kinds should be cleared on Hello");
        assert!(s.metric_values.is_empty(), "metric values should be cleared on Hello");
    }

    #[test]
    fn span_timestamps_anchored_to_hello_wallclock() {
        // wallclock = 1s at Hello time; first frame sets first_t=100.
        // start should be exactly at anchor; end should be 1ms later.
        let mut s = State::new(FakeWallClock(1_000_000_000));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "x" });
        s.handle(&Frame::SpanStart { id: SpanId(1), parent: SpanId(0), name_id: StringId(1), t: 100, task_id: 0, hart_id: 0 });
        let span = s.handle(&Frame::SpanEnd { id: SpanId(1), t: 10_100 }).unwrap();
        assert_eq!(span.start_time_ns, 1_000_000_000);
        assert_eq!(span.end_time_ns,   1_001_000_000);
    }

    #[test]
    fn advance_anchor_tracks_minimum_tick() {
        // First span arrives at t=1_000 (sets first_t=1_000).
        // Second span arrives at t=100 (should pull first_t down to 100).
        // We verify by checking end_time_ns of the second span, which
        // depends on the correct first_t being 100, not 1_000.
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "x" });
        s.handle(&Frame::SpanStart { id: SpanId(1), parent: SpanId(0), name_id: StringId(1), t: 1_000, task_id: 0, hart_id: 0 });
        s.handle(&Frame::SpanStart { id: SpanId(2), parent: SpanId(0), name_id: StringId(1), t: 100, task_id: 0, hart_id: 0 });
        let span = s.handle(&Frame::SpanEnd { id: SpanId(2), t: 600 }).unwrap();
        // first_t=100: start=(100-100)/10MHz=0, end=(600-100)/10MHz=50µs
        assert_eq!(span.start_time_ns, 0);
        assert_eq!(span.end_time_ns, 50_000);
    }

    #[test]
    fn ticks_to_wall_ns_zero_delta() {
        // t == first_t: no time has passed, result is exactly wallclock_ns
        assert_eq!(ticks_to_wall_ns(100, 100, 10_000_000, 1_000_000_000), 1_000_000_000);
    }

    #[test]
    fn ticks_to_wall_ns_positive_delta() {
        // 10_000 ticks at 10 MHz = 1 ms = 1_000_000 ns; wallclock = 0
        assert_eq!(ticks_to_wall_ns(10_100, 100, 10_000_000, 0), 1_000_000);
    }

    #[test]
    fn ticks_to_wall_ns_adds_to_wallclock() {
        // wallclock = 5 s; delta = 1 ms → result = 5.001 s
        assert_eq!(
            ticks_to_wall_ns(10_100, 100, 10_000_000, 5_000_000_000),
            5_001_000_000,
        );
    }

    #[test]
    fn ticks_to_wall_ns_zero_timebase_returns_wallclock() {
        assert_eq!(ticks_to_wall_ns(999, 0, 0, 42), 42);
    }

    #[test]
    fn histogram_observe_routes_to_first_bucket() {
        let mut h = Histogram::default();
        h.observe(50, State::HISTOGRAM_BOUNDS);
        assert_eq!(h.buckets[0], 1);
        assert_eq!(h.count, 1);
        assert_eq!(h.sum, 50);
    }

    #[test]
    fn histogram_observe_on_boundary_lands_in_that_bucket() {
        let mut h = Histogram::default();
        h.observe(100, State::HISTOGRAM_BOUNDS);
        assert_eq!(h.buckets[0], 1);
        assert_eq!(h.inf_count, 0);
    }

    #[test]
    fn histogram_observe_exceeds_all_bounds_goes_to_inf() {
        let mut h = Histogram::default();
        h.observe(2_000_000, State::HISTOGRAM_BOUNDS);
        assert_eq!(h.buckets.iter().sum::<u64>(), 0);
        assert_eq!(h.inf_count, 1);
        assert_eq!(h.count, 1);
        assert_eq!(h.sum, 2_000_000);
    }

    #[test]
    fn histogram_accumulates_sum_and_count_across_observations() {
        let mut h = Histogram::default();
        h.observe(50, State::HISTOGRAM_BOUNDS);
        h.observe(200, State::HISTOGRAM_BOUNDS);
        assert_eq!(h.count, 2);
        assert_eq!(h.sum, 250);
        assert_eq!(h.buckets[0], 1);
        assert_eq!(h.buckets[1], 1);
    }

    #[test]
    fn histogram_metric_routes_to_histogram_table_not_values() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Histogram });
        s.handle(&Frame::Metric { name_id: StringId(1), value: 50, t: 100, hart_id: 0 });
        assert!(!s.metric_values.contains_key(&(1, 0)));
        assert!(s.histograms.contains_key(&(1, 0)));
    }

    #[test]
    fn pre_init_spans_land_before_anchor() {
        // Hello arrives with t=100. A pre-init span had t=10. Its
        // wall-clock should be *before* the Hello anchor.
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        // first_t is now 0; first real frame updates it to its own t.
        s.handle(&Frame::SpanStart {
            id: SpanId(1),
            parent: SpanId(0),
            name_id: StringId(0),
            t: 100,
            task_id: 0,
            hart_id: 0,
        });
        s.handle(&Frame::StringRegister {
            id: StringId(0),
            value: "x",
        });
        // Now end with a smaller t — pre-init quirk simulation. In
        // practice the *start* arrives with a smaller t than later
        // frames, but the math is the same.
        let result = s.handle(&Frame::SpanEnd {
            id: SpanId(1),
            t: 50,
        });
        let span = result.unwrap();
        // end is before start in wall-clock terms because t went
        // backwards. start_time_ns > end_time_ns.
        assert!(span.start_time_ns > span.end_time_ns);
    }
}
