//! cram-corpus — assembly and caching of the corpora `cram` trains on.
//!
//! babble generation is deterministic but **not cheap**: every emitted token
//! re-parses the whole prefix, so a corpus is quadratic in program length and
//! linear in program count. Generating one per training run would dominate the
//! loop, so corpora are generated once and cached on disk.
//!
//! A cached corpus is a *derived artifact* in the sense
//! `docs/generated/` uses: what is pinned is the generator and its seed, not
//! the bytes. The [`Manifest`] beside each corpus records the parameters that
//! produced it plus a cheap fingerprint of the generator's behaviour, so a
//! corpus that predates a babble change is detected rather than silently
//! trained on.

/// Seeds whose babbled output fingerprints the generator.
///
/// Three programs is cheap enough to recompute on every cache check and
/// specific enough that a change to the oracle, the bias tables or the
/// wordlists moves the digest.
const PROBE_SEEDS: [u64; 3] = [0, 1, 2];

/// The corpus file layout this build reads and writes.
///
/// Bumped whenever [`SEPARATOR`] or the file's structure changes. Without it a
/// corpus written by an older layout looks perfectly fresh — same seed, same
/// count, same generator — and parses as one enormous program. Caught the first
/// time the separator changed; a format is exactly the kind of thing a cache
/// key forgets.
pub const FORMAT_VERSION: u32 = 2;

/// What produced a cached corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    /// Layout of the corpus file beside this manifest.
    pub format_version: u32,
    /// Seed of the first program; program *n* uses `seed + n`.
    pub seed: u64,
    /// How many programs the corpus holds.
    pub program_count: usize,
    /// Fingerprint of the generator that produced it.
    pub probe_digest: u64,
}

impl Manifest {
    /// The current generator's fingerprint.
    pub fn probe_digest() -> u64 {
        PROBE_SEEDS.iter().fold(FNV_OFFSET, |digest, &seed| {
            fnv1a(digest, babble::generate(seed).as_bytes())
        })
    }

    /// Whether a corpus described by this manifest can be reused for a request
    /// for `seed` × `program_count`.
    ///
    /// Deliberately not `PartialEq` on the whole manifest: the question is
    /// "does the cache answer this request", and the generator fingerprint is
    /// checked against *now*, not against what was recorded.
    pub fn is_stale_for(&self, seed: u64, program_count: usize) -> bool {
        self.format_version != FORMAT_VERSION
            || self.seed != seed
            || self.program_count != program_count
            || self.probe_digest != Self::probe_digest()
    }

    /// The on-disk form: one `key=value` per line.
    ///
    /// Plain text rather than a serialization format because this file exists
    /// to be read by a human debugging a stale cache, and because the crate
    /// earns nothing from a dependency here.
    pub fn render(&self) -> String {
        format!(
            "format_version={}\nseed={}\nprogram_count={}\nprobe_digest={}\n",
            self.format_version, self.seed, self.program_count, self.probe_digest
        )
    }

    /// Parse the on-disk form. A malformed or truncated manifest is `None` —
    /// which the caller treats as "regenerate", never as a panic.
    pub fn parse(text: &str) -> Option<Self> {
        let field = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        };

        Some(Self {
            format_version: field("format_version")?.parse().ok()?,
            seed: field("seed")?.parse().ok()?,
            program_count: field("program_count")?.parse().ok()?,
            probe_digest: field("probe_digest")?.parse().ok()?,
        })
    }
}

/// Separates programs in a corpus file: its own line, `---` for a human
/// skimming the file, led by an ASCII Record Separator so the split stays
/// unambiguous.
///
/// The `---` alone would be a guess about what a program cannot contain; the
/// control character is what actually guarantees it, since it is outside
/// Stitch's lexical alphabet. Keeping both means the file is readable *and* the
/// parse needs no escaping. The bracketing newlines are part of the separator,
/// not part of the programs, so the roundtrip stays exact.
pub const SEPARATOR: &str = "\n\u{1e}---\n";

