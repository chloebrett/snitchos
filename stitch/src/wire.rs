//! Lowering Stitch's high-level [`TelemetryEvent`]s into `SnitchOS` wire
//! [`Frame`](protocol::Frame)s — the bridge that turns "Stitch recorded some
//! telemetry" into "telemetry the collector can decode into Tempo/Prometheus".
//!
//! The two shapes don't match. A `TelemetryEvent` is string-named, untimed, and
//! carries a runtime `Value`; a `Frame` is interned (`StringId`), explicitly
//! parented (`SpanId` + `parent`), timestamped, and metric values are `i64`.
//! [`lower`] is the pass that closes that gap, replaying the kernel's own
//! tracing discipline over Stitch's event stream:
//!
//! - **spans** get a freshly allocated [`SpanId`]; nesting is recovered from the
//!   event order via a parent stack (Stitch `span()` brackets its body, so the
//!   stream is always well nested).
//! - **names** are interned to [`StringId`], emitting a `StringRegister` the
//!   first time each name is seen.
//! - **metrics** declare themselves with a `MetricRegister` once per name, then
//!   their `Value` is coerced to `i64`.
//!
//! Timestamps are a monotonic sequence counter here — a placeholder. The
//! post-hoc lowering of an already-recorded event vec has no real clock; the
//! push-based on-target path (lowering at emit time) takes `t` from the
//! `ClockNow` syscall instead.

use alloc::collections::{BTreeMap, BTreeSet};

use protocol::{Frame, MetricKind, SpanId, StringId};

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;
use crate::value::{TelemetryEvent, Value};

/// Receives a lowered frame. Mirrors the kernel's `FrameSink` so the on-target
/// path can reuse the same lowering against the real virtio-console sink.
pub trait FrameSink {
    fn emit(&mut self, frame: &Frame<'_>);
}

/// Lower a recorded telemetry stream into wire frames, pushing each into `sink`.
pub fn lower(events: &[TelemetryEvent], sink: &mut impl FrameSink) {
    let mut state = Lowerer::default();
    for event in events {
        state.lower_one(event, sink);
    }
}

/// The running state a stream lowering threads through every event: the clock
/// placeholder, the span-id allocator + parent stack, and the string interner.
#[derive(Default)]
struct Lowerer {
    t: u64,
    next_span_id: u64,
    span_stack: Vec<SpanId>,
    interned: BTreeMap<String, u32>,
    /// String ids that have already had a `MetricRegister` emitted — a metric
    /// declares its kind exactly once, like the kernel's intern table does.
    registered_metrics: BTreeSet<u32>,
}

impl Lowerer {
    fn lower_one(&mut self, event: &TelemetryEvent, sink: &mut impl FrameSink) {
        match event {
            TelemetryEvent::SpanOpen { name } => {
                let name_id = self.intern(name, sink);
                self.next_span_id += 1;
                let id = SpanId(self.next_span_id);
                let parent = self.span_stack.last().copied().unwrap_or(SpanId(0));
                sink.emit(&Frame::SpanStart {
                    id,
                    parent,
                    name_id,
                    t: self.tick(),
                    task_id: 0,
                    hart_id: 0,
                });
                self.span_stack.push(id);
            }
            TelemetryEvent::SpanClose { .. } => {
                if let Some(id) = self.span_stack.pop() {
                    sink.emit(&Frame::SpanEnd { id, t: self.tick() });
                }
            }
            TelemetryEvent::Emit { name, value } => {
                let Some(value) = coerce_i64(value) else {
                    return;
                };
                let StringId(id) = self.intern(name, sink);
                if self.registered_metrics.insert(id) {
                    sink.emit(&Frame::MetricRegister {
                        name_id: StringId(id),
                        kind: MetricKind::Gauge,
                        task_id: 0,
                    });
                }
                sink.emit(&Frame::Metric {
                    name_id: StringId(id),
                    value,
                    t: self.tick(),
                    hart_id: 0,
                });
            }
        }
    }

    /// The next monotonic timestamp. Placeholder: a sequence counter, not a real
    /// clock (see module docs — `ClockNow` feeds the push-based path).
    fn tick(&mut self) -> u64 {
        let now = self.t;
        self.t += 1;
        now
    }

