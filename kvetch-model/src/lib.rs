//! kvetch-model — the transformer the ladder shares, and the registry of its
//! rungs.
//!
//! **A rung is a config plus a checkpoint, never a crate.** drivel, quip,
//! cliché, ballad and saga differ only in hyperparameters over one frozen vocab
//! and one architecture, so adding a rung is a [`Rung`] variant and a trained
//! checkpoint — the same purely-additive registry shape the kernel uses for
//! runtime workloads. See `docs/generative-ladder.md`.
//!
//! This crate holds the *forward pass*: what the on-target kvetch runner needs
//! and what the host-side `cram` trainer must agree with, byte for byte. It
//! stays `no_std` + alloc with no dependencies.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

/// A rung of the generative ladder.
///
/// babble is rung 0 and is deliberately absent: it has no weights and no
/// config, so it implements the sampling interface without appearing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// ~1M — the scaling curve's bottom anchor and the cheapest CI fixture.
    Drivel,
    /// ~3M — the tracer bullet, and the speculative-decode draft model.
    Quip,
    /// ~10M — keystroke-latency line completion on the VF2.
    Cliche,
    /// ~30M — the product; the VF2's interactive sweet spot.
    Ballad,
    /// ~100M — host, browser and relay tiers only.
    Saga,
}

/// The shape of a rung's transformer.
///
/// No biases anywhere and tied input/output embeddings: both are standard for
/// models this size, and both matter more here than usual because the embedding
/// table is a large fraction of a 1M budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelConfig {
    pub d_model: usize,
    pub layers: usize,
    pub heads: usize,
    /// Inner width of the feed-forward block. Always `4 × d_model` today, kept
    /// explicit because that ratio is a tuning knob, not a law.
    pub ffn: usize,
}

impl ModelConfig {
    /// Total learned parameters at a given vocab size.
    ///
    /// Per layer: `4·d²` of attention projections (q, k, v, o), `2·d·ffn` of
    /// feed-forward, and `2·d` of `RMSNorm` scales. Plus the tied embedding table
    /// and one final norm.
    ///
    /// Vocab is an argument rather than a constant because it genuinely moves
    /// the total at the bottom of the ladder — at drivel the embedding table is
    /// a quarter of the budget, which is the whole reason the vocab-size
    /// decision is load-bearing.
    pub const fn param_count(&self, vocab: usize) -> usize {
        let attention = 4 * self.d_model * self.d_model;
        let feed_forward = 2 * self.d_model * self.ffn;
        let norms = 2 * self.d_model;
        let per_layer = attention + feed_forward + norms;

        vocab * self.d_model + self.layers * per_layer + self.d_model
    }

    /// Width of a single attention head.
    pub const fn head_dim(&self) -> usize {
        self.d_model / self.heads
    }

    /// Parameters in one transformer layer.
    pub const fn layer_params(&self) -> usize {
        4 * self.d_model * self.d_model + 2 * self.d_model * self.ffn + 2 * self.d_model
    }

    /// Where each of a layer's weight blocks starts in the flat vector.
    ///
    /// **The single source of layout truth.** Forward reads through this and so
    /// does backward; a layout replicated in two places would put gradients in
    /// the wrong slots, which trains a model that is subtly not the one being
    /// evaluated.
    pub const fn layer_offsets(&self, vocab: usize, layer: usize) -> LayerOffsets {
        let base = vocab * self.d_model + layer * self.layer_params();
        let square = self.d_model * self.d_model;

        LayerOffsets {
            attention_norm: base,
            wq: base + self.d_model,
            wk: base + self.d_model + square,
            wv: base + self.d_model + 2 * square,
            wo: base + self.d_model + 3 * square,
            ffn_norm: base + self.d_model + 4 * square,
            w1: base + 2 * self.d_model + 4 * square,
            w2: base + 2 * self.d_model + 4 * square + self.d_model * self.ffn,
        }
    }

    /// Where the final norm's scale starts.
    pub const fn final_norm_offset(&self, vocab: usize) -> usize {
        vocab * self.d_model + self.layers * self.layer_params()
    }
}

/// Byte-free offsets into the flat weight vector for one layer.
#[derive(Debug, Clone, Copy)]
pub struct LayerOffsets {
    pub attention_norm: usize,
    pub wq: usize,
    pub wk: usize,
    pub wv: usize,
    pub wo: usize,
    pub ffn_norm: usize,
    pub w1: usize,
    pub w2: usize,
}

impl Rung {
    /// Every rung, bottom to top.
    pub const ALL: [Self; 5] = [
        Self::Drivel,
        Self::Quip,
        Self::Cliche,
        Self::Ballad,
        Self::Saga,
    ];

    /// The size this rung is named for. Nominal — [`ModelConfig::param_count`]
    /// is the truth, and it depends on the vocab.
    pub const fn nominal_params(self) -> usize {
        match self {
            Self::Drivel => 1_000_000,
            Self::Quip => 3_000_000,
            Self::Cliche => 10_000_000,
            Self::Ballad => 30_000_000,
            Self::Saga => 100_000_000,
        }
    }

    /// This rung's shape.
    pub const fn config(self) -> ModelConfig {
        let (d_model, layers, heads) = match self {
            Self::Drivel => (128, 4, 4),
            Self::Quip => (192, 6, 6),
            Self::Cliche => (256, 12, 8),
            Self::Ballad => (384, 16, 12),
            Self::Saga => (576, 22, 12),
        };

        ModelConfig {
            d_model,
            layers,
            heads,
            ffn: 4 * d_model,
        }
    }

    /// The name used in checkpoints and reports.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Drivel => "drivel",
            Self::Quip => "quip",
            Self::Cliche => "cliche",
            Self::Ballad => "ballad",
            Self::Saga => "saga",
        }
    }
}

/// The shape of one matrix multiply: `C = A·B`, all row-major, `C` overwritten.
///
/// `m`, `k` and `n` describe the operands **as multiplied**, not as stored: with
/// `transpose_a`, `a` holds a `k × m` matrix. Transpose flags rather than
/// pre-transposed buffers because the backward pass needs `dY·Wᵀ` and `Xᵀ·dY`,
/// and materializing those transposes would cost more than the multiply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GemmSpec {
    pub m: usize,
    pub k: usize,
    pub n: usize,
    pub transpose_a: bool,
    pub transpose_b: bool,
}

