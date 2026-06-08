//! Prometheus `/metrics` endpoint.
//!
//! Tiny HTTP server (one thread, blocking) that serves the current
//! state's metric values in Prometheus text exposition format. Scraped
//! by the docker-compose Prometheus instance every 5 seconds.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::thread;

use protocol::MetricKind;

use crate::state::State;

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
/// One metric family per registered name. Names like
/// `snitchos.heartbeat.count` become `snitchos_heartbeat_count` —
/// Prometheus forbids dots.
fn format_metrics(state: &State) -> String {
    let mut out = String::new();

    // Counters and gauges. State is keyed by (name_id, hart_id); group
    // by name so HELP/TYPE is emitted once per family, then one
    // `{hart="N"}`-labelled value line per emitting hart. BTreeMap keeps
    // the output deterministic (family order + hart order).
    for (name_id, harts) in group_by_name(state.metric_values.iter().map(|(&k, &v)| (k, v))) {
        let Some(raw_name) = state.name(name_id) else {
            continue;
        };
        let Some(kind) = state.metric_kind(name_id) else {
            continue;
        };
        if matches!(kind, MetricKind::Histogram) {
            continue; // handled below
        }
        let prom_name = sanitize(raw_name);
        let kind_str = match kind {
            MetricKind::Counter => "counter",
            MetricKind::Gauge => "gauge",
            MetricKind::Histogram => unreachable!(),
        };
        let _ = writeln!(out, "# HELP {prom_name} {raw_name}");
        let _ = writeln!(out, "# TYPE {prom_name} {kind_str}");
        for (hart_id, value) in harts {
            let _ = writeln!(out, "{prom_name}{{hart=\"{hart_id}\"}} {value}");
        }
    }

    // Histograms — bucket counts (cumulative), sum, count. Same
    // group-by-name treatment; each hart gets its own labelled series.
    for (name_id, harts) in group_by_name(state.histograms.iter().map(|(&k, v)| (k, v))) {
        let Some(raw_name) = state.name(name_id) else {
            continue;
        };
        let prom_name = sanitize(raw_name);
        let _ = writeln!(out, "# HELP {prom_name} {raw_name}");
        let _ = writeln!(out, "# TYPE {prom_name} histogram");

        for (hart_id, hist) in harts {
            // Prometheus expects cumulative bucket counts.
            let mut cumulative: u64 = 0;
            for (i, &bound) in State::HISTOGRAM_BOUNDS.iter().enumerate() {
                if let Some(&c) = hist.buckets.get(i) {
                    cumulative += c;
                }
                let _ = writeln!(out, "{prom_name}_bucket{{hart=\"{hart_id}\",le=\"{bound}\"}} {cumulative}");
            }
            cumulative += hist.inf_count;
            let _ = writeln!(out, "{prom_name}_bucket{{hart=\"{hart_id}\",le=\"+Inf\"}} {cumulative}");
            let _ = writeln!(out, "{prom_name}_sum{{hart=\"{hart_id}\"}} {}", hist.sum);
            let _ = writeln!(out, "{prom_name}_count{{hart=\"{hart_id}\"}} {}", hist.count);
        }
    }

    out
}

/// Regroup `(name_id, hart_id)`-keyed entries into name → sorted
/// `(hart_id, value)` list. `BTreeMap` + the inner sort make the
/// exposition order deterministic regardless of `HashMap` iteration.
fn group_by_name<V>(
    entries: impl Iterator<Item = ((u32, u8), V)>,
) -> std::collections::BTreeMap<u32, Vec<(u8, V)>> {
    let mut by_name: std::collections::BTreeMap<u32, Vec<(u8, V)>> =
        std::collections::BTreeMap::new();
    for ((name_id, hart_id), v) in entries {
        by_name.entry(name_id).or_default().push((hart_id, v));
    }
    for harts in by_name.values_mut() {
        harts.sort_by_key(|(hart_id, _)| *hart_id);
    }
    by_name
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
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind });
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
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Counter });
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
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Histogram });
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
        s.handle(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Histogram });
        // 2_000_000 exceeds all bounds → inf_count
        s.handle(&Frame::Metric { name_id: StringId(1), value: 2_000_000, t: 100, hart_id: 0 });

        let out = format_metrics(&s);
        assert!(out.contains("irq_duration_bucket{hart=\"0\",le=\"+Inf\"} 1\n"), "got:\n{out}");
        assert!(out.contains("irq_duration_bucket{hart=\"0\",le=\"1000000\"} 0\n"), "got:\n{out}");
        assert!(out.contains("irq_duration_sum{hart=\"0\"} 2000000\n"), "got:\n{out}");
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
