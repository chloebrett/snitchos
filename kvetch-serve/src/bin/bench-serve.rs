//! Where a Tab actually goes: the four-bucket split of one completion request.
//!
//! `cargo run --release -p kvetch-serve --bin bench-serve`
//!
//! The question this exists to answer is
//! [`notes/drivel-on-vf2-speedup-ideas.md`](../../../notes/drivel-on-vf2-speedup-ideas.md)
//! §0: *inside userspace, is the time going to the transformer or to the grammar
//! oracle?* Every optimisation ranked below that section is a guess until it is
//! answered, and it is answerable on the host in seconds because
//! [`kvetch_serve::serve`] is pure `no_std` — no syscalls, no kernel, no emulator.
//!
//! **The absolute numbers do not transfer to the U74; the ratio does.** Both halves
//! are scalar `f32` and pointer-chasing work with no vendor acceleration on either
//! machine, so what a host run buys is the *split*, not a tok/s prediction. Run it
//! `--release`: the board is at opt-0 today, but a debug host build penalises the
//! bounds-checked GEMM far harder than the oracle's allocator traffic, which would
//! tilt the very ratio being measured.
//!
//! # The instrument checks itself before it reports
//!
//! Timing the buckets separately needs the legality predicate to sit inside a closure
//! this file owns, so [`timed_request`] restates [`Server::handle_request`]'s loop
//! rather than calling it. A restatement that has quietly drifted does not error — it
//! *agrees*, and reports a confident split of a path nobody runs. So `main` refuses to
//! print a single number until [`instrument_agrees`] has shown that both loops serve
//! byte-identical completions across every prefix and seed below. If that check ever
//! fails, the numbers are meaningless and the process exits non-zero.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use kvetch_model::Model;
use kvetch_serve::model::ModelLogits;
use kvetch_serve::sample::draw;
use kvetch_serve::serve::{Logits, Server};
use kvetch_vocab::{TokenId, Vocab};

/// What one Tab asks for, from `stitch/src/platform.rs` — 6 tokens into a 256-byte
/// buffer. Benching anything else would measure a request the REPL never sends.
const COMPLETION_TOKENS: u32 = 6;
const COMPLETION_BUF: usize = 256;

/// Prefixes a Tab is actually pressed after, shortest first.
///
/// The spread is the point rather than the realism of any one line: every legality
/// probe re-parses the **whole** prefix, so if the oracle dominates, its share must
/// climb with prefix length — which is §2d's quadratic claim, visible here or not at
/// all. The last one is near the 256-byte ceiling the client imposes.
const PREFIXES: &[(&str, &str)] = &[
    ("short", "greet(name) {"),
    ("assign", "let total = "),
    (
        "in-body",
        "greet(name) {\n    let water = createPerson(name)\n    let total = 0\n    print(",
    ),
    (
        "long",
        "// value every line under the active pricing strategy\nvalueAll(items, strategy) {\n    let total = 0\n    let count = 0\n    let report = makeReport(items, strategy)\n    let summary = describe(report)\n    print(",
    ),
];

/// Seeds to average each prefix over. A single seed measures one refusal pattern, and
/// the refusal count is exactly what the oracle bucket is proportional to.
const SEEDS: &[u64] = &[7, 42, 99, 1234];

fn main() -> ExitCode {
    let Some(pair) = Checkpoint::load() else {
        eprintln!("bench-serve: checkpoints/{CANONICAL}.{{kvetch,vocab}} did not load");
        return ExitCode::FAILURE;
    };

    // A prefix the oracle already calls dead refuses all 17 candidates at step 0 and
    // serves nothing — so its row reports the cost of *seventeen refusals*, not of a
    // long line, while looking exactly like a legitimate measurement. The first
    // draft's `long` prefix was dead and did precisely that.
    if let Some((label, prefix)) = PREFIXES.iter().find(|(_, prefix)| !viable(prefix)) {
        eprintln!("bench-serve: prefix {label:?} is already dead, so its row would measure refusals:");
        eprintln!("  {prefix:?}");
        return ExitCode::FAILURE;
    }

    if let Err(disagreement) = instrument_agrees(&pair) {
        eprintln!("bench-serve: the instrument does not walk handle_request's path.");
        eprintln!("{disagreement}");
        eprintln!("Numbers withheld: a drifted restatement reports a confident split of nothing.");
        return ExitCode::FAILURE;
    }

    report(&pair);
    ExitCode::SUCCESS
}

/// The committed pair, re-decoded on demand.
///
/// Re-decoded rather than shared because a [`ModelLogits`] carries a `Session`, and a
/// warm cache is a different measurement from a cold Tab. Cold is both the common case
/// (the first Tab after typing a fresh line) and the honest one.
struct Checkpoint {
    weights: Vec<u8>,
    vocab_bytes: Vec<u8>,
}

const CANONICAL: &str = kvetch_serve::CANONICAL_CHECKPOINT;

impl Checkpoint {
    fn load() -> Option<Self> {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.join("checkpoints");
        Some(Self {
            weights: std::fs::read(dir.join(format!("{CANONICAL}.kvetch"))).ok()?,
            vocab_bytes: std::fs::read(dir.join(format!("{CANONICAL}.vocab"))).ok()?,
        })
    }

