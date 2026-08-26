//! Metric samples, kept per metric so one cannot crowd out another.
//!
//! [`crate::store`] retains *frames* under a durable/windowed split, which is right
//! for the folds and wrong for series: its window is shared, and this guest emits
//! thousands of `ContextSwitch` frames a second. A metric's history would be evicted
//! by traffic that has nothing to do with it — measured, one heartbeat's frames fill
//! a 400-entry window entirely.
//!
//! So samples get their own retention, bounded **per metric name**. A metric emitted
//! every heartbeat and one emitted rarely each keep their own history, and neither
//! can starve the other.

use protocol::stream::OwnedFrame;
use protocol::{MetricKind, StringId};
use std::collections::HashMap;
use std::collections::VecDeque;

/// Samples kept per metric.
///
/// Per metric, so the number is a *history depth* rather than a budget several
/// hundred metrics compete over. This guest emits ~60 metrics per heartbeat, so 600
/// points is roughly a minute and a half of history each at the rate observed — long
/// enough to see a trend, short enough that folding it costs nothing.
pub const MAX_POINTS_PER_SERIES: usize = 600;

/// One metric's history: `(guest time, value)` in arrival order.
///
/// `Serialize` because this crosses to the page as JSON, and the shape is a contract
/// with TypeScript, which has no compiler to notice a renamed field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Series {
    pub name: String,
    /// From `MetricRegister`, or `None` if the guest has not described it yet.
    ///
    /// A chart needs this to know whether a counter's *rate* or a gauge's *value* is
    /// the interesting quantity — so it is carried rather than inferred from the
    /// numbers, which cannot distinguish a flat counter from a steady gauge.
    pub kind: Option<MetricKind>,
    pub points: Vec<(u64, i64)>,
}

/// Per-metric sample retention.
#[derive(Debug, Default)]
pub struct SeriesStore {
    /// First-seen order, so a chart's colour assignment is stable as metrics appear.
    /// Colour follows the entity, never its rank.
    order: Vec<String>,
    points: HashMap<String, VecDeque<(u64, i64)>>,
    /// Kinds by `StringId` rather than name: a `MetricRegister` can arrive before the
    /// name is interned, and often before any sample.
    kinds: HashMap<StringId, MetricKind>,
    /// The name a `StringId` resolved to, so a kind registered under an id can find
    /// its series later.
    named: HashMap<StringId, String>,
}

