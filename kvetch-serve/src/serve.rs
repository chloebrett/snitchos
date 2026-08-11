//! Answering a completion request from weights.
//!
//! The same request babble answers, and the same guarantee: what comes back extends
//! the prefix *legally* and stops on the budget, leaving the buffer viable. Only the
//! opinion about which legal continuation to pick is different.

use alloc::string::String;
use alloc::vec::Vec;

use kvetch_proto::{Reply, Status};
use kvetch_vocab::{TokenId, Vocab};

use crate::sample::draw;

/// Where a next-token distribution comes from.
///
/// A trait so the loop can be tested without a checkpoint: the real implementation
/// runs the transformer, the tests hand back fixed logits. That matters more than the
/// usual decoupling argument — the alternative is unit tests that load 4.2 MB and run
/// a forward pass to assert something about *truncation*.
pub trait Logits {
    /// Logits for the position *after* `tokens`, one per vocabulary entry.
    fn next(&mut self, tokens: &[TokenId]) -> Vec<f32>;
}

/// A model paired with the vocab it was trained against.
///
/// Constructing one is where the pairing is checked, so a mismatch is refused before
/// a single request is served rather than producing fluent nonsense for every one.
pub struct Server<L: Logits> {
    logits: L,
    vocab: Vocab,
}

impl<L: Logits> Server<L> {
    /// Pair a distribution source with a vocab, or `None` if the checkpoint was not
    /// trained against this vocab.
    ///
    /// `checkpoint_fingerprint` is the model's own record of its vocab
    /// ([`kvetch_model::Model::vocab_fingerprint`]). An `UNSTAMPED` checkpoint is
    /// refused too: unverifiable is not verified, and the failure this guards against
    /// is silent everywhere else.
    pub fn new(logits: L, vocab: Vocab, checkpoint_fingerprint: u64) -> Option<Self> {
        (checkpoint_fingerprint == vocab.fingerprint()).then_some(Self { logits, vocab })
    }

    /// Complete in place: `buf[..prefix_len]` is the prefix, the completion is
    /// appended after it, and the reply says how many bytes were added.
    ///
    /// The buffer is both input and output because the four-word message has no room
    /// for two — babble's constraint, kept so the two servers are interchangeable
    /// behind one endpoint.
    #[must_use]
    pub fn handle_request(
        &mut self,
        buf: &mut [u8],
        prefix_len: usize,
        max_tokens: u32,
        seed: u64,
    ) -> Reply {
        let Some(prefix_bytes) = buf.get(..prefix_len) else {
            return Reply { status: Status::Malformed, written: 0 };
        };
        // The prefix arrives from another process; it is text only if it says so.
        let Ok(prefix) = core::str::from_utf8(prefix_bytes) else {
            return Reply { status: Status::Malformed, written: 0 };
        };

        // Nothing can fit, so say that rather than spending a forward pass to
        // discover it. Both halves matter: the caller gets `NoRoom` instead of an
        // `Ok` it must guess at, and a REPL whose line has filled up stops paying for
        // a transformer step per Tab.
        if prefix_len == buf.len() {
            return Reply { status: Status::NoRoom, written: 0 };
        }

        let mut text = String::from(prefix);
        let mut tokens = self.vocab.encode(prefix);
        let mut committed = text.len();
        let mut status = Status::Ok;

        for step in 0..max_tokens {
            let logits = self.logits.next(&tokens);
            // A fresh seed per position, mixed from the request's, so the same
            // request replays byte for byte while successive tokens do not all draw
            // from the same point of the stream.
            //
            // (Mutation testing leaves `^` → `|` alive here. It is a real but
            // *quality* difference — OR drives bits toward one as `step` grows, so
            // later positions lose entropy — not a correctness one, and no cheap test
            // separates them. `&` is a genuine bug and is pinned by
            // `the_seed_reaches_the_very_first_token`.)
            let step_seed = seed ^ (u64::from(step).wrapping_mul(0x9e37_79b9_7f4a_7c15));

            let Some(token) = draw(&logits, step_seed, |candidate| {
                extends_legally(&text, &self.vocab, candidate)
            }) else {
                break; // nothing legal here: stop rather than force a token
            };

            let bytes = self.vocab.decode(&[token]);
            let Ok(piece) = core::str::from_utf8(&bytes) else {
                break; // a token that is not text on its own; stop cleanly
            };

            // Only commit a token that fits *whole*. Half a token is not a shorter
            // completion, it is a different one — and possibly a different Stitch
            // token from the one the oracle approved (babble's `truncation` rule,
            // and the reason it is a rule rather than a nicety).
            if committed + piece.len() > buf.len() {
                // Out of room before writing anything: the line is full, which is a
                // different answer from having no opinion. Once something *is*
                // committed the client has a completion to insert, so stopping here
                // is an ordinary success.
                if committed == prefix_len {
                    status = Status::NoRoom;
                }
                break;
            }
            text.push_str(piece);
            tokens.push(token);
            committed = text.len();
        }

        let completion = &text.as_bytes()[prefix_len..committed];
        buf[prefix_len..committed].copy_from_slice(completion);
        Reply { status, written: (committed - prefix_len) as u32 }
    }
}

