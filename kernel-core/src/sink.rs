//! `FrameSink` — abstraction over "where does an encoded frame go."
//! The kernel implements this against virtio-console + the pre-init
//! buffer; tests implement it as an in-memory capture.

use protocol::Frame;

/// Receives a frame for transmission. Implementors decide whether to
/// encode and ship immediately, buffer, or just record (in tests).
///
/// Takes `&Frame<'_>` rather than `&[u8]` so implementors can choose
/// their own encoding boundary. The kernel-side impl encodes with
/// postcard; the test impl can store the frame structure directly.
pub trait FrameSink {
    fn emit(&mut self, frame: &Frame<'_>);
}

#[cfg(test)]
pub(crate) mod capture {
    //! Test-only capturing sink. Records frames as postcard-encoded
    //! bytes and exposes them via `decoded()` for assertions.
    //!
    //! Encoding is the realistic path — the kernel does the same encode
    //! before pushing to the wire — but tests get a typed `Frame` back
    //! to assert on. The `Vec<Vec<u8>>` storage is fine because test
    //! builds link std.

    use super::*;
    use protocol::Frame;
    extern crate std;
    use std::vec::Vec;

    pub struct CapturingSink {
        encoded: Vec<Vec<u8>>,
    }

    impl CapturingSink {
        pub fn new() -> Self {
            Self { encoded: Vec::new() }
        }

        /// Decode all captured frames. Returns owned bytes per frame so
        /// the caller can `postcard::from_bytes` at the assertion site.
        pub fn raw(&self) -> &[Vec<u8>] {
            &self.encoded
        }

        pub fn len(&self) -> usize {
            self.encoded.len()
        }
    }

    impl FrameSink for CapturingSink {
        fn emit(&mut self, frame: &Frame<'_>) {
            let mut buf = [0u8; 256];
            let bytes = postcard::to_slice(frame, &mut buf)
                .expect("test frame must fit in 256 bytes");
            self.encoded.push(bytes.to_vec());
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use protocol::SpanId;

        #[test]
        fn captures_emitted_frame() {
            let mut sink = CapturingSink::new();
            sink.emit(&Frame::SpanEnd { id: SpanId(7), t: 42 });
            assert_eq!(sink.len(), 1);
            let decoded: Frame = postcard::from_bytes(&sink.raw()[0]).unwrap();
            assert_eq!(decoded, Frame::SpanEnd { id: SpanId(7), t: 42 });
        }

        #[test]
        fn captures_in_order() {
            let mut sink = CapturingSink::new();
            sink.emit(&Frame::SpanEnd { id: SpanId(1), t: 10 });
            sink.emit(&Frame::SpanEnd { id: SpanId(2), t: 20 });
            assert_eq!(sink.len(), 2);
            let first: Frame = postcard::from_bytes(&sink.raw()[0]).unwrap();
            let second: Frame = postcard::from_bytes(&sink.raw()[1]).unwrap();
            assert_eq!(first, Frame::SpanEnd { id: SpanId(1), t: 10 });
            assert_eq!(second, Frame::SpanEnd { id: SpanId(2), t: 20 });
        }
    }
}
