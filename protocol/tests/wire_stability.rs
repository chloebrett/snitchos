//! Golden-bytes guard for the on-wire encoding. Postcard encodes enum variants
//! (and struct fields) **positionally**, so reordering or inserting a `Frame`
//! variant — or a field within one — silently breaks every prior capture and any
//! out-of-tree consumer. Roundtrip tests can't catch it: the encoder and decoder
//! rebuild together, so a mid-enum insert passes them all. This pins the exact
//! bytes of a fixed exemplar of every `Frame` variant and every supporting-enum
//! arm. A wire change makes the snapshot diff, and must be deliberately re-blessed
//! (`cargo insta accept`) — the append-only rule, enforced rather than social.
//!
//! Adding a variant/arm at the END adds a line here (and bumps `PROTOCOL_VERSION`).

use std::fmt::Write as _;

use protocol::{
    CapEventKind, CapObject, Frame, HartRole, MetricKind, RefusalReason, SpanId, StringId,
    SwitchReason, PROTOCOL_VERSION,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Postcard-encode `value` to bytes (alloc dev-feature).
fn enc<T: serde::Serialize>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).expect("postcard encode")
}

#[test]
fn wire_encoding_is_stable() {
    let mut out = String::new();
    writeln!(out, "PROTOCOL_VERSION = {PROTOCOL_VERSION}\n").unwrap();

    out.push_str("== Frame variants (discriminant = first byte; append at END) ==\n");
    let name = [0u8; snitchos_abi::CAP_NAME_LEN];
    let frames: Vec<(&str, Vec<u8>)> = vec![
        ("Hello", enc(&Frame::Hello { timebase_hz: 1, protocol_version: 6 })),
        ("SpanStart", enc(&Frame::SpanStart { id: SpanId(1), parent: SpanId(2), name_id: StringId(3), t: 4, task_id: 5, hart_id: 6 })),
        ("SpanEnd", enc(&Frame::SpanEnd { id: SpanId(1), t: 2 })),
        ("Event", enc(&Frame::Event { span_id: SpanId(1), name_id: StringId(2), t: 3 })),
        ("Metric", enc(&Frame::Metric { name_id: StringId(1), value: -2, t: 3, hart_id: 4 })),
        ("Dropped", enc(&Frame::Dropped { count: 7 })),
        ("StringRegister", enc(&Frame::StringRegister { id: StringId(1), value: "x" })),
        ("MetricRegister", enc(&Frame::MetricRegister { name_id: StringId(1), kind: MetricKind::Gauge, task_id: 2 })),
        ("ThreadRegister", enc(&Frame::ThreadRegister { id: 1, name: "t", priority: 2 })),
        ("ContextSwitch", enc(&Frame::ContextSwitch { from: 1, to: 2, t: 3, reason: SwitchReason::Preempt, hart_id: 4 })),
        ("HartRegister", enc(&Frame::HartRegister { id: 1, mhartid: 2, role: HartRole::Worker })),
        ("CapEvent", enc(&Frame::CapEvent { kind: CapEventKind::Transferred, cap_id: 1, parent_cap_id: 2, holder: 3, object: CapObject::Endpoint, rights: 4, badge: 5, t: 6, hart_id: 7, name })),
        ("SyscallRefused", enc(&Frame::SyscallRefused { syscall: 1, reason: RefusalReason::Quota, task_id: 2, t: 3, hart_id: 4 })),
        ("Log", enc(&Frame::Log { msg: "l", task_id: 1, t: 2, hart_id: 3 })),
        ("Message", enc(&Frame::Message { endpoint: 1, from: 2, to: 3, parent_span: SpanId(4), t: 5, hart_id: 6 })),
        ("NotifySignal", enc(&Frame::NotifySignal { notification: 1, mask: 2, from_task: 3, t: 4, hart_id: 5 })),
        ("NotifyWait", enc(&Frame::NotifyWait { notification: 1, bits: 2, to_task: 3, t: 4, hart_id: 5 })),
    ];
    for (label, bytes) in &frames {
        writeln!(out, "{label:<16} {}", hex(bytes)).unwrap();
    }

    out.push_str("\n== Supporting enums (arm = discriminant byte; append at END) ==\n");
    let mut enum_arm = |label: String, bytes: Vec<u8>| {
        writeln!(out, "{label:<28} {}", hex(&bytes)).unwrap();
    };
    for k in [CapEventKind::Granted, CapEventKind::Transferred, CapEventKind::Revoked, CapEventKind::Minted] {
        enum_arm(format!("CapEventKind::{k:?}"), enc(&k));
    }
    for o in [CapObject::TelemetrySink, CapObject::SpanSink, CapObject::Endpoint, CapObject::Reply, CapObject::Notification] {
        enum_arm(format!("CapObject::{o:?}"), enc(&o));
    }
    for r in [SwitchReason::Yield, SwitchReason::Preempt, SwitchReason::Blocked, SwitchReason::Exit] {
        enum_arm(format!("SwitchReason::{r:?}"), enc(&r));
    }
    for m in [MetricKind::Counter, MetricKind::Gauge, MetricKind::Histogram] {
        enum_arm(format!("MetricKind::{m:?}"), enc(&m));
    }
    for h in [HartRole::Boot, HartRole::Worker] {
        enum_arm(format!("HartRole::{h:?}"), enc(&h));
    }
    for reason in [
        RefusalReason::UnknownSyscall,
        RefusalReason::NoProcess,
        RefusalReason::CapNotFound,
        RefusalReason::CapWrongRights,
        RefusalReason::CapWrongObject,
        RefusalReason::BadUserRange,
        RefusalReason::BadUtf8,
        RefusalReason::Quota,
        RefusalReason::BadSpanId,
        RefusalReason::OutOfMemory,
        RefusalReason::UnknownProgram,
        RefusalReason::BadMetricHandle,
        RefusalReason::BadMetricKind,
        RefusalReason::NotificationBusy,
        RefusalReason::CapNotDelegable,
    ] {
        enum_arm(format!("RefusalReason::{reason:?}"), enc(&reason));
    }

    insta::assert_snapshot!(out);
}
