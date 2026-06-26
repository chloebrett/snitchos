//! Prometheus `/metrics` endpoint.
//!
//! Tiny HTTP server (one thread, blocking) that serves the current
//! state's metric values in Prometheus text exposition format. Scraped
//! by the docker-compose Prometheus instance every 5 seconds.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::thread;

use protocol::MetricKind;

use crate::state::{Histogram, State};

/// Spawn the metrics server on the given port. Runs until the process
/// exits. Errors during request handling are logged to stderr but don't
/// take the server down.
#[cfg_attr(test, mutants::skip)] // binds a real TCP socket — not unit-testable
pub fn serve(state: Arc<Mutex<State>>, port: u16) -> std::io::Result<()> {
    let addr = format!("0.0.0.0:{port}");
    let server = tiny_http::Server::http(&addr).map_err(|e| {
        std::io::Error::other(
            format!("tiny_http bind: {e}"),
        )
    })?;

    thread::spawn(move || {
        for request in server.incoming_requests() {
            let response = match request.url() {
                "/metrics" => {
                    let body = {
                        let state = state.lock().unwrap();
                        format_metrics(&state)
                    };
                    tiny_http::Response::from_string(body)
                        .with_header(
                            "Content-Type: text/plain; version=0.0.4"
                                .parse::<tiny_http::Header>()
                                .unwrap(),
                        )
                }
                _ => tiny_http::Response::from_string("not found")
                    .with_status_code(404),
            };
            if let Err(e) = request.respond(response) {
                eprintln!("prom: respond failed: {e}");
            }
        }
    });

    Ok(())
}

/// Format `State`'s metric tables as Prometheus exposition text.
///
/// One metric family per registered **name string** (not per `name_id`): two
/// processes that named a metric identically get distinct `name_id`s that resolve
/// to the same string, so they must share one `# HELP`/`# TYPE` and be separated
/// by a `task="…"` label — otherwise the output is a duplicate family (invalid
/// exposition). Names like `snitchos.heartbeat.count` become
/// `snitchos_heartbeat_count` — Prometheus forbids dots.
fn format_metrics(state: &State) -> String {
    let mut out = String::new();
    format_scalars(state, &mut out);
    format_histograms(state, &mut out);
    out
}

/// The label set for one metric series: `hart="N"`, prefixed with `task="…"`
/// when the metric has a resolvable emitter (a userspace registrant). Returned
/// without braces so histogram bucket lines can append `,le="…"`. The `task`
/// label is what keeps two same-named metrics from different processes distinct.
fn series_labels(state: &State, name_id: u32, hart_id: u8) -> String {
    match state.metric_emitter_label(name_id) {
        Some(task) => format!("task=\"{task}\",hart=\"{hart_id}\""),
        None => format!("hart=\"{hart_id}\""),
    }
}

/// One scalar series within a family, before formatting: `(name_id, hart_id,
/// value)`. `name_id` distinguishes emitters that share a name; `hart_id` the
/// harts; both sort into a deterministic order.
type ScalarSeries = (u32, u8, i64);

/// Counters and gauges. Grouped by resolved name string; within a family each
/// `(name_id, hart_id)` is a distinct labelled series. `BTreeMap` (family order)
/// + the per-family sort (by emitter then hart) keep the output deterministic.
fn format_scalars(state: &State, out: &mut String) {
    let mut families: BTreeMap<&str, (MetricKind, Vec<ScalarSeries>)> = BTreeMap::new();
    for (&(name_id, hart_id), &value) in &state.metric_values {
        let (Some(raw_name), Some(kind)) = (state.name(name_id), state.metric_kind(name_id)) else {
            continue;
        };
        if matches!(kind, MetricKind::Histogram) {
            continue; // handled in format_histograms
        }
        families.entry(raw_name).or_insert((kind, Vec::new())).1.push((name_id, hart_id, value));
    }
    for (raw_name, (kind, mut series)) in families {
        series.sort_unstable();
        let prom_name = sanitize(raw_name);
        let kind_str = match kind {
            MetricKind::Counter => "counter",
            MetricKind::Gauge => "gauge",
            MetricKind::Histogram => unreachable!(),
        };
        let _ = writeln!(out, "# HELP {prom_name} {raw_name}");
        let _ = writeln!(out, "# TYPE {prom_name} {kind_str}");
        for (name_id, hart_id, value) in series {
            let labels = series_labels(state, name_id, hart_id);
            let _ = writeln!(out, "{prom_name}{{{labels}}} {value}");
        }
    }
}