/// Does appending `candidate`'s bytes leave the buffer *viable* — still extendable to
/// something the parser would accept?
///
/// This is the whole grammar contract, in one predicate. Note it is asked of a
/// **sub-word** piece, which may land mid-Stitch-token (`wh` of `while`): that is
/// fine and is why the question is "is this still extendable" rather than "is this a
/// complete program". A piece that lexes as a partial identifier keeps the buffer
/// alive; one that closes a block that was never opened does not.
fn extends_legally(text: &str, vocab: &Vocab, candidate: TokenId) -> bool {
    let bytes = vocab.decode(&[candidate]);
    let Ok(piece) = core::str::from_utf8(&bytes) else {
        return false;
    };
    let mut extended = String::from(text);
    extended.push_str(piece);
    viable(&extended)
}

/// Both REPL readings, unioned — a line may be a declaration or an expression, and
/// `ModelCompleter` will judge the suggestion the same way.
fn viable(text: &str) -> bool {
    use stitch::oracle::{Entry, valid_next_in};
    !valid_next_in(text, text.len(), Entry::Program)
        .union(valid_next_in(text, text.len(), Entry::Expr))
        .is_empty()
}

#[cfg(test)]
mod tests {
    use super::{Logits, Server, viable};
    use alloc::vec;
    use alloc::vec::Vec;
    use kvetch_proto::Status;
    use kvetch_vocab::{TokenId, Vocab};

    /// A byte-level vocab makes token ids *be* bytes, so a test can say "the model
    /// adores a semicolon" without a trained tokenizer in the way.
    fn byte_vocab() -> Vocab {
        Vocab::byte_level()
    }

    /// Fixed logits, ignoring context: enough to test the loop, and nothing about the
    /// loop depends on the distribution being any good.
    struct Fixed {
        favourite: Option<u8>,
    }

    impl Logits for Fixed {
        fn next(&mut self, _tokens: &[TokenId]) -> Vec<f32> {
            let mut logits = vec![0.0f32; 256];
            if let Some(byte) = self.favourite {
                logits[byte as usize] = 12.0;
            }
            logits
        }
    }

    fn server(favourite: Option<u8>) -> Server<Fixed> {
        let vocab = byte_vocab();
        let fingerprint = vocab.fingerprint();
        Server::new(Fixed { favourite }, vocab, fingerprint).expect("matching pair")
    }

    fn serve(server: &mut Server<Fixed>, prefix: &str, cap: usize, max_tokens: u32, seed: u64) -> (Status, alloc::string::String) {
        let mut buf = vec![0u8; cap];
        buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
        let reply = server.handle_request(&mut buf, prefix.len(), max_tokens, seed);
        let end = prefix.len() + reply.written as usize;
        (reply.status, alloc::string::String::from_utf8(buf[..end].to_vec()).expect("utf8"))
    }