    /// A cold model and its vocab. `expect` is load-bearing and deliberate: the pair is
    /// committed, `load` already read both files, and a decode failure here means the
    /// artifact in git is broken — which must be loud, not benched around.
    fn parts(&self) -> (ModelLogits, Vocab) {
        let model = Model::decode(&self.weights).expect("the committed checkpoint decodes");
        let vocab = Vocab::decode_vocab(&self.vocab_bytes).expect("the committed vocab decodes");
        (ModelLogits::new(model), vocab)
    }

    fn server(&self) -> Server<ModelLogits> {
        let (logits, vocab) = self.parts();
        let fingerprint = logits.vocab_fingerprint();
        Server::new(logits, vocab, fingerprint).expect("the committed pair must agree")
    }
}

/// Where one request's time went. Disjoint by construction: `sample` is the draw with
/// the legality closure's own time subtracted back out, and `rest` is the residual, so
/// the five always sum to `total`.
#[derive(Default, Clone, Copy)]
struct Split {
    /// `Vocab::encode` of the prefix — once per request, not per token (§2e).
    encode: Duration,
    /// `Session::logits_for` on the **first** step. On a cold session that is the
    /// prefill and it processes the whole prefix; on a warm one the cache already
    /// covers everything but the tail, which is exactly why the two are reported
    /// separately — prefill is `O(prefix)` and is what the KV cache exists to charge
    /// only once, while `decode` is the per-token work §3a–3d target.
    prefill: Duration,
    /// `Session::logits_for` on every later step: one position of work per token.
    decode: Duration,
    /// `extends_legally` — the prefix copy and both `valid_next_in` probes (§2a–2c).
    oracle: Duration,
    /// `draw` minus the closure: the softmax and the inverse-CDF walks (§3e).
    sample: Duration,
    total: Duration,
    /// How many legality verdicts this request asked for. `draw` allows at most
    /// `MAX_REFUSALS + 1 = 17` per token; the gap between 1 and 17 is the whole
    /// difference between "the model proposes legally" and "the oracle is the loop".
    verdicts: u32,
    tokens: u32,
}

impl Split {
    fn rest(&self) -> Duration {
        self.total
            .saturating_sub(self.encode)
            .saturating_sub(self.prefill)
            .saturating_sub(self.decode)
            .saturating_sub(self.oracle)
            .saturating_sub(self.sample)
    }
}

/// [`Server::handle_request`]'s loop, restated with a clock in each bucket.
///
/// Every line below has a counterpart in `serve.rs`; the only additions are the
/// `Instant`s and the verdict counter. Keep them in step — [`instrument_agrees`] is
/// what notices when they are not.
fn timed_request(
    logits: &mut ModelLogits,
    vocab: &Vocab,
    prefix: &str,
    max_tokens: u32,
    seed: u64,
) -> (Split, String) {
    let mut split = Split::default();
    let started = Instant::now();

    let mut text = String::from(prefix);
    let encode_started = Instant::now();
    let mut tokens = vocab.encode(prefix);
    split.encode = encode_started.elapsed();
    let mut committed = text.len();

    for step in 0..max_tokens {
        let model_started = Instant::now();
        let step_logits = logits.next(&tokens);
        let forward = model_started.elapsed();
        if step == 0 {
            split.prefill = forward;
        } else {
            split.decode += forward;
        }

        let step_seed = seed ^ (u64::from(step).wrapping_mul(0x9e37_79b9_7f4a_7c15));

        let mut oracle = Duration::ZERO;
        let draw_started = Instant::now();
        let drawn = draw(&step_logits, step_seed, |candidate| {
            let verdict_started = Instant::now();
            let verdict = extends_legally(&text, vocab, candidate);
            oracle += verdict_started.elapsed();
            split.verdicts += 1;
            verdict
        });
        split.sample += draw_started.elapsed().saturating_sub(oracle);
        split.oracle += oracle;

        let Some(token) = drawn else {
            break;
        };

        let bytes = vocab.decode(&[token]);
        let Ok(piece) = std::str::from_utf8(&bytes) else {
            break;
        };
        if committed + piece.len() > COMPLETION_BUF {
            break;
        }
        text.push_str(piece);
        tokens.push(token);
        committed = text.len();
        split.tokens += 1;
    }

    split.total = started.elapsed();
    text.truncate(committed);
    (split, text)
}

/// `serve.rs`'s `extends_legally`, verbatim.
fn extends_legally(text: &str, vocab: &Vocab, candidate: TokenId) -> bool {
    let bytes = vocab.decode(&[candidate]);
    let Ok(piece) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let mut extended = String::from(text);
    extended.push_str(piece);
    viable(&extended)
}

/// `serve.rs`'s `viable`, verbatim.
fn viable(text: &str) -> bool {
    use stitch::oracle::{Entry, valid_next_in};
    !valid_next_in(text, text.len(), Entry::Program)
        .union(valid_next_in(text, text.len(), Entry::Expr))
        .is_empty()
}

