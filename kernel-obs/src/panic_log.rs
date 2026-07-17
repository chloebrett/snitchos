//! Panic-safe encoding of the kernel's dying breath into a telemetry frame.
//!
//! A kernel panic can fire from anywhere — inside the allocator, the virtio TX
//! lock, or the intern table — so the panic path must not allocate or intern.
//! It reuses [`protocol::Frame::Log`], which inlines its message as a `&str`
//! (no `StringId`, no interning), and encodes it into a **caller-provided fixed
//! buffer** with no heap. This module is that pure encode step, host-tested; the
//! kernel side (see `plans/legacy/panic-emits-telemetry.md`) supplies a `static` buffer
//! and the panic-safe task/hart/timestamp reads, then pushes the bytes to the
//! virtio-console via a non-blocking `try_lock`.

use protocol::Frame;

/// Encode a panic [`Frame::Log`] into `buf` (postcard, no allocation). Returns
/// the number of bytes written, or `None` if `buf` is too small — the safety
/// valve that keeps the panic path from ever writing past its fixed `static`
/// buffer. `Log` inlines `msg` as a `&str`, so no interning or heap is touched.
#[must_use]
pub fn encode(buf: &mut [u8], msg: &str, task_id: u32, t: u64, hart_id: u8) -> Option<usize> {
    postcard::to_slice(&Frame::Log { msg, task_id, t, hart_id }, buf)
        .ok()
        .map(|written| written.len())
}

/// A [`core::fmt::Write`] sink over a caller-provided fixed buffer, used by the
/// panic path to format the dying `PanicInfo` (location + reason) with **no
/// allocation**. Once the buffer fills, further writes are dropped at a UTF-8
/// char boundary — a multi-byte char is never split and nothing is ever written
/// past the end — so [`as_str`](MsgWriter::as_str) is valid UTF-8 by
/// construction. `write_str` never errors: a dying kernel wants the prefix it
/// managed to format, not a formatting failure.
pub struct MsgWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> MsgWriter<'a> {
    #[must_use]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    /// The formatted prefix so far. Always valid UTF-8 — only whole chars are
    /// ever appended — so the `unwrap_or` fallback can never actually fire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for MsgWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for ch in s.chars() {
            let end = self.len + ch.len_utf8();
            if end > self.buf.len() {
                break;
            }
            ch.encode_utf8(&mut self.buf[self.len..end]);
            self.len = end;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::encode;
    use protocol::Frame;

    #[test]
    fn a_panic_log_encodes_and_roundtrips() {
        // The whole point: encode with no allocation into a fixed buffer, and the
        // bytes decode back to exactly the Log we asked for.
        let mut buf = [0u8; 256];
        let n = encode(&mut buf, "kernel panic", 3, 42, 1).expect("fits in 256 bytes");
        let decoded: Frame = postcard::from_bytes(&buf[..n]).expect("decodes");
        assert_eq!(
            decoded,
            Frame::Log { msg: "kernel panic", task_id: 3, t: 42, hart_id: 1 }
        );
    }

    #[test]
    fn a_buffer_too_small_returns_none_not_a_write_past_the_end() {
        // The safety valve: the panic path hands a `static` buffer of fixed size;
        // if the message wouldn't fit, `encode` must refuse (None), never write
        // past the buffer. A 2-byte buffer can't hold the frame.
        let mut buf = [0u8; 2];
        assert_eq!(encode(&mut buf, "kernel panic", 0, 0, 0), None);
    }

    #[test]
    fn a_dynamic_message_formats_into_a_fixed_buffer_with_no_alloc() {
        // Increment 6: the panic path formats the real `PanicInfo` (location +
        // reason) into a fixed buffer via `core::fmt::Write` — no heap. Whatever
        // fits reads back verbatim as a `&str`.
        use super::MsgWriter;
        use core::fmt::Write;
        let mut buf = [0u8; 64];
        let mut w = MsgWriter::new(&mut buf);
        write!(w, "kernel panic: {}", "boom at line 7").expect("write never errors");
        assert_eq!(w.as_str(), "kernel panic: boom at line 7");
    }

    #[test]
    fn a_message_that_exactly_fills_the_buffer_is_kept_whole() {
        // The boundary itself: a char whose last byte lands on the final buffer
        // slot must be kept (`end == len` fits), not dropped. Guards the `>` in
        // the overflow check from sliding to `>=`, which would lose the last char
        // of any message that fits exactly.
        use super::MsgWriter;
        use core::fmt::Write;
        let mut buf = [0u8; 5];
        let mut w = MsgWriter::new(&mut buf);
        let _ = write!(w, "hello");
        assert_eq!(w.as_str(), "hello");
    }

    #[test]
    fn an_overflowing_message_truncates_at_a_char_boundary() {
        // A dying kernel wants the prefix it got, not a formatting failure or a
        // split multi-byte char. 'é' is two bytes; a 5-byte buffer holds two of
        // them (4 bytes) and must drop the third whole — `as_str` stays valid
        // UTF-8, never a partial code point, never a write past the end.
        use super::MsgWriter;
        use core::fmt::Write;
        let mut buf = [0u8; 5];
        let mut w = MsgWriter::new(&mut buf);
        let _ = write!(w, "ééé");
        assert_eq!(w.as_str(), "éé");
    }
}