    #[test]
    fn a_completion_extends_the_prefix_and_leaves_it_viable() {
        let (status, text) = serve(&mut server(None), "greet(name) {", 4096, 6, 7);
        assert_eq!(status, Status::Ok);
        assert!(text.starts_with("greet(name) {"));
        assert!(text.len() > "greet(name) {".len(), "nothing was added: {text:?}");
        assert!(viable(&text), "completion left the buffer dead: {text:?}");
    }

    /// **The guarantee weights do not get to weaken.** `;` is lexed but never
    /// grammatical in either reading, so however much the model wants one, it must
    /// not be appended — the mask, not the model, decides what is legal.
    #[test]
    fn a_byte_the_grammar_rejects_is_never_appended_however_much_the_model_wants_it() {
        let (status, text) = serve(&mut server(Some(b';')), "let x = 1", 4096, 8, 3);
        assert_eq!(status, Status::Ok);
        assert!(!text.contains(';'), "an illegal byte reached the buffer: {text:?}");
        assert!(viable(&text));
    }

    #[test]
    fn the_same_seed_serves_the_same_bytes() {
        let mut server = server(None);
        assert_eq!(serve(&mut server, "let x =", 4096, 8, 42), serve(&mut server, "let x =", 4096, 8, 42));
    }

    #[test]
    fn a_completion_never_overflows_its_buffer() {
        // Every cap from "no room at all" upwards: the served text must fit, stay
        // viable, and never be cut mid-token.
        let mut server = server(None);
        let prefix = "greet(name) {";
        for extra in 0..24 {
            let (status, text) = serve(&mut server, prefix, prefix.len() + extra, 12, 11);
            assert!(matches!(status, Status::Ok | Status::NoRoom), "cap +{extra}: {status:?}");
            assert!(text.len() <= prefix.len() + extra, "overflowed at cap +{extra}");
            assert!(viable(&text), "cap +{extra} left the buffer dead: {text:?}");
            // `NoRoom` is a claim about room, so it must never accompany a completion.
            if status == Status::NoRoom {
                assert_eq!(text, prefix, "cap +{extra} appended bytes and still claimed NoRoom");
            }
        }
    }

    #[test]
    fn different_seeds_serve_different_completions() {
        let mut server = server(None);
        let mut served: Vec<alloc::string::String> =
            (0..16).map(|seed| serve(&mut server, "let x =", 4096, 6, seed).1).collect();
        served.sort();
        served.dedup();
        assert!(served.len() > 1, "every seed served the same completion");
    }

    /// **The seed must reach the *first* token, not just later ones.** The per-step
    /// seed is `seed ^ (step × K)`, and at step 0 the mixed-in term is zero — so a
    /// mixer that ate the seed there (`&` instead of `^`) would open every
    /// completion with the same token while still varying from token two onward.
    /// Whole-completion comparison cannot see that; asking for exactly one token can.
    #[test]
    fn the_seed_reaches_the_very_first_token() {
        let mut server = server(None);
        let mut first: Vec<alloc::string::String> =
            (0..16).map(|seed| serve(&mut server, "let x =", 4096, 1, seed).1).collect();
        first.sort();
        first.dedup();
        assert!(first.len() > 1, "every seed opened with the same token");
    }

    /// A token that exactly fills the remaining room is served, not dropped. The
    /// boundary matters because the buffer is sized by the client and a completion
    /// that stops one token short of the space it was given looks like the model
    /// running out of opinions rather than the server running out of room.
    #[test]
    fn a_token_that_exactly_fills_the_buffer_is_still_served() {
        // A space is legal after `let x = 1` and the model is made to adore one, so
        // three tokens are three bytes and the arithmetic is exact.
        let prefix = "let x = 1";
        let (status, text) = serve(&mut server(Some(b' ')), prefix, prefix.len() + 3, 3, 5);
        assert_eq!(status, Status::Ok);
        assert_eq!(text.len(), prefix.len() + 3, "served {text:?}, one token short of the room given");
    }