impl SeriesStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `frame` if it is a metric or a metric registration.
    ///
    /// `resolve` is the decoder's intern table. A sample whose name will not resolve
    /// is **not** recorded: an unlabelled series cannot be charted, and inventing a
    /// placeholder would put a fiction on an axis.
    pub fn observe(&mut self, frame: &OwnedFrame, resolve: &dyn Fn(StringId) -> Option<String>) {
        match *frame {
            OwnedFrame::MetricRegister { name_id, kind, .. } => {
                self.kinds.insert(name_id, kind);
            }
            OwnedFrame::Metric { name_id, value, t, .. } => {
                let Some(name) = resolve(name_id) else { return };
                self.named.insert(name_id, name.clone());

                let points = self.points.entry(name.clone()).or_insert_with(|| {
                    self.order.push(name);
                    VecDeque::new()
                });
                points.push_back((t, value));
                // One sample per call, so at most one can ever need evicting — a loop
                // here would be termination that depends on the comparison being
                // right, which has produced a browser-freezing spin three times in
                // this crate already.
                if points.len() > MAX_POINTS_PER_SERIES {
                    points.pop_front();
                }
            }
            _ => {}
        }
    }

    /// Every series, in first-seen order.
    #[must_use]
    pub fn series(&self) -> Vec<Series> {
        let kind_by_name: HashMap<&String, MetricKind> = self
            .named
            .iter()
            .filter_map(|(id, name)| self.kinds.get(id).map(|kind| (name, *kind)))
            .collect();

        self.order
            .iter()
            .filter_map(|name| {
                self.points.get(name).map(|points| Series {
                    name: name.clone(),
                    kind: kind_by_name.get(name).copied(),
                    points: points.iter().copied().collect(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{SeriesStore, MAX_POINTS_PER_SERIES};
    use protocol::stream::OwnedFrame;
    use protocol::{MetricKind, StringId};

    fn metric(name_id: u32, value: i64, t: u64) -> OwnedFrame {
        OwnedFrame::Metric { name_id: StringId(name_id), value, t, hart_id: 0 }
    }

    fn register(name_id: u32, kind: MetricKind) -> OwnedFrame {
        OwnedFrame::MetricRegister { name_id: StringId(name_id), kind, task_id: 0 }
    }

    /// Names come from the intern table, which the decoder owns.
    fn names() -> impl Fn(StringId) -> Option<String> {
        |id| Some(format!("metric.{}", id.0))
    }

    #[test]
    fn samples_accumulate_per_metric() {
        let mut s = SeriesStore::new();
        s.observe(&metric(1, 10, 100), &names());
        s.observe(&metric(1, 11, 200), &names());

        let series = s.series();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].points, vec![(100, 10), (200, 11)]);
    }

    #[test]
    fn different_metrics_are_separate_series() {
        let mut s = SeriesStore::new();
        s.observe(&metric(1, 1, 10), &names());
        s.observe(&metric(2, 2, 10), &names());

        assert_eq!(s.series().len(), 2);
    }

    /// **The reason this is not just another frame bucket.** A chatty metric must not
    /// evict a quiet one — under a shared window it would, and the quiet metric is
    /// often the interesting one.
    #[test]
    fn a_busy_metric_does_not_evict_a_quiet_one() {
        let mut s = SeriesStore::new();
        s.observe(&metric(1, 42, 1), &names());
        for i in 0..(MAX_POINTS_PER_SERIES as u64 * 3) {
            s.observe(&metric(2, i as i64, i), &names());
        }

        let quiet = s.series().into_iter().find(|x| x.name == "metric.1").expect("kept");
        assert_eq!(quiet.points, vec![(1, 42)], "the quiet metric kept its history");
    }

    /// Each series is bounded on its own, oldest-first.
    #[test]
    fn a_series_drops_its_oldest_points_when_full() {
        let mut s = SeriesStore::new();
        for i in 0..(MAX_POINTS_PER_SERIES as u64 + 5) {
            s.observe(&metric(1, i as i64, i), &names());
        }

        let series = s.series();
        assert_eq!(series[0].points.len(), MAX_POINTS_PER_SERIES);
        assert_eq!(series[0].points[0].0, 5, "the first five were dropped");
    }

    /// The kind decides how a chart renders a series — a counter's rate, a gauge's
    /// value — so it is carried rather than guessed from the numbers.
    #[test]
    fn a_registered_kind_is_carried_with_the_series() {
        let mut s = SeriesStore::new();
        s.observe(&register(1, MetricKind::Counter), &names());
        s.observe(&metric(1, 5, 10), &names());

        assert_eq!(s.series()[0].kind, Some(MetricKind::Counter));
    }

    /// Registration can arrive after the first sample, and often does.
    #[test]
    fn a_kind_registered_later_still_reaches_its_series() {
        let mut s = SeriesStore::new();
        s.observe(&metric(1, 5, 10), &names());
        s.observe(&register(1, MetricKind::Gauge), &names());

        assert_eq!(s.series()[0].kind, Some(MetricKind::Gauge));
    }

    /// An unregistered metric is recorded with an unknown kind rather than dropped:
    /// losing data because a description has not arrived would be the wrong trade,
    /// and the chart can fall back to plotting the raw value.
    #[test]
    fn an_unregistered_metric_is_kept_with_an_unknown_kind() {
        let mut s = SeriesStore::new();
        s.observe(&metric(9, 1, 1), &names());

        assert_eq!(s.series()[0].kind, None);
        assert_eq!(s.series()[0].points.len(), 1);
    }

    /// A sample whose name has not been interned yet cannot be labelled, and an
    /// unlabelled series is not chartable — so it waits rather than arriving under a
    /// made-up name.
    #[test]
    fn a_sample_with_no_resolvable_name_is_not_recorded() {
        let mut s = SeriesStore::new();
        s.observe(&metric(1, 1, 1), &|_| None);

        assert!(s.series().is_empty());
    }

    /// Series come back in a stable order, so a chart's colours do not reshuffle as
    /// new metrics appear — colour follows the entity, never its rank.
    #[test]
    fn series_come_back_in_first_seen_order() {
        let mut s = SeriesStore::new();
        s.observe(&metric(2, 1, 1), &names());
        s.observe(&metric(1, 1, 1), &names());
        s.observe(&metric(3, 1, 1), &names());

        let order: Vec<String> = s.series().into_iter().map(|x| x.name).collect();
        assert_eq!(order, ["metric.2", "metric.1", "metric.3"]);
    }

    /// The JSON shape the page reads — pinned, because TypeScript has no compiler to
    /// notice a renamed field or a re-spelled kind.
    ///
    /// `kind` in particular: serde writes a unit variant as its name, so the page
    /// matches on the literal "Counter", and a rename on either side would silently
    /// stop every counter being charted as a rate.
    #[test]
    fn the_serialized_shape_is_what_the_page_reads() {
        let mut s = SeriesStore::new();
        s.observe(&register(1, MetricKind::Counter), &names());
        s.observe(&metric(1, 7, 100), &names());

        assert_eq!(
            serde_json::to_string(&s.series()).expect("serializes"),
            r#"[{"name":"metric.1","kind":"Counter","points":[[100,7]]}]"#
        );
    }

    /// An undescribed metric crosses as `null`, which the page must be able to tell
    /// from a kind it does not recognise.
    #[test]
    fn an_unknown_kind_serializes_as_null() {
        let mut s = SeriesStore::new();
        s.observe(&metric(1, 1, 1), &names());

        assert!(serde_json::to_string(&s.series()).expect("serializes").contains(r#""kind":null"#));
    }

    /// Anything that is not a metric is not this store's business.
    #[test]
    fn non_metric_frames_are_ignored() {
        let mut s = SeriesStore::new();
        s.observe(&OwnedFrame::Dropped { count: 3 }, &names());

        assert!(s.series().is_empty());
    }
}
