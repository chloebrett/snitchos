//! The real distribution source: a trained checkpoint.
//!
//! Thin by design — everything with a decision in it lives in [`crate::serve`] and
//! [`crate::sample`], which are testable without 4.2 MB of weights. This is the
//! adapter that makes a `Model` look like a [`Logits`].

use alloc::vec::Vec;

use kvetch_model::Model;
use kvetch_vocab::TokenId;

use crate::serve::Logits;

/// A checkpoint, answering "what comes next" for the position after a token run.
pub struct ModelLogits {
    model: Model,
}

impl ModelLogits {
    #[must_use]
    pub const fn new(model: Model) -> Self {
        Self { model }
    }

    /// The vocab this checkpoint was trained against, for
    /// [`Server::new`](crate::serve::Server::new).
    #[must_use]
    pub const fn vocab_fingerprint(&self) -> u64 {
        self.model.vocab_fingerprint()
    }
}

impl Logits for ModelLogits {
    fn next(&self, tokens: &[TokenId]) -> Vec<f32> {
        // `forward` returns logits for *every* position, `tokens.len() × vocab`
        // row-major, because training needs them. Serving wants the last row only.
        //
        // An empty prompt has no position to predict from, so there is nothing to
        // say — an empty distribution, which `draw` reads as "stop". A REPL prefix is
        // never empty in practice (Tab on a blank line is answered by the grammar
        // without asking anyone), but the loop must not index past the end to find
        // that out.
        if tokens.is_empty() {
            return Vec::new();
        }
        let vocab = self.model.vocab();
        let mut logits = self.model.forward(tokens);
        logits.split_off(logits.len() - vocab)
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
