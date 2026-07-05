//! Panic-safe encoding of the kernel's dying breath into a telemetry frame.
//!
//! A kernel panic can fire from anywhere — inside the allocator, the virtio TX
//! lock, or the intern table — so the panic path must not allocate or intern.
//! It reuses [`protocol::Frame::Log`], which inlines its message as a `&str`
//! (no `StringId`, no interning), and encodes it into a **caller-provided fixed
//! buffer** with no heap. This module is that pure encode step, host-tested; the
//! kernel side (see `plans/panic-emits-telemetry.md`) supplies a `static` buffer
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
}
