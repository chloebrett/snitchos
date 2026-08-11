//! The real distribution source: a trained checkpoint.
//!
//! Thin by design — everything with a decision in it lives in [`crate::serve`] and
//! [`crate::sample`], which are testable without 4.2 MB of weights. This is the
//! adapter that makes a `Model` look like a [`Logits`].

use alloc::vec::Vec;

use kvetch_model::{Model, RowGemm, Session};
use kvetch_vocab::TokenId;

use crate::serve::Logits;

/// A checkpoint, answering "what comes next" for the position after a token run.
pub struct ModelLogits {
    model: Model,
    /// Keys and values for the run asked about last time.
    ///
    /// Held across calls because a completion asks about a growing prefix, one token
    /// at a time — which is exactly the shape a KV cache turns from `O(prefix)` per
    /// token into `O(1)`. The session reconciles the run itself, so a sampler that
    /// backtracks cannot read a stale answer out of it.
    session: Session,
}

impl ModelLogits {
    #[must_use]
    pub fn new(model: Model) -> Self {
        Self { model, session: Session::new() }
    }

    /// The vocab this checkpoint was trained against, for
    /// [`Server::new`](crate::serve::Server::new).
    #[must_use]
    pub const fn vocab_fingerprint(&self) -> u64 {
        self.model.vocab_fingerprint()
    }
}

impl Logits for ModelLogits {
    fn next(&mut self, tokens: &[TokenId]) -> Vec<f32> {
        // An empty prompt has no position to predict from, so there is nothing to
        // say — an empty distribution, which `draw` reads as "stop". A REPL prefix is
        // never empty in practice (Tab on a blank line is answered by the grammar
        // without asking anyone), but the loop must not index past the end to find
        // that out.
        if tokens.is_empty() {
            return Vec::new();
        }
        // Not `Model::forward`: that computes logits for every position and keeps
        // every training intermediate, so generating N tokens re-runs the prefix N
        // times. The session gives the same numbers — bit for bit, which
        // `generating_with_a_cache_is_bit_identical_to_re_running_the_prefix` pins —
        // for one position of work per token.
        //
        // `RowGemm` rather than `NaiveGemm`: same arithmetic, same bits, walked in the
        // order the weights are laid out. 77% of the forward pass is a multiply that
        // `NaiveGemm` reads one useful float per cache line from — see
        // `RowGemm`'s own docs and `notes/drivel-on-vf2-speedup-ideas.md` §3.
        self.session.logits_for(&self.model, tokens, &RowGemm)
    }
}

#[cfg(test)]
mod tests {
    use super::ModelLogits;
    use crate::serve::Logits;
    use alloc::vec;
    use kvetch_model::{Model, ModelConfig};

    /// The smallest model whose shape is checkable: real weights, real forward pass,
    /// tiny enough to run in a unit test.
    fn tiny(vocab: usize) -> Model {
        let config = ModelConfig { d_model: 4, layers: 1, heads: 2, ffn: 8 };
        let weights = vec![0.05f32; config.param_count(vocab)];
        Model::new(config, vocab, weights).expect("shape matches")
    }

    /// **`forward` returns every position; serving wants the last.** Getting this
    /// slice wrong does not crash — it predicts from the wrong position and produces
    /// a plausible completion for a prefix nobody typed, which is the kind of bug
    /// that survives a demo.
    #[test]
    fn the_distribution_is_the_last_position_not_the_first() {
        let vocab = 11;
        let model = tiny(vocab);
        let tokens = [1u16, 2, 3, 4];

        let all = model.forward(&tokens);
        let next = ModelLogits::new(tiny(vocab)).next(&tokens);

        assert_eq!(next.len(), vocab, "one logit per vocabulary entry, not per position");
        assert_eq!(next.as_slice(), &all[all.len() - vocab..], "not the final row");
    }

    /// Nothing to predict *from* is not the same as a flat distribution over
    /// everything: an empty run must yield an empty answer, which `draw` reads as
    /// "stop", rather than indexing past the end to discover that.
    #[test]
    fn an_empty_token_run_has_nothing_to_say() {
        assert!(ModelLogits::new(tiny(11)).next(&[]).is_empty());
    }

    #[test]
    fn the_checkpoints_vocab_fingerprint_is_carried_through() {
        // The server pairs on this value; a wrapper that forgot it would refuse
        // every valid pair, or worse, accept every invalid one.
        let stamped = tiny(11).stamped_with(0xfeed_face_dead_beef);
        assert_eq!(ModelLogits::new(stamped).vocab_fingerprint(), 0xfeed_face_dead_beef);
    }
}
