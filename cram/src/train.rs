//! The backward pass, hand-written.
//!
//! Every op here comes in a forward/backward pair, and every backward is
//! validated against a finite-difference estimate of the same quantity. With no
//! framework there is no second implementation to agree with, so correctness
//! comes from the mathematical definition instead — which is a stronger check
//! than agreement, because two implementations can share a misconception and
//! finite differences cannot share one with us.

use kvetch_model::{
    Gemm, GemmSpec, Model, RotationTable, attention_scale, gather_head, scatter_head,
};

use alloc_free_math::softmax_in_place;

mod alloc_free_math {
    /// Softmax over a row, in place, max-subtracted for stability.
    pub fn softmax_in_place(row: &mut [f32]) {
        let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let mut total = 0.0;

        for value in row.iter_mut() {
            *value = (*value - max).exp();
            total += *value;
        }
        for value in row.iter_mut() {
            *value /= total;
        }
    }
}

/// Mean cross-entropy over positions, and its gradient with respect to the
/// logits.
///
/// Returned together because the softmax is computed once and serves both: the
/// gradient of cross-entropy over softmax is `p − onehot(target)`, which is why
/// the two are never separated in practice.
///
/// `logits` is `positions × vocab`, row-major; `targets[t]` is the token that
/// should follow position `t`.
pub fn cross_entropy(logits: &[f32], targets: &[u16], vocab: usize) -> (f32, Vec<f32>) {
    let positions = targets.len();
    let mut gradient = logits.to_vec();
    let mut loss = 0.0;

    for (position, &target) in targets.iter().enumerate() {
        let row = &mut gradient[position * vocab..][..vocab];
        softmax_in_place(row);

        loss -= row[target as usize].max(f32::MIN_POSITIVE).ln();
        row[target as usize] -= 1.0;

        for value in row.iter_mut() {
            *value /= positions as f32;
        }
    }

    (loss / positions as f32, gradient)
}

/// Gradients of `Y = X·W` with respect to both operands.
///
/// Both are matmuls, which is why [`Gemm`] carries the whole performance story
/// of training and not just of inference: `dX = dY·Wᵀ` and `dW = Xᵀ·dY`.
pub fn matmul_backward<G: Gemm>(
    gemm: &G,
    inputs: &[f32],
    weights: &[f32],
    d_outputs: &[f32],
    shape: (usize, usize, usize),
) -> (Vec<f32>, Vec<f32>) {
    let (m, k, n) = shape;

    let mut d_inputs = vec![0.0; m * k];
    gemm.sgemm(
        GemmSpec {
            m,
            k: n,
            n: k,
            transpose_a: false,
            transpose_b: true,
        },
        d_outputs,
        weights,
        &mut d_inputs,
    );

    let mut d_weights = vec![0.0; k * n];
    gemm.sgemm(
        GemmSpec {
            m: k,
            k: m,
            n,
            transpose_a: true,
            transpose_b: false,
        },
        inputs,
        d_outputs,
        &mut d_weights,
    );

    (d_inputs, d_weights)
}

/// Gradients of [`kvetch_model::rms_norm`] with respect to its input and scale.
///
/// The second term is the one hand-derivations drop: normalizing couples every
/// output in a row to every input in it, through `1/rms`. Without it the
/// gradient is plausible, stable, and wrong — which is exactly the failure the
/// finite-difference check exists to catch.
pub fn rms_norm_backward(
    rows: &[f32],
    scale: &[f32],
    inverse_rms: &[f32],
    d_output: &[f32],
    d_model: usize,
) -> (Vec<f32>, Vec<f32>) {
    let mut d_rows = vec![0.0; rows.len()];
    let mut d_scale = vec![0.0; d_model];

    for (position, row) in rows.chunks(d_model).enumerate() {
        let inverse = inverse_rms[position];
        let d_row = &d_output[position * d_model..][..d_model];
        let target = &mut d_rows[position * d_model..][..d_model];

        let coupling: f32 = row
            .iter()
            .zip(d_row)
            .zip(scale)
            .map(|((value, gradient), gain)| gradient * gain * value)
            .sum();

        for index in 0..d_model {
            target[index] = scale[index] * inverse * d_row[index]
                - inverse * inverse * inverse * row[index] * coupling / d_model as f32;
            d_scale[index] += d_row[index] * row[index] * inverse;
        }
    }

    (d_rows, d_scale)
}

