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

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// A token's identity in the frozen vocab.
///
/// `u16` because the ladder's vocab is 2–4K entries: it halves the memory of
/// every encoded corpus and every on-target context buffer, and the ceiling is
/// far enough away to be a non-issue. Widening it is a vocab-version change.
pub type TokenId = u16;

/// Identifies a serialized vocab, distinguishing "not a vocab" from "a vocab I
/// cannot read".
const VOCAB_MAGIC: [u8; 8] = *b"KVETCHVC";

/// Bumped when the format or the merge semantics change, so an older file is
/// refused rather than silently misread.
const VOCAB_VERSION: u32 = 1;

/// Magic, version, merge count.
const VOCAB_HEADER: usize = 8 + 4 + 4;

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

    /// Learn a vocab of `target_size` tokens from `corpus` by repeatedly
    /// merging its most frequent adjacent pair.
    ///
    /// Deterministic: ties go to the lexicographically smallest pair, so the
    /// same corpus always yields the same vocab. That is what lets a vocab be
    /// identified by a hash and reproduced from a seed rather than shipped as a
    /// snapshot.
    ///
    /// **No pre-tokenization.** Mainstream BPE splits on a word regex first, so
    /// merges cannot span whitespace. Code wants the opposite: indentation runs
    /// and `\n` + indent are among the most valuable tokens in a source corpus.
    /// The cost is that a merge may straddle a token boundary, which the
    /// keyword-atomicity test is there to bound.
    ///
    /// Cost is `O(target_size × distinct_chunks)`, **not** `× corpus_len`:
    /// [`pre_tokenize`] splits the corpus into chunks that repeat heavily, so
    /// training counts over each distinct chunk once, weighted by how often it
    /// occurs. A corpus of millions of bytes collapses to thousands of distinct
    /// chunks, which is what makes a full-size vocab minutes rather than hours.
    pub fn train(corpus: &[&str], target_size: usize) -> Self {
        let mut chunks = chunk_frequencies(corpus);
        let mut merges: Vec<(TokenId, TokenId)> = Vec::new();

        while BYTE_TOKENS + merges.len() < target_size {
            let Some(pair) = most_frequent_pair(&chunks) else {
                break;
            };
            let merged_id = (BYTE_TOKENS + merges.len()) as TokenId;

            chunks = chunks
                .into_iter()
                .map(|(ids, count)| (collapse_pair(&ids, pair, merged_id), count))
                .collect();
            merges.push(pair);
        }

        Self::with_merges(&merges)
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

    /// Serialize to the on-disk vocab format: magic, version, merge count, then
    /// the merges as little-endian `u16` pairs.
    ///
    /// **A checkpoint without its vocab is meaningless** — the weights index a
    /// token table, and a different tokenization makes them index the wrong
    /// rows. The merges alone are enough, since the byte tokens are implicit and
    /// [`Vocab::with_merges`] rebuilds the table deterministically from them.
    ///
    /// Merge order is wire law, as it is everywhere else in this crate.
    pub fn encode_vocab(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(VOCAB_HEADER + self.merges.len() * 4);
        out.extend_from_slice(&VOCAB_MAGIC);
        out.extend_from_slice(&VOCAB_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.merges.len() as u32).to_le_bytes());

        for &(left, right) in &self.merges {
            out.extend_from_slice(&left.to_le_bytes());
            out.extend_from_slice(&right.to_le_bytes());
        }

        out
    }

    /// A 64-bit identity for this vocab, over its serialized form.
    ///
    /// **What it is for.** A checkpoint's weights are meaningless without the vocab
    /// they were trained against, and a mispairing is the worst kind of bug: every
    /// array is the right size, every id is in range, and the model emits fluent
    /// nonsense. `cram` stores this in the checkpoint header so the checkpoint
    /// asserts its own provenance, rather than something downstream asserting a
    /// coincidence of sizes.
    ///
    /// Over `encode_vocab()` rather than over `self.merges` directly, so it covers
    /// exactly what a reader will reconstruct — including **merge order**, which is
    /// half of what a vocab *is* and the half a token count silently ignores.
    ///
    /// FNV-1a: a dozen lines, no dependency, `no_std`. This guards against an
    /// accident — a half-updated pair, a copied filename — not against an adversary,
    /// and nothing here should be read as a security property.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        self.encode_vocab().iter().fold(OFFSET, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(PRIME)
        })
    }

    /// Load a serialized vocab, or `None` if it is not one this build can read.
    ///
    /// Every rejection is `None` rather than a panic or a guess, for the same
    /// reason a checkpoint's is: a misread vocab produces a model that runs and
    /// is quietly wrong.
    pub fn decode_vocab(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < VOCAB_HEADER || bytes[..8] != VOCAB_MAGIC {
            return None;
        }

        let word = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        if word(8)? != VOCAB_VERSION {
            return None;
        }
        let declared = word(12)? as usize;

        let payload = bytes.get(VOCAB_HEADER..)?;
        if payload.len() != declared * 4 {
            return None;
        }

        let merges: Vec<(TokenId, TokenId)> = payload
            .chunks_exact(4)
            .map(|entry| {
                (
                    TokenId::from_le_bytes([entry[0], entry[1]]),
                    TokenId::from_le_bytes([entry[2], entry[3]]),
                )
            })
            .collect();

        Some(Self::with_merges(&merges))
    }

    /// Encode text into token ids. Total: any input encodes.
    ///
    /// Each [`pre_tokenize`] chunk is encoded independently and the results
    /// concatenated, so no token ever spans a word boundary — the encoder has
    /// to honour the same split the trainer did, or it would produce tokens the
    /// vocab was never trained to expect.
    ///
    /// Within a chunk, merges are replayed in learning order, each pass
    /// collapsing every adjacent occurrence of that pair. Applying them in
    /// order (rather than repeatedly taking the highest-priority pair present)
    /// is what makes the encoding a pure function of the merge list.
    pub fn encode(&self, text: &str) -> Vec<TokenId> {
        pre_tokenize(text)
            .into_iter()
            .flat_map(|chunk| self.encode_chunk(chunk))
            .collect()
    }

    fn encode_chunk(&self, chunk: &str) -> Vec<TokenId> {
        let initial = chunk.as_bytes().iter().map(|&b| TokenId::from(b)).collect();

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

/// Split text into the chunks a merge may not cross.
///
/// A chunk is a run of non-whitespace with **at most one leading space**, or a
/// run of whitespace. GPT-2's rule, and it earns its keep twice here:
///
/// - *Quality.* Without it, BPE trained on a small corpus spends its tail on
///   whole phrases (`"prod frame < "`), memorizing the corpus instead of
///   learning its lexicon.
/// - *Cost.* Chunks repeat enormously, so training can count over **distinct
///   chunks weighted by frequency** rather than over the corpus. That is the
///   difference between a vocab that trains in seconds and one that takes hours.
///
/// Keeping the single leading space means a lexeme and its separator are one
/// token (`" let"`), which is where the token budget wants them; keeping
/// whitespace runs whole means indentation can become a token, which is what
/// code corpora need and word-splitting alone would forbid.
pub fn pre_tokenize(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        let space_len = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());

        if space_len == rest.len() {
            chunks.push(rest);
            break;
        }

        // Split a run of whitespace so exactly one separator stays with the
        // word: `"\n    let"` becomes `"\n   "` then `" let"`.
        if space_len > 1 {
            let (run, tail) = rest.split_at(space_len - 1);
            chunks.push(run);
            rest = tail;
        }

        let separator = rest
            .chars()
            .next()
            .filter(|c| c.is_whitespace())
            .map_or(0, char::len_utf8);
        let word_end = rest[separator..]
            .find(char::is_whitespace)
            .map_or(rest.len(), |offset| separator + offset);

        chunks.push(&rest[..word_end]);
        rest = &rest[word_end..];
    }

    chunks
}