/// Generate `program_count` babbled programs, program *n* from `seed + n`.
///
/// Consecutive rather than independently-drawn seeds so a corpus is described
/// by two numbers and any program in it is reproducible on its own.
///
/// Generation is spread across all available cores. This is safe *because* the
/// seed is a pure function of the program's index: workers share nothing, and
/// results are reassembled in index order, so the corpus is byte-identical to
/// the sequential one on any machine regardless of core count. Determinism is
/// pinned by `generation_is_reproducible_from_the_seed`.
pub fn generate(seed: u64, program_count: usize) -> Vec<String> {
    if program_count == 0 {
        return Vec::new();
    }

    let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let batch = program_count.div_ceil(workers.min(program_count));

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..program_count)
            .step_by(batch)
            .map(|start| {
                let end = (start + batch).min(program_count);
                scope.spawn(move || {
                    (start..end)
                        .map(|n| babble::generate(seed + n as u64))
                        .collect::<Vec<_>>()
                })
            })
            .collect();

        handles
            .into_iter()
            .flat_map(|handle| {
                // A panicking worker means the corpus is incomplete; carrying
                // on would silently produce a short corpus that still looks
                // fresh to the manifest.
                handle.join().expect("babble generation panicked")
            })
            .collect()
    })
}

/// Render programs to the on-disk corpus form.
pub fn render_corpus(programs: &[String]) -> String {
    programs.join(SEPARATOR)
}

/// Recover programs from the on-disk corpus form. Empty input is an empty
/// corpus rather than one empty program.
pub fn parse_corpus(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split(SEPARATOR).map(str::to_string).collect()
}

/// How many token classes are legal at each decision a decoder would face while
/// producing `src`: once before every token, and once at the end for the choice
/// to stop.
///
/// This is the quantity that sizes both grammar-derived decode savings — forced
/// tokens (`n == 1`, no forward pass at all) and babble-drafting (small `n`,
/// acceptance bounded below by `1/n`). See
/// [`docs/speculative-decoding-design.md`](../../docs/speculative-decoding-design.md).
///
/// Measured at *token boundaries*, because that is where a decoder chooses. A
/// byte-offset scan would answer a different and less useful question.
pub fn legal_counts(src: &str) -> Vec<usize> {
    let boundaries = stitch::lexer::lex(src)
        .tokens
        .into_iter()
        .map(|token| token.span.start);

    core::iter::once(0)
        .chain(boundaries.skip(1))
        .chain(core::iter::once(src.len()))
        .map(|pos| stitch::oracle::valid_next(src, pos).to_vec().len())
        .collect()
}

