//! Drawing a token the grammar allows.
//!
//! **Why not simply mask the whole distribution.** The textbook constrained-decoding
//! move is to zero every illegal logit and renormalise. That needs a legality verdict
//! for all 2048 tokens at every step, and here a verdict costs a lex *and* a parse of
//! the extended prefix — thousands of parses per token, on a machine whose whole
//! completion budget is a few million instructions.
//!
//! So legality is tested lazily, in descending probability: draw, ask, and on a
//! refusal draw again from what is left. That is sampling **without replacement**
//! from the masked distribution — identical in outcome to masking everything, but it
//! pays for verdicts only on the tokens it actually considers, and the model proposes
//! a legal token first the overwhelming majority of the time.
//!
//! # Two surviving mutants, on purpose
//!
//! `cargo xtask mutants kvetch-serve` leaves two, both in [`weighted_pick`]'s guards,
//! and neither is worth a test:
//!
//! - `total > 0.0` → `>=` is **equivalent**. A zero total means every weight is zero,
//!   so the walk finds no positive weight either way and returns `None` by both routes.
//! - `weight > 0.0` → `>=` differs only when the draw point lands on exactly `0.0`
//!   *and* a struck candidate precedes the survivor, in which case a refused token is
//!   offered a second time. [`draw`] re-checks legality on everything it is handed, so
//!   the whole cost is one of sixteen refusals. Pinning an exact float coincidence
//!   would buy a fragile test for a rounding-width bug.

use alloc::vec::Vec;

use kvetch_vocab::TokenId;

/// How many refusals to absorb before giving up on a position.
///
/// A cap rather than an exhaustive walk, because the tail is worthless: if the top
/// candidates are all illegal, the model has no useful opinion here and the honest
/// answer is to stop the completion rather than keep digging for the least-bad legal
/// token. Stopping early is always safe — a completion is a *fragment*, and a shorter
/// fragment is still a valid one.
pub const MAX_REFUSALS: usize = 16;

/// Draw a token that `legal` accepts, or `None` if this position has no useful
/// continuation.
///
/// `logits` is the model's raw output for one position; `seed` makes the draw
/// reproducible (the same request replays byte for byte, which is what lets a
/// recorded completion be re-derived from its trace).
///
/// `legal` is called at most [`MAX_REFUSALS`] + 1 times, in descending probability
/// order of the candidates actually drawn — never once per vocabulary entry.
pub fn draw<F: FnMut(TokenId) -> bool>(
    logits: &[f32],
    seed: u64,
    mut legal: F,
) -> Option<TokenId> {
    let mut weights = weights_from_logits(logits);
    let mut rng = Lcg::new(seed);

    for _ in 0..=MAX_REFUSALS {
        let candidate = weighted_pick(&weights, &mut rng)?;
        if legal(candidate) {
            return Some(candidate);
        }
        // Refused: strike it and draw again from what remains. Striking (rather than
        // re-drawing from the same distribution) is what makes this terminate — and
        // what makes it *sampling without replacement* rather than rejection
        // sampling, which on a mostly-illegal distribution could spin forever.
        weights[candidate as usize] = 0.0;
    }
    None
}

/// Relative weights from logits: `exp(logit - max)`, **deliberately not normalised**.
///
/// Dividing by the total would be dead work. [`weighted_pick`] draws against the sum
/// of whatever it is handed, so scale cancels — and striking a refused candidate
/// changes that sum anyway, which is precisely why the pick cannot rely on a
/// precomputed normalisation. (Mutation testing is what surfaced this: swapping the
/// division for a multiplication changed nothing observable, because the ratios it
/// preserved were the only thing anyone read.)
///
/// The `- max` shift is *not* optional: trained logits reach into the tens, `exp(89)`
/// is already infinity in `f32`, and one infinity turns every probability into `NaN`
/// and every comparison below into `false`.
fn weights_from_logits(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    logits.iter().map(|&x| libm::expf(x - max)).collect()
}

/// Pick an index with probability proportional to its weight. `None` when every
/// weight has been struck out (nothing left to draw).
///
/// The walk tracks the last positive weight as it goes, so a `point` that
/// floating-point drift carries past the end lands on a real candidate instead of
/// falling out as "nothing left" — a rounding error must not truncate a completion.
/// Folding that into the loop (rather than a separate rescan afterwards) keeps one
/// code path, which is the difference between a fallback that is exercised by every
/// call and one that is exercised by nothing.
fn weighted_pick(weights: &[f32], rng: &mut Lcg) -> Option<TokenId> {
    let total: f32 = weights.iter().sum();
    if !(total > 0.0) {
        return None;
    }
    let mut point = rng.next_unit() * total;
    let mut last_positive = None;
    for (index, &weight) in weights.iter().enumerate() {
        if weight > 0.0 {
            last_positive = TokenId::try_from(index).ok();
            point -= weight;
            if point <= 0.0 {
                return last_positive;
            }
        }
    }
    last_positive
}

/// The same 64-bit LCG babble draws with — deterministic, seedable, and adequate for
/// choosing among tokens. Shared lineage matters more than quality here: two rungs
/// that disagree about what a seed means cannot be compared on the same prompt.
struct Lcg(u64);