/// The one operation worth making fast.
///
/// Over 95% of both training and prefill FLOPs are matmul, and the backward
/// pass is matmul too, so this single method carries the whole performance
/// story. Everything else in the model — norms, softmax, activations — is
/// memory-bound and stays in plain Rust.
///
/// Implementations must agree within floating-point tolerance; that agreement
/// is what makes the fast paths safe to trust, and it is a test.
pub trait Gemm {
    fn sgemm(&self, spec: GemmSpec, a: &[f32], b: &[f32], c: &mut [f32]);
}

/// The reference multiply: three loops, no blocking, no intrinsics.
///
/// Slow by design. It is `no_std`, dependency-free, the ancestor of the
/// on-target kernels, and the thing every faster backend is checked against.
pub struct NaiveGemm;

impl Gemm for NaiveGemm {
    fn sgemm(&self, spec: GemmSpec, a: &[f32], b: &[f32], c: &mut [f32]) {
        let GemmSpec {
            m,
            k,
            n,
            transpose_a,
            transpose_b,
        } = spec;

        for row in 0..m {
            for column in 0..n {
                c[row * n + column] = (0..k)
                    .map(|inner| {
                        let left = if transpose_a {
                            a[inner * m + row]
                        } else {
                            a[row * k + inner]
                        };
                        let right = if transpose_b {
                            b[column * k + inner]
                        } else {
                            b[inner * n + column]
                        };
                        left * right
                    })
                    .sum();
            }
        }
    }
}

/// Incremental generation: the keys and values of everything already seen.
///
/// **Why this exists.** [`Model::forward`] computes logits for *every* position and
/// keeps every intermediate, because that is what training needs. Generating token
/// *n* that way re-runs the whole prefix, so a completion costs `O(N·T·params)` — for
/// an eight-token completion off a seven-token prompt, eighty-four position-forwards
/// where eight would do. Measured on target before this existed: 4–8 **billion**
/// guest instructions for one completion.
///
/// A session keeps each layer's keys and values, so a new token is one position of
/// work: `O(params)` per token, plus an attention pass linear in the prefix.
///
/// It also holds no `Trace`. Serving allocated every training intermediate and threw
/// them away, on a `talc` heap inside a 16 MiB process.
///
/// **The cache is invisible.** [`logits_for`](Session::logits_for) takes the whole
/// token run and reconciles it against what is cached — extending on a match,
/// rebuilding what diverges. So it is an optimisation, never a protocol: a caller
/// that backtracks (the sampler strikes a refused token and redraws) cannot get a
/// stale answer by forgetting to reset anything.
#[derive(Default)]
pub struct Session {
    /// Per layer, the rotated keys for each cached position, `positions × d_model`.
    keys: Vec<Vec<f32>>,
    /// Per layer, the values for each cached position, `positions × d_model`.
    values: Vec<Vec<f32>>,
    /// The token run these caches were built from.
    tokens: Vec<u16>,
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many positions are currently cached — the work a further token avoids.
    #[must_use]
    pub fn cached_positions(&self) -> usize {
        self.tokens.len()
    }

    /// Logits for the position *after* `tokens`, reusing whatever of `tokens` is
    /// already cached.
    ///
    /// Returns `vocab` values — the last row of what [`Model::forward`] would return
    /// for the same run, bit for bit.
    pub fn logits_for<G: Gemm>(&mut self, model: &Model, tokens: &[u16], gemm: &G) -> Vec<f32> {
        let d_model = model.config.d_model;
        let shared = self
            .tokens
            .iter()
            .zip(tokens)
            .take_while(|(cached, wanted)| cached == wanted)
            .count();

        // Asked for exactly what is cached, the last position must still be *run* to
        // produce logits — the cache holds keys and values, not answers. Rewinding
        // one position and redoing it is the whole cost, and it keeps the common
        // case (extending by a token) free of a special case.
        let replay_from = shared.min(tokens.len().saturating_sub(1));
        self.truncate(replay_from, d_model);

        let mut logits = Vec::new();
        for (position, &token) in tokens.iter().enumerate().skip(replay_from) {
            logits = model.step(self, token, position, gemm);
            self.tokens.push(token);
        }
        logits
    }

    /// Drop everything cached beyond `positions`.
    fn truncate(&mut self, positions: usize, d_model: usize) {
        self.tokens.truncate(positions);
        for layer in self.keys.iter_mut().chain(self.values.iter_mut()) {
            layer.truncate(positions * d_model);
        }
    }
}

/// Everything one layer computed on the way forward that its gradient needs.
pub struct LayerTrace {
    /// The residual stream entering the layer.
    pub input: Vec<f32>,
    pub attention_normed: Vec<f32>,
    pub attention_inverse_rms: Vec<f32>,
    /// Post-rotation, since that is what attention consumed.
    pub queries: Vec<f32>,
    pub keys: Vec<f32>,
    pub values: Vec<f32>,
    pub probabilities: Vec<f32>,
    pub attended: Vec<f32>,
    /// The stream after the attention residual, entering the feed-forward half.
    pub post_attention: Vec<f32>,
    pub ffn_normed: Vec<f32>,
    pub ffn_inverse_rms: Vec<f32>,
    /// Pre-activation, which is what `silu`'s derivative is taken at.
    pub hidden: Vec<f32>,
    pub activated: Vec<f32>,
}

/// A whole forward pass, kept for the backward pass.
pub struct Trace {
    pub layers: Vec<LayerTrace>,
    pub final_input: Vec<f32>,
    pub final_inverse_rms: Vec<f32>,
    pub final_normed: Vec<f32>,
    pub logits: Vec<f32>,
}

/// Identifies a checkpoint file. Its absence is what distinguishes "not a
/// checkpoint" from "a checkpoint I cannot read".
const CHECKPOINT_MAGIC: [u8; 8] = *b"KVETCHCK";

