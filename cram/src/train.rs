//! The backward pass, hand-written.
//!
//! Every op here comes in a forward/backward pair, and every backward is
//! validated against a finite-difference estimate of the same quantity. With no
//! framework there is no second implementation to agree with, so correctness
//! comes from the mathematical definition instead — which is a stronger check
//! than agreement, because two implementations can share a misconception and
//! finite differences cannot share one with us.

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

#[cfg(test)]
mod tests {
    use super::*;
    use kvetch_model::pseudo_random_weights;

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
