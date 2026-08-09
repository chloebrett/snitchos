//! Inside the forward pass: which matmul shape the 90% is actually in.
//!
//! `cargo run --release -p kvetch-serve --bin bench-forward`
//!
//! `bench-serve` answered
//! [`notes/drivel-on-vf2-speedup-ideas.md`](../../../notes/drivel-on-vf2-speedup-ideas.md)
//! §0 at the *request* level: the transformer is 72–97% of a Tab. It cannot say which
//! part of `Model::step` that is, and §3a–3d each bet on a different answer. §3b was
//! the standing demonstration that betting is expensive — the section called it the
//! largest lever in the training sweep and it measured **1.4%** — so this exists
//! before the next one is attempted rather than after.
//!
//! # The seam is the `Gemm` trait, so production code is untouched
//!
//! `Model::step` takes its multiply as a parameter and routes >95% of the forward
//! pass's FLOPs through it, so a decorator that delegates to [`NaiveGemm`] and keeps a
//! stopwatch attributes every matmul by shape without a line changing in
//! `kvetch-model`. What the decorator cannot see — the norms, the rotation, `silu`,
//! `gather_head`'s copies, and the ~116 allocations a step makes — is reported as the
//! residual, which is exactly the quantity §3c and §3d claim is large.
//!
//! **The delegation is the instrument check**: a wrapped run must produce logits
//! bit-identical to an unwrapped one, which both pins that the stopwatch changed no
//! arithmetic and that this bin drives the same path `ModelLogits` does.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use kvetch_model::{Gemm, GemmSpec, Model, ModelConfig, NaiveGemm, Session};
use kvetch_vocab::Vocab;

/// The prefix to profile. Long enough that prefill is the dominant cost — which is the
/// regime `bench-serve` showed a cold Tab is in — and real Stitch rather than filler.
const PREFIX: &str = "// value every line under the active pricing strategy\nvalueAll(items, strategy) {\n    let total = 0\n    let count = 0\n    let report = makeReport(items, strategy)\n    let summary = describe(report)\n    print(";

fn main() -> ExitCode {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap_or_else(|| std::path::Path::new(".")).join("checkpoints");
    let stem = kvetch_serve::CANONICAL_CHECKPOINT;
    let (Ok(weights), Ok(vocab_bytes)) = (
        std::fs::read(dir.join(format!("{stem}.kvetch"))),
        std::fs::read(dir.join(format!("{stem}.vocab"))),
    ) else {
        eprintln!("bench-forward: checkpoints/{stem}.{{kvetch,vocab}} did not load");
        return ExitCode::FAILURE;
    };
    let Some(model) = Model::decode(&weights) else {
        eprintln!("bench-forward: the committed checkpoint did not decode");
        return ExitCode::FAILURE;
    };
    let Some(vocab) = Vocab::decode_vocab(&vocab_bytes) else {
        eprintln!("bench-forward: the committed vocab did not decode");
        return ExitCode::FAILURE;
    };

    let tokens = vocab.encode(PREFIX);
    if let Err(disagreement) = timing_changes_no_arithmetic(&model, &tokens) {
        eprintln!("bench-forward: {disagreement}");
        eprintln!("Numbers withheld: an instrument that perturbs the run measures itself.");
        return ExitCode::FAILURE;
    }

    println!(
        "drivel `{stem}`: d_model {}, layers {}, heads {}, ffn {}, vocab {}",
        model.config().d_model,
        model.config().layers,
        model.config().heads,
        model.config().ffn,
        model.vocab(),
    );
    println!("prefix {} bytes = {} tokens\n", PREFIX.len(), tokens.len());

    // Prefill: a cold session over the whole prefix. Decode: one more token against a
    // session already holding it. `bench-serve` showed a cold Tab is 89% the former
    // and a warm one 59-76% the latter, so both are worth attributing.
    report("prefill (whole prefix, cold session)", &model, &tokens, None);
    report("decode (one token, warm session)", &model, &tokens, Some(&tokens));

    ExitCode::SUCCESS
}