/// Bumped whenever the header or weight layout changes. Old checkpoints are
/// then refused rather than silently misread.
///
/// **Version 2 appends the vocab fingerprint** (two `u32`s, low half first). Version
/// 1 is still *read* — it decodes with a fingerprint of `0`, meaning "unstamped" —
/// because refusing it would have stranded every checkpoint trained before the field
/// existed and forced a retrain to recover numbers that are still valid. Writing is
/// always version 2, so a stamp is one decode-and-re-encode away. New versions may
/// drop old readers; this one had a cheap reason not to.
const CHECKPOINT_VERSION: u32 = 2;

/// The first version this build can read. See [`CHECKPOINT_VERSION`].
const CHECKPOINT_MIN_VERSION: u32 = 1;

/// Magic plus seven `u32` header fields (version 1).
const CHECKPOINT_HEADER: usize = 8 + 7 * 4;

/// Version 2 adds the fingerprint's two `u32` halves.
const CHECKPOINT_HEADER_V2: usize = CHECKPOINT_HEADER + 2 * 4;

/// A checkpoint that predates the vocab fingerprint reads back as this.
///
/// Distinguishable from every real fingerprint because `Vocab::fingerprint` is
/// FNV-1a over a non-empty serialization, which cannot produce zero for any vocab —
/// so "unstamped" is a state a caller can act on rather than a value it must guess at.
pub const UNSTAMPED: u64 = 0;

/// `RMSNorm`'s denominator guard.
const NORM_EPS: f32 = 1e-5;

/// Base for `RoPE`'s rotation frequencies. The usual 10000; positional
/// information is rotary rather than learned so that [`ModelConfig::param_count`]
/// stays a function of width and depth alone.
const ROPE_BASE: f32 = 10000.0;

/// A rung's weights, laid out flat.
///
/// **The order below is checkpoint law** — the same rule as `protocol::Frame`
/// variants. `cram` writes this order and [`Model::forward`] reads it; changing
/// it invalidates every checkpoint:
///
/// 1. token embedding, `vocab × d_model` (tied with the output projection)
/// 2. per layer: attention norm `d`, then `wq`, `wk`, `wv`, `wo` each `d × d`,
///    then feed-forward norm `d`, then `w1` `d × ffn` and `w2` `ffn × d`
/// 3. final norm, `d`
pub struct Model {
    config: ModelConfig,
    vocab: usize,
    weights: Vec<f32>,
    /// Identity of the vocab this model was trained against, or [`UNSTAMPED`].
    /// See [`Model::vocab_fingerprint`].
    vocab_fingerprint: u64,
}

impl Model {
    /// Build a model, rejecting a weight vector that does not match the config.
    ///
    /// A silent mismatch here would produce plausible-looking garbage rather
    /// than an error, which is the worst failure mode a checkpoint loader has.
    pub fn new(config: ModelConfig, vocab: usize, weights: Vec<f32>) -> Option<Self> {
        (weights.len() == config.param_count(vocab)).then_some(Self {
            config,
            vocab,
            weights,
            vocab_fingerprint: UNSTAMPED,
        })
    }

    /// Record which vocab this model was trained against.
    ///
    /// A builder rather than a `new` parameter so the trainer opts in at the one
    /// place that knows the answer, and every existing caller keeps compiling with
    /// the honest default ([`UNSTAMPED`]) rather than being handed a plausible
    /// wrong value to satisfy a signature.
    #[must_use]
    pub fn stamped_with(mut self, vocab_fingerprint: u64) -> Self {
        self.vocab_fingerprint = vocab_fingerprint;
        self
    }

    /// The fingerprint of the vocab this model was trained against, or
    /// [`UNSTAMPED`] for a checkpoint written before the field existed.
    ///
    /// A server should refuse both a mismatch *and* `UNSTAMPED`: unverifiable is not
    /// the same as verified, and the failure it guards against — fluent nonsense from
    /// a same-size stranger vocab — is silent everywhere else.
    #[must_use]
    pub const fn vocab_fingerprint(&self) -> u64 {
        self.vocab_fingerprint
    }

    pub const fn config(&self) -> ModelConfig {
        self.config
    }

    pub const fn vocab(&self) -> usize {
        self.vocab
    }

    /// The flat weight vector, in the layout [`Model`] documents.
    pub fn weights(&self) -> &[f32] {
        &self.weights
    }

    /// Serialize to the checkpoint format.
    ///
    /// **Field order and widths are wire law**, the same rule as
    /// `protocol::Frame`: a checkpoint written today must load on the board
    /// years from now, and the reader has no way to detect a reordering.
    /// Little-endian throughout because both the host and RISC-V are.
    pub fn encode(&self) -> Vec<u8> {
        let header = [
            CHECKPOINT_VERSION,
            self.config.d_model as u32,
            self.config.layers as u32,
            self.config.heads as u32,
            self.config.ffn as u32,
            self.vocab as u32,
            self.weights.len() as u32,
            self.vocab_fingerprint as u32,
            (self.vocab_fingerprint >> 32) as u32,
        ];

        let mut out = Vec::with_capacity(CHECKPOINT_HEADER_V2 + self.weights.len() * 4);
        out.extend_from_slice(&CHECKPOINT_MAGIC);
        for field in header {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for weight in &self.weights {
            out.extend_from_slice(&weight.to_le_bytes());
        }

        out
    }

    /// Load a checkpoint, or `None` if it is not one this build can read.
    ///
    /// Every rejection path — bad magic, unknown version, truncation, a declared
    /// weight count that disagrees with the payload — returns `None` rather than
    /// panicking or guessing. A checkpoint is the one artifact that arrives from
    /// outside the program; misreading one produces a model that runs and is
    /// wrong, which is far worse than refusing to start.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < CHECKPOINT_HEADER || bytes[..8] != CHECKPOINT_MAGIC {
            return None;
        }

        let field = |index: usize| {
            let start = 8 + index * 4;
            Some(u32::from_le_bytes(
                bytes.get(start..start + 4)?.try_into().ok()?,
            ) as usize)
        };

        let version = field(0)? as u32;
        if !(CHECKPOINT_MIN_VERSION..=CHECKPOINT_VERSION).contains(&version) {
            return None;
        }
        let config = ModelConfig {
            d_model: field(1)?,
            layers: field(2)?,
            heads: field(3)?,
            ffn: field(4)?,
        };
        let vocab = field(5)?;
        let declared = field(6)?;