impl Lcg {
    const fn new(seed: u64) -> Self {
        // A zero seed would leave a pure-multiplicative LCG stuck at zero forever.
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }

    /// A value in `[0, 1)`. Takes the top 24 bits, which is all an `f32` mantissa can
    /// hold — taking more would round back up to exactly 1.0 and index past the end.
    fn next_unit(&mut self) -> f32 {
        let bits = self.next_u64() >> 40;
        bits as f32 / (1u32 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_REFUSALS, draw};
    use alloc::vec;
    use alloc::vec::Vec;

    /// **The test the whole design exists for.** Hand the model its favourite token
    /// and refuse it: the draw must come back with something the grammar allows, not
    /// with the confident illegal one. Delete the legality check and this fails.
    #[test]
    fn a_token_the_oracle_rejects_is_never_drawn_however_confident_the_model_is() {
        // Token 0 has almost all the mass; only token 3 is legal.
        let logits = vec![20.0, 0.0, 0.0, 0.1];
        for seed in 0..64 {
            assert_eq!(draw(&logits, seed, |t| t == 3), Some(3), "seed {seed}");
        }
    }

    #[test]
    fn the_same_seed_draws_the_same_token() {
        // Reproducibility is not a nicety here: a recorded completion is replayable
        // only if the seed fully determines it.
        let logits = vec![1.0, 1.0, 1.0, 1.0, 1.0];
        let first = draw(&logits, 12345, |_| true);
        assert_eq!(first, draw(&logits, 12345, |_| true));
        assert!(first.is_some());
    }

    #[test]
    fn a_position_with_nothing_legal_stops_rather_than_inventing() {
        // Every token refused ⇒ no completion at this position. Stopping is safe: a
        // completion is a fragment, and a shorter fragment is still valid.
        let logits = vec![1.0, 2.0, 3.0];
        assert_eq!(draw(&logits, 7, |_| false), None);
    }

    #[test]
    fn the_model_is_still_obeyed_among_legal_tokens() {
        // Masking must not flatten the distribution — the point of weights is that a
        // likelier legal token wins more often. Both are legal; one is 20x likelier.
        let logits = vec![3.0, 0.0];
        let likelier = (0..200).filter(|&s| draw(&logits, s, |_| true) == Some(0)).count();
        assert!(likelier > 150, "expected the high-logit token to dominate, got {likelier}/200");
    }

    #[test]
    fn refusals_are_bounded_rather_than_walking_the_whole_vocabulary() {
        // Legality costs a lex + a parse, so an unbounded walk is a performance bug
        // hiding in a correctness-shaped function. Count the verdicts asked for.
        let logits: Vec<f32> = (0..2048).map(|_| 1.0).collect();
        let mut asked = 0;
        let drawn = draw(&logits, 3, |_| {
            asked += 1;
            false
        });
        assert_eq!(drawn, None);
        assert_eq!(asked, MAX_REFUSALS + 1);
    }

    #[test]
    fn a_struck_token_is_never_offered_twice() {
        // Sampling *without* replacement is what makes the refusal loop terminate;
        // re-drawing from the same distribution could offer the same illegal token
        // every time and burn the budget for nothing.
        let logits = vec![10.0, 9.0, 8.0];
        let mut seen: Vec<u16> = Vec::new();
        let drawn = draw(&logits, 99, |t| {
            seen.push(t);
            false
        });
        assert_eq!(drawn, None);
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "a token was offered twice: {seen:?}");
    }

    /// A constant RNG passes every test above — same seed, same token; the likeliest
    /// token still dominates — while making the sampler useless: every seed would
    /// return the same completion, and `request_seed` would be decoration. Mutation
    /// testing caught exactly this (replacing the LCG with `0` survived).
    #[test]
    fn different_seeds_draw_different_tokens() {
        let logits: Vec<f32> = vec![1.0; 8];
        let mut drawn: Vec<u16> = (0..64).filter_map(|s| draw(&logits, s, |_| true)).collect();
        drawn.sort_unstable();
        drawn.dedup();
        assert!(drawn.len() > 1, "every seed drew the same token: {drawn:?}");
    }

    /// The draw must track the *shape* of the distribution, not merely its argmax.
    /// A 3:1 logit gap should show up as roughly a 3:1 split — which is what pins the
    /// arithmetic in `weighted_pick` (scaling the pick point, or short-circuiting the
    /// walk, both collapse this onto token 0 while leaving "the likeliest wins" true).
    #[test]
    fn the_draw_tracks_the_distribution_not_just_its_argmax() {
        // exp(1.0986) ≈ 3, so token 0 should take ~75% of draws.
        let logits = vec![1.0986, 0.0];
        let n = 400;
        let zeros = (0..n).filter(|&s| draw(&logits, s, |_| true) == Some(0)).count();
        assert!(
            (240..=360).contains(&zeros),
            "expected ~75% (300/400) draws of the 3x-likelier token, got {zeros}"
        );
    }

    #[test]
    fn an_extreme_logit_does_not_poison_the_distribution() {
        // `exp(89)` is already infinity in f32; without the max-shift in `softmax`
        // every probability becomes NaN, every comparison false, and the draw
        // silently returns nothing.
        let logits = vec![500.0, 1.0, 2.0];
        assert_eq!(draw(&logits, 1, |_| true), Some(0));
    }
}
