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
    /// `thread.id` and `host.cpu_id` (always) and `thread.name` (when
    /// `ThreadRegister` has resolved the `task_id`). Tempo renders them
    /// in the trace detail view; `host.cpu_id` lets traces be sliced by
    /// the hart the span ran on. Built by `span_attributes`.
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
        let proto_span = build_proto_span(span, &self.trace_id);

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

/// Build an OTLP `Span` proto from a `CompletedSpan` and a `trace_id`.
/// Pure — no I/O — so it is fully host-testable without a mock server.
fn build_proto_span(span: &CompletedSpan, trace_id: &[u8; 16]) -> Span {
    Span {
        trace_id: trace_id.to_vec(),
        span_id: span.span_id.to_be_bytes().to_vec(),
        trace_state: String::new(),
        parent_span_id: if span.parent_span_id == 0 {
            Vec::new()
        } else {
            span.parent_span_id.to_be_bytes().to_vec()
        },
        name: span.name.clone(),
        kind: 1, // INTERNAL
        start_time_unix_nano: clamp_u128_to_u64(span.start_time_ns),
        end_time_unix_nano: clamp_u128_to_u64(span.end_time_ns),
        attributes: span_attributes(span),
    }
}

fn clamp_u128_to_u64(v: u128) -> u64 {
    v.min(u128::from(u64::MAX)) as u64
}

/// Build the per-span OTLP attributes (`OTel` semantic conventions):
/// `thread.id` and `host.cpu_id` always; `thread.name` only once a
/// `ThreadRegister` has resolved the `task_id`. Pure so the export
/// path's real-HTTP `mutants::skip` doesn't leave the attribute set
/// untested.
fn span_attributes(span: &CompletedSpan) -> Vec<KeyValue> {
    let mut attributes = vec![
        KeyValue {
            key: "thread.id".to_string(),
            value: Some(AnyValue {
                string_value: span.task_id.to_string(),
            }),
        },
        KeyValue {
            key: "host.cpu_id".to_string(),
            value: Some(AnyValue {
                string_value: span.hart_id.to_string(),
            }),
        },
    ];
    if let Some(name) = &span.thread_name {
        attributes.push(KeyValue {
            key: "thread.name".to_string(),
            value: Some(AnyValue {
                string_value: name.clone(),
            }),
        });
    }
    if let Some(priority) = span.thread_priority {
        attributes.push(KeyValue {
            key: "thread.priority".to_string(),
            value: Some(AnyValue {
                string_value: priority_label(priority).to_string(),
            }),
        });
    }
    attributes
}

/// Human-readable label for a scheduling priority level (matches
/// `kernel_core::sched::Priority`). Unknown levels fall through to the raw
/// number so a future variant still renders something.
fn priority_label(level: u8) -> &'static str {
    match level {
        0 => "Low",
        1 => "Normal",
        2 => "High",
        _ => "?",
    }
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

    fn completed(task_id: u32, thread_name: Option<&str>, hart_id: u8) -> CompletedSpan {
        CompletedSpan {
            name: "task_b.tick".to_string(),
            span_id: 1,
            parent_span_id: 0,
            start_time_ns: 0,
            end_time_ns: 1,
            task_id,
            thread_name: thread_name.map(str::to_string),
            thread_priority: None,
            hart_id,
        }
    }

    fn attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a str> {
        attrs
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| kv.value.as_ref())
            .map(|v| v.string_value.as_str())
    }

    #[test]
    fn span_attributes_surface_hart_as_host_cpu_id() {
        let attrs = span_attributes(&completed(3, Some("task_b"), 1));
        assert_eq!(attr(&attrs, "host.cpu_id"), Some("1"));
    }

    #[test]
    fn span_attributes_always_carry_thread_id() {
        let attrs = span_attributes(&completed(3, None, 0));
        assert_eq!(attr(&attrs, "thread.id"), Some("3"));
    }

    #[test]
    fn span_attributes_omit_thread_name_when_unresolved() {
        let attrs = span_attributes(&completed(3, None, 0));
        assert_eq!(attr(&attrs, "thread.name"), None);
    }

    #[test]
    fn span_attributes_include_thread_name_when_resolved() {
        let attrs = span_attributes(&completed(3, Some("task_b"), 0));
        assert_eq!(attr(&attrs, "thread.name"), Some("task_b"));
    }

    #[test]
    fn build_proto_span_maps_fields_onto_proto() {
        let span = CompletedSpan {
            name: "kernel.boot".to_string(),
            span_id: 42,
            parent_span_id: 7,
            start_time_ns: 1_000_000,
            end_time_ns: 2_000_000,
            task_id: 1,
            thread_name: None,
            thread_priority: None,
            hart_id: 0,
        };
        let trace_id: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let proto = build_proto_span(&span, &trace_id);
        assert_eq!(proto.trace_id, trace_id.to_vec());
        assert_eq!(proto.span_id, 42u64.to_be_bytes().to_vec());
        assert_eq!(proto.parent_span_id, 7u64.to_be_bytes().to_vec());
        assert_eq!(proto.name, "kernel.boot");
        assert_eq!(proto.start_time_unix_nano, 1_000_000);
        assert_eq!(proto.end_time_unix_nano, 2_000_000);
    }

    #[test]
    fn build_proto_span_uses_empty_bytes_for_root_span() {
        let span = completed(0, None, 0); // parent_span_id == 0
        let trace_id = [0u8; 16];
        let proto = build_proto_span(&span, &trace_id);
        assert_eq!(proto.parent_span_id, Vec::<u8>::new());
    }

    #[test]
    fn span_attributes_omit_thread_priority_when_unresolved() {
        let attrs = span_attributes(&completed(3, None, 0));
        assert_eq!(attr(&attrs, "thread.priority"), None);
    }

    #[test]
    fn span_attributes_label_thread_priority_when_resolved() {
        let mut span = completed(3, Some("greedy"), 0);
        span.thread_priority = Some(2);
        let attrs = span_attributes(&span);
        assert_eq!(attr(&attrs, "thread.priority"), Some("High"));
    }

    #[test]
    fn priority_label_covers_each_level_and_falls_back() {
        assert_eq!(priority_label(0), "Low");
        assert_eq!(priority_label(1), "Normal");
        assert_eq!(priority_label(2), "High");
        assert_eq!(priority_label(7), "?");
    }
}