    /// The `StringId` for `name`, emitting a `StringRegister` the first time the
    /// name is seen. Ids are dense and assigned in first-appearance order.
    fn intern(&mut self, name: &str, sink: &mut impl FrameSink) -> StringId {
        if let Some(&id) = self.interned.get(name) {
            return StringId(id);
        }
        let id = self.interned.len() as u32;
        self.interned.insert(name.into(), id);
        sink.emit(&Frame::StringRegister {
            id: StringId(id),
            value: name,
        });
        StringId(id)
    }
}

/// Coerce a Stitch metric value to the wire's `i64`. `Int` is exact; `Bool`
/// becomes 0/1; `Float` truncates (the wire metric is integral). Any other
/// value isn't a number and is dropped — the caller emits no metric for it.
/// Shared with the on-target backend ([`crate::telemetry`]) so host and metal
/// coerce identically.
pub(crate) fn coerce_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(n) => Some(*n),
        Value::Bool(b) => Some(i64::from(*b)),
        #[allow(
            clippy::cast_possible_truncation,
            reason = "wire Metric is i64; lossy float->int is the documented contract"
        )]
        Value::Float(f) => Some(*f as i64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::TelemetryEvent;
    use protocol::SpanId;
    use protocol::stream::OwnedFrame;

    /// A test sink that keeps every frame in decode-friendly owned form.
    #[derive(Default)]
    struct CapturingSink {
        frames: Vec<OwnedFrame>,
    }

    impl FrameSink for CapturingSink {
        fn emit(&mut self, frame: &Frame<'_>) {
            self.frames.push(OwnedFrame::from_borrowed(frame));
        }
    }

    fn lower_to_frames(events: &[TelemetryEvent]) -> Vec<OwnedFrame> {
        let mut sink = CapturingSink::default();
        lower(events, &mut sink);
        sink.frames
    }

    #[test]
    fn an_emit_lowers_to_register_declare_metric() {
        let events = [TelemetryEvent::Emit {
            name: "hits".into(),
            value: crate::value::Value::Int(5),
        }];

        let frames = lower_to_frames(&events);

        assert_eq!(
            frames,
            vec![
                OwnedFrame::StringRegister {
                    id: protocol::StringId(0),
                    value: "hits".into(),
                },
                OwnedFrame::MetricRegister {
                    name_id: protocol::StringId(0),
                    kind: protocol::MetricKind::Gauge,
                    task_id: 0,
                },
                OwnedFrame::Metric {
                    name_id: protocol::StringId(0),
                    value: 5,
                    t: 0,
                    hart_id: 0,
                },
            ],
        );
    }

    #[test]
    fn nested_spans_parent_the_inner_to_the_outer() {
        let events = [
            TelemetryEvent::SpanOpen {
                name: "outer".into(),
            },
            TelemetryEvent::SpanOpen {
                name: "inner".into(),
            },
            TelemetryEvent::SpanClose {
                name: "inner".into(),
            },
            TelemetryEvent::SpanClose {
                name: "outer".into(),
            },
        ];

        let frames = lower_to_frames(&events);

        let starts: Vec<_> = frames
            .iter()
            .filter_map(|f| match f {
                OwnedFrame::SpanStart { id, parent, .. } => Some((*id, *parent)),
                _ => None,
            })
            .collect();

        assert_eq!(
            starts,
            vec![
                (SpanId(1), SpanId(0)), // outer roots at SpanId(0)
                (SpanId(2), SpanId(1)), // inner parents to outer
            ],
        );
    }

    #[test]
    fn a_repeated_name_registers_its_string_once() {
        let events = [
            TelemetryEvent::Emit {
                name: "hits".into(),
                value: Value::Int(1),
            },
            TelemetryEvent::Emit {
                name: "hits".into(),
                value: Value::Int(2),
            },
        ];

        let frames = lower_to_frames(&events);

        let registers = frames
            .iter()
            .filter(|f| matches!(f, OwnedFrame::StringRegister { .. }))
            .count();
        let metric_registers = frames
            .iter()
            .filter(|f| matches!(f, OwnedFrame::MetricRegister { .. }))
            .count();
        let metrics = frames
            .iter()
            .filter(|f| matches!(f, OwnedFrame::Metric { .. }))
            .count();

        assert_eq!((registers, metric_registers, metrics), (1, 1, 2));
    }

    #[test]
    fn a_non_numeric_metric_value_emits_nothing() {
        let events = [TelemetryEvent::Emit {
            name: "label".into(),
            value: Value::Str("nope".into()),
        }];

        assert_eq!(lower_to_frames(&events), vec![]);
    }

    #[test]
    fn a_span_lowers_to_register_start_end() {
        let events = [
            TelemetryEvent::SpanOpen {
                name: "work".into(),
            },
            TelemetryEvent::SpanClose {
                name: "work".into(),
            },
        ];

        let frames = lower_to_frames(&events);

        assert_eq!(
            frames,
            vec![
                OwnedFrame::StringRegister {
                    id: protocol::StringId(0),
                    value: "work".into(),
                },
                OwnedFrame::SpanStart {
                    id: SpanId(1),
                    parent: SpanId(0),
                    name_id: protocol::StringId(0),
                    t: 0,
                    task_id: 0,
                    hart_id: 0,
                },
                OwnedFrame::SpanEnd {
                    id: SpanId(1),
                    t: 1
                },
            ],
        );
    }
}
