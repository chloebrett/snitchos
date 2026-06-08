//! OTLP/HTTP push of itest baseline metrics.
//!
//! Mirrors the data emitted by `render_prometheus`, but pushed live
//! as OTLP gauge metrics to an OTLP-compatible receiver (Prometheus
//! with `--web.enable-otlp-receiver`, OTel collector, etc.). Step H2
//! of the itest history/pending plan.
//!
//! Why both textfile (`prom`) AND live push (this module)? Textfile
//! is zero-infra and survives no-network; OTLP is live and integrates
//! into existing OTel pipelines. Same data, two transports.
//!
//! Proto subset is hand-rolled (matching `collector/src/otlp.rs`) to
//! avoid pulling the full `opentelemetry-proto` crate just for nine
//! gauge metrics.

use std::io;

use prost::Message;

use crate::baseline::BaselineFile;
use crate::stats::wilson_score_95;

// --- OTLP proto subset (prost-derived) -------------------------------------
//
// Source: OpenTelemetry proto `opentelemetry/proto/{common,resource,metrics}/v1`.
// We only carry the field tags we actually emit. Field tag numbers MUST
// match the upstream proto exactly — they're the wire identifiers.

#[derive(Clone, PartialEq, Message)]
struct ExportMetricsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    resource_metrics: Vec<ResourceMetrics>,
}

#[derive(Clone, PartialEq, Message)]
struct ResourceMetrics {
    #[prost(message, optional, tag = "1")]
    resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    scope_metrics: Vec<ScopeMetrics>,
}

#[derive(Clone, PartialEq, Message)]
struct Resource {
    #[prost(message, repeated, tag = "1")]
    attributes: Vec<KeyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct ScopeMetrics {
    #[prost(message, optional, tag = "1")]
    scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    metrics: Vec<Metric>,
}

#[derive(Clone, PartialEq, Message)]
struct InstrumentationScope {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, tag = "2")]
    version: String,
}

#[derive(Clone, PartialEq, Message)]
struct Metric {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, tag = "2")]
    description: String,
    #[prost(string, tag = "3")]
    unit: String,
    /// `data` is a proto `oneof` over gauge/sum/histogram/etc. We only
    /// emit gauges, so only tag 5 (gauge) is wired here.
    #[prost(oneof = "metric_data::Data", tags = "5")]
    data: Option<metric_data::Data>,
}

mod metric_data {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Data {
        #[prost(message, tag = "5")]
        Gauge(super::Gauge),
    }
}

#[derive(Clone, PartialEq, Message)]
struct Gauge {
    #[prost(message, repeated, tag = "1")]
    data_points: Vec<NumberDataPoint>,
}

#[derive(Clone, PartialEq, Message)]
struct NumberDataPoint {
    #[prost(message, repeated, tag = "7")]
    attributes: Vec<KeyValue>,
    #[prost(fixed64, tag = "2")]
    start_time_unix_nano: u64,
    #[prost(fixed64, tag = "3")]
    time_unix_nano: u64,
    /// `value` is a `oneof { double as_double = 4; sfixed64 as_int = 6; }`.
    #[prost(oneof = "number_data_point::Value", tags = "4, 6")]
    value: Option<number_data_point::Value>,
}

mod number_data_point {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Value {
        #[prost(double, tag = "4")]
        AsDouble(f64),
        #[prost(sfixed64, tag = "6")]
        AsInt(i64),
    }
}

#[derive(Clone, PartialEq, Message)]
struct KeyValue {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(message, optional, tag = "2")]
    value: Option<AnyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct AnyValue {
    #[prost(string, tag = "1")]
    string_value: String,
}

// --- Builder ----------------------------------------------------------------

/// Build the OTLP wire payload from a baseline file. One gauge per
/// metric, one data point per (metric, scenario) pair. Returns
/// protobuf-encoded bytes ready for an OTLP/HTTP POST body.
///
/// `now_ns` is the timestamp stamped onto every data point. Caller
/// supplies it so tests are deterministic and so the live push can
/// share one timestamp across the whole batch.
pub fn build_payload(file: &BaselineFile, now_ns: u64) -> Vec<u8> {
    let req = build_request(file, now_ns);
    let mut buf = Vec::with_capacity(req.encoded_len());
    // encode() on Vec only fails on OOM — we accept that as a panic.
    req.encode(&mut buf).expect("encode to Vec cannot fail");
    buf
}