        // Version 1 has no fingerprint and no room for one; it reads as `UNSTAMPED`.
        let (header_len, vocab_fingerprint) = if version >= 2 {
            let low = field(7)? as u64;
            let high = field(8)? as u64;
            (CHECKPOINT_HEADER_V2, low | (high << 32))
        } else {
            (CHECKPOINT_HEADER, UNSTAMPED)
        };

        let payload = bytes.get(header_len..)?;
        if payload.len() != declared * 4 {
            return None;
        }

        let weights = payload
            .chunks_exact(4)
            .map(|word| f32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect();

        // `new` re-derives the expected count from the config, so a header
        // whose dimensions disagree with its own weight count is caught here.
        Self::new(config, vocab, weights).map(|model| model.stamped_with(vocab_fingerprint))
    }

    /// One position of the forward pass, against a session's cached keys and values.
    ///
    /// Deliberately built from the *same* pieces as [`trace_with`](Self::trace_with)
    /// — `rms_norm`, `rope_angle`, `softmax_in_place`, `attention_scale`, and the
    /// caller's `Gemm` — rather than a second implementation of the architecture.
    /// Two implementations of a transformer that must agree bit-for-bit is a bug
    /// waiting for a long prefix, and gradient checking could not catch it: it
    /// validates the backward pass against whichever forward it is handed.
    ///
    /// `position` is where this token sits in the run, which is what the rotation
    /// and the causal mask are made of. It is *not* `session.tokens.len()` read
    /// inside, because the caller replays a position when it rewinds.
    fn step<G: Gemm>(&self, session: &mut Session, token: u16, position: usize, gemm: &G) -> Vec<f32> {
        let ModelConfig { d_model, layers, heads, ffn } = self.config;
        let head_dim = self.config.head_dim();
        let embedding = &self.weights[..self.vocab * d_model];

        if session.keys.len() != layers {
            session.keys = vec![Vec::new(); layers];
            session.values = vec![Vec::new(); layers];
        }

        let row = token as usize * d_model;
        let mut stream: Vec<f32> = embedding[row..row + d_model].to_vec();

        for layer in 0..layers {
            let offsets = self.config.layer_offsets(self.vocab, layer);
            let block = |start: usize, len: usize| &self.weights[start..start + len];
            let square = d_model * d_model;

            let project = |input: &[f32], weight: &[f32], in_dim: usize, out_dim: usize| {
                let mut out = vec![0.0; out_dim];
                gemm.sgemm(
                    GemmSpec { m: 1, k: in_dim, n: out_dim, transpose_a: false, transpose_b: false },
                    input,
                    weight,
                    &mut out,
                );
                out
            };

            let (normed, _) = rms_norm(&stream, block(offsets.attention_norm, d_model), d_model);
            let mut queries = project(&normed, block(offsets.wq, square), d_model, d_model);
            let mut keys = project(&normed, block(offsets.wk, square), d_model, d_model);
            let values = project(&normed, block(offsets.wv, square), d_model, d_model);
            rope_one(&mut queries, head_dim, position);
            rope_one(&mut keys, head_dim, position);

            session.keys[layer].extend_from_slice(&keys);
            session.values[layer].extend_from_slice(&values);

            let attended = attend_last(
                gemm,
                &queries,
                &session.keys[layer],
                &session.values[layer],
                position + 1,
                heads,
                head_dim,
            );
            add_into(&mut stream, &project(&attended, block(offsets.wo, square), d_model, d_model));

            let (ffn_normed, _) = rms_norm(&stream, block(offsets.ffn_norm, d_model), d_model);
            let hidden = project(&ffn_normed, block(offsets.w1, d_model * ffn), d_model, ffn);
            let activated: Vec<f32> = hidden.iter().copied().map(silu).collect();
            add_into(&mut stream, &project(&activated, block(offsets.w2, ffn * d_model), ffn, d_model));
        }

        let final_offset = self.config.final_norm_offset(self.vocab);
        let (final_normed, _) =
            rms_norm(&stream, &self.weights[final_offset..final_offset + d_model], d_model);

        let mut logits = vec![0.0; self.vocab];
        gemm.sgemm(
            GemmSpec { m: 1, k: d_model, n: self.vocab, transpose_a: false, transpose_b: true },
            &final_normed,
            embedding,
            &mut logits,
        );
        logits
    }

    /// Logits for every position, using the reference multiply.
    pub fn forward(&self, tokens: &[u16]) -> Vec<f32> {
        self.forward_with(tokens, &NaiveGemm)
    }

    /// Logits for every position: `tokens.len() × vocab`, row-major.
    ///
    /// All positions rather than just the last because training needs them, and
    /// because per-position output is what makes causality testable.
    ///
    /// Attention's own matmuls stay in plain Rust: they are per-head and
    /// strided, `O(T²·d)` rather than `O(T·d²)`, and at the context lengths this
    /// ladder targets the projections dominate. Revisit if a profile disagrees.
    pub fn forward_with<G: Gemm>(&self, tokens: &[u16], gemm: &G) -> Vec<f32> {
        self.trace_with(tokens, gemm).logits
    }

