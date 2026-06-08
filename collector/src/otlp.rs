//! Minimal OTLP/HTTP trace exporter.
//!
//! We carry only the subset of the OTLP proto we actually emit:
//! `ExportTraceServiceRequest` → `ResourceSpans` → `ScopeSpans` → Span.
//! No attributes, no events, no links. Plenty for v0.2 — we just
//! want spans with start/end times and parent linkage in Tempo.
//!
//! Per-frame export: each `CompletedSpan` is one HTTP POST. Easy to
//! batch later by buffering in `Exporter`.

use prost::Message;

use crate::SpanExporter;
use crate::state::CompletedSpan;

// --- OTLP proto subset (prost-derived) ---------------------------------

#[derive(Clone, PartialEq, Message)]
struct ExportTraceServiceRequest {
    #[prost(message, repeated, tag = "1")]
    resource_spans: Vec<ResourceSpans>,
}

#[derive(Clone, PartialEq, Message)]
struct ResourceSpans {
    #[prost(message, optional, tag = "1")]
    resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    scope_spans: Vec<ScopeSpans>,
}

#[derive(Clone, PartialEq, Message)]
struct Resource {
    #[prost(message, repeated, tag = "1")]
    attributes: Vec<KeyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct ScopeSpans {
    #[prost(message, optional, tag = "1")]
    scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    spans: Vec<Span>,
}

#[derive(Clone, PartialEq, Message)]
struct InstrumentationScope {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, tag = "2")]
    version: String,
}

#[derive(Clone, PartialEq, Message)]
struct Span {
    #[prost(bytes = "vec", tag = "1")]
    trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    span_id: Vec<u8>,
    #[prost(string, tag = "3")]
    trace_state: String,
    #[prost(bytes = "vec", tag = "4")]
    parent_span_id: Vec<u8>,
    #[prost(string, tag = "5")]
    name: String,
    /// `SpanKind` enum: 0 = unspecified, 1 = internal, 2 = server, ...
    /// We always use INTERNAL.
    #[prost(int32, tag = "6")]
    kind: i32,
    #[prost(fixed64, tag = "7")]
    start_time_unix_nano: u64,
    #[prost(fixed64, tag = "8")]
    end_time_unix_nano: u64,
    /// Per-span attributes (`OTel` semantic conventions). We emit
    /// `thread.id` (always) and `thread.name` (when `ThreadRegister`
    /// has resolved the `task_id`). Tempo renders them in the trace
    /// detail view.
    #[prost(message, repeated, tag = "9")]
    attributes: Vec<KeyValue>,
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

// --- Exporter ----------------------------------------------------------

/// Per-frame OTLP/HTTP exporter. Holds the endpoint URL, a ureq agent
/// for connection reuse, and a 16-byte `trace_id` (unique per session)
/// used for all spans in this session.
///
/// Known weaknesses:
/// - **Per-frame POSTs.** One HTTP request per span. Fine at heartbeat
///   rates; would buffer/batch under load.
/// - **One `trace_id` per Exporter instance.** All session spans get the
///   same `trace_id`, so they all appear under one trace in Tempo. New
///   `Exporter::new()` for a new kernel session.
/// - **No retry / no backpressure.** If Tempo is slow or down, exports
///   fail silently (logged to stderr).
pub struct Exporter {
    endpoint: String,
    agent: ureq::Agent,
    trace_id: [u8; 16],
}

impl Exporter {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: crate::url::ensure_suffix(endpoint, "/v1/traces"),
            agent: ureq::AgentBuilder::new().build(),
            trace_id: session_trace_id(),
        }
    }

    /// Build an OTLP request containing one span and POST it.
    #[cfg_attr(test, mutants::skip)] // makes real HTTP calls — not unit-testable without a mock server
    fn export(&self, span: &CompletedSpan) {
        let mut attributes = vec![KeyValue {
            key: "thread.id".to_string(),
            value: Some(AnyValue {
                string_value: span.task_id.to_string(),
            }),
        }];
        if let Some(name) = &span.thread_name {
            attributes.push(KeyValue {
                key: "thread.name".to_string(),
                value: Some(AnyValue {
                    string_value: name.clone(),
                }),
            });
        }

        let proto_span = Span {
            trace_id: self.trace_id.to_vec(),
            span_id: span.span_id.to_be_bytes().to_vec(),
            trace_state: String::new(),
            parent_span_id: if span.parent_span_id == 0 {
                Vec::new() // OTLP convention: empty bytes = no parent
            } else {
                span.parent_span_id.to_be_bytes().to_vec()
            },
            name: span.name.clone(),
            kind: 1, // INTERNAL
            start_time_unix_nano: clamp_u128_to_u64(span.start_time_ns),
            end_time_unix_nano: clamp_u128_to_u64(span.end_time_ns),
            attributes,
        };

        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_string(),
                        value: Some(AnyValue {
                            string_value: "snitchos".to_string(),
                        }),
                    }],
                }),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope {
                        name: "snitchos.kernel".to_string(),
                        version: "0.1".to_string(),
                    }),
                    spans: vec![proto_span],
                }],
            }],
        };

        let mut buf = Vec::with_capacity(req.encoded_len());
        if let Err(e) = req.encode(&mut buf) {
            eprintln!("otlp: encode failed: {e:?}");
            return;
        }

        match self
            .agent
            .post(&self.endpoint)
            .set("Content-Type", "application/x-protobuf")
            .send_bytes(&buf)
        {
            Ok(resp) => {
                // First few successful posts: print so the user knows
                // they're flowing.
                static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let status = resp.status();
                // Read the body so the connection releases.
                let body = resp.into_string().unwrap_or_default();
                if status != 200 {
                    eprintln!("otlp: POST status={status} body={body}");
                }
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n < 3 {
                    eprintln!(
                        "otlp: posted span '{}' ({} bytes), status={}",
                        span.name,
                        buf.len(),
                        status
                    );
                }
            }
            Err(e) => {
                eprintln!("otlp: POST failed: {e}");
            }
        }
    }
}

#[cfg_attr(test, mutants::skip)] // delegates to inherent export; real skip is on that method
impl SpanExporter for Exporter {
    fn export(&self, span: &CompletedSpan) {
        self.export(span);
    }
}

/// A 16-byte `trace_id` for this collector session. Derived from the
/// start-time nanoseconds — all we need is uniqueness per collector run
/// (so each kernel session lands under its own Tempo trace), not entropy.
#[cfg_attr(test, mutants::skip)] // time-dependent — output cannot be asserted
fn session_trace_id() -> [u8; 16] {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
        .to_le_bytes()
}

fn clamp_u128_to_u64(v: u128) -> u64 {
    v.min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_passes_through_values_within_range() {
        assert_eq!(clamp_u128_to_u64(0), 0);
        assert_eq!(clamp_u128_to_u64(1_000_000), 1_000_000);
        assert_eq!(clamp_u128_to_u64(u128::from(u64::MAX)), u64::MAX);
    }

    #[test]
    fn clamp_saturates_at_u64_max() {
        assert_eq!(clamp_u128_to_u64(u128::from(u64::MAX) + 1), u64::MAX);
        assert_eq!(clamp_u128_to_u64(u128::MAX), u64::MAX);
    }

    #[test]
    fn exporter_wires_the_v1_traces_path() {
        let e = Exporter::new("http://localhost:4318");
        assert_eq!(e.endpoint, "http://localhost:4318/v1/traces");
    }
}