fn build_request(file: &BaselineFile, now_ns: u64) -> ExportMetricsServiceRequest {
    let mut metrics: Vec<Metric> = Vec::with_capacity(9);
    // Pre-build per-scenario data sets, one Vec per metric. Each will
    // become the `data_points` of its Gauge.
    let mut runs_pts: Vec<NumberDataPoint> = Vec::new();
    let mut fails_pts: Vec<NumberDataPoint> = Vec::new();
    let mut rate_pts: Vec<NumberDataPoint> = Vec::new();
    let mut wlo_pts: Vec<NumberDataPoint> = Vec::new();
    let mut wup_pts: Vec<NumberDataPoint> = Vec::new();
    let mut mean_pts: Vec<NumberDataPoint> = Vec::new();
    let mut p95_pts: Vec<NumberDataPoint> = Vec::new();
    let mut partial_pts: Vec<NumberDataPoint> = Vec::new();
    let mut recorded_pts: Vec<NumberDataPoint> = Vec::new();

    for (name, entry) in &file.scenarios {
        let Some(b) = &entry.current else { continue };
        let attrs = vec![KeyValue {
            key: "scenario".to_string(),
            value: Some(AnyValue {
                string_value: name.clone(),
            }),
        }];
        let ci = wilson_score_95(b.failures, b.runs);
        let rate = if b.runs == 0 {
            0.0
        } else {
            f64::from(b.failures) / f64::from(b.runs)
        };

        runs_pts.push(int_point(attrs.clone(), now_ns, i64::from(b.runs)));
        fails_pts.push(int_point(attrs.clone(), now_ns, i64::from(b.failures)));
        rate_pts.push(double_point(attrs.clone(), now_ns, rate));
        wlo_pts.push(double_point(attrs.clone(), now_ns, ci.lower));
        wup_pts.push(double_point(attrs.clone(), now_ns, ci.upper));
        if let Some(m) = b.mean_duration_ms {
            mean_pts.push(double_point(attrs.clone(), now_ns, m));
        }
        if let Some(p) = b.p95_duration_ms {
            p95_pts.push(double_point(attrs.clone(), now_ns, p));
        }
        partial_pts.push(int_point(
            attrs.clone(),
            now_ns,
            if b.partial.is_some() { 1 } else { 0 },
        ));
        recorded_pts.push(int_point(
            attrs,
            now_ns,
            b.recorded_at.unix_timestamp(),
        ));
    }

    let mut push = |name: &str, desc: &str, unit: &str, points: Vec<NumberDataPoint>| {
        if points.is_empty() {
            return;
        }
        metrics.push(Metric {
            name: name.to_string(),
            description: desc.to_string(),
            unit: unit.to_string(),
            data: Some(metric_data::Data::Gauge(Gauge { data_points: points })),
        });
    };

    push(
        "snitchos.itest.baseline.runs",
        "Number of --repeat iterations in the current baseline.",
        "1",
        runs_pts,
    );
    push(
        "snitchos.itest.baseline.failures",
        "Number of failed iterations in the current baseline.",
        "1",
        fails_pts,
    );
    push(
        "snitchos.itest.baseline.failure_rate",
        "Observed failure rate in the current baseline (0.0-1.0).",
        "1",
        rate_pts,
    );
    push(
        "snitchos.itest.baseline.wilson_lower",
        "Wilson-score 95% CI lower bound on the failure rate.",
        "1",
        wlo_pts,
    );
    push(
        "snitchos.itest.baseline.wilson_upper",
        "Wilson-score 95% CI upper bound on the failure rate.",
        "1",
        wup_pts,
    );
    push(
        "snitchos.itest.baseline.mean_duration_ms",
        "Mean per-iteration wall-clock duration.",
        "ms",
        mean_pts,
    );
    push(
        "snitchos.itest.baseline.p95_duration_ms",
        "p95 per-iteration wall-clock duration.",
        "ms",
        p95_pts,
    );
    push(
        "snitchos.itest.baseline.partial",
        "1 if the current baseline reflects an interrupted run, else 0.",
        "1",
        partial_pts,
    );
    push(
        "snitchos.itest.baseline.recorded_at_seconds",
        "Unix timestamp (seconds) when the current baseline was recorded.",
        "s",
        recorded_pts,
    );

    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_string(),
                    value: Some(AnyValue {
                        string_value: "snitchos.itest".to_string(),
                    }),
                }],
            }),
            scope_metrics: vec![ScopeMetrics {
                scope: Some(InstrumentationScope {
                    name: "snitchos.itest-harness".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                }),
                metrics,
            }],
        }],
    }
}

