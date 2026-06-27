//! Wire format for `SnitchOS` telemetry. Postcard-encoded `Frame` enum.
//! Postcard's encoding is self-delimiting, so frames are written
//! back-to-back with no outer length prefix; the decoder peels one frame
//! at a time (see `stream::try_decode_frame`).
//!
//! `no_std` so the kernel can use it; tests are hosted.

#![no_std]

use serde::{Serialize, Deserialize};

#[cfg(feature = "std")]
pub mod stream;

/// Wire-format version. The kernel emits this in `Frame::Hello` so the
/// host can sanity-check it understands the payload. Bumped on every
/// *breaking* change to the encoded layout — adding a field to an
/// existing variant (positional encoding), reordering variants, etc.
/// Adding a new variant at the end of the enum is technically
/// non-breaking but still bumps in practice because old collectors
/// won't decode the new variant.
///
/// History:
///   - 1: v0.1 — initial. Pre-`task_id` on `SpanStart`, pre-ContextSwitch.
///   - 2: v0.6 — added `hart_id` to `SpanStart` + `ContextSwitch`, added
///     `HartRegister` variant. The wire-format break performed before
///     any external consumer of v0.6 captures exists.
///   - 3: v0.6 closeout — added `hart_id` to `Metric` so the collector
///     keys metric state by `(name_id, hart_id)` instead of letting
///     same-named counters from different harts clobber each other.
///   - 4: added `task_id` to `MetricRegister` (the emitter dimension) so two
///     processes that register a metric with the same name stay distinct
///     Prometheus series rather than colliding into one family.
pub const PROTOCOL_VERSION: u8 = 4;

/// `MetricRegister.task_id` sentinel for a **kernel-global** metric — one
/// registered by the kernel itself (the `&'static` `register_counter`/`gauge`/
/// `histogram` path), not by a userspace process. The collector attaches no
/// emitter label to these, preserving their existing series. A real task id is
/// never `u32::MAX` (ids are small and dense), so the sentinel can't collide.
pub const NO_EMITTER: u32 = u32::MAX;

/// Identifier of a string in the runtime intern table. `StringRegister`
/// frames populate the table; every `*_name_id` field references it.
/// `u32` because the table has far fewer entries than spans do.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy, Hash)]
#[serde(transparent)]
pub struct StringId(pub u32);

/// Identifier of a span. Minted by the kernel as a per-CPU-partitioned
/// counter — `u64` because the design assumes long-running kernels with
/// many harts producing many spans.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy, Hash)]
#[serde(transparent)]
pub struct SpanId(pub u64);

/// Semantic kind of a metric. Declared once per metric name via
/// `Frame::MetricRegister`; the host uses this to format the metric
/// correctly (Prometheus counter vs gauge vs histogram).
///
/// Counters are monotonically increasing; gauges are snapshot values;
/// histograms hold distributions, observed via repeated `Metric` frames
/// and bucketed host-side by the collector.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy, Hash)]
pub enum MetricKind {
  Counter,
  Gauge,
  Histogram,
}

/// What role a hart plays in the system. Carried on
/// `Frame::HartRegister` so the host can label dashboards and traces.
/// Distinguishes the boot hart (runs heartbeat, ran pre-MMU setup)
/// from secondary worker harts.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
pub enum HartRole {
  Boot,
  Worker,
}