/// Gradient of [`kvetch_model::rope`]: the inverse rotation.
///
/// A rotation is orthogonal, so its transpose is its inverse — backward rotates
/// by `−θ` and needs no saved activations at all. The angles come from the same
/// [`RotationTable`] the forward pass used, so the two cannot drift apart on the
/// geometry.
pub fn rope_backward(
    d_output: &[f32],
    head_dim: usize,
    d_model: usize,
    table: &RotationTable,
) -> Vec<f32> {
    let mut d_input = d_output.to_vec();

    for (position, row) in d_input.chunks_mut(d_model).enumerate() {
        for head in row.chunks_mut(head_dim) {
            for pair in 0..head_dim / 2 {
                let (sin, cos) = table.at(position, pair);
                let (left, right) = (head[2 * pair], head[2 * pair + 1]);

                head[2 * pair] = left * cos + right * sin;
                head[2 * pair + 1] = -left * sin + right * cos;
            }
        }
    }

    d_input
}

/// What [`kvetch_model::attention`] computed on the way forward, kept for the
/// backward pass.
pub struct AttentionForward<'a> {
    pub queries: &'a [f32],
    pub keys: &'a [f32],
    pub values: &'a [f32],
    pub probabilities: &'a [f32],
}

/// The dimensions one attention block runs at.
#[derive(Debug, Clone, Copy)]
pub struct AttentionShape {
    pub positions: usize,
    pub heads: usize,
    pub head_dim: usize,
}

/// Gradients of [`kvetch_model::attention`] with respect to queries, keys and
/// values.
///
/// The softmax term is the subtle one: because the probabilities in a row sum to
/// one, raising any score lowers all the others, so each score's gradient
/// carries `−Σ p·dp` from its own row. Dropping it gives gradients that still
/// train, slowly and to the wrong place.
///
/// Causality needs no separate handling: the forward pass masked those scores to
/// `-inf`, so their probabilities are exactly zero, and every gradient here is
/// multiplied by one.
pub fn attention_backward<G: Gemm>(
    gemm: &G,
    saved: &AttentionForward<'_>,
    d_output: &[f32],
    shape: AttentionShape,
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let AttentionShape {
        positions,
        heads,
        head_dim,
    } = shape;
    let scale = attention_scale(head_dim);
    let d_model = heads * head_dim;

    let mut d_queries = vec![0.0; positions * d_model];
    let mut d_keys = vec![0.0; positions * d_model];
    let mut d_values = vec![0.0; positions * d_model];

    let gather = |data: &[f32], head: usize| gather_head(data, head, positions, heads, head_dim);
    let multiply = |a: &[f32], b: &[f32], out: &mut [f32], m, k, n, ta, tb| {
        gemm.sgemm(
            GemmSpec {
                m,
                k,
                n,
                transpose_a: ta,
                transpose_b: tb,
            },
            a,
            b,
            out,
        );
    };

    for head in 0..heads {
        let probabilities =
            &saved.probabilities[head * positions * positions..][..positions * positions];
        let d_out = gather(d_output, head);
        let query = gather(saved.queries, head);
        let key = gather(saved.keys, head);
        let value = gather(saved.values, head);

        // dV = Pᵀ·dOut
        let mut d_value = vec![0.0; positions * head_dim];
        multiply(
            probabilities,
            &d_out,
            &mut d_value,
            positions,
            positions,
            head_dim,
            true,
            false,
        );

        // dP = dOut·Vᵀ, then softmax's row coupling turns it into dScores.
        let mut d_scores = vec![0.0; positions * positions];
        multiply(
            &d_out,
            &value,
            &mut d_scores,
            positions,
            head_dim,
            positions,
            false,
            true,
        );
        for (query_pos, row) in d_scores.chunks_mut(positions).enumerate() {
            let weights = &probabilities[query_pos * positions..][..positions];
            let coupling: f32 = weights.iter().zip(row.iter()).map(|(p, d)| p * d).sum();
            for (slot, weight) in row.iter_mut().zip(weights) {
                *slot = weight * (*slot - coupling) * scale;
            }
        }

        // dQ = dScores·K and dK = dScoresᵀ·Q. Masked entries carry zero
        // probability, so causality needs no separate handling here.
        let mut d_query = vec![0.0; positions * head_dim];
        multiply(
            &d_scores,
            &key,
            &mut d_query,
            positions,
            positions,
            head_dim,
            false,
            false,
        );
        let mut d_key = vec![0.0; positions * head_dim];
        multiply(
            &d_scores,
            &query,
            &mut d_key,
            positions,
            positions,
            head_dim,
            true,
            false,
        );

        scatter_head(&mut d_queries, &d_query, head, positions, heads, head_dim);
        scatter_head(&mut d_keys, &d_key, head, positions, heads, head_dim);
        scatter_head(&mut d_values, &d_value, head, positions, heads, head_dim);
    }

    (d_queries, d_keys, d_values)
}