fn double_point(attrs: Vec<KeyValue>, now_ns: u64, value: f64) -> NumberDataPoint {
    NumberDataPoint {
        attributes: attrs,
        start_time_unix_nano: now_ns,
        time_unix_nano: now_ns,
        value: Some(number_data_point::Value::AsDouble(value)),
    }
}

fn int_point(attrs: Vec<KeyValue>, now_ns: u64, value: i64) -> NumberDataPoint {
    NumberDataPoint {
        attributes: attrs,
        start_time_unix_nano: now_ns,
        time_unix_nano: now_ns,
        value: Some(number_data_point::Value::AsInt(value)),
    }
}

/// Normalise an OTLP base URL to a metrics-endpoint URL. Caller passes
/// either the receiver root (`http://host:port`) or a path-bearing URL;
/// we append `/v1/metrics` if it's missing. Matches the
/// `collector::otlp::Exporter::new` normalisation for traces.
pub fn metrics_endpoint(base: &str) -> String {
    if base.ends_with("/v1/metrics") {
        base.to_string()
    } else {
        format!("{}/v1/metrics", base.trim_end_matches('/'))
    }
}

/// POST `body` (already protobuf-encoded by `build_payload`) to an
/// OTLP/HTTP receiver. Returns the HTTP status on a server response,
/// or an `io::Error` on transport failure. Caller decides whether
/// to retry.
pub fn post(endpoint: &str, body: &[u8]) -> io::Result<u16> {
    let agent = ureq::AgentBuilder::new().build();
    match agent
        .post(endpoint)
        .set("Content-Type", "application/x-protobuf")
        .send_bytes(body)
    {
        Ok(resp) => Ok(resp.status()),
        Err(ureq::Error::Status(code, _)) => Ok(code),
        Err(e) => Err(io::Error::other(format!("OTLP POST failed: {e}"))),
    }
}

