//! Answering a completion request — the pure half of the kvetch server.
//!
//! Everything here is host-testable: it works on a byte slice, not on the
//! client pointer the wire carries. `user/kvetch` does the one unsafe
//! conversion from `(ptr, cap)` to `&mut [u8]` and calls in here, keeping the
//! target-side glue thin enough to need no unit tests of its own (the repo's
//! statics/`TrapFrame`/MMIO boundary rule).

use kvetch_proto::{Reply, Status};

use crate::{Stop, Tables, walk_from};

/// Complete in place.
///
/// `buf[..prefix_len]` is the prefix; the completion is appended after it and
/// the reply says how many bytes were added. The buffer is both input and
/// output because the four-word message has no room for two of them.
///
/// A completion is a **fragment** — it extends the prefix legally and stops on
/// the token budget, leaving the buffer *viable* (still extendable to
/// something valid). It does not try to finish the program.
#[must_use]
pub fn handle_request(buf: &mut [u8], prefix_len: usize, max_tokens: u32, seed: u64) -> Reply {
    let Some(prefix_bytes) = buf.get(..prefix_len) else {
        return Reply { status: Status::Malformed, written: 0 };
    };
    // The prefix arrives from another process; it is text only if it says so.
    let Ok(prefix) = core::str::from_utf8(prefix_bytes) else {
        return Reply { status: Status::Malformed, written: 0 };
    };

    let walked = walk_from(prefix, seed, &Tables::DEFAULT, Stop::AfterTokens(max_tokens as usize));

    // Trim to what fits — but only ever at a token boundary. Cutting mid-token
    // could leave a dead buffer (or a different token from the one the oracle
    // approved), destroying the one property a fragment must keep.
    let fits = |end: usize| end <= buf.len();
    let cut = walked
        .steps
        .iter()
        .map(|step| step.source_before.len())
        .chain(core::iter::once(walked.source.len()))
        .filter(|end| fits(*end))
        .max()
        .unwrap_or(prefix_len);

    let completion = &walked.source.as_bytes()[prefix_len..cut];
    buf[prefix_len..cut].copy_from_slice(completion);
    Reply { status: Status::Ok, written: (cut - prefix_len) as u32 }
}

#[cfg(test)]
mod tests {
    use super::handle_request;
    use alloc::vec;
    use kvetch_proto::Status;

    /// Run a request against a buffer of `cap` bytes holding `prefix`.
    fn serve(prefix: &str, cap: usize, max_tokens: u32, seed: u64) -> (Status, alloc::string::String) {
        let mut buf = vec![0u8; cap];
        buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
        let reply = handle_request(&mut buf, prefix.len(), max_tokens, seed);
        let end = prefix.len() + reply.written as usize;
        (reply.status, alloc::string::String::from_utf8(buf[..end].to_vec()).expect("utf8"))
    }

    #[test]
    fn a_completion_extends_the_prefix_and_leaves_it_viable() {
        let (status, text) = serve("greet(name) {", 4096, 6, 7);
        assert_eq!(status, Status::Ok);
        assert!(text.starts_with("greet(name) {"));
        assert!(text.len() > "greet(name) {".len(), "nothing was added: {text:?}");
        assert!(
            !stitch::oracle::valid_next(&text, text.len()).is_empty(),
            "completion left the buffer dead: {text:?}"
        );
    }

    #[test]
    fn the_same_seed_serves_the_same_bytes() {
        assert_eq!(serve("let x =", 4096, 8, 42), serve("let x =", 4096, 8, 42));
    }

    #[test]
    fn truncation_never_splits_a_token() {
        // The property that matters, and the one a "buffer got shorter" check
        // misses: cutting mid-token can silently yield a *different* token
        // from the one the oracle approved — truncating ` without` to ` with`
        // turns an identifier-shaped cut into a keyword. So assert the served
        // token stream is a genuine prefix of the untruncated one, at every
        // buffer size.
        use stitch::lexer::lex;
        let prefix = "greet(name) {";
        let (_, full) = serve(prefix, 4096, 20, 11);
        let full_tokens: alloc::vec::Vec<_> =
            lex(&full).tokens.into_iter().map(|t| t.kind).collect();

        for extra in 0..(full.len() - prefix.len()) {
            let (status, text) = serve(prefix, prefix.len() + extra, 20, 11);
            assert_eq!(status, Status::Ok);
            assert!(text.len() <= prefix.len() + extra, "overflowed the buffer");
            let tokens: alloc::vec::Vec<_> =
                lex(&text).tokens.into_iter().map(|t| t.kind).collect();
            // Both lexes end in `Eof`; compare the real tokens.
            let served = &tokens[..tokens.len() - 1];
            assert!(
                full_tokens.starts_with(served),
                "at cap +{extra} the served tokens {served:?} are not a prefix of {full_tokens:?}"
            );
            assert!(
                !stitch::oracle::valid_next(&text, text.len()).is_empty(),
                "truncation left the buffer dead: {text:?}"
            );
        }
    }

    #[test]
    fn a_buffer_with_no_room_serves_nothing_rather_than_overflowing() {
        let prefix = "greet(name) {";
        let (status, text) = serve(prefix, prefix.len(), 20, 3);
        assert_eq!(status, Status::Ok);
        assert_eq!(text, prefix, "nothing should have been appended");
    }

    #[test]
    fn a_prefix_longer_than_its_buffer_is_refused() {
        let mut buf = vec![0u8; 8];
        let reply = handle_request(&mut buf, 9, 4, 1);
        assert_eq!(reply.status, Status::Malformed);
        assert_eq!(reply.written, 0);
    }

    #[test]
    fn a_prefix_that_is_not_text_is_refused() {
        // The prefix crosses a process boundary; it is text only if it says so.
        let mut buf = vec![0xffu8; 16];
        let reply = handle_request(&mut buf, 4, 4, 1);
        assert_eq!(reply.status, Status::Malformed);
        assert_eq!(reply.written, 0);
    }
}
