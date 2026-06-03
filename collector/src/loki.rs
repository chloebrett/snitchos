//! Loki log-push exporter.
//!
//! Each `CompletedSpan` becomes one log line pushed to Loki's HTTP
//! ingest API (`POST /loki/api/v1/push`). Lines are human-readable:
//!
//!   kernel.heartbeat [1.00ms] id=42 parent=7
//!
//! All spans share a single stream labelled `service=snitchos`.

use crate::SpanExporter;
use crate::state::CompletedSpan;

pub struct Exporter {
    endpoint: String,
    agent: ureq::Agent,
}

impl Exporter {
    pub fn new(endpoint: &str) -> Self {
        let endpoint = if endpoint.ends_with("/loki/api/v1/push") {
            endpoint.to_string()
        } else {
            format!("{}/loki/api/v1/push", endpoint.trim_end_matches('/'))
        };
        Self {
            endpoint,
            agent: ureq::AgentBuilder::new().build(),
        }
    }
}

impl SpanExporter for Exporter {
    #[cfg_attr(test, mutants::skip)] // HTTP I/O — not unit-testable without a mock server
    fn export(&self, span: &CompletedSpan) {
        let line = format_line(span);
        let ts = span.start_time_ns.to_string();
        let body = format!(
            r#"{{"streams":[{{"stream":{{"service":"snitchos"}},"values":[["{ts}","{line}"]]}}]}}"#
        );
        match self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/json")
            .send_string(&body)
        {
            Ok(resp) if resp.status() == 204 => {}
            Ok(resp) => {
                let status = resp.status();
                let body = resp.into_string().unwrap_or_default();
                eprintln!("loki: POST status={status} body={body}");
            }
            Err(e) => eprintln!("loki: POST failed: {e}"),
        }
    }
}

/// Format a `CompletedSpan` as a human-readable log line.
pub fn format_line(span: &CompletedSpan) -> String {
    let duration_ns = span.end_time_ns.saturating_sub(span.start_time_ns);
    format!(
        "{} [{}] id={} parent={}",
        span.name,
        format_duration(duration_ns),
        span.span_id,
        span.parent_span_id,
    )
}

/// Format a nanosecond duration as a human-readable string, choosing
/// the largest unit that keeps at least two significant digits.
fn format_duration(ns: u128) -> String {
    if ns < 1_000 {
        format!("{ns}ns")
    } else if ns < 1_000_000 {
        format!("{:.2}µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.2}ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2}s", ns as f64 / 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(name: &str, start_ns: u128, end_ns: u128, id: u64, parent: u64) -> CompletedSpan {
        CompletedSpan {
            name: name.to_string(),
            span_id: id,
            parent_span_id: parent,
            start_time_ns: start_ns,
            end_time_ns: end_ns,
        }
    }

    // --- format_duration ---

    #[test]
    fn duration_under_1us_shows_ns() {
        assert_eq!(format_duration(500), "500ns");
    }

    #[test]
    fn duration_boundary_1us_shows_us() {
        assert_eq!(format_duration(1_000), "1.00µs");
    }

    #[test]
    fn duration_under_1ms_shows_us() {
        assert_eq!(format_duration(12_500), "12.50µs");
    }

    #[test]
    fn duration_boundary_1ms_shows_ms() {
        assert_eq!(format_duration(1_000_000), "1.00ms");
    }

    #[test]
    fn duration_under_1s_shows_ms() {
        assert_eq!(format_duration(1_234_000), "1.23ms");
    }

    #[test]
    fn duration_boundary_1s_shows_s() {
        assert_eq!(format_duration(1_000_000_000), "1.00s");
    }

    #[test]
    fn duration_zero_shows_ns() {
        assert_eq!(format_duration(0), "0ns");
    }

    // --- format_line ---

    #[test]
    fn line_includes_span_name_and_duration() {
        let s = span("kernel.heartbeat", 0, 1_000_000, 42, 7);
        assert_eq!(format_line(&s), "kernel.heartbeat [1.00ms] id=42 parent=7");
    }

    #[test]
    fn line_root_span_has_parent_zero() {
        let s = span("kernel.boot", 0, 500_000, 1, 0);
        assert_eq!(format_line(&s), "kernel.boot [500.00µs] id=1 parent=0");
    }

    #[test]
    fn line_zero_duration_span() {
        let s = span("kernel.init", 100, 100, 5, 1);
        assert_eq!(format_line(&s), "kernel.init [0ns] id=5 parent=1");
    }

    // --- Exporter::new endpoint normalisation ---

    #[test]
    fn exporter_appends_push_path_to_bare_url() {
        let e = Exporter::new("http://localhost:3100");
        assert_eq!(e.endpoint, "http://localhost:3100/loki/api/v1/push");
    }

    #[test]
    fn exporter_does_not_double_append_push_path() {
        let e = Exporter::new("http://localhost:3100/loki/api/v1/push");
        assert_eq!(e.endpoint, "http://localhost:3100/loki/api/v1/push");
    }
}