/// The corpus as distinct byte-level chunks paired with how often each occurs.
///
/// This is the aggregation that makes training tractable: identical chunks —
/// and in code they repeat relentlessly — are counted once and carry their
/// weight, instead of being walked again for every merge.
fn chunk_frequencies(corpus: &[&str]) -> Vec<(Vec<TokenId>, usize)> {
    let byte_level = Vocab::byte_level();
    let mut frequencies: BTreeMap<Vec<TokenId>, usize> = BTreeMap::new();

    for text in corpus {
        for chunk in pre_tokenize(text) {
            *frequencies.entry(byte_level.encode(chunk)).or_insert(0) += 1;
        }
    }

    frequencies.into_iter().collect()
}

/// The most frequent adjacent pair across the weighted `chunks`, or `None` when
/// no chunk has two tokens left to pair.
///
/// Ties resolve to the smallest pair: counts are accumulated in a `BTreeMap`
/// (sorted) and the scan keeps the first strict maximum, so the choice never
/// depends on iteration order.
fn most_frequent_pair(chunks: &[(Vec<TokenId>, usize)]) -> Option<(TokenId, TokenId)> {
    let mut counts: BTreeMap<(TokenId, TokenId), usize> = BTreeMap::new();

    for (ids, weight) in chunks {
        for window in ids.windows(2) {
            *counts.entry((window[0], window[1])).or_insert(0) += weight;
        }
    }

    counts
        .into_iter()
        .fold(None, |best, (pair, count)| match best {
            Some((_, best_count)) if count <= best_count => best,
            _ => Some((pair, count)),
        })
        .map(|(pair, _)| pair)
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

    /// **The check a token count cannot make.** Vocab size is a frozen
    /// hyper-parameter — 2048 across the whole ladder — so "both have N tokens" is
    /// true of every mispairing anyone could actually make. What distinguishes two
    /// vocabs is the merge list and its order, because that is what decides what a
    /// token id *means*. Pair a checkpoint with a same-size stranger and every array
    /// is the right length, every index is in range, and the output is fluent
    /// nonsense with nothing reporting a problem.
    #[test]
    fn two_vocabs_of_the_same_size_but_different_merges_fingerprint_differently() {
        let left = Vocab::with_merges(&[(101, 102), (103, 104)]);
        let right = Vocab::with_merges(&[(105, 106), (107, 108)]);
        assert_eq!(left.len(), right.len(), "the test is only interesting at equal size");
        assert_ne!(left.fingerprint(), right.fingerprint());
    }

    /// Merge *order* is part of a vocab's identity: the same pairs learned in a
    /// different order tokenize differently, so they must not share a fingerprint.
    #[test]
    fn the_same_merges_in_a_different_order_fingerprint_differently() {
        let forward = Vocab::with_merges(&[(101, 102), (103, 104)]);
        let reversed = Vocab::with_merges(&[(103, 104), (101, 102)]);
        assert_eq!(forward.len(), reversed.len());
        assert_ne!(forward.fingerprint(), reversed.fingerprint());
    }

    #[test]
    fn a_vocab_fingerprints_the_same_every_time() {
        // It is stored in a checkpoint and compared on another machine, so it has to
        // be a pure function of the vocab — no addresses, no iteration order.
        let vocab = Vocab::with_merges(&[(65, 66), (67, 68), (256, 67)]);
        assert_eq!(vocab.fingerprint(), Vocab::with_merges(&[(65, 66), (67, 68), (256, 67)]).fingerprint());
    }

    #[test]
    fn the_fingerprint_is_never_zero_because_zero_means_unstamped() {
        // A checkpoint written before fingerprints existed reads back as 0, and the
        // server refuses that. A real vocab must never collide with it.
        assert_ne!(Vocab::byte_level().fingerprint(), 0);
        assert_ne!(Vocab::with_merges(&[(65, 66)]).fingerprint(), 0);
    }

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

    fn encoded_len(vocab: &Vocab, corpus: &[&str]) -> usize {
        corpus.iter().map(|text| vocab.encode(text).len()).sum()
    }

    #[test]
    fn a_vocab_round_trips_through_its_serialized_form() {
        let original = let_vocab();

        let restored =
            Vocab::decode_vocab(&original.encode_vocab()).expect("own output must decode");

        assert_eq!(restored.len(), original.len());
        for text in tricky_texts() {
            assert_eq!(
                restored.encode(text),
                original.encode(text),
                "a restored vocab must tokenize identically; anything else \
                 silently changes what a checkpoint's weights mean"
            );
        }
    }

    #[test]
    fn a_damaged_vocab_is_rejected_rather_than_misread() {
        let encoded = let_vocab().encode_vocab();

        let mut wrong_magic = encoded.clone();
        wrong_magic[0] ^= 0xff;
        assert!(
            Vocab::decode_vocab(&wrong_magic).is_none(),
            "magic not checked"
        );

        assert!(Vocab::decode_vocab(&[]).is_none(), "empty input not rejected");
        assert!(
            Vocab::decode_vocab(&encoded[..encoded.len() - 2]).is_none(),
            "truncation not detected"
        );
    }

    #[test]
    fn training_stops_when_the_corpus_lexicon_is_exhausted() {
        let corpus = ["let x", "let x", "let x"];

        let vocab = Vocab::train(&corpus, BYTE_TOKENS + 1000);

        assert!(
            vocab.len() < BYTE_TOKENS + 1000,
            "a tiny lexicon cannot fill a large vocab"
        );
        assert_eq!(
            vocab.encode("let x").len(),
            2,
            "once every chunk is one token there is nothing left to merge"
        );
    }

    #[test]
    fn no_learned_token_spans_a_word_boundary() {
        let corpus = ["let x = 1", "let y = 2", "let x = 3", "let y = 1"];

        let vocab = Vocab::train(&corpus, BYTE_TOKENS + 32);

        let spanning: Vec<String> = (BYTE_TOKENS..vocab.len())
            .map(|id| vocab.decode(&[id as TokenId]))
            .filter(|bytes| pre_tokenize(&String::from_utf8_lossy(bytes)).len() > 1)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .collect();

        assert!(
            spanning.is_empty(),
            "tokens spanning a word boundary memorize phrases: {spanning:?}"
        );
    }

    #[test]
    fn training_learns_merges_that_compress_the_corpus_losslessly() {
        let corpus = ["let x = 1", "let y = 2", "let z = 3"];
        let target = BYTE_TOKENS + 4;

        let vocab = Vocab::train(&corpus, target);

        assert_eq!(vocab.len(), target, "trainer missed the requested size");
        assert!(
            encoded_len(&vocab, &corpus) < encoded_len(&Vocab::byte_level(), &corpus),
            "training did not compress the corpus it was trained on"
        );
        for text in corpus {
            assert_eq!(
                vocab.decode(&vocab.encode(text)),
                text.as_bytes(),
                "trained vocab lost bytes for {text:?}"
            );
        }
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
