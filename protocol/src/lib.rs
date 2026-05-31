//! Wire format for SnitchOS telemetry. Postcard-encoded `Frame` enum,
//! length-prefixed on the wire (the framing is the transport's job, not
//! this crate's).
//!
//! `no_std` so the kernel can use it; tests are hosted.

#![no_std]

use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
enum Frame {
  Hello { timebase_hz: u64, protocol_version: u8 },
  SpanStart { id: u64, parent: u64, name_id: u32, t: u64 },
  SpanEnd { id: u64, t: u64 },
  Event { span_id: u64, name_id: u32, t: u64 },
  Metric { name_id: u32, value: i64, t: u64 },
  Dropped { count: u32 },
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
      id: 511,
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
      id: 42,
      parent: 7,
      name_id: 3,
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
      span_id: 42,
      name_id: 9,
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
      name_id: 12,
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
}
