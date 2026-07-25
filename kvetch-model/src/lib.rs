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
        })
    }

    pub const fn config(&self) -> ModelConfig {
        self.config
    }

    pub const fn vocab(&self) -> usize {
        self.vocab
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
        let ModelConfig {
            d_model,
            layers,
            heads,
            ffn,
        } = self.config;
        let head_dim = self.config.head_dim();
        let positions = tokens.len();

        let mut cursor = 0;
        let embedding = self.take(&mut cursor, self.vocab * d_model);

        // Residual stream: one d_model vector per position.
        let mut stream: Vec<f32> = tokens
            .iter()
            .flat_map(|&token| {
                let row = token as usize * d_model;
                embedding[row..row + d_model].iter().copied()
            })
            .collect();

        for _ in 0..layers {
            let attention_norm = self.take(&mut cursor, d_model);
            let wq = self.take(&mut cursor, d_model * d_model);
            let wk = self.take(&mut cursor, d_model * d_model);
            let wv = self.take(&mut cursor, d_model * d_model);
            let wo = self.take(&mut cursor, d_model * d_model);
            let ffn_norm = self.take(&mut cursor, d_model);
            let w1 = self.take(&mut cursor, d_model * ffn);
            let w2 = self.take(&mut cursor, ffn * d_model);

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

            let normed = rms_norm_rows(&stream, attention_norm, d_model);
            let queries = rope(&project(&normed, wq, d_model, d_model), head_dim, d_model);
            let keys = rope(&project(&normed, wk, d_model, d_model), head_dim, d_model);
            let values = project(&normed, wv, d_model, d_model);

            let (attended, _) = attention(&queries, &keys, &values, positions, heads, head_dim);
            add_into(&mut stream, &project(&attended, wo, d_model, d_model));

            let normed = rms_norm_rows(&stream, ffn_norm, d_model);
            let hidden = project(&normed, w1, d_model, ffn);
            let activated: Vec<f32> = hidden.iter().copied().map(silu).collect();
            add_into(&mut stream, &project(&activated, w2, ffn, d_model));
        }

        let final_norm = self.take(&mut cursor, d_model);
        let normed = rms_norm_rows(&stream, final_norm, d_model);

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
            &normed,
            embedding,
            &mut logits,
        );
        logits
    }

    fn take(&self, cursor: &mut usize, len: usize) -> &[f32] {
        let slice = &self.weights[*cursor..*cursor + len];
        *cursor += len;
        slice
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

fn rms_norm_rows(rows: &[f32], scale: &[f32], d_model: usize) -> Vec<f32> {
    rms_norm(rows, scale, d_model).0
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

/// Rotary position embedding, applied per head over adjacent dimension pairs.
pub fn rope(rows: &[f32], head_dim: usize, d_model: usize) -> Vec<f32> {
    let mut out = rows.to_vec();

    for (position, row) in out.chunks_mut(d_model).enumerate() {
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
pub fn attention(
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
        let offset = head * head_dim;

        for query_pos in 0..positions {
            let query = &queries[query_pos * d_model + offset..][..head_dim];

            let scores: Vec<f32> = (0..=query_pos)
                .map(|key_pos| {
                    let key = &keys[key_pos * d_model + offset..][..head_dim];
                    query.iter().zip(key).map(|(q, k)| q * k).sum::<f32>() * scale
                })
                .collect();

            for (key_pos, weight) in softmax(&scores).into_iter().enumerate() {
                probabilities[(head * positions + query_pos) * positions + key_pos] = weight;

                let value = &values[key_pos * d_model + offset..][..head_dim];
                let target = &mut out[query_pos * d_model + offset..][..head_dim];
                for (slot, v) in target.iter_mut().zip(value) {
                    *slot += weight * v;
                }
            }
        }
    }

    (out, probabilities)
}

/// The `1/√head_dim` factor on attention scores, shared with the backward pass.
pub fn attention_scale(head_dim: usize) -> f32 {
    1.0 / libm::sqrtf(head_dim as f32)
}

/// Max-subtracted softmax — the subtraction is what keeps `exp` from
/// overflowing on confident attention scores.
fn softmax(scores: &[f32]) -> Vec<f32> {
    let max = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exponentiated: Vec<f32> = scores
        .iter()
        .map(|score| libm::expf(score - max))
        .collect();
    let total: f32 = exponentiated.iter().sum();

    exponentiated.into_iter().map(|value| value / total).collect()
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
