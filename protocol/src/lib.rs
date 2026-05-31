//! Wire format for SnitchOS telemetry. Postcard-encoded `Frame` enum,
//! length-prefixed on the wire (the framing is the transport's job, not
//! this crate's).
//!
//! `no_std` so the kernel can use it; tests are hosted.

#![no_std]

use serde::{Serialize, Deserialize};

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

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum Frame<'a> {
  Hello { timebase_hz: u64, protocol_version: u8 },
  SpanStart { id: SpanId, parent: SpanId, name_id: StringId, t: u64 },
  SpanEnd { id: SpanId, t: u64 },
  Event { span_id: SpanId, name_id: StringId, t: u64 },
  Metric { name_id: StringId, value: i64, t: u64 },
  Dropped { count: u32 },
  StringRegister { id: StringId, value: &'a str },
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Roundtrip a `Frame::Hello` through postcard and back.
  #[test]
  fn hello_roundtrips() {
    let frame = Frame::Hello {
      timebase_hz: 10_000_000,
      protocol_version: 1,
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
}
