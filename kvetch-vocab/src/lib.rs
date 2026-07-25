//! kvetch-vocab — the frozen tokenizer every rung of the generative ladder
//! shares.
//!
//! Byte-level BPE. The vocab is **wire law**: drivel, quip, cliché, ballad and
//! saga are only comparable — and speculative decoding between them is only
//! sound — because they encode text identically. Changing it invalidates every
//! checkpoint, so it is versioned and hash-pinned like the `protocol::Frame`
//! encoding, never edited casually. See `docs/generative-ladder.md`.
//!
//! Linked by the host-side `cram` pipeline and by the on-target kvetch runner,
//! so it stays `no_std` + alloc with no dependencies.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

/// A token's identity in the frozen vocab.
///
/// `u16` because the ladder's vocab is 2–4K entries: it halves the memory of
/// every encoded corpus and every on-target context buffer, and the ceiling is
/// far enough away to be a non-issue. Widening it is a vocab-version change.
pub type TokenId = u16;

/// The number of base tokens: one per byte value.
///
/// Byte-level is what makes encoding *total* — there is no unknown-character
/// path, so no corpus text can fail to encode. Learned merges take ids from
/// [`BYTE_TOKENS`] upward, which is why byte ids are never renumbered.
pub const BYTE_TOKENS: usize = 256;

/// The frozen tokenizer: a table from [`TokenId`] to the bytes it stands for.
///
/// Representing the vocab as its own decode table (rather than as merge rules
/// replayed at decode time) keeps decoding a lookup-and-concatenate, which is
/// what the on-target runner wants and what makes a merge nothing more than a
/// longer entry.
pub struct Vocab {
    tokens: Vec<Vec<u8>>,
    merges: Vec<(TokenId, TokenId)>,
}

impl Vocab {
    /// The vocab with no learned merges: 256 tokens, one per byte.
    ///
    /// This is the identity tokenizer, and the base every trained vocab
    /// extends.
    pub fn byte_level() -> Self {
        Self::with_merges(&[])
    }

    /// A vocab extending the byte tokens with `merges`, **in learning order**.
    ///
    /// Order is the whole content of a BPE vocab: merge *n* may combine tokens
    /// produced by merges before it, and encoding replays them in the same
    /// sequence. Reordering the list is a different tokenizer, not a
    /// rearrangement of the same one — the same rule that governs
    /// `protocol::Frame` variants.
    ///
    /// A merge naming a token that does not exist yet contributes no bytes,
    /// which makes it dead weight rather than a panic; the trainer cannot emit
    /// one, and the freeze hash is what guards against a hand-edited table.
    pub fn with_merges(merges: &[(TokenId, TokenId)]) -> Self {
        let mut tokens: Vec<Vec<u8>> = (0..BYTE_TOKENS).map(|b| Vec::from([b as u8])).collect();

        for &(left, right) in merges {
            let mut merged = Self::bytes_of(&tokens, left);
            merged.extend(Self::bytes_of(&tokens, right));
            tokens.push(merged);
        }

        Self {
            tokens,
            merges: merges.to_vec(),
        }
    }

    fn bytes_of(tokens: &[Vec<u8>], id: TokenId) -> Vec<u8> {
        tokens.get(id as usize).cloned().unwrap_or_default()
    }

    /// How many tokens this vocab defines.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// A vocab always has its byte tokens, so this is always `false`. Present
    /// because `len` without `is_empty` is a clippy lint and an odd API.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Encode text into token ids. Total: any input encodes.
    ///
    /// Merges are replayed in learning order, each pass collapsing every
    /// adjacent occurrence of that pair. Applying them in order (rather than
    /// repeatedly taking the highest-priority pair present) is what makes the
    /// encoding a pure function of the merge list.
    pub fn encode(&self, text: &str) -> Vec<TokenId> {
        let initial = text.as_bytes().iter().map(|&b| TokenId::from(b)).collect();

        self.merges
            .iter()
            .enumerate()
            .fold(initial, |ids, (index, &pair)| {
                let merged_id = (BYTE_TOKENS + index) as TokenId;
                collapse_pair(&ids, pair, merged_id)
            })
    }

    /// Decode token ids back into bytes.
    ///
    /// Ids outside this vocab contribute nothing — `encode` cannot produce
    /// them, and a decoder that panicked on model output would turn a bad
    /// sample into a dead process.
    pub fn decode(&self, tokens: &[TokenId]) -> Vec<u8> {
        tokens
            .iter()
            .filter_map(|&id| self.tokens.get(id as usize))
            .flatten()
            .copied()
            .collect()
    }
}

/// Replace every non-overlapping adjacent occurrence of `pair` with `merged_id`.
///
/// Left-to-right and non-overlapping: in `aaa` with the pair `(a, a)`, the
/// first two collapse and the third is left alone. Any other choice makes
/// encoding depend on scan direction.
fn collapse_pair(ids: &[TokenId], pair: (TokenId, TokenId), merged_id: TokenId) -> Vec<TokenId> {
    let mut out = Vec::with_capacity(ids.len());
    let mut index = 0;

    while index < ids.len() {
        let matches_pair = ids.get(index..=index + 1) == Some(&[pair.0, pair.1][..]);

        if matches_pair {
            out.push(merged_id);
            index += 2;
        } else {
            out.push(ids[index]);
            index += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Inputs chosen to break a tokenizer that assumes ASCII, assumes
    /// non-empty, or loses whitespace — the three ways a roundtrip silently
    /// corrupts a corpus. The Stitch samples carry the operators most likely to
    /// be mangled by a naive lexeme-aware splitter (`|>`, `..`, `>=`).
    fn tricky_texts() -> Vec<&'static str> {
        Vec::from([
            "",
            "x",
            "ext let label = .. true >= 3 |> not - ( @ , false )",
            "greet(name) {\n\tsay(name)\n}\n",
            "\n\n  \t \n",
            "λ ← 🎵 — ünïcödé",
            "use M.{a, b}",
        ])
    }

    /// `l`+`e` → 256, then that pair +`t` → 257, so `let` is a single token.
    /// Built by hand rather than trained: this pins what merges *mean*
    /// independently of whether the trainer picks good ones.
    fn let_vocab() -> Vocab {
        Vocab::with_merges(&[
            (TokenId::from(b'l'), TokenId::from(b'e')),
            (256, TokenId::from(b't')),
        ])
    }

    #[test]
    fn merges_shorten_the_encoding_without_losing_bytes() {
        let vocab = let_vocab();
        let text = "let x = let y";

        let encoded = vocab.encode(text);

        assert_eq!(vocab.encode("let"), [257], "merged pair is not one token");
        assert!(
            encoded.len() < Vocab::byte_level().encode(text).len(),
            "merges did not shorten the encoding"
        );
        assert_eq!(vocab.decode(&encoded), text.as_bytes(), "merges lost bytes");
    }

    #[test]
    fn every_vocab_roundtrips_arbitrary_text() {
        for vocab in [Vocab::byte_level(), let_vocab()] {
            for text in tricky_texts() {
                let decoded = vocab.decode(&vocab.encode(text));

                assert_eq!(
                    decoded,
                    text.as_bytes(),
                    "roundtrip lost bytes for {text:?} at vocab size {}",
                    vocab.len()
                );
            }
        }
    }
}
