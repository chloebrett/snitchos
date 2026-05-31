//! Wire format for SnitchOS telemetry. Postcard-encoded `Frame` enum,
//! length-prefixed on the wire (the framing is the transport's job, not
//! this crate's).
//!
//! `no_std` so the kernel can use it; tests are hosted.

#![no_std]

use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
enum Frame {
  Hello { timebase_hz: u64, protocol_version: u8 }
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
}