/// Derivative of [`kvetch_model::silu`] at `value`: `σ + x·σ·(1 − σ)`.
pub fn silu_backward(value: f32) -> f32 {
    let sigmoid = 1.0 / (1.0 + (-value).exp());
    sigmoid + value * sigmoid * (1.0 - sigmoid)
}

/// Mean cross-entropy of `targets` given `tokens`, and its gradient with
/// respect to every weight — same layout as [`Model::weights`].
///
/// Walks the [`Trace`] backwards, composing the per-op gradients above. The
/// residual stream is why each half adds two contributions: a residual connection
/// forks the gradient, so the stream's gradient is what flowed from above *plus*
/// what came back through the block.
pub fn loss_and_gradient<G: Gemm>(
    model: &Model,
    tokens: &[u16],
    targets: &[u16],
    gemm: &G,
) -> (f32, Vec<f32>) {
    let config = model.config();
    let vocab = model.vocab();
    let (d_model, ffn, heads, head_dim) =
        (config.d_model, config.ffn, config.heads, config.head_dim());
    let positions = tokens.len();
    let square = d_model * d_model;

    let trace = model.trace_with(tokens, gemm);
    let (loss, d_logits) = cross_entropy(&trace.logits, targets, vocab);
    // One table for the whole backward pass, matching the forward's.
    let rotations = RotationTable::new(positions, head_dim);

    let weights = model.weights();
    let mut gradient = vec![0.0; weights.len()];
    let block = |start: usize, len: usize| &weights[start..start + len];

    // Unembedding, tied: the table's gradient collects here and again at the
    // very end from the lookup, which is exactly what tying means.
    let mut d_stream = vec![0.0; positions * d_model];
    gemm.sgemm(
        GemmSpec {
            m: positions,
            k: vocab,
            n: d_model,
            transpose_a: false,
            transpose_b: false,
        },
        &d_logits,
        block(0, vocab * d_model),
        &mut d_stream,
    );
    let mut d_embedding = vec![0.0; vocab * d_model];
    gemm.sgemm(
        GemmSpec {
            m: vocab,
            k: positions,
            n: d_model,
            transpose_a: true,
            transpose_b: false,
        },
        &d_logits,
        &trace.final_normed,
        &mut d_embedding,
    );
    accumulate(&mut gradient, 0, &d_embedding);

    let final_offset = config.final_norm_offset(vocab);
    let (mut d_stream, d_final_scale) = rms_norm_backward(
        &trace.final_input,
        block(final_offset, d_model),
        &trace.final_inverse_rms,
        &d_stream,
        d_model,
    );
    accumulate(&mut gradient, final_offset, &d_final_scale);

    for (layer, saved) in trace.layers.iter().enumerate().rev() {
        let offsets = config.layer_offsets(vocab, layer);

        // Feed-forward half.
        let (d_activated, d_w2) = matmul_backward(
            gemm,
            &saved.activated,
            block(offsets.w2, ffn * d_model),
            &d_stream,
            (positions, ffn, d_model),
        );
        accumulate(&mut gradient, offsets.w2, &d_w2);

        let d_hidden: Vec<f32> = d_activated
            .iter()
            .zip(&saved.hidden)
            .map(|(gradient, &pre_activation)| gradient * silu_backward(pre_activation))
            .collect();

        let (d_ffn_normed, d_w1) = matmul_backward(
            gemm,
            &saved.ffn_normed,
            block(offsets.w1, d_model * ffn),
            &d_hidden,
            (positions, d_model, ffn),
        );
        accumulate(&mut gradient, offsets.w1, &d_w1);

        let (d_through_ffn, d_ffn_scale) = rms_norm_backward(
            &saved.post_attention,
            block(offsets.ffn_norm, d_model),
            &saved.ffn_inverse_rms,
            &d_ffn_normed,
            d_model,
        );
        accumulate(&mut gradient, offsets.ffn_norm, &d_ffn_scale);
        let d_post_attention = added(&d_stream, &d_through_ffn);

        // Attention half.
        let (d_attended, d_wo) = matmul_backward(
            gemm,
            &saved.attended,
            block(offsets.wo, square),
            &d_post_attention,
            (positions, d_model, d_model),
        );
        accumulate(&mut gradient, offsets.wo, &d_wo);

        let (d_queries, d_keys, d_values) = attention_backward(
            gemm,
            &AttentionForward {
                queries: &saved.queries,
                keys: &saved.keys,
                values: &saved.values,
                probabilities: &saved.probabilities,
            },
            &d_attended,
            AttentionShape {
                positions,
                heads,
                head_dim,
            },
        );

        // Rotation is applied to q and k only, so only they unwind through it.
        let projected = [
            (
                offsets.wq,
                rope_backward(&d_queries, head_dim, d_model, &rotations),
            ),
            (
                offsets.wk,
                rope_backward(&d_keys, head_dim, d_model, &rotations),
            ),
            (offsets.wv, d_values),
        ];

        let mut d_attention_normed = vec![0.0; positions * d_model];
        for (offset, d_projection) in projected {
            let (d_input, d_weight) = matmul_backward(
                gemm,
                &saved.attention_normed,
                block(offset, square),
                &d_projection,
                (positions, d_model, d_model),
            );
            accumulate(&mut gradient, offset, &d_weight);
            for (slot, value) in d_attention_normed.iter_mut().zip(&d_input) {
                *slot += value;
            }
        }

        let (d_through_attention, d_attention_scale) = rms_norm_backward(
            &saved.input,
            block(offsets.attention_norm, d_model),
            &saved.attention_inverse_rms,
            &d_attention_normed,
            d_model,
        );
        accumulate(&mut gradient, offsets.attention_norm, &d_attention_scale);

        d_stream = added(&d_post_attention, &d_through_attention);
    }

    // The embedding lookup: scatter each position's gradient back to the row it
    // came from. Repeated tokens accumulate, which is why this is `+=`.
    for (position, &token) in tokens.iter().enumerate() {
        let row = token as usize * d_model;
        for dimension in 0..d_model {
            gradient[row + dimension] += d_stream[position * d_model + dimension];
        }
    }

    (loss, gradient)
}