/// Bucket `legal_counts` across many programs into a histogram indexed by set
/// size, so `histogram[1]` is how many decisions had exactly one legal class.
pub fn legal_histogram(programs: &[String]) -> Vec<usize> {
    programs
        .iter()
        .flat_map(|program| legal_counts(program))
        .fold(Vec::new(), |mut histogram, n| {
            if histogram.len() <= n {
                histogram.resize(n + 1, 0);
            }
            histogram[n] += 1;
            histogram
        })
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `bytes`, continuing from `digest`.
///
/// Uniqueness category, not security — this detects an accidentally stale
/// cache, not an adversary. See `docs/randomness-and-entropy.md`.
fn fnv1a(digest: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(digest, |acc, &byte| (acc ^ u64::from(byte)).wrapping_mul(FNV_PRIME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvetch_vocab::Vocab;

    fn manifest() -> Manifest {
        Manifest {
            format_version: FORMAT_VERSION,
            seed: 7,
            program_count: 64,
            probe_digest: Manifest::probe_digest(),
        }
    }

    /// Every lexeme the oracle can propose, sourced from the grammar rather
    /// than a hand-kept list so a new Stitch keyword joins this test for free.
    fn representative_lexemes() -> Vec<String> {
        stitch::oracle::all_classes()
            .iter()
            .filter_map(|&class| stitch::oracle::representative(class))
            .map(str::to_string)
            .collect()
    }

    /// How often a lexeme must occur before the vocab is expected to have a
    /// token for it. BPE learns what is frequent, so a lexeme the corpus barely
    /// contains is a *coverage* fact about babble, not a tokenizer defect —
    /// tying the assertion to frequency states the real contract and keeps the
    /// test from drifting into "how big was the corpus that day".
    const ATOMIC_ABOVE: usize = 40;

    #[test]
    fn a_trained_vocab_says_every_frequent_stitch_lexeme_in_one_token() {
        let corpus = generate(0, 200);
        let texts: Vec<&str> = corpus.iter().map(String::as_str).collect();
        let vocab = Vocab::train(&texts, 512);
        let joined = corpus.join(" ");
        // Whole lexemes, not substrings: `"x "` occurs inside `"index "`, which
        // counts a rare lexeme as frequent and then blames the vocab for it.
        let emitted: Vec<&str> = joined.split_whitespace().collect();

        // As the lexeme actually occurs: pre-tokenization keeps one leading
        // space with its word, so a lexeme's natural unit is `" let"`.
        let occurrences =
            |lexeme: &str| emitted.iter().filter(|&&token| token == lexeme).count();

        let frequent: Vec<String> = representative_lexemes()
            .into_iter()
            .filter(|lexeme| occurrences(lexeme) >= ATOMIC_ABOVE)
            .collect();
        let split: Vec<&String> = frequent
            .iter()
            .filter(|lexeme| vocab.encode(&format!(" {lexeme}")).len() > 1)
            .collect();

        assert!(!frequent.is_empty(), "no lexeme cleared the threshold");
        assert!(
            split.is_empty(),
            "frequent but not one token: {split:?} (of {} frequent lexemes)",
            frequent.len()
        );
    }

    #[test]
    fn legal_counts_measure_every_decision_a_decoder_would_face() {
        let src = "let x = 1";
        let token_count = stitch::lexer::lex(src).tokens.len();

        let counts = legal_counts(src);

        assert_eq!(
            counts.len(),
            token_count + 1,
            "one decision per token, plus the choice to stop"
        );
        assert!(
            counts.iter().all(|&n| n >= 1),
            "a legal program cannot reach a position with no legal continuation: {counts:?}"
        );
        assert_eq!(
            counts[0],
            stitch::oracle::valid_next("", 0).to_vec().len(),
            "the first decision is the set of program openers"
        );
    }

    #[test]
    fn a_corpus_round_trips_through_its_on_disk_form() {
        let programs = generate(3, 8);

        let recovered = parse_corpus(&render_corpus(&programs));

        assert_eq!(recovered, programs);
        assert_eq!(programs.len(), 8, "generate ignored the requested count");
    }

    #[test]
    fn generation_is_reproducible_from_the_seed() {
        assert_eq!(generate(11, 4), generate(11, 4));
        assert_ne!(generate(11, 4), generate(12, 4));
    }

    #[test]
    fn manifest_round_trips_through_its_on_disk_form() {
        let rendered = manifest().render();

        let parsed = Manifest::parse(&rendered);

        assert_eq!(parsed, Some(manifest()));
    }

    #[test]
    fn a_corpus_is_stale_when_any_parameter_or_the_generator_changed() {
        let cached = manifest();

        assert!(!cached.is_stale_for(cached.seed, cached.program_count));
        assert!(cached.is_stale_for(cached.seed + 1, cached.program_count));
        assert!(cached.is_stale_for(cached.seed, cached.program_count + 1));

        let after_babble_changed = Manifest {
            probe_digest: cached.probe_digest ^ 1,
            ..cached
        };
        assert!(
            after_babble_changed.is_stale_for(cached.seed, cached.program_count),
            "a corpus generated by a different babble must not be reused"
        );

        let older_layout = Manifest {
            format_version: FORMAT_VERSION - 1,
            ..cached
        };
        assert!(
            older_layout.is_stale_for(cached.seed, cached.program_count),
            "a corpus in an older file layout must not be reused"
        );
    }
}