    /// The forward pass, keeping every intermediate the backward pass needs.
    ///
    /// [`forward_with`](Self::forward_with) is this with the trace discarded, so
    /// there is exactly one forward implementation. A second, training-only
    /// copy would let training and serving drift apart — and gradient checking
    /// could not catch it, because it validates the backward pass against
    /// whichever forward it was given. Both would be consistently wrong.
    pub fn trace_with<G: Gemm>(&self, tokens: &[u16], gemm: &G) -> Trace {
        let ModelConfig {
            d_model,
            layers,
            heads,
            ffn,
        } = self.config;
        let head_dim = self.config.head_dim();
        let positions = tokens.len();

        let embedding = &self.weights[..self.vocab * d_model];
        // Built once for the whole pass: the rotations are the same in every
        // layer and every head.
        let rotations = RotationTable::new(positions, head_dim);

        // Residual stream: one d_model vector per position.
        let mut stream: Vec<f32> = tokens
            .iter()
            .flat_map(|&token| {
                let row = token as usize * d_model;
                embedding[row..row + d_model].iter().copied()
            })
            .collect();
        let mut traces = Vec::with_capacity(layers);

        for layer in 0..layers {
            let offsets = self.config.layer_offsets(self.vocab, layer);
            let block = |start: usize, len: usize| &self.weights[start..start + len];
            let square = d_model * d_model;

            let attention_norm = block(offsets.attention_norm, d_model);
            let wq = block(offsets.wq, square);
            let wk = block(offsets.wk, square);
            let wv = block(offsets.wv, square);
            let wo = block(offsets.wo, square);
            let ffn_norm = block(offsets.ffn_norm, d_model);
            let w1 = block(offsets.w1, d_model * ffn);
            let w2 = block(offsets.w2, ffn * d_model);

            let project = |input: &[f32], weight: &[f32], in_dim: usize, out_dim: usize| {
                let mut out = vec![0.0; positions * out_dim];
                gemm.sgemm(
                    GemmSpec {
                        m: positions,
                        k: in_dim,
                        n: out_dim,
                        transpose_a: false,
                        transpose_b: false,
                    },
                    input,
                    weight,
                    &mut out,
                );
                out
            };

            let input = stream.clone();
            let (attention_normed, attention_inverse_rms) =
                rms_norm(&stream, attention_norm, d_model);
            let queries = rope(
                &project(&attention_normed, wq, d_model, d_model),
                head_dim,
                d_model,
                &rotations,
            );
            let keys = rope(
                &project(&attention_normed, wk, d_model, d_model),
                head_dim,
                d_model,
                &rotations,
            );
            let values = project(&attention_normed, wv, d_model, d_model);

            let (attended, probabilities) =
                attention(gemm, &queries, &keys, &values, positions, heads, head_dim);
            add_into(&mut stream, &project(&attended, wo, d_model, d_model));

            let post_attention = stream.clone();
            let (ffn_normed, ffn_inverse_rms) = rms_norm(&stream, ffn_norm, d_model);
            let hidden = project(&ffn_normed, w1, d_model, ffn);
            let activated: Vec<f32> = hidden.iter().copied().map(silu).collect();
            add_into(&mut stream, &project(&activated, w2, ffn, d_model));

            traces.push(LayerTrace {
                input,
                attention_normed,
                attention_inverse_rms,
                queries,
                keys,
                values,
                probabilities,
                attended,
                post_attention,
                ffn_normed,
                ffn_inverse_rms,
                hidden,
                activated,
            });
        }

        let final_offset = self.config.final_norm_offset(self.vocab);
        let final_norm = &self.weights[final_offset..final_offset + d_model];
        let final_input = stream;
        let (final_normed, final_inverse_rms) = rms_norm(&final_input, final_norm, d_model);

        // Tied output projection: logits are the normed stream against the
        // embedding table, so `embedding` serves as both lookup and unembedding.
        // The table is `vocab × d_model`, hence the transpose.
        let mut logits = vec![0.0; positions * self.vocab];
        gemm.sgemm(
            GemmSpec {
                m: positions,
                k: d_model,
                n: self.vocab,
                transpose_a: false,
                transpose_b: true,
            },
            &final_normed,
            embedding,
            &mut logits,
        );

        Trace {
            layers: traces,
            final_input,
            final_inverse_rms,
            final_normed,
            logits,
        }
    }
}

/// `RMSNorm` over each row, returning the normalized rows **and** each row's
/// `1/rms`.
///
/// The reciprocal is returned because the backward pass needs it and
/// recomputing it there would be a second definition of the same quantity —
/// the forward pass is the only place it should be decided. Forward-only
/// callers discard it; it costs one float per position.
pub fn rms_norm(rows: &[f32], scale: &[f32], d_model: usize) -> (Vec<f32>, Vec<f32>) {
    let inverse_rms: Vec<f32> = rows
        .chunks(d_model)
        .map(|row| {
            let mean_square = row.iter().map(|value| value * value).sum::<f32>() / d_model as f32;
            1.0 / libm::sqrtf(mean_square + NORM_EPS)
        })
        .collect();

    let normalized = rows
        .chunks(d_model)
        .zip(&inverse_rms)
        .flat_map(|(row, &inverse)| {
            row.iter()
                .zip(scale)
                .map(move |(value, gain)| value * inverse * gain)
        })
        .collect();

    (normalized, inverse_rms)
}

/// The feed-forward activation: `x · sigmoid(x)`.
pub fn silu(value: f32) -> f32 {
    value / (1.0 + libm::expf(-value))
}

/// The rotation angle applied to dimension pair `pair` of a head at `position`.
///
/// Shared by the forward rotation and its inverse so the two cannot disagree
/// about the geometry — a backward pass that rotates by a subtly different angle
/// produces gradients that are wrong only for long sequences, which is the
/// hardest kind of wrong to notice.
pub fn rope_angle(position: usize, pair: usize, head_dim: usize) -> f32 {
    libm::powf(ROPE_BASE, -2.0 * pair as f32 / head_dim as f32) * position as f32
}

/// Precomputed `RoPE` rotations for one sequence length.
///
/// The rotation depends on `(position, pair)` and **not on the head**, and the
/// frequency depends on `pair` alone. Computing it inline therefore repeated a
/// `powf` per head per position — measured as the single largest cost in a
/// training step, above every matmul. Building the table once and reading it is
/// ~24× fewer transcendental calls.
pub struct RotationTable {
    pairs: usize,
    sin: Vec<f32>,
    cos: Vec<f32>,
}

impl RotationTable {
    pub fn new(positions: usize, head_dim: usize) -> Self {
        let pairs = head_dim / 2;
        let mut sin = Vec::with_capacity(positions * pairs);
        let mut cos = Vec::with_capacity(positions * pairs);

        // One `powf` per pair, hoisted out of the position loop entirely.
        let frequencies: Vec<f32> = (0..pairs)
            .map(|pair| libm::powf(ROPE_BASE, -2.0 * pair as f32 / head_dim as f32))
            .collect();

        for position in 0..positions {
            for &frequency in &frequencies {
                let angle = frequency * position as f32;
                sin.push(libm::sinf(angle));
                cos.push(libm::cosf(angle));
            }
        }

        Self { pairs, sin, cos }
    }