/// One-shot push: build the payload from `file`, POST to `endpoint`,
/// return the HTTP status. `now_ns` is the timestamp stamped on every
/// data point. Caller-supplied so tests are deterministic.
pub fn push(endpoint: &str, file: &BaselineFile, now_ns: u64) -> io::Result<u16> {
    let endpoint = metrics_endpoint(endpoint);
    let body = build_payload(file, now_ns);
    post(&endpoint, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::{Baseline, PartialMarker};
    use prost::Message;
    use time::macros::datetime;

    fn baseline_with(runs: u32, failures: u32) -> Baseline {
        Baseline {
            commit: "abc1234".to_string(),
            build_hash: None,
            runs,
            failures,
            recorded_at: datetime!(2026-06-08 12:00:00 UTC),
            mean_duration_ms: Some(1200.0),
            p95_duration_ms: Some(1500.0),
            partial: None,
        }
    }

    #[test]
    fn metrics_endpoint_appends_when_missing() {
        assert_eq!(
            metrics_endpoint("http://localhost:9090/api/v1/otlp"),
            "http://localhost:9090/api/v1/otlp/v1/metrics"
        );
        assert_eq!(
            metrics_endpoint("http://localhost:9090/api/v1/otlp/"),
            "http://localhost:9090/api/v1/otlp/v1/metrics"
        );
    }

    #[test]
    fn metrics_endpoint_idempotent_when_already_present() {
        let s = "http://localhost:9090/api/v1/otlp/v1/metrics";
        assert_eq!(metrics_endpoint(s), s);
    }

    #[test]
    fn payload_decodes_round_trip() {
        let mut f = BaselineFile::new();
        f.update_current("heartbeat-cadence", baseline_with(100, 3));
        f.update_current("boot-reaches-heartbeat", baseline_with(50, 0));
        let body = build_payload(&f, 1_700_000_000_000_000_000);
        let decoded = ExportMetricsServiceRequest::decode(&*body).unwrap();
        assert_eq!(decoded.resource_metrics.len(), 1);
        let rm = &decoded.resource_metrics[0];
        assert_eq!(rm.scope_metrics.len(), 1);
        let sm = &rm.scope_metrics[0];
        // 9 metrics emitted (every gauge has at least one data point
        // because both scenarios have all fields populated).
        assert_eq!(sm.metrics.len(), 9);

        let runs = sm
            .metrics
            .iter()
            .find(|m| m.name == "snitchos.itest.baseline.runs")
            .expect("runs metric");
        let Some(metric_data::Data::Gauge(g)) = &runs.data else {
            panic!("expected gauge");
        };
        assert_eq!(g.data_points.len(), 2);
        // Each data point should have the scenario attribute.
        for dp in &g.data_points {
            let kv = &dp.attributes[0];
            assert_eq!(kv.key, "scenario");
            assert!(matches!(&kv.value, Some(v) if !v.string_value.is_empty()));
            assert_eq!(dp.time_unix_nano, 1_700_000_000_000_000_000);
        }
    }

    #[test]
    fn payload_skips_metrics_with_no_data() {
        let mut f = BaselineFile::new();
        let mut b = baseline_with(10, 0);
        b.mean_duration_ms = None;
        b.p95_duration_ms = None;
        f.update_current("scn", b);
        let body = build_payload(&f, 1_000_000);
        let decoded = ExportMetricsServiceRequest::decode(&*body).unwrap();
        let names: Vec<&str> = decoded.resource_metrics[0].scope_metrics[0]
            .metrics
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert!(!names.contains(&"snitchos.itest.baseline.mean_duration_ms"));
        assert!(!names.contains(&"snitchos.itest.baseline.p95_duration_ms"));
        // Required metrics still present.
        assert!(names.contains(&"snitchos.itest.baseline.runs"));
        assert!(names.contains(&"snitchos.itest.baseline.failure_rate"));
    }

    #[test]
    fn payload_marks_partial_baseline_as_one() {
        let mut f = BaselineFile::new();
        let mut b = baseline_with(27, 1);
        b.partial = Some(PartialMarker {
            requested_runs: 100,
            interrupted_at: datetime!(2026-06-08 12:30:00 UTC),
            run_dir: None,
        });
        f.update_current("scn", b);
        let body = build_payload(&f, 0);
        let decoded = ExportMetricsServiceRequest::decode(&*body).unwrap();
        let m = decoded.resource_metrics[0].scope_metrics[0]
            .metrics
            .iter()
            .find(|m| m.name == "snitchos.itest.baseline.partial")
            .unwrap();
        let Some(metric_data::Data::Gauge(g)) = &m.data else { panic!() };
        assert_eq!(g.data_points.len(), 1);
        assert!(matches!(
            g.data_points[0].value,
            Some(number_data_point::Value::AsInt(1))
        ));
    }

    #[test]
    fn payload_skips_scenarios_without_current() {
        let mut f = BaselineFile::new();
        f.update_current("real", baseline_with(10, 0));
        f.scenarios.insert("ghost".to_string(), Default::default());
        let body = build_payload(&f, 0);
        let decoded = ExportMetricsServiceRequest::decode(&*body).unwrap();
        let runs = decoded.resource_metrics[0].scope_metrics[0]
            .metrics
            .iter()
            .find(|m| m.name == "snitchos.itest.baseline.runs")
            .unwrap();
        let Some(metric_data::Data::Gauge(g)) = &runs.data else { panic!() };
        // Only "real" should produce a data point.
        assert_eq!(g.data_points.len(), 1);
        let kv = &g.data_points[0].attributes[0];
        assert_eq!(kv.value.as_ref().unwrap().string_value, "real");
    }

    #[test]
    fn resource_carries_service_name_attribute() {
        let mut f = BaselineFile::new();
        f.update_current("x", baseline_with(1, 0));
        let body = build_payload(&f, 0);
        let decoded = ExportMetricsServiceRequest::decode(&*body).unwrap();
        let res = decoded.resource_metrics[0].resource.as_ref().unwrap();
        let svc = res
            .attributes
            .iter()
            .find(|kv| kv.key == "service.name")
            .unwrap();
        assert_eq!(svc.value.as_ref().unwrap().string_value, "snitchos.itest");
    }

    #[test]
    fn rate_metric_carries_double_value() {
        let mut f = BaselineFile::new();
        f.update_current("x", baseline_with(100, 25));
        let body = build_payload(&f, 0);
        let decoded = ExportMetricsServiceRequest::decode(&*body).unwrap();
        let rate = decoded.resource_metrics[0].scope_metrics[0]
            .metrics
            .iter()
            .find(|m| m.name == "snitchos.itest.baseline.failure_rate")
            .unwrap();
        let Some(metric_data::Data::Gauge(g)) = &rate.data else { panic!() };
        let v = g.data_points[0].value.as_ref().unwrap();
        match v {
            number_data_point::Value::AsDouble(d) => assert!((d - 0.25).abs() < 1e-9),
            number_data_point::Value::AsInt(_) => panic!("rate must be double"),
        }
    }
}