fn accumulate(gradient: &mut [f32], offset: usize, values: &[f32]) {
    for (slot, value) in gradient[offset..offset + values.len()].iter_mut().zip(values) {
        *slot += value;
    }
}

fn added(left: &[f32], right: &[f32]) -> Vec<f32> {
    left.iter().zip(right).map(|(a, b)| a + b).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockedGemm;
    use crate::optim::{AdamW, AdamWConfig};
    use kvetch_model::{
        ModelConfig, NaiveGemm, attention, pseudo_random_weights, rms_norm, rope, silu,
    };

    /// Step size for the finite-difference estimate.
    ///
    /// Larger than the textbook `1e-4` because the loss is computed in f32: the
    /// difference of two nearby losses loses precision to cancellation at a
    /// rate of `machine_eps · |loss| / h`, so shrinking `h` makes the *estimate*
    /// worse, not better. At `h = 1e-2` cancellation (~1e-5) and the central
    /// difference's `O(h²)` truncation error (~2e-5) are comparably small; at
    /// `1e-3` cancellation alone was ~1e-4 and swamped the signal.
    const EPSILON: f32 = 1e-2;

    /// Absolute noise floor of the finite-difference estimate, per above.
    const ABSOLUTE_TOLERANCE: f32 = 1e-4;

    /// Relative tolerance, applied on top of the floor so that large gradients
    /// are not held to an absolute standard the estimate cannot meet.
    const RELATIVE_TOLERANCE: f32 = 1e-2;

    /// Compare an analytic gradient against a central finite difference of the
    /// same scalar function.
    ///
    /// Central rather than forward difference: the error is `O(h²)` instead of
    /// `O(h)`, which is what makes this usable in f32 at all.
    fn assert_matches_finite_differences(
        inputs: &mut [f32],
        analytic: &[f32],
        mut loss_of: impl FnMut(&[f32]) -> f32,
    ) {
        for index in 0..inputs.len() {
            let original = inputs[index];

            inputs[index] = original + EPSILON;
            let raised = loss_of(inputs);
            inputs[index] = original - EPSILON;
            let lowered = loss_of(inputs);
            inputs[index] = original;

            let numeric = (raised - lowered) / (2.0 * EPSILON);
            let allowed = ABSOLUTE_TOLERANCE + RELATIVE_TOLERANCE * analytic[index].abs();

            assert!(
                (numeric - analytic[index]).abs() <= allowed,
                "gradient at {index}: analytic {} vs numeric {numeric} (allowed {allowed})",
                analytic[index]
            );
        }
    }

    #[test]
    fn cross_entropy_gradient_matches_finite_differences() {
        let (positions, vocab) = (4, 7);
        let targets: Vec<u16> = vec![3, 0, 6, 1];
        let mut logits = pseudo_random_weights(positions * vocab, 9);
        // Scale up: near-uniform logits make every gradient tiny, which would
        // let a wrong-by-a-constant-factor backward pass slip through. Not so
        // far that the softmax saturates, though — a saturated softmax has
        // near-zero gradients again, and tests nothing.
        for value in &mut logits {
            *value *= 10.0;
        }

        let (_, gradient) = cross_entropy(&logits, &targets, vocab);

        assert_matches_finite_differences(&mut logits, &gradient, |probe| {
            cross_entropy(probe, &targets, vocab).0
        });
    }

    /// A scalar loss from a vector output: `Σ output·probe`.
    ///
    /// Its gradient with respect to the output is exactly `probe`, so feeding
    /// `probe` in as `d_output` and differentiating this loss checks the
    /// backward op without needing a real loss function attached.
    fn linear_probe(output: &[f32], probe: &[f32]) -> f32 {
        output.iter().zip(probe).map(|(a, b)| a * b).sum()
    }

    #[test]
    fn rms_norm_backward_matches_finite_differences() {
        let (positions, d_model) = (3, 8);
        let mut rows = pseudo_random_weights(positions * d_model, 5);
        for value in &mut rows {
            *value *= 20.0;
        }
        let scale = pseudo_random_weights(d_model, 6)
            .iter()
            .map(|value| 1.0 + value * 4.0)
            .collect::<Vec<f32>>();
        let probe = pseudo_random_weights(positions * d_model, 7);

        let (_, inverse_rms) = rms_norm(&rows, &scale, d_model);
        let (d_rows, d_scale) = rms_norm_backward(&rows, &scale, &inverse_rms, &probe, d_model);

        assert_matches_finite_differences(&mut rows, &d_rows, |candidate| {
            linear_probe(&rms_norm(candidate, &scale, d_model).0, &probe)
        });

        let mut scale_probe = scale.clone();
        assert_matches_finite_differences(&mut scale_probe, &d_scale, |candidate| {
            linear_probe(&rms_norm(&rows, candidate, d_model).0, &probe)
        });
    }

    #[test]
    fn matmul_backward_matches_finite_differences() {
        let (m, k, n) = (3, 5, 4);
        let mut inputs = pseudo_random_weights(m * k, 11);
        let mut weights = pseudo_random_weights(k * n, 12);
        for value in inputs.iter_mut().chain(weights.iter_mut()) {
            *value *= 20.0;
        }
        let probe = pseudo_random_weights(m * n, 13);

        let forward = |x: &[f32], w: &[f32]| {
            let mut out = vec![0.0; m * n];
            NaiveGemm.sgemm(
                GemmSpec {
                    m,
                    k,
                    n,
                    transpose_a: false,
                    transpose_b: false,
                },
                x,
                w,
                &mut out,
            );
            out
        };

        let (d_inputs, d_weights) =
            matmul_backward(&NaiveGemm, &inputs, &weights, &probe, (m, k, n));

        let fixed_weights = weights.clone();
        assert_matches_finite_differences(&mut inputs, &d_inputs, |candidate| {
            linear_probe(&forward(candidate, &fixed_weights), &probe)
        });

        let fixed_inputs = inputs.clone();
        assert_matches_finite_differences(&mut weights, &d_weights, |candidate| {
            linear_probe(&forward(&fixed_inputs, candidate), &probe)
        });
    }

    #[test]
    fn rope_backward_matches_finite_differences() {
        let (positions, heads, head_dim) = (4, 2, 4);
        let d_model = heads * head_dim;
        let mut rows = pseudo_random_weights(positions * d_model, 31);
        for value in &mut rows {
            *value *= 20.0;
        }
        let probe = pseudo_random_weights(positions * d_model, 32);

        let rotations = RotationTable::new(positions, head_dim);
        let analytic = rope_backward(&probe, head_dim, d_model, &rotations);

        assert_matches_finite_differences(&mut rows, &analytic, |candidate| {
            linear_probe(&rope(candidate, head_dim, d_model, &rotations), &probe)
        });
    }

    #[test]
    fn attention_backward_matches_finite_differences() {
        let (positions, heads, head_dim) = (4, 2, 4);
        let d_model = heads * head_dim;
        let scaled = |seed: u64| {
            pseudo_random_weights(positions * d_model, seed)
                .iter()
                .map(|value| value * 20.0)
                .collect::<Vec<f32>>()
        };
        let (mut queries, mut keys, mut values) = (scaled(41), scaled(42), scaled(43));
        let probe = pseudo_random_weights(positions * d_model, 44);

        let attend = |q: &[f32], k: &[f32], v: &[f32]| {
            attention(&NaiveGemm, q, k, v, positions, heads, head_dim).0
        };
        let (_, probabilities) = attention(
            &NaiveGemm,
            &queries,
            &keys,
            &values,
            positions,
            heads,
            head_dim,
        );
        let (d_queries, d_keys, d_values) = attention_backward(
            &NaiveGemm,
            &AttentionForward {
                queries: &queries,
                keys: &keys,
                values: &values,
                probabilities: &probabilities,
            },
            &probe,
            AttentionShape {
                positions,
                heads,
                head_dim,
            },
        );

        let (fixed_keys, fixed_values) = (keys.clone(), values.clone());
        assert_matches_finite_differences(&mut queries, &d_queries, |candidate| {
            linear_probe(&attend(candidate, &fixed_keys, &fixed_values), &probe)
        });

        let (fixed_queries, fixed_values) = (queries.clone(), values.clone());
        assert_matches_finite_differences(&mut keys, &d_keys, |candidate| {
            linear_probe(&attend(&fixed_queries, candidate, &fixed_values), &probe)
        });

        let (fixed_queries, fixed_keys) = (queries.clone(), keys.clone());
        assert_matches_finite_differences(&mut values, &d_values, |candidate| {
            linear_probe(&attend(&fixed_queries, &fixed_keys, candidate), &probe)
        });
    }

    #[test]
    fn silu_backward_matches_finite_differences() {
        let mut inputs = pseudo_random_weights(16, 21)
            .iter()
            .map(|value| value * 60.0)
            .collect::<Vec<f32>>();
        let analytic: Vec<f32> = inputs.iter().map(|&value| silu_backward(value)).collect();

        assert_matches_finite_differences(&mut inputs, &analytic, |candidate| {
            // One input at a time: each output depends only on its own input,
            // so summing them makes every partial derivative independent.
            candidate.iter().copied().map(silu).sum()
        });
    }

    /// The whole-model check: every weight's gradient against a finite
    /// difference of the actual loss.
    ///
    /// This is what the per-op checks build up to. They can each be right while
    /// the composition is wrong — a gradient added to the wrong offset, a
    /// residual fork counted once instead of twice, the tied embedding
    /// collecting from only one of its two uses. None of those show up until the
    /// pieces are wired together.
    #[test]
    fn model_gradient_matches_finite_differences() {
        let config = ModelConfig {
            d_model: 8,
            layers: 2,
            heads: 2,
            ffn: 16,
        };
        let vocab = 12;
        let tokens: Vec<u16> = vec![3, 7, 1, 5];
        let targets: Vec<u16> = vec![7, 1, 5, 2];

        let mut weights = pseudo_random_weights(config.param_count(vocab), 77);
        for value in &mut weights {
            *value *= 8.0;
        }

        let loss_of = |candidate: &[f32]| {
            let model = Model::new(config, vocab, candidate.to_vec())
                .expect("weight count matches the config");
            cross_entropy(&model.forward(&tokens), &targets, vocab).0
        };

        let model = Model::new(config, vocab, weights.clone()).expect("weight count matches");
        let (_, gradient) = loss_and_gradient(&model, &tokens, &targets, &NaiveGemm);

        assert_matches_finite_differences(&mut weights, &gradient, loss_of);
    }

    /// The rig's own smoke test: can it memorize eight fixed sequences?
    ///
    /// A model that cannot drive training loss toward zero on a handful of
    /// examples it sees over and over is broken, and this is the cheapest place
    /// to find that out. It says nothing about generalization — memorizing is
    /// exactly what is being asked for — but every real training bug that
    /// matters shows up here first.
    #[test]
    fn the_rig_can_memorize_a_single_batch() {
        let config = ModelConfig {
            d_model: 32,
            layers: 2,
            heads: 4,
            ffn: 64,
        };
        let vocab = 16;

        let batch: Vec<(Vec<u16>, Vec<u16>)> = (0..8u16)
            .map(|index| {
                let tokens: Vec<u16> = (0..6).map(|step| (index * 3 + step) % 15 + 1).collect();
                let targets: Vec<u16> = tokens[1..].iter().copied().chain([0]).collect();
                (tokens, targets)
            })
            .collect();

        let mut weights = pseudo_random_weights(config.param_count(vocab), 5150);
        for value in &mut weights {
            *value *= 4.0;
        }
        let mut optimizer = AdamW::new(
            weights.len(),
            AdamWConfig {
                learning_rate: 3e-3,
                weight_decay: 0.0,
                ..AdamWConfig::default()
            },
        );

        let mut loss = f32::INFINITY;
        for _ in 0..300 {
            let model = Model::new(config, vocab, weights.clone()).expect("weight count matches");
            let mut batch_gradient = vec![0.0; weights.len()];
            loss = 0.0;

            for (tokens, targets) in &batch {
                let (sequence_loss, gradient) =
                    loss_and_gradient(&model, tokens, targets, &BlockedGemm);
                loss += sequence_loss / batch.len() as f32;
                for (slot, value) in batch_gradient.iter_mut().zip(&gradient) {
                    *slot += value / batch.len() as f32;
                }
            }

            optimizer.step(&mut weights, &batch_gradient);
        }

        assert!(
            loss < 0.05,
            "training did not memorize the batch: final loss {loss}"
        );
    }

    #[test]
    fn cross_entropy_is_lowest_when_the_target_is_certain() {
        let vocab = 4;
        let confident = [0.0, 10.0, 0.0, 0.0];
        let wrong = [10.0, 0.0, 0.0, 0.0];

        let (good, _) = cross_entropy(&confident, &[1], vocab);
        let (bad, _) = cross_entropy(&wrong, &[1], vocab);

        assert!(good < 0.01, "a confident correct prediction should cost ~0");
        assert!(bad > good, "a confident wrong prediction must cost more");
    }
}