/// Run `tokens` through a timed multiply and print where the forward pass went.
///
/// `warm_with`, when given, is run through an untimed session first, so the timed run
/// measures only the positions the cache does not already hold.
fn report(label: &str, model: &Model, tokens: &[u16], warm_with: Option<&[u16]>) {
    let mut session = Session::new();
    let mut timed: Vec<u16> = tokens.to_vec();
    if let Some(warm) = warm_with {
        session.logits_for(model, warm, &NaiveGemm);
        // One further token, drawn from the vocabulary rather than sampled: this
        // measures a step, not a completion, and any id does the same work.
        timed.push(1);
    }

    let gemm = TimingGemm::new(model.config(), model.vocab());
    let started = Instant::now();
    session.logits_for(model, &timed, &gemm);
    let total = started.elapsed();

    let buckets = gemm.buckets.into_inner();
    let matmul: Duration = buckets.values().map(|bucket| bucket.elapsed).sum();
    let calls: u32 = buckets.values().map(|bucket| bucket.calls).sum();

    println!("{label} — {:.1} ms total", total.as_secs_f64() * 1e3);
    println!("  {:<12} {:>7} {:>10} {:>7}", "bucket", "calls", "ms", "share");
    for (name, bucket) in &buckets {
        print_bucket(name, bucket.calls, bucket.elapsed, total);
    }
    print_bucket("— matmul", calls, matmul, total);
    print_bucket("— other", 0, total.saturating_sub(matmul), total);
    println!();
}

fn print_bucket(name: &str, calls: u32, elapsed: Duration, total: Duration) {
    let share = 100.0 * elapsed.as_secs_f64() / total.as_secs_f64();
    let calls = if calls == 0 { String::from("-") } else { calls.to_string() };
    println!("  {name:<12} {calls:>7} {:>10.2} {share:>6.1}%", elapsed.as_secs_f64() * 1e3);
}

#[derive(Default, Clone, Copy)]
struct Bucket {
    calls: u32,
    elapsed: Duration,
}

/// [`NaiveGemm`] with a stopwatch, bucketed by which multiply in the architecture the
/// shape belongs to.
struct TimingGemm {
    inner: NaiveGemm,
    config: ModelConfig,
    vocab: usize,
    /// `BTreeMap` so the report comes out in a stable order run to run — a profile
    /// whose rows move is one nobody can diff against yesterday's.
    buckets: RefCell<BTreeMap<&'static str, Bucket>>,
}

impl TimingGemm {
    fn new(config: ModelConfig, vocab: usize) -> Self {
        Self { inner: NaiveGemm, config, vocab, buckets: RefCell::new(BTreeMap::new()) }
    }

    /// Which multiply in `Model::step` this shape is.
    ///
    /// Keyed on `(k, n, transpose_b)` because that triple is unique per site at any
    /// one rung — the two attention multiplies are the only ones whose shape moves
    /// with the prefix, and they move in `n` and `k` respectively.
    fn classify(&self, spec: GemmSpec) -> &'static str {
        let ModelConfig { d_model, ffn, .. } = self.config;
        let head_dim = self.config.head_dim();

        match (spec.k, spec.n, spec.transpose_b) {
            (k, n, true) if k == d_model && n == self.vocab => "logits",
            (k, n, false) if k == d_model && n == d_model => "proj q/k/v/o",
            (k, n, false) if k == d_model && n == ffn => "ffn up",
            (k, n, false) if k == ffn && n == d_model => "ffn down",
            (k, _, true) if k == head_dim => "attn scores",
            (_, n, false) if n == head_dim => "attn apply",
            _ => "unclassified",
        }
    }
}

impl Gemm for TimingGemm {
    fn sgemm(&self, spec: GemmSpec, a: &[f32], b: &[f32], c: &mut [f32]) {
        let started = Instant::now();
        self.inner.sgemm(spec, a, b, c);
        let elapsed = started.elapsed();

        let mut buckets = self.buckets.borrow_mut();
        let entry = buckets.entry(self.classify(spec)).or_default();
        entry.calls += 1;
        entry.elapsed += elapsed;
    }
}

/// A wrapped run must produce exactly what an unwrapped one produces.
///
/// Delegation makes that true by construction, which is the point: the assertion is
/// cheap and it fails loudly if the decorator ever grows a shortcut, a reordering, or
/// a different inner backend — any of which would leave a profile of something other
/// than the code being profiled.
fn timing_changes_no_arithmetic(model: &Model, tokens: &[u16]) -> Result<(), String> {
    let plain = Session::new().logits_for(model, tokens, &NaiveGemm);
    let timed = Session::new().logits_for(model, tokens, &TimingGemm::new(model.config(), model.vocab()));

    if plain == timed {
        return Ok(());
    }
    let differing = plain.iter().zip(&timed).filter(|(left, right)| left != right).count();
    Err(format!("the timed multiply changed {differing} of {} logits", plain.len()))
}