/// Does [`timed_request`] serve what [`Server::handle_request`] serves?
///
/// Byte-equality across every prefix and seed the report will use, which is a strong
/// check precisely because the sampler is deterministic in the seed: a restatement that
/// mixed the step seed differently, capped refusals differently, or committed a token
/// the real loop rejects diverges within a token or two and cannot come back.
fn instrument_agrees(pair: &Checkpoint) -> Result<(), String> {
    for (label, prefix) in PREFIXES {
        for &seed in SEEDS {
            let mut buf = vec![0u8; COMPLETION_BUF];
            buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
            let reply = pair.server().handle_request(&mut buf, prefix.len(), COMPLETION_TOKENS, seed);
            let served = String::from_utf8(buf[..prefix.len() + reply.written as usize].to_vec())
                .map_err(|e| format!("handle_request served non-utf8 for {label}/{seed}: {e}"))?;

            let (mut logits, vocab) = pair.parts();
            let (_, restated) = timed_request(&mut logits, &vocab, prefix, COMPLETION_TOKENS, seed);

            if served != restated {
                return Err(format!(
                    "  {label} seed {seed}\n    handle_request: {served:?}\n    timed_request:  {restated:?}"
                ));
            }
        }
    }
    Ok(())
}

fn report(pair: &Checkpoint) {
    println!("drivel `{CANONICAL}`, {COMPLETION_TOKENS} tokens into {COMPLETION_BUF} bytes, cold session.");
    println!("Mean over {} seeds. Buckets are disjoint and sum to total.\n", SEEDS.len());
    println!(
        "{:<9} {:>5} {:>4} {:>5} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12} {:>9}",
        "prefix", "bytes", "tok", "asks", "encode", "prefill", "decode", "oracle", "sample", "rest", "total"
    );

    for (label, prefix) in PREFIXES {
        print_row(label, prefix, &measure(pair, prefix));
    }

    println!(
        "\nSecond Tab on the same line, same `Session`. The on-target server process is\n\
         long-lived, so this — not the cold row above — is what a REPL session mostly pays."
    );
    println!(
        "{:<9} {:>5} {:>4} {:>5} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12} {:>9}",
        "prefix", "bytes", "tok", "asks", "encode", "prefill", "decode", "oracle", "sample", "rest", "total"
    );
    for (label, prefix) in PREFIXES {
        let splits: Vec<Split> = SEEDS
            .iter()
            .map(|&seed| {
                let (mut logits, vocab) = pair.parts();
                let (_, extended) = timed_request(&mut logits, &vocab, prefix, COMPLETION_TOKENS, seed);
                timed_request(&mut logits, &vocab, &extended, COMPLETION_TOKENS, seed).0
            })
            .collect();
        print_row(label, prefix, &splits);
    }

    println!("\nUnit costs, and what each prefix was actually served (seed {}):", SEEDS[0]);
    for (label, prefix) in PREFIXES {
        let (mut logits, vocab) = pair.parts();
        let (split, served) = timed_request(&mut logits, &vocab, prefix, COMPLETION_TOKENS, SEEDS[0]);
        let per = |part: Duration, n: u32| part.as_secs_f64() * 1e6 / f64::from(n.max(1));
        println!(
            "  {label:<9} {:>6.0}us/verdict  {:>7.0}us/decoded-token  {:?}",
            per(split.oracle, split.verdicts),
            per(split.decode, split.tokens.saturating_sub(1)),
            &served[prefix.len()..],
        );
    }
}

/// One table row: every bucket as milliseconds and as a share of the row's total.
fn print_row(label: &str, prefix: &str, splits: &[Split]) {
    let runs = u32::try_from(splits.len()).unwrap_or(1);
    let mean = |pick: fn(&Split) -> Duration| splits.iter().map(pick).sum::<Duration>() / runs;
    let total = mean(|s| s.total);
    let cell = |part: Duration| {
        let share = 100.0 * part.as_secs_f64() / total.as_secs_f64();
        format!("{:>6.1}ms{share:>4.0}%", part.as_secs_f64() * 1e3)
    };

    println!(
        "{label:<9} {:>5} {:>4} {:>5} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12} {:>7.1}ms",
        prefix.len(),
        splits.iter().map(|s| s.tokens).sum::<u32>() / runs,
        splits.iter().map(|s| s.verdicts).sum::<u32>() / runs,
        cell(mean(|s| s.encode)),
        cell(mean(|s| s.prefill)),
        cell(mean(|s| s.decode)),
        cell(mean(|s| s.oracle)),
        cell(mean(|s| s.sample)),
        cell(mean(Split::rest)),
        total.as_secs_f64() * 1e3,
    );
}

/// One [`Split`] per seed, each from a cold session.
fn measure(pair: &Checkpoint, prefix: &str) -> Vec<Split> {
    SEEDS
        .iter()
        .map(|&seed| {
            let (mut logits, vocab) = pair.parts();
            timed_request(&mut logits, &vocab, prefix, COMPLETION_TOKENS, seed).0
        })
        .collect()
}