    pub fn at(&self, position: usize, pair: usize) -> (f32, f32) {
        let index = position * self.pairs + pair;
        (self.sin[index], self.cos[index])
    }
}

/// Rotary position embedding, applied per head over adjacent dimension pairs.
pub fn rope(rows: &[f32], head_dim: usize, d_model: usize, table: &RotationTable) -> Vec<f32> {
    let mut out = rows.to_vec();

    for (position, row) in out.chunks_mut(d_model).enumerate() {
        for head in row.chunks_mut(head_dim) {
            for pair in 0..head_dim / 2 {
                let (sin, cos) = table.at(position, pair);
                let (left, right) = (head[2 * pair], head[2 * pair + 1]);

                head[2 * pair] = left * cos - right * sin;
                head[2 * pair + 1] = left * sin + right * cos;
            }
        }
    }

    out
}

/// [`rope`] for a single row at a known position — the generating counterpart of
/// the table-driven batch rotation.
///
/// No table: a table amortises `powf` across positions, and there is one position
/// here. The angle is [`rope_angle`], which is the expression `RotationTable::new`
/// evaluates, in the same order — so a generated token's rotation is bit-identical
/// to the same token's rotation in a full forward pass, which is what lets the two
/// paths agree exactly.
fn rope_one(row: &mut [f32], head_dim: usize, position: usize) {
    for head in row.chunks_mut(head_dim) {
        for pair in 0..head_dim / 2 {
            let angle = rope_angle(position, pair, head_dim);
            let (sin, cos) = (libm::sinf(angle), libm::cosf(angle));
            let (left, right) = (head[2 * pair], head[2 * pair + 1]);

            head[2 * pair] = left * cos - right * sin;
            head[2 * pair + 1] = left * sin + right * cos;
        }
    }
}

/// Attention for **one** query against `positions` cached keys and values.
///
/// The batch path computes the full `T × T` score matrix and masks the upper
/// triangle to `-inf`; here the only query is the last position, so every cached key
/// is legal and there is nothing to mask. That is not a shortcut around causality —
/// it *is* causality, expressed as "the cache holds exactly the past".
///
/// The arithmetic matches the batch path term for term: the same `Gemm`, the same
/// score scaling, the same `softmax_in_place`. The terms the batch path has and this
/// does not are the masked ones, whose probability is exactly zero.
fn attend_last<G: Gemm>(
    gemm: &G,
    query: &[f32],
    keys: &[f32],
    values: &[f32],
    positions: usize,
    heads: usize,
    head_dim: usize,
) -> Vec<f32> {
    let d_model = heads * head_dim;
    let scale = attention_scale(head_dim);
    let mut out = vec![0.0; d_model];

    for head in 0..heads {
        let key = gather_head(keys, head, positions, heads, head_dim);
        let value = gather_head(values, head, positions, heads, head_dim);

        let mut scores = vec![0.0; positions];
        gemm.sgemm(
            GemmSpec { m: 1, k: head_dim, n: positions, transpose_a: false, transpose_b: true },
            &query[head * head_dim..][..head_dim],
            &key,
            &mut scores,
        );
        for score in &mut scores {
            *score *= scale;
        }
        softmax_in_place(&mut scores);

        let mut attended = vec![0.0; head_dim];
        gemm.sgemm(
            GemmSpec { m: 1, k: positions, n: head_dim, transpose_a: false, transpose_b: false },
            &scores,
            &value,
            &mut attended,
        );
        out[head * head_dim..][..head_dim].copy_from_slice(&attended);
    }

    out
}

/// Causal multi-head attention. A position attends to itself and everything
/// before it, never after — the property `logits_at_a_position_ignore_every_
/// token_after_it` exists to hold.
///
/// Returns the attended values **and** the attention probabilities, laid out
/// `heads × positions × positions` with zeros above the diagonal. The
/// probabilities are what the backward pass differentiates through, and
/// recomputing them there would mean two definitions of the same softmax.
/// Both of attention's inner products are matmuls — `Q·Kᵀ` for the scores and
/// `P·V` for the result — so both go through [`Gemm`] rather than scalar loops.
///
/// A head's data is strided through the residual stream, and BLAS wants
/// contiguous rows, so each head is gathered into a `positions × head_dim`
/// buffer first. That copy is `O(T·d)` against the multiply's `O(T²·d)` — a
/// rounding error next to running the multiply itself in scalar Rust, which
/// measured ~100× slower than the accelerated path.
///
/// The score matrix is computed in full and then masked, rather than computing
/// only the lower triangle. Twice the arithmetic, but arithmetic is exactly what
/// just got cheap; a triangular loop cannot be a GEMM.
pub fn attention<G: Gemm>(
    gemm: &G,
    queries: &[f32],
    keys: &[f32],
    values: &[f32],
    positions: usize,
    heads: usize,
    head_dim: usize,
) -> (Vec<f32>, Vec<f32>) {
    let d_model = heads * head_dim;
    let scale = attention_scale(head_dim);
    let mut out = vec![0.0; positions * d_model];
    let mut probabilities = vec![0.0; heads * positions * positions];

    for head in 0..heads {
        let query = gather_head(queries, head, positions, heads, head_dim);
        let key = gather_head(keys, head, positions, heads, head_dim);
        let value = gather_head(values, head, positions, heads, head_dim);

        let scores = &mut probabilities[head * positions * positions..][..positions * positions];
        gemm.sgemm(
            GemmSpec {
                m: positions,
                k: head_dim,
                n: positions,
                transpose_a: false,
                transpose_b: true,
            },
            &query,
            &key,
            scores,
        );

        for (query_pos, row) in scores.chunks_mut(positions).enumerate() {
            for (key_pos, score) in row.iter_mut().enumerate() {
                // Causality: everything after this position is unreachable, and
                // `-inf` is what makes softmax assign it exactly zero.
                *score = if key_pos > query_pos {
                    f32::NEG_INFINITY
                } else {
                    *score * scale
                };
            }
            softmax_in_place(row);
        }

        let mut attended = vec![0.0; positions * head_dim];
        gemm.sgemm(
            GemmSpec {
                m: positions,
                k: positions,
                n: head_dim,
                transpose_a: false,
                transpose_b: false,
            },
            scores,
            &value,
            &mut attended,
        );
        scatter_head(&mut out, &attended, head, positions, heads, head_dim);
    }

    (out, probabilities)
}