/// Histograms — bucket counts (cumulative), sum, count. Same group-by-name +
/// per-emitter labelling as the scalars.
fn format_histograms(state: &State, out: &mut String) {
    let mut families: BTreeMap<&str, Vec<(u32, u8, &Histogram)>> = BTreeMap::new();
    for (&(name_id, hart_id), hist) in &state.histograms {
        let Some(raw_name) = state.name(name_id) else {
            continue;
        };
        families.entry(raw_name).or_default().push((name_id, hart_id, hist));
    }
    for (raw_name, mut series) in families {
        series.sort_unstable_by_key(|(name_id, hart_id, _)| (*name_id, *hart_id));
        let prom_name = sanitize(raw_name);
        let _ = writeln!(out, "# HELP {prom_name} {raw_name}");
        let _ = writeln!(out, "# TYPE {prom_name} histogram");
        for (name_id, hart_id, hist) in series {
            let labels = series_labels(state, name_id, hart_id);
            // Prometheus expects cumulative bucket counts.
            let mut cumulative: u64 = 0;
            for (i, &bound) in State::HISTOGRAM_BOUNDS.iter().enumerate() {
                if let Some(&c) = hist.buckets.get(i) {
                    cumulative += c;
                }
                let _ = writeln!(out, "{prom_name}_bucket{{{labels},le=\"{bound}\"}} {cumulative}");
            }
            cumulative += hist.inf_count;
            let _ = writeln!(out, "{prom_name}_bucket{{{labels},le=\"+Inf\"}} {cumulative}");
            let _ = writeln!(out, "{prom_name}_sum{{{labels}}} {}", hist.sum);
            let _ = writeln!(out, "{prom_name}_count{{{labels}}} {}", hist.count);
        }
    }
}

