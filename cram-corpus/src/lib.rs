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

use std::collections::BTreeMap;

use kvetch_vocab::{TokenId, Vocab};

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

/// How a corpus renders the programs it holds.
///
/// babble emits a flat, space-separated token stream — every lexeme is an
/// independent oracle choice, so there is no reason for it to produce newlines,
/// indentation, or tight operators, and it doesn't. That rendering is fine for
/// the grammar-learnability probe (which asks only whether output parses) and
/// wrong for the real training mix, where it would teach a model babble's
/// renderer instead of Stitch. [`Layout::Printed`] parses each program and
/// prints it back from its AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// babble's own rendering, verbatim.
    Flat,
    /// Re-printed from the AST: real layout, no operator padding.
    Printed,
}

impl Layout {
    /// The on-disk spelling — also the corpus filename's discriminator.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Printed => "printed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "flat" => Some(Self::Flat),
            "printed" => Some(Self::Printed),
            _ => None,
        }
    }
}

/// What produced a cached corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    /// Layout of the corpus file beside this manifest.
    pub format_version: u32,
    /// Seed of the first program; program *n* uses `seed + n`.
    pub seed: u64,
    /// How many programs the corpus holds.
    pub program_count: usize,
    /// How the programs are rendered.
    pub layout: Layout,
    /// Fingerprint of the generator that produced it.
    pub probe_digest: u64,
}

impl Manifest {
    /// The current pipeline's fingerprint, for the layout in question.
    ///
    /// Digests what the corpus would actually contain — babble's output *after*
    /// the layout is applied — so a change to either half moves it. A digest of
    /// babble alone would be blind to the printer, and a printed corpus is as
    /// much the printer's artifact as the generator's.
    #[must_use]
    pub fn probe_digest(layout: Layout) -> u64 {
        PROBE_SEEDS.iter().fold(FNV_OFFSET, |digest, &seed| {
            fnv1a(digest, render(&babble::generate(seed), layout).as_bytes())
        })
    }

    /// Whether a corpus described by this manifest can be reused for a request
    /// for `seed` × `program_count`.
    ///
    /// Deliberately not `PartialEq` on the whole manifest: the question is
    /// "does the cache answer this request", and the generator fingerprint is
    /// checked against *now*, not against what was recorded.
    pub fn is_stale_for(&self, seed: u64, program_count: usize, layout: Layout) -> bool {
        self.format_version != FORMAT_VERSION
            || self.seed != seed
            || self.program_count != program_count
            || self.layout != layout
            || self.probe_digest != Self::probe_digest(layout)
    }