/// Copy one head's columns out of the residual stream into contiguous rows.
pub fn gather_head(
    data: &[f32],
    head: usize,
    positions: usize,
    heads: usize,
    head_dim: usize,
) -> Vec<f32> {
    let d_model = heads * head_dim;
    let offset = head * head_dim;

    (0..positions)
        .flat_map(|position| data[position * d_model + offset..][..head_dim].iter().copied())
        .collect()
}

/// The inverse of [`gather_head`], adding into the destination.
pub fn scatter_head(
    data: &mut [f32],
    head_data: &[f32],
    head: usize,
    positions: usize,
    heads: usize,
    head_dim: usize,
) {
    let d_model = heads * head_dim;
    let offset = head * head_dim;

    for position in 0..positions {
        let target = &mut data[position * d_model + offset..][..head_dim];
        for (slot, value) in target.iter_mut().zip(&head_data[position * head_dim..]) {
            *slot += value;
        }
    }
}

/// Max-subtracted softmax, in place. `-inf` entries become exactly zero.
pub fn softmax_in_place(row: &mut [f32]) {
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut total = 0.0;

    for value in row.iter_mut() {
        *value = libm::expf(*value - max);
        total += *value;
    }
    for value in row.iter_mut() {
        *value /= total;
    }
}

/// The `1/√head_dim` factor on attention scores, shared with the backward pass.
pub fn attention_scale(head_dim: usize) -> f32 {
    1.0 / libm::sqrtf(head_dim as f32)
}

fn add_into(stream: &mut [f32], delta: &[f32]) {
    for (slot, value) in stream.iter_mut().zip(delta) {
        *slot += value;
    }
}