/// Replace any character not in `[a-zA-Z0-9_:]` with `_`. Required so
/// our dotted names like `snitchos.heartbeat.count` become valid
/// Prometheus identifiers.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::FakeWallClock;
    use protocol::{Frame, MetricKind, StringId};

    fn state_with_scalar(name: &'static str, kind: MetricKind, value: i64) -> State {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: name });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind, task_id: protocol::NO_EMITTER });
        s.handle(&Frame::Metric { name_id: StringId(1), value, t: 100, hart_id: 0 });
        s
    }

    #[test]
    fn format_counter_emits_type_and_hart_labelled_value() {
        let s = state_with_scalar("snitchos.heartbeat.count", MetricKind::Counter, 42);
        let out = format_metrics(&s);
        assert!(out.contains("# TYPE snitchos_heartbeat_count counter\n"), "got:\n{out}");
        assert!(out.contains("snitchos_heartbeat_count{hart=\"0\"} 42\n"), "got:\n{out}");
    }

    #[test]
    fn format_gauge_emits_type_and_hart_labelled_value() {
        let s = state_with_scalar("cpu.temp", MetricKind::Gauge, 72);
        let out = format_metrics(&s);
        assert!(out.contains("# TYPE cpu_temp gauge\n"), "got:\n{out}");
        assert!(out.contains("cpu_temp{hart=\"0\"} 72\n"), "got:\n{out}");
    }

    #[test]
    fn format_same_counter_from_two_harts_emits_one_family_two_labelled_lines() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "snitchos.sched.switches" });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Counter, task_id: protocol::NO_EMITTER });
        s.handle(&Frame::Metric { name_id: StringId(1), value: 10, t: 100, hart_id: 0 });
        s.handle(&Frame::Metric { name_id: StringId(1), value: 7, t: 100, hart_id: 1 });

        let out = format_metrics(&s);
        // HELP/TYPE appear exactly once for the family.
        assert_eq!(out.matches("# TYPE snitchos_sched_switches counter").count(), 1, "got:\n{out}");
        // One value line per hart.
        assert!(out.contains("snitchos_sched_switches{hart=\"0\"} 10\n"), "got:\n{out}");
        assert!(out.contains("snitchos_sched_switches{hart=\"1\"} 7\n"), "got:\n{out}");
    }

    #[test]
    fn format_histogram_emits_cumulative_buckets_sum_count() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "irq.duration" });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Histogram, task_id: protocol::NO_EMITTER });
        // 50 → bucket[0] (≤100), 200 → bucket[1] (≤250)
        s.handle(&Frame::Metric { name_id: StringId(1), value: 50, t: 100, hart_id: 0 });
        s.handle(&Frame::Metric { name_id: StringId(1), value: 200, t: 100, hart_id: 0 });

        let out = format_metrics(&s);
        assert!(out.contains("# TYPE irq_duration histogram\n"), "got:\n{out}");
        // non-cumulative: bucket[0]=1, bucket[1]=1 → cumulative: le=100→1, le=250→2
        assert!(out.contains("irq_duration_bucket{hart=\"0\",le=\"100\"} 1\n"), "got:\n{out}");
        assert!(out.contains("irq_duration_bucket{hart=\"0\",le=\"250\"} 2\n"), "got:\n{out}");
        // remaining buckets all still 2
        assert!(out.contains("irq_duration_bucket{hart=\"0\",le=\"+Inf\"} 2\n"), "got:\n{out}");
        assert!(out.contains("irq_duration_sum{hart=\"0\"} 250\n"), "got:\n{out}");
        assert!(out.contains("irq_duration_count{hart=\"0\"} 2\n"), "got:\n{out}");
    }

    #[test]
    fn format_histogram_inf_observation_appears_in_inf_bucket() {
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "irq.duration" });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Histogram, task_id: protocol::NO_EMITTER });
        // 2_000_000 exceeds all bounds → inf_count
        s.handle(&Frame::Metric { name_id: StringId(1), value: 2_000_000, t: 100, hart_id: 0 });

        let out = format_metrics(&s);
        assert!(out.contains("irq_duration_bucket{hart=\"0\",le=\"+Inf\"} 1\n"), "got:\n{out}");
        assert!(out.contains("irq_duration_bucket{hart=\"0\",le=\"1000000\"} 0\n"), "got:\n{out}");
        assert!(out.contains("irq_duration_sum{hart=\"0\"} 2000000\n"), "got:\n{out}");
    }

    #[test]
    fn two_emitters_of_one_metric_name_share_a_family_with_distinct_task_labels() {
        // The emitter dimension: two processes register a metric with the SAME
        // name → distinct `name_id`s resolving to the same string. The export must
        // emit ONE family (one `# TYPE`) with two `task`-labelled series, not a
        // duplicate family (invalid Prometheus exposition).
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::ThreadRegister { id: 4, name: "probe_a", priority: 1 });
        s.handle(&Frame::ThreadRegister { id: 5, name: "probe_b", priority: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "snitchos.probe.custom" });
        s.handle(&Frame::StringRegister { id: StringId(2), value: "snitchos.probe.custom" });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Gauge, task_id: 4 });
        s.handle(&Frame::MetricRegister { name_id: StringId(2), kind: MetricKind::Gauge, task_id: 5 });
        s.handle(&Frame::Metric { name_id: StringId(1), value: 10, t: 1, hart_id: 0 });
        s.handle(&Frame::Metric { name_id: StringId(2), value: 20, t: 1, hart_id: 0 });

        let out = format_metrics(&s);
        assert_eq!(
            out.matches("# TYPE snitchos_probe_custom gauge").count(),
            1,
            "one family only, not a duplicate; got:\n{out}"
        );
        assert!(out.contains("snitchos_probe_custom{task=\"probe_a\",hart=\"0\"} 10\n"), "got:\n{out}");
        assert!(out.contains("snitchos_probe_custom{task=\"probe_b\",hart=\"0\"} 20\n"), "got:\n{out}");
    }

    #[test]
    fn a_kernel_global_metric_gets_no_task_label() {
        // Back-compat pin: a metric registered by the kernel (`NO_EMITTER`) keeps
        // its existing `{hart="N"}` series with no `task` label.
        let s = state_with_scalar("snitchos.frames.allocated_total", MetricKind::Counter, 5);
        let out = format_metrics(&s);
        assert!(out.contains("snitchos_frames_allocated_total{hart=\"0\"} 5\n"), "got:\n{out}");
        assert!(!out.contains("task="), "kernel-global metric must carry no task label; got:\n{out}");
    }

    #[test]
    fn an_unnamed_emitter_falls_back_to_its_numeric_task_id() {
        // The metric's emitter has no `ThreadRegister` yet → label by the numeric
        // task id rather than dropping the dimension.
        let mut s = State::new(FakeWallClock(0));
        s.handle(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        s.handle(&Frame::StringRegister { id: StringId(1), value: "snitchos.probe.custom" });
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Gauge, task_id: 3 });
        s.handle(&Frame::Metric { name_id: StringId(1), value: 7, t: 1, hart_id: 0 });

        let out = format_metrics(&s);
        assert!(out.contains("snitchos_probe_custom{task=\"3\",hart=\"0\"} 7\n"), "got:\n{out}");
    }

    #[test]
    fn sanitize_replaces_dots() {
        assert_eq!(sanitize("snitchos.heartbeat.count"), "snitchos_heartbeat_count");
    }

    #[test]
    fn sanitize_preserves_underscores_and_colons() {
        assert_eq!(sanitize("foo_bar:baz"), "foo_bar:baz");
    }

    #[test]
    fn sanitize_replaces_other_punctuation() {
        assert_eq!(sanitize("a-b/c d"), "a_b_c_d");
    }
}