/// Why the scheduler picked a different task. Carried on
/// `Frame::ContextSwitch` so traces show *why* a switch happened, not
/// just that one did.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
pub enum SwitchReason {
  /// Running task voluntarily called `yield_now`.
  Yield,
  /// Running task was preempted by the timer IRQ. Not used in v0.5
  /// (cooperative only); reserved for v0.5.x.
  Preempt,
  /// Running task hit a blocking primitive and went off-CPU.
  /// Placeholder until v0.5.x adds real blocking.
  Blocked,
  /// Running task ran its entry function to completion. Not emitted in
  /// v0.5 (tasks are `-> !`); reserved for the task-exit feature.
  Exit,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum Frame<'a> {
  Hello { timebase_hz: u64, protocol_version: u8 },
  SpanStart { id: SpanId, parent: SpanId, name_id: StringId, t: u64, task_id: u32, hart_id: u8 },
  SpanEnd { id: SpanId, t: u64 },
  /// A point-in-time annotation on a span (the OTLP "span event"
  /// primitive). **Reserved, no emitter yet:** the wire slot and the
  /// roundtrip test exist, but no kernel site produces one and the
  /// collector parks it. First emitter is expected around v0.8, when
  /// IPC gives spans something worth annotating mid-flight (e.g. a
  /// message send/receive marker on a cross-process span). Kept in
  /// place rather than removed because postcard's positional enum
  /// encoding means deleting it would renumber every later variant and
  /// break the wire format. See `docs/observability-design.md`
  /// ("profiling rides on Event").
  Event { span_id: SpanId, name_id: StringId, t: u64 },
  Metric { name_id: StringId, value: i64, t: u64, hart_id: u8 },
  Dropped { count: u32 },
  StringRegister { id: StringId, value: &'a str },
  /// Declares a metric's [`MetricKind`] and **emitter** once per name. `task_id`
  /// is the task that registered it — the dimension that keeps two processes
  /// which named a metric identically (distinct `StringId`s, same string) as
  /// distinct Prometheus series; [`NO_EMITTER`] marks a kernel-global metric
  /// (no emitter label). Appended at the END of the struct so postcard's
  /// positional encoding stays wire-compatible (cf. `ThreadRegister.priority`).
  MetricRegister { name_id: StringId, kind: MetricKind, task_id: u32 },
  /// One emitted per `spawn()`. Lets the collector resolve numeric
  /// task ids in subsequent frames to human-readable names. `priority`
  /// is the task's static scheduling level (0 = Low, 1 = Normal, 2 = High)
  /// — appended at the END of the struct so postcard wire compat holds.
  ThreadRegister { id: u32, name: &'a str, priority: u8 },
  /// Scheduler swapped from `from` to `to` at time `t`. New variants
  /// of `Frame` go at the END of the enum — postcard encodes
  /// discriminants positionally and reordering breaks wire compat
  /// for all prior captures.
  ContextSwitch { from: u32, to: u32, t: u64, reason: SwitchReason, hart_id: u8 },
  /// One emitted per hart at bring-up. `id` is the dense `0..MAX_HARTS`
  /// logical hart id used by all other frames; `mhartid` is the
  /// platform-assigned id from the SBI handoff (may be sparse or
  /// non-zero based). `role` labels the hart's purpose for dashboards.
  HartRegister { id: u8, mhartid: u64, role: HartRole },
  /// An **authority decision** — a capability's lifecycle event. v0.7b
  /// emits only `Granted` (the bootstrap `TelemetrySink`); the kernel
  /// snitches authority being *created*, which a counter can't describe
  /// (granter, object, rights). Designed for host-side reconstruction of
  /// the capability derivation tree: `cap_id` is a **global** id minted
  /// per grant (NOT the per-process `Handle`, which is local), and
  /// `parent_cap_id` is the cap this one was derived from (`0` = root;
  /// always `0` until v0.8 introduces transfer/attenuation). New variants
  /// go at the END — postcard encodes discriminants positionally. See
  /// `docs/capability-system-design.md` ("authority as a host-reconstructed
  /// tree").
  CapEvent {
    kind: CapEventKind,
    cap_id: u64,
    parent_cap_id: u64,
    holder: u32,
    object: CapObject,
    rights: u32,
    /// The endpoint cap's server-chosen demux value (v0.9c); `0` for objects
    /// that carry no badge (bootstrap grants, reply caps). Kernel-opaque.
    badge: u64,
    t: u64,
    hart_id: u8,
  },
  /// The kernel **refused a syscall**, and why. A first-class observability
  /// event so a denied U-mode request is never silent — `syscall` is the raw
  /// `a7` number, `reason` says what failed, `task_id` attributes the caller.
  /// Turns "no result frame appeared" debugging into a labelled signal. New
  /// variants go at the END — postcard encodes discriminants positionally.
  SyscallRefused {
    syscall: u8,
    reason: RefusalReason,
    task_id: u32,
    t: u64,
    hart_id: u8,
  },
  /// A userspace **stdout line** — the bytes a program wrote via `DebugWrite`
  /// (backing `println!`). Making stdout a wire frame keeps it observable
  /// (the collector can surface it as logs), attributed to `task_id`. New
  /// variants go at the END — postcard encodes discriminants positionally.
  Log { msg: &'a str, task_id: u32, t: u64, hart_id: u8 },
  /// A synchronous **IPC rendezvous** (v0.9): a message crossed from task
  /// `from` to task `to` over endpoint `endpoint`. `parent_span` is the
  /// sender's open span at send time, carried so the host can root the
  /// receiver's handling span under it — the trace following the message
  /// across the process boundary. New variants go at the END — postcard
  /// encodes discriminants positionally.
  Message { endpoint: u32, from: u32, to: u32, parent_span: SpanId, t: u64, hart_id: u8 },
  /// A **notification was signalled** (v0.12): `from_task` OR-ed `mask` into
  /// `notification`'s pending bits — the producer end of the async kernel→user
  /// signal. Paired with [`NotifyWait`](Self::NotifyWait), these make the
  /// out-of-band wake visible: a signal at one time, a waiter waking at another,
  /// linked by `notification` — a dependency arrow that is *not* a call stack.
  /// To keep a high-rate source from flooding the wire, the kernel emits on the
  /// empty→nonempty transition + each delivered wake, not every redundant OR.
  /// New variants go at the END — postcard encodes discriminants positionally.
  NotifySignal { notification: u32, mask: u64, from_task: u32, t: u64, hart_id: u8 },
  /// A **notification waiter woke** (v0.12): `to_task` took `bits` from
  /// `notification` (read-and-cleared), either immediately (bits were pending)
  /// or after blocking until a [`NotifySignal`](Self::NotifySignal). The consumer
  /// half of the async edge. New variants go at the END — postcard encodes
  /// discriminants positionally.
  NotifyWait { notification: u32, bits: u64, to_task: u32, t: u64, hart_id: u8 },
}

/// The lifecycle phase of a [`Frame::CapEvent`]. v0.7b emits only
/// `Granted`; the rest are reserved wire slots (append new kinds at the
/// END — postcard is positional). `Invoked`/`Denied` are audit events;
/// `Revoked`/`Transferred` are the derivation-tree edges v0.8 adds.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
pub enum CapEventKind {
  /// A new capability was created and handed to `holder`.
  Granted,
  /// A capability was handed from one holder to another (v0.9b): the kernel
  /// minting a one-shot reply cap into the server at a `call` rendezvous is the
  /// first instance. `parent_cap_id` is the cap it derived from.
  Transferred,
}

/// What a [`Frame::CapEvent`]'s capability points at. v0.7b has one object
/// type; append future kinds (`Endpoint`, `MemoryRegion`, …) at the END.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
pub enum CapObject {
  /// Permission to emit telemetry to a bound counter.
  TelemetrySink,
  /// Permission to open and close spans on the holder's span cursor.
  SpanSink,
  /// A synchronous IPC endpoint (v0.9). The bound endpoint id lives
  /// kernel-side; this wire tag attributes the grant to the endpoint kind.
  Endpoint,
  /// A one-shot reply authority (v0.9b) — the cap the kernel mints into a
  /// server so it can answer a blocked `call`er exactly once.
  Reply,
  /// A notification — the general async kernel→user signal (v0.12). The bound
  /// notification id lives kernel-side; this wire tag attributes the grant to
  /// the notification kind.
  Notification,
}

/// Why the kernel refused a syscall (the `reason` in [`Frame::SyscallRefused`]).
/// One per distinguishable failure so a denied request is self-describing on
/// the wire. Append new reasons at the END — postcard encodes positionally.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
pub enum RefusalReason {
  /// `a7` named no known syscall.
  UnknownSyscall,
  /// No user process is bound to the calling hart (should not happen).
  NoProcess,
  /// The capability handle resolved to nothing (out of bounds or stale).
  CapNotFound,
  /// The capability lacked the right the operation needs.
  CapWrongRights,
  /// The capability named a different object kind than the op targets.
  CapWrongObject,
  /// A user pointer/length was not a valid, in-bounds user buffer.
  BadUserRange,
  /// A copied-in name was not valid UTF-8.
  BadUtf8,
  /// The caller hit its per-process span-name quota.
  Quota,
  /// A span-close named an id that isn't the caller's innermost open span
  /// (out-of-order or forged close).
  BadSpanId,
  /// A memory request could not be satisfied — out of physical frames, or
  /// past the per-process memory cap.
  OutOfMemory,
  /// A `Spawn` named a program id that is not in the spawnable registry.
  UnknownProgram,
  /// An `EmitMetric` named a metric handle the calling process never
  /// registered (out of range in its per-process metric table) — the
  /// userspace-defined-metrics forgery boundary.
  BadMetricHandle,
  /// A `RegisterMetric` carried a metric-kind selector that names no
  /// `MetricKind` (not Counter/Gauge/Histogram).
  BadMetricKind,
  /// A `WaitNotify` targeted a notification that already has a parked waiter —
  /// one waiter per notification in v0.12 (the second waiter is refused, never
  /// silently dropped, so the first parker can't be stranded).
  NotificationBusy,
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Roundtrip a `Frame::Hello` through postcard and back.
  #[test]
  fn hello_roundtrips() {
    let frame = Frame::Hello {
      timebase_hz: 10_000_000,
      protocol_version: PROTOCOL_VERSION,
    };

    // Encode into a fixed buffer; no allocator needed.
    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();

    // Decode back.
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::SpanEnd` through postcard and back.
  #[test]
  fn span_end_roundtrips() {
    let frame = Frame::SpanEnd {
      id: SpanId(511),
      t: 1234,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::CapEvent` through postcard and back — the
  /// authority-lifecycle event (v0.7b emits `Granted`).
  #[test]
  fn cap_event_roundtrips() {
    let frame = Frame::CapEvent {
      kind: CapEventKind::Granted,
      cap_id: 1,
      parent_cap_id: 0,
      holder: 7,
      object: CapObject::TelemetrySink,
      rights: 0b0001,
      badge: 0,
      t: 1234,
      hart_id: 1,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::CapEvent` carrying the `SpanSink` object — the
  /// second capability kind, granted alongside the bootstrap `TelemetrySink`.
  #[test]
  fn cap_event_spansink_roundtrips() {
    let frame = Frame::CapEvent {
      kind: CapEventKind::Granted,
      cap_id: 2,
      parent_cap_id: 0,
      holder: 7,
      object: CapObject::SpanSink,
      rights: 0b0001,
      badge: 0,
      t: 5678,
      hart_id: 1,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::CapEvent` carrying the `Endpoint` object — the
  /// v0.9 IPC capability kind, appended after the v0.7b objects.
  #[test]
  fn cap_event_endpoint_roundtrips() {
    let frame = Frame::CapEvent {
      kind: CapEventKind::Granted,
      cap_id: 3,
      parent_cap_id: 0,
      holder: 7,
      object: CapObject::Endpoint,
      rights: 0b0010,
      badge: 0,
      t: 9012,
      hart_id: 1,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::Message` — the v0.9 IPC rendezvous record. Carries
  /// the trace link (`parent_span`, the sender's open span) so the host can
  /// stitch the receiver's work under the sender's span across the boundary.
  #[test]
  fn message_roundtrips() {
    let frame = Frame::Message {
      endpoint: 2,
      from: 4,
      to: 5,
      parent_span: SpanId(42),
      t: 1234,
      hart_id: 1,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::CapEvent` carrying the `Transferred` kind + the
  /// `Reply` object — the v0.9b reply-cap grant (a derivation-tree edge:
  /// the reply cap derived from the `call`).
  #[test]
  fn cap_event_transferred_reply_roundtrips() {
    let frame = Frame::CapEvent {
      kind: CapEventKind::Transferred,
      cap_id: 4,
      parent_cap_id: 1,
      holder: 8,
      object: CapObject::Reply,
      rights: 0,
      badge: 0,
      t: 3456,
      hart_id: 1,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::CapEvent` carrying the `Notification` object — the
  /// v0.12 notification-cap grant. Appended at the end of `CapObject`, so its
  /// postcard discriminant must not disturb the earlier kinds.
  #[test]
  fn cap_event_granted_notification_roundtrips() {
    let frame = Frame::CapEvent {
      kind: CapEventKind::Granted,
      cap_id: 6,
      parent_cap_id: 0,
      holder: 2,
      object: CapObject::Notification,
      rights: 0b11_0000,
      badge: 0,
      t: 7890,
      hart_id: 0,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::CapEvent` carrying a nonzero `badge` — the v0.9c
  /// server-chosen demux value the kernel delivers to the receiver. Proves the
  /// badge survives the wire (and, via `OwnedFrame`, the decode side).
  #[test]
  fn cap_event_carries_a_badge_roundtrips() {
    let frame = Frame::CapEvent {
      kind: CapEventKind::Transferred,
      cap_id: 9,
      parent_cap_id: 2,
      holder: 5,
      object: CapObject::Endpoint,
      rights: 0b0010,
      badge: 0xCAFE,
      t: 7777,
      hart_id: 1,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::SpanStart` through postcard and back.
  #[test]
  fn span_start_roundtrips() {
    let frame = Frame::SpanStart {
      id: SpanId(42),
      parent: SpanId(7),
      name_id: StringId(3),
      t: 1234,
      task_id: 0,
      hart_id: 0,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::Event` through postcard and back.
  #[test]
  fn event_roundtrips() {
    let frame = Frame::Event {
      span_id: SpanId(42),
      name_id: StringId(9),
      t: 1234,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::Metric` through postcard and back. Includes a
  /// negative value to exercise postcard's zigzag varint encoding for
  /// signed integers.
  #[test]
  fn metric_roundtrips() {
    let frame = Frame::Metric {
      name_id: StringId(12),
      value: -42,
      t: 1234,
      hart_id: 0,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// `Metric` carries `hart_id` (v0.6 closeout) so the collector can
  /// key same-named counters by the hart that emitted them rather than
  /// clobbering across harts. Verify with a non-zero hart id so an
  /// "always 0" mutant can't pass.
  #[test]
  fn metric_carries_hart_id() {
    let frame = Frame::Metric {
      name_id: StringId(12),
      value: -42,
      t: 1234,
      hart_id: 1,
    };
    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();
    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::Dropped` through postcard and back.
  #[test]
  fn dropped_roundtrips() {
    let frame = Frame::Dropped { count: 17 };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::StringRegister` through postcard and back. The
  /// `value` field is a borrowed `&str` — the decoded frame borrows from
  /// the encode buffer, which must outlive the decoded value.
  #[test]
  fn string_register_roundtrips() {
    let frame = Frame::StringRegister {
      id: StringId(99),
      value: "kernel.heartbeat",
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::ThreadRegister` through postcard and back.
  /// One emitted per `spawn()` so the collector can resolve numeric
  /// task ids to human-readable names.
  #[test]
  fn thread_register_roundtrips() {
    let frame = Frame::ThreadRegister {
      id: 7,
      name: "task_heartbeat",
      priority: 2,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::ContextSwitch` through postcard for each
  /// `SwitchReason`.
  #[test]
  fn context_switch_roundtrips_each_reason() {
    for reason in [
      SwitchReason::Yield,
      SwitchReason::Preempt,
      SwitchReason::Blocked,
      SwitchReason::Exit,
    ] {
      let frame = Frame::ContextSwitch {
        from: 2,
        to: 3,
        t: 1234,
        reason,
        hart_id: 0,
      };

      let mut buf = [0u8; 64];
      let used = postcard::to_slice(&frame, &mut buf).unwrap();
      let decoded: Frame = postcard::from_bytes(used).unwrap();

      assert_eq!(frame, decoded);
    }
  }

  /// `SpanStart` now carries `task_id` (post v0.5 step 3). Verify the
  /// roundtrip with a non-zero task id.
  #[test]
  fn span_start_carries_task_id() {
    let frame = Frame::SpanStart {
      id: SpanId(42),
      parent: SpanId(7),
      name_id: StringId(3),
      t: 1234,
      task_id: 5,
      hart_id: 0,
    };

    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();

    assert_eq!(frame, decoded);
  }

  /// `ContextSwitch` carries `hart_id` (v0.6 step 3) so a trace can
  /// distinguish "task 5 → idle on hart 0" from "task 5 → idle on
  /// hart 1." Verify with a non-zero hart id.
  #[test]
  fn context_switch_carries_hart_id() {
    let frame = Frame::ContextSwitch {
      from: 1,
      to: 2,
      t: 1234,
      reason: SwitchReason::Yield,
      hart_id: 1,
    };
    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();
    assert_eq!(frame, decoded);
  }

  /// `SpanStart` now also carries `hart_id` (v0.6 step 3). Verify
  /// the roundtrip with a non-zero hart id so an "always 0" mutant
  /// can't pass.
  #[test]
  fn span_start_carries_hart_id() {
    let frame = Frame::SpanStart {
      id: SpanId(42),
      parent: SpanId(7),
      name_id: StringId(3),
      t: 1234,
      task_id: 0,
      hart_id: 1,
    };
    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();
    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::HartRegister` for each `HartRole`. v0.6
  /// emits one of these per hart at bring-up so the collector can
  /// resolve `hart_id` (dense `0..MAX_HARTS`) to the platform
  /// `mhartid` and to a role label for trace/dashboard display.
  #[test]
  fn hart_register_roundtrips() {
    for role in [HartRole::Boot, HartRole::Worker] {
      let frame = Frame::HartRegister {
        id: 0,
        mhartid: 0,
        role,
      };
      let mut buf = [0u8; 64];
      let used = postcard::to_slice(&frame, &mut buf).unwrap();
      let decoded: Frame = postcard::from_bytes(used).unwrap();
      assert_eq!(frame, decoded);
    }
  }

  /// Roundtrip a `Frame::MetricRegister` for each `MetricKind`, carrying the
  /// registering task (the emitter dimension) — a real task id and the
  /// `NO_EMITTER` sentinel (a kernel-global metric). Declares metric type +
  /// emitter once per name; subsequent `Metric` frames look up both by `name_id`.
  #[test]
  fn metric_register_roundtrips() {
    for kind in [MetricKind::Counter, MetricKind::Gauge, MetricKind::Histogram] {
      for task_id in [4u32, NO_EMITTER] {
        let frame = Frame::MetricRegister {
          name_id: StringId(7),
          kind,
          task_id,
        };

        let mut buf = [0u8; 64];
        let used = postcard::to_slice(&frame, &mut buf).unwrap();
        let decoded: Frame = postcard::from_bytes(used).unwrap();

        assert_eq!(frame, decoded);
      }
    }
  }

  /// Roundtrip a `Frame::Log` — a userspace stdout line on the wire.
  #[test]
  fn log_roundtrips() {
    let frame = Frame::Log {
      msg: "hello from userspace",
      task_id: 5,
      t: 1234,
      hart_id: 1,
    };
    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();
    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::SyscallRefused` for each `RefusalReason` — the
  /// self-describing "the kernel said no, and here's why" event.
  #[test]
  fn syscall_refused_roundtrips_each_reason() {
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
    ] {
      let frame = Frame::SyscallRefused {
        syscall: 3,
        reason,
        task_id: 5,
        t: 1234,
        hart_id: 1,
      };
      let mut buf = [0u8; 64];
      let used = postcard::to_slice(&frame, &mut buf).unwrap();
      let decoded: Frame = postcard::from_bytes(used).unwrap();
      assert_eq!(frame, decoded);
    }
  }

  /// Roundtrip a `Frame::NotifySignal` — a producer signalled a notification
  /// (v0.12). The async edge: `from_task` signals `notification` with `mask`.
  #[test]
  fn notify_signal_roundtrips() {
    let frame = Frame::NotifySignal {
      notification: 3,
      mask: 0b101,
      from_task: 7,
      t: 4242,
      hart_id: 1,
    };
    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();
    assert_eq!(frame, decoded);
  }

  /// Roundtrip a `Frame::NotifyWait` — a consumer woke on a notification
  /// (v0.12). `to_task` received `bits` from `notification`; paired with a
  /// `NotifySignal`, these draw the async dependency arrow in Tempo.
  #[test]
  fn notify_wait_roundtrips() {
    let frame = Frame::NotifyWait {
      notification: 3,
      bits: 0b101,
      to_task: 9,
      t: 4243,
      hart_id: 1,
    };
    let mut buf = [0u8; 64];
    let used = postcard::to_slice(&frame, &mut buf).unwrap();
    let decoded: Frame = postcard::from_bytes(used).unwrap();
    assert_eq!(frame, decoded);
  }
}