/// Deterministic pseudo-random weights, scaled like a fresh initialization.
///
/// Not a substitute for the trainer's init — this exists so tests and the
/// cross-implementation agreement check can build the *same* model on both
/// sides from a seed, without shipping a checkpoint fixture.
pub fn pseudo_random_weights(count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed | 1;

    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // Map to roughly [-0.05, 0.05]: small enough that a deep stack of
            // residual updates stays finite without any normalization tuning.
            ((state >> 40) as f32 / 16777216.0 - 0.5) * 0.1
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vocab used for sizing. The ladder's target range is 2–4K; the low end is
    /// the honest one to size against, since a bigger vocab only adds params.
    const SIZING_VOCAB: usize = 2048;

    /// `a` is 2×3, `b` is 3×2, so every transpose combination is a distinct
    /// shape — a spec that silently ignores its transpose flags cannot pass all
    /// four.
    const A: [f32; 6] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    const B: [f32; 6] = [7.0, 8.0, 9.0, 10.0, 11.0, 12.0];

    fn product(spec: GemmSpec, a: &[f32], b: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; spec.m * spec.n];
        NaiveGemm.sgemm(spec, a, b, &mut out);
        out
    }

    #[test]
    fn naive_gemm_multiplies_in_every_transpose_combination() {
        let plain = GemmSpec {
            m: 2,
            k: 3,
            n: 2,
            transpose_a: false,
            transpose_b: false,
        };
        assert_eq!(product(plain, &A, &B), [58.0, 64.0, 139.0, 154.0]);

        // b viewed as 2×3, so bᵀ is 3×2 — same shape as b, different contents.
        let b_transposed = GemmSpec {
            transpose_b: true,
            ..plain
        };
        assert_eq!(product(b_transposed, &A, &B), [50.0, 68.0, 122.0, 167.0]);

        // a viewed as 3×2, so aᵀ is 2×3.
        let a_transposed = GemmSpec {
            transpose_a: true,
            ..plain
        };
        assert_eq!(product(a_transposed, &A, &B), [89.0, 98.0, 116.0, 128.0]);

        let both = GemmSpec {
            transpose_a: true,
            transpose_b: true,
            ..plain
        };
        assert_eq!(product(both, &A, &B), [76.0, 103.0, 100.0, 136.0]);
    }

    const TINY: ModelConfig = ModelConfig {
        d_model: 8,
        layers: 2,
        heads: 2,
        ffn: 16,
    };
    const TINY_VOCAB: usize = 12;

    fn tiny_model() -> Model {
        Model::new(TINY, TINY_VOCAB, pseudo_random_weights(TINY.param_count(TINY_VOCAB), 42))
            .expect("weight count matches the config by construction")
    }

    /// **The contract the cache lives or dies by.** Generating with a KV cache must
    /// produce *exactly* what re-running the whole prefix produces — not "close",
    /// exactly. Two reasons it has to be bit-equality and not a tolerance:
    ///
    /// - the completion a checkpoint serves is asserted byte-for-byte against a host
    ///   recomputation, and a one-ULP difference in a logit can cross a sampling
    ///   boundary and change a token;
    /// - a tolerance would hide the exact bug this is most likely to have — an
    ///   off-by-one in the causal mask or the rotation position, which perturbs
    ///   results slightly and plausibly.
    ///
    /// It *can* be exact: `NaiveGemm` sums `0..k` in order from zero, and the terms
    /// the batch path has that the cached one does not are the masked positions,
    /// whose probability is exactly `0.0`.
    #[test]
    fn generating_with_a_cache_is_bit_identical_to_re_running_the_prefix() {
        let model = tiny_model();
        let tokens = [3u16, 1, 4, 1, 5, 9, 2, 6];

        let mut session = Session::new();
        for length in 1..=tokens.len() {
            let cached = session.logits_for(&model, &tokens[..length], &NaiveGemm);

            let full = model.forward(&tokens[..length]);
            let last = &full[full.len() - model.vocab()..];
            assert_eq!(cached.as_slice(), last, "diverged at length {length}");
        }
    }

    /// A session that has cached one run and is asked about a *different* one must
    /// rebuild rather than answer from stale keys. Sampling backtracks (a refused
    /// token is struck and redrawn), so this is a live path, not a defensive one.
    #[test]
    fn a_session_reused_on_a_divergent_prefix_rebuilds_instead_of_lying() {
        let model = tiny_model();
        let mut session = Session::new();

        session.logits_for(&model, &[3, 1, 4, 1], &NaiveGemm);
        let after_divergence = session.logits_for(&model, &[3, 1, 5, 9], &NaiveGemm);

        let fresh = Session::new().logits_for(&model, &[3, 1, 5, 9], &NaiveGemm);
        assert_eq!(after_divergence, fresh);
    }

    /// Shrinking back to a prefix of what is cached is the common backtrack, and it
    /// must not leave the extra positions attending from the cache.
    #[test]
    fn a_session_asked_for_a_shorter_prefix_forgets_the_tail() {
        let model = tiny_model();
        let mut session = Session::new();

        session.logits_for(&model, &[3, 1, 4, 1, 5], &NaiveGemm);
        let shortened = session.logits_for(&model, &[3, 1, 4], &NaiveGemm);

        assert_eq!(shortened, Session::new().logits_for(&model, &[3, 1, 4], &NaiveGemm));
    }

    #[test]
    fn logits_at_a_position_ignore_every_token_after_it() {
        let model = tiny_model();

        let early = model.forward(&[3, 5, 7]);
        let changed_tail = model.forward(&[3, 5, 9]);

        assert_eq!(
            early[..2 * TINY_VOCAB],
            changed_tail[..2 * TINY_VOCAB],
            "a later token changed an earlier position's logits — attention is not causal"
        );
        assert_ne!(
            early[2 * TINY_VOCAB..],
            changed_tail[2 * TINY_VOCAB..],
            "the changed token had no effect at its own position"
        );
    }

    #[test]
    fn forward_yields_one_logit_per_vocab_entry_per_position() {
        let model = tiny_model();

        let logits = model.forward(&[1, 2, 3, 4]);

        assert_eq!(logits.len(), 4 * TINY_VOCAB);
        assert!(
            logits.iter().all(|value| value.is_finite()),
            "non-finite logits mean the numerics are wrong, not the shape"
        );
    }

    #[test]
    fn a_checkpoint_round_trips_a_model_exactly() {
        let original = tiny_model();

        let restored = Model::decode(&original.encode()).expect("own output must decode");

        assert_eq!(restored.config(), original.config());
        assert_eq!(restored.vocab(), original.vocab());
        assert_eq!(
            restored.weights(),
            original.weights(),
            "weights must survive bit-for-bit; a lossy checkpoint silently \
             changes the model between training and serving"
        );
    }

    #[test]
    fn a_checkpoint_carries_the_fingerprint_of_the_vocab_it_was_trained_with() {
        // The whole point: weights and vocab travel together, so a server can refuse
        // a pairing nobody trained rather than serve fluent nonsense from it.
        let stamped = tiny_model().stamped_with(0xfeed_face_dead_beef);

        let restored = Model::decode(&stamped.encode()).expect("own output must decode");

        assert_eq!(restored.vocab_fingerprint(), 0xfeed_face_dead_beef);
    }

    #[test]
    fn a_checkpoint_written_before_fingerprints_still_loads_as_unstamped() {
        // Version 1 is still readable on purpose: refusing it would have stranded
        // every checkpoint trained before the field existed and forced a retrain to
        // recover numbers that are still valid. It reads as `UNSTAMPED`, which a
        // server can refuse on its own terms — unverifiable is not verified.
        let model = tiny_model();
        let mut v1 = model.encode();
        // Rewrite the header as version 1: drop the two fingerprint words and set the
        // version field back.
        v1.splice(CHECKPOINT_HEADER..CHECKPOINT_HEADER_V2, core::iter::empty());
        v1[8..12].copy_from_slice(&1u32.to_le_bytes());

        let restored = Model::decode(&v1).expect("version 1 must still load");

        assert_eq!(restored.vocab_fingerprint(), UNSTAMPED);
        assert_eq!(restored.weights(), model.weights());
    }

    #[test]
    fn a_checkpoint_from_a_future_version_is_refused_rather_than_guessed_at() {
        let mut future = tiny_model().encode();
        future[8..12].copy_from_slice(&(CHECKPOINT_VERSION + 1).to_le_bytes());

        assert!(Model::decode(&future).is_none());
    }

    #[test]
    fn a_damaged_checkpoint_is_rejected_rather_than_misread() {
        let encoded = tiny_model().encode();

        let mut wrong_magic = encoded.clone();
        wrong_magic[0] ^= 0xff;
        assert!(Model::decode(&wrong_magic).is_none(), "magic not checked");

        assert!(
            Model::decode(&encoded[..encoded.len() - 4]).is_none(),
            "truncation not detected"
        );
        assert!(Model::decode(&[]).is_none(), "empty input not rejected");

        let mut wrong_count = encoded.clone();
        // Corrupt the recorded weight count: the header would then describe a
        // model whose weights are not there.
        wrong_count[24] ^= 0x01;
        assert!(
            Model::decode(&wrong_count).is_none(),
            "declared size not cross-checked against the payload"
        );
    }

    #[test]
    fn every_rung_lands_near_the_size_it_is_named_for() {
        for rung in Rung::ALL {
            let params = rung.config().param_count(SIZING_VOCAB);
            let nominal = rung.nominal_params();
            let ratio = params as f64 / nominal as f64;

            assert!(
                (0.7..=1.4).contains(&ratio),
                "{} is {params} params, {ratio:.2}× its nominal {nominal}",
                rung.name()
            );
        }
    }

    #[test]
    fn the_ladder_increases_and_heads_divide_the_model_width() {
        let sizes: Vec<usize> = Rung::ALL
            .iter()
            .map(|rung| rung.config().param_count(SIZING_VOCAB))
            .collect();

        assert!(
            sizes.windows(2).all(|pair| pair[0] < pair[1]),
            "rungs must strictly increase: {sizes:?}"
        );

        for rung in Rung::ALL {
            let config = rung.config();
            assert_eq!(
                config.head_dim() * config.heads,
                config.d_model,
                "{}: heads must divide d_model",
                rung.name()
            );
        }
    }
}
