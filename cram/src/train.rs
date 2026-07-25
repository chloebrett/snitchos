//! The backward pass, hand-written.
//!
//! Every op here comes in a forward/backward pair, and every backward is
//! validated against a finite-difference estimate of the same quantity. With no
//! framework there is no second implementation to agree with, so correctness
//! comes from the mathematical definition instead — which is a stronger check
//! than agreement, because two implementations can share a misconception and
//! finite differences cannot share one with us.

use kvetch_model::{Gemm, GemmSpec};

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

/// Derivative of [`kvetch_model::silu`] at `value`: `σ + x·σ·(1 − σ)`.
pub fn silu_backward(value: f32) -> f32 {
    let sigmoid = 1.0 / (1.0 + (-value).exp());
    sigmoid + value * sigmoid * (1.0 - sigmoid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvetch_model::{NaiveGemm, pseudo_random_weights, rms_norm, silu};

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
