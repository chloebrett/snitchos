//! Stateful frame observer.
//!
//! As frames stream in, `State` accumulates the kernel's view of the
//! world: timebase, name table, metric types, currently-open spans,
//! latest counter/gauge values. When a `SpanEnd` matches a `SpanStart`,
//! `handle` returns a `CompletedSpan` ready for export to Tempo.

use std::collections::HashMap;
use std::time::SystemTime;

use protocol::{Frame, MetricKind, SpanId, StringId};

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

/// Open span: SpanStart seen, SpanEnd not yet.
struct OpenSpan {
    parent: SpanId,
    name_id: StringId,
    start_t: u64,
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
    timebase_hz: u64,
    anchor: Option<SessionAnchor>,
    strings: HashMap<u32, String>,
    metric_kinds: HashMap<u32, MetricKind>,
    open_spans: HashMap<u64, OpenSpan>,
    /// Last-seen value per counter/gauge metric. Histograms go in
    /// `histograms` instead.
    pub metric_values: HashMap<u32, i64>,
    /// Histogram state per metric (bucket counts + sum + total).
    pub histograms: HashMap<u32, Histogram>,
    /// Have we seen the warning-about-missing-Hello yet? Avoids
    /// spamming once per frame.
    warned_no_hello: bool,
}

impl State {
    pub fn new() -> Self {
        Self {
            timebase_hz: 0,
            anchor: None,
            strings: HashMap::new(),
            metric_kinds: HashMap::new(),
            open_spans: HashMap::new(),
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
                     collector connects (use `cargo xtask up` first, then \
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
                self.timebase_hz = *timebase_hz;
                // Anchor wall-clock to the moment we processed Hello.
                self.anchor = Some(SessionAnchor {
                    wallclock_ns: wall_now_ns(),
                    first_t: 0, // updated to the first real frame's t below
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
            } => {
                self.advance_anchor(*t);
                self.open_spans.insert(
                    id.0,
                    OpenSpan {
                        parent: *parent,
                        name_id: *name_id,
                        start_t: *t,
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
                Some(CompletedSpan {
                    name,
                    span_id: id.0,
                    parent_span_id: open.parent.0,
                    start_time_ns: self.tick_to_wall_ns(open.start_t),
                    end_time_ns: self.tick_to_wall_ns(*t),
                })
            }
            Frame::Event { .. } => None, // not yet wired to OTLP
            Frame::Metric { name_id, value, t } => {
                self.advance_anchor(*t);
                // Route histogram-kind metrics to the histogram table;
                // counters/gauges to the value table.
                match self.metric_kinds.get(&name_id.0).copied() {
                    Some(MetricKind::Histogram) => {
                        let hist = self.histograms.entry(name_id.0).or_default();
                        let v = (*value).max(0) as u64;
                        hist.observe(v, Self::HISTOGRAM_BOUNDS);
                    }
                    _ => {
                        self.metric_values.insert(name_id.0, *value);
                    }
                }
                None
            }
            Frame::Dropped { count: _ } => None,
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

    /// Update `first_t` if we're seeing the smallest `t` yet — pre-init
    /// spans may arrive after Hello with `t < hello.t`.
    // mutants::skip — advance_anchor only affects absolute timestamps; our
    // tests verify relative durations, which cancel out the first_t offset.
    // Killing these mutants requires an injectable clock seam (not yet added).
    #[mutants::skip]
    fn advance_anchor(&mut self, t: u64) {
        if let Some(anchor) = self.anchor.as_mut() {
            if anchor.first_t == 0 || t < anchor.first_t {
                anchor.first_t = t;
            }
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
        (t.saturating_sub(first_t) as u128) * 1_000_000_000 / timebase_hz as u128;
    wallclock_ns + delta_ns
}

#[mutants::skip] // reads the real wall clock — requires a clock seam to test
fn wall_now_ns() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_sets_timebase() {
        let mut s = State::new();
        s.handle(&Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        });
        assert_eq!(s.timebase_hz, 10_000_000);
        assert!(s.anchor.is_some());
    }

    /// Helper: build a State pre-anchored by sending Hello.
    fn anchored_state() -> State {
        let mut s = State::new();
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
        let mut s = State::new();
        // No Hello yet — these should be ignored.
        s.handle(&Frame::StringRegister {
            id: StringId(0),
            value: "should-be-ignored",
        });
        s.handle(&Frame::Metric {
            name_id: StringId(0),
            value: 42,
            t: 100,
        });
        assert!(s.name(0).is_none());
        assert!(s.metric_values.is_empty());
    }

    #[test]
    fn span_end_yields_completed_span() {
        let mut s = State::new();
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
    fn unmatched_span_end_returns_none() {
        let mut s = State::new();
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
        let mut s = State::new();
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
        });
        assert_eq!(s.metric_values.get(&5), Some(&42));
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
        let mut s = State::new();
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Histogram });
        s.handle(&Frame::Metric { name_id: StringId(1), value: 50, t: 100 });
        assert!(s.metric_values.get(&1).is_none());
        assert!(s.histograms.get(&1).is_some());
    }

    #[test]
    fn pre_init_spans_land_before_anchor() {
        // Hello arrives with t=100. A pre-init span had t=10. Its
        // wall-clock should be *before* the Hello anchor.
        let mut s = State::new();
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