    #[test]
    fn a_buffer_with_no_room_serves_nothing_rather_than_overflowing() {
        let prefix = "greet(name) {";
        let (status, text) = serve(&mut server(None), prefix, prefix.len(), 20, 3);
        assert_eq!(status, Status::NoRoom);
        assert_eq!(text, prefix, "nothing should have been appended");
    }

    /// **"I had no room" and "I had no opinion" must not look alike.** Both used to
    /// come back as `Ok` with `written: 0`, and the REPL's completer reads that single
    /// shape as "nothing to suggest" — so a line that had simply filled up produced a
    /// token menu, which reads as the grammar refusing the text. Observed on the VF2;
    /// reproduced on the host by `repl-tabs`, where a line reaches the 256-byte request
    /// buffer after eleven Tabs.
    ///
    /// The client cannot re-derive this: it knows its own buffer size, but "does the
    /// next *token* fit" is a fact about a tokenizer it does not link.
    #[test]
    fn a_full_buffer_is_distinguishable_from_having_nothing_to_say() {
        let prefix = "greet(name) {";

        let full = serve(&mut server(None), prefix, prefix.len(), 20, 3);
        // Room to spare, but no budget to use it: nothing written, and nothing to do
        // with room.
        let no_budget = serve(&mut server(None), prefix, prefix.len() + 64, 0, 3);

        assert_eq!(full.0, Status::NoRoom);
        assert_eq!(no_budget.0, Status::Ok);
        assert_eq!(full.1, no_budget.1, "both appended nothing; only the reason differs");
    }

    /// A completion that stops *part way* because the next token would not fit is an
    /// ordinary success — the client has something to insert, and the line is not full
    /// until nothing fits at all.
    #[test]
    fn running_out_of_room_after_writing_something_is_still_ok() {
        let prefix = "let x = 1";
        let (status, text) = serve(&mut server(Some(b' ')), prefix, prefix.len() + 3, 20, 5);
        assert_eq!(status, Status::Ok);
        assert!(text.len() > prefix.len(), "expected a partial completion, got {text:?}");
    }

    #[test]
    fn a_prefix_longer_than_its_buffer_is_refused() {
        let mut buf = vec![0u8; 8];
        let reply = server(None).handle_request(&mut buf, 9, 4, 1);
        assert_eq!(reply.status, Status::Malformed);
        assert_eq!(reply.written, 0);
    }

    #[test]
    fn a_prefix_that_is_not_text_is_refused() {
        let mut buf = vec![0xffu8; 16];
        let reply = server(None).handle_request(&mut buf, 4, 4, 1);
        assert_eq!(reply.status, Status::Malformed);
        assert_eq!(reply.written, 0);
    }

    /// **The mispairing check, at the only moment it can be made cheaply.** A vocab
    /// the checkpoint was not trained against indexes the same-sized tables with
    /// different meanings — every array is the right length and the output is fluent
    /// nonsense. Refuse to construct the server at all.
    #[test]
    fn a_vocab_the_checkpoint_was_not_trained_with_is_refused_rather_than_served() {
        let stranger = Vocab::with_merges(&[(101, 102)]);
        let trained_against = Vocab::with_merges(&[(103, 104)]);
        assert_eq!(stranger.len(), trained_against.len(), "same size, different meaning");

        let paired = Server::new(Fixed { favourite: None }, stranger, trained_against.fingerprint());

        assert!(paired.is_none());
    }

    #[test]
    fn an_unstamped_checkpoint_is_refused_because_unverifiable_is_not_verified() {
        let vocab = byte_vocab();
        assert!(Server::new(Fixed { favourite: None }, vocab, kvetch_model::UNSTAMPED).is_none());
    }
}
