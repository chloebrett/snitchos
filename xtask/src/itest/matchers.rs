//! Frame-matcher helpers. Each returns a closure
//! `(&OwnedFrame, &StringTable) -> bool` so scenarios can drop them
//! straight into `Harness::wait_for`.

use protocol::stream::OwnedFrame;
use protocol::{CapEventKind, CapObject};

use super::harness::StringTable;

pub fn is_hello() -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    |f, _| matches!(f, OwnedFrame::Hello { .. })
}

pub fn is_span_start_named(name: &'static str) -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    move |f, strings| match f {
        OwnedFrame::SpanStart { name_id, .. } => {
            strings.get(name_id).map(String::as_str) == Some(name)
        }
        _ => false,
    }
}

pub fn is_dropped(expected: u32) -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    move |f, _| matches!(f, OwnedFrame::Dropped { count } if *count == expected)
}

pub fn is_string_register_named(name: &'static str) -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    move |f, _| matches!(f, OwnedFrame::StringRegister { value, .. } if value == name)
}

pub fn is_thread_register_named(name: &'static str) -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    move |f, _| matches!(f, OwnedFrame::ThreadRegister { name: n, .. } if n == name)
}

pub fn is_metric_named(name: &'static str) -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    move |f, strings| match f {
        OwnedFrame::Metric { name_id, .. } => {
            strings.get(name_id).map(String::as_str) == Some(name)
        }
        _ => false,
    }
}

/// A `CapEvent::Granted` for a `TelemetrySink` whose rights carry `EMIT`
/// (bit 0) — the bootstrap grant as a first-class authority event.
pub fn is_cap_granted_telemetry() -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    |f, _| matches!(
        f,
        OwnedFrame::CapEvent {
            kind: CapEventKind::Granted,
            object: CapObject::TelemetrySink,
            rights,
            ..
        } if rights & 0b0001 != 0
    )
}

/// A `CapEvent::Granted` for a `SpanSink` whose rights carry `EMIT` (bit 0) —
/// the second bootstrap grant, the authority to open spans from U-mode.
pub fn is_cap_granted_span() -> impl Fn(&OwnedFrame, &StringTable) -> bool {
    |f, _| matches!(
        f,
        OwnedFrame::CapEvent {
            kind: CapEventKind::Granted,
            object: CapObject::SpanSink,
            rights,
            ..
        } if rights & 0b0001 != 0
    )
}
