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
pub const PROTOCOL_VERSION: u8 = 2;

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
  Metric { name_id: StringId, value: i64, t: u64 },
  Dropped { count: u32 },
  StringRegister { id: StringId, value: &'a str },
  MetricRegister { name_id: StringId, kind: MetricKind },
  /// One emitted per `spawn()`. Lets the collector resolve numeric
  /// task ids in subsequent frames to human-readable names.
  ThreadRegister { id: u32, name: &'a str },
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

  /// Roundtrip a `Frame::MetricRegister` for each `MetricKind`.
  /// Declares metric type once per name; subsequent `Metric` frames
  /// look up the kind by `name_id`.
  #[test]
  fn metric_register_roundtrips() {
    for kind in [MetricKind::Counter, MetricKind::Gauge, MetricKind::Histogram] {
      let frame = Frame::MetricRegister {
        name_id: StringId(7),
        kind,
      };

      let mut buf = [0u8; 64];
      let used = postcard::to_slice(&frame, &mut buf).unwrap();
      let decoded: Frame = postcard::from_bytes(used).unwrap();

      assert_eq!(frame, decoded);
    }
  }
}