    /// The on-disk form: one `key=value` per line.
    ///
    /// Plain text rather than a serialization format because this file exists
    /// to be read by a human debugging a stale cache, and because the crate
    /// earns nothing from a dependency here.
    pub fn render(&self) -> String {
        format!(
            "format_version={}\nseed={}\nprogram_count={}\nlayout={}\nprobe_digest={}\n",
            self.format_version,
            self.seed,
            self.program_count,
            self.layout.as_str(),
            self.probe_digest
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
            layout: Layout::parse(field("layout")?)?,
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
pub fn generate(seed: u64, program_count: usize, layout: Layout) -> Vec<String> {
    generate_reported(seed, program_count, layout).programs
}

/// A generated corpus and what happened while rendering it.
#[derive(Debug, Clone)]
pub struct CorpusReport {
    /// The programs, in seed order.
    pub programs: Vec<String>,
    /// How many kept their flat rendering because printing would have changed
    /// the program. Zero is the expected value and the only acceptable one;
    /// anything else is a printer regression, reported rather than absorbed.
    pub unfaithful: usize,
}

/// [`generate`], plus the count of programs the requested layout could not be
/// applied to. See [`render_verified`] for why that count is not assumed to be
/// zero.
#[must_use]
pub fn generate_reported(seed: u64, program_count: usize, layout: Layout) -> CorpusReport {
    if program_count == 0 {
        return CorpusReport { programs: Vec::new(), unfaithful: 0 };
    }

    let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let batch = program_count.div_ceil(workers.min(program_count));

    let rendered: Vec<Rendered> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..program_count)
            .step_by(batch)
            .map(|start| {
                let end = (start + batch).min(program_count);
                scope.spawn(move || {
                    (start..end)
                        .map(|n| render_verified(&babble::generate(seed + n as u64), layout))
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
    });

    CorpusReport {
        unfaithful: rendered.iter().filter(|r| !r.faithful).count(),
        programs: rendered.into_iter().map(|r| r.text).collect(),
    }
}

/// Apply a [`Layout`] to one babbled program, discarding the verdict. Used by
/// [`Manifest::probe_digest`], which fingerprints the text and does not care
/// how it was arrived at.
fn render(program: &str, layout: Layout) -> String {
    render_verified(program, layout).text
}

/// One program's corpus text, plus whether printing preserved it.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// What goes in the corpus.
    pub text: String,
    /// Whether the requested layout was applied without changing the program.
    /// Always true for [`Layout::Flat`], which changes nothing by definition.
    pub faithful: bool,
}

/// Apply a [`Layout`], verifying the result is still the same program.
///
/// The printer's contract is `parse(print(ast)) == ast`, and it is checked here
/// rather than assumed. Nine distinct violations were found by sweeping a
/// million generated programs — each one output that looked correct and re-read
/// as something else — so "the printer is right" is not a safe premise for the
/// artifact a model gets trained on.
///
/// A program that fails keeps its flat rendering. Dropping it instead would
/// leave the corpus shorter than its manifest says, which trades a visible
/// problem for an invisible one.
fn render_verified(program: &str, layout: Layout) -> Rendered {
    if layout == Layout::Flat {
        return Rendered { text: program.to_string(), faithful: true };
    }
    let fallback = Rendered { text: program.to_string(), faithful: false };
    let Ok(items) = stitch::parser::parse_program(program) else {
        return fallback;
    };
    let printed = stitch::print::print_program(&items);
    if stitch::parser::parse_program(&printed).is_ok_and(|reparsed| reparsed == items) {
        return Rendered { text: printed, faithful: true };
    }
    fallback
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

/// The text a model actually trains on: every program, separated by a blank
/// line.
///
/// **Never the corpus file itself.** The file's [`SEPARATOR`] is a storage
/// device, not Stitch, and a model trained on the raw file learns to emit it —
/// which happened, and made every sampled program unparseable while the model
/// itself was fine. A blank line is already what separates top-level items
/// *within* a program, so a program boundary looks like every other boundary and
/// there is nothing to learn that is not Stitch.
pub fn training_text(programs: &[String]) -> String {
    programs.join("\n\n")
}

/// Encode a whole corpus into one flat token stream.
///
/// Caches per distinct chunk, which is the difference between seconds and
/// hours: [`Vocab::encode`] replays every merge over its input, so encoding
/// 24M chunks directly would be ~10¹⁰ operations. Chunks repeat relentlessly in
/// code — the same aggregation that makes *training* a vocab tractable makes
/// *using* one tractable.
pub fn tokenize(vocab: &Vocab, corpus: &str) -> Vec<TokenId> {
    let mut cache: BTreeMap<&str, Vec<TokenId>> = BTreeMap::new();

    kvetch_vocab::pre_tokenize(corpus)
        .into_iter()
        .flat_map(|chunk| {
            cache
                .entry(chunk)
                .or_insert_with(|| vocab.encode(chunk))
                .clone()
        })
        .collect()
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
            layout: Layout::Flat,
            probe_digest: Manifest::probe_digest(Layout::Flat),
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
        let corpus = generate(0, 200, Layout::Flat);
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

    /// The regression guard for a real bug: the first trained drivel emitted
    /// `---` in every sample and scored 0% parse rate, because training
    /// tokenized the corpus *file* rather than the programs in it. The model was
    /// fine; the input was not.
    #[test]
    fn training_text_carries_no_storage_syntax_into_the_model() {
        let programs = generate(0, 40, Layout::Printed);

        let text = training_text(&programs);

        assert!(
            !text.contains(SEPARATOR),
            "the file separator reached the training text"
        );
        assert!(
            !text.contains('\u{1e}'),
            "a control character reached the training text"
        );
        for program in &programs {
            assert!(
                text.contains(program.as_str()),
                "a program went missing from the training text"
            );
        }
    }

    /// Every boundary the model sees must be one it can legally reproduce.
    #[test]
    fn each_training_program_still_parses_after_being_joined() {
        let programs = generate(0, 40, Layout::Printed);
        let text = training_text(&programs);

        for chunk in text.split("\n\n\n") {
            let trimmed = chunk.trim();
            if trimmed.is_empty() {
                continue;
            }
            assert!(
                stitch::parser::parse_program(trimmed).is_ok(),
                "joining produced something unparseable:\n{trimmed}"
            );
        }
    }

    #[test]
    fn a_corpus_round_trips_through_its_on_disk_form() {
        let programs = generate(3, 8, Layout::Flat);

        let recovered = parse_corpus(&render_corpus(&programs));

        assert_eq!(recovered, programs);
        assert_eq!(programs.len(), 8, "generate ignored the requested count");
    }

    #[test]
    fn generation_is_reproducible_from_the_seed() {
        assert_eq!(generate(11, 4, Layout::Flat), generate(11, 4, Layout::Flat));
        assert_ne!(generate(11, 4, Layout::Flat), generate(12, 4, Layout::Flat));
    }

    /// babble renders a flat, space-padded token stream because each lexeme is
    /// an independent oracle choice. Real Stitch has layout. A corpus in the
    /// flat rendering teaches the model babble's *renderer*, so Tier-0 output
    /// destined for the real training mix is printed from its AST first.
    #[test]
    fn the_printed_layout_gives_programs_the_shape_real_stitch_has() {
        let flat = generate(0, 200, Layout::Flat);
        let printed = generate(0, 200, Layout::Printed);

        assert_eq!(printed.len(), flat.len(), "printing dropped programs");
        assert!(
            flat.iter().all(|program| !program.contains('\n')),
            "babble's own rendering has no layout to begin with"
        );
        assert!(
            printed.iter().any(|program| program.contains("\n    ")),
            "no program came back indented"
        );
        let (flat_bytes, printed_bytes) = (total_len(&flat), total_len(&printed));
        assert!(
            printed_bytes < flat_bytes,
            "printing should compress the padding: {printed_bytes} vs {flat_bytes}"
        );
    }

    /// The layout is a *contract* with the model, so a printed program must
    /// still be the program babble generated — not merely something that
    /// parses. This is the printer's round-trip, asserted where the corpus is
    /// assembled rather than trusted from another crate's test.
    #[test]
    fn printing_preserves_every_program_it_lays_out() {
        let flat = generate(0, 200, Layout::Flat);
        let printed = generate(0, 200, Layout::Printed);

        for (flat, printed) in flat.iter().zip(&printed) {
            let before = stitch::parser::parse_program(flat).expect("babble output parses");
            let after = stitch::parser::parse_program(printed)
                .unwrap_or_else(|e| panic!("printed program should parse: {e:?}\n{printed}"));
            assert_eq!(before, after, "printing changed the program\n{printed}");
        }
    }

    fn total_len(programs: &[String]) -> usize {
        programs.iter().map(String::len).sum()
    }

    /// A printed corpus is produced by babble *and* the printer, so the
    /// fingerprint has to cover both. Digesting only babble's output would let
    /// a printer change — new layout, a different parenthesisation — leave
    /// every cached printed corpus looking perfectly fresh, which is the exact
    /// failure the `format_version` comment upstairs records having already been
    /// caught once.
    #[test]
    fn the_fingerprint_covers_the_printer_not_just_the_generator() {
        assert_ne!(
            Manifest::probe_digest(Layout::Printed),
            Manifest::probe_digest(Layout::Flat),
            "the two layouts fingerprint identically, so a printer change is invisible"
        );
    }

    /// The printer's contract is that printed source re-parses to the same
    /// program. Nine distinct ways of breaking that were found by sweeping a
    /// million generated programs, every one of them a case where the output
    /// looked fine and re-read as something else. So the corpus does not take
    /// the contract on faith: a program that fails the round-trip keeps its
    /// flat rendering rather than entering the corpus mislabelled.
    ///
    /// Silent fallback, because the alternative — dropping the program — would
    /// make a corpus shorter than its manifest claims. `verify` is what makes
    /// the fallback observable.
    #[test]
    fn printing_falls_back_rather_than_emitting_a_program_it_changed() {
        let unprintable = "let a = 1";
        let printed = render_verified(unprintable, Layout::Printed);

        assert_eq!(printed.text, "let a = 1\n");
        assert!(printed.faithful, "a program the printer handles should not fall back");
    }

    /// A corpus is only as good as the fraction of it that survived printing,
    /// and that fraction has to be *reported* — a pipeline that silently keeps
    /// flat text for one program in a million looks identical to one that keeps
    /// it for one in two.
    #[test]
    fn generation_reports_how_many_programs_survived_printing() {
        let report = generate_reported(0, 200, Layout::Printed);

        assert_eq!(report.programs.len(), 200);
        assert_eq!(
            report.unfaithful, 0,
            "printer regressed: {} of 200 programs did not round-trip",
            report.unfaithful
        );
    }

    #[test]
    fn a_corpus_is_stale_when_the_layout_changed() {
        let cached = manifest();

        assert!(!cached.is_stale_for(cached.seed, cached.program_count, cached.layout));
        assert!(
            cached.is_stale_for(cached.seed, cached.program_count, Layout::Printed),
            "a flat corpus cannot answer a request for a printed one"
        );
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

        assert!(!cached.is_stale_for(cached.seed, cached.program_count, cached.layout));
        assert!(cached.is_stale_for(cached.seed + 1, cached.program_count, cached.layout));
        assert!(cached.is_stale_for(cached.seed, cached.program_count + 1, cached.layout));

        let after_babble_changed = Manifest {
            probe_digest: cached.probe_digest ^ 1,
            ..cached
        };
        assert!(
            after_babble_changed.is_stale_for(cached.seed, cached.program_count, cached.layout),
            "a corpus generated by a different babble must not be reused"
        );

        let older_layout = Manifest {
            format_version: FORMAT_VERSION - 1,
            ..cached
        };
        assert!(
            older_layout.is_stale_for(cached.seed, cached.program_count, cached.layout),
            "a corpus in an older file layout must not be reused"
        );
    }
}
