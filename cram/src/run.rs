//! The training loop, and the report it keeps on itself.
//!
//! A training run is a long-lived process whose failure modes are mostly
//! *silent* — a loss that plateaus, a throughput that quietly halves, a
//! schedule that never warms up. So the loop reports on itself continuously and
//! writes a machine-readable curve alongside the checkpoint, in the same spirit
//! as everything else here: the thing that could lie about its own progress
//! instead files a report.

use std::time::{Duration, Instant};

use kvetch_model::{Gemm, Model, Rung, pseudo_random_weights};

use crate::optim::{AdamW, AdamWConfig};
use crate::train::loss_and_gradient;

/// One training run, fully specified. Everything here is recorded in the run's
/// report so a curve can be attributed to the settings that produced it.
#[derive(Debug, Clone, Copy)]
pub struct TrainingConfig {
    pub rung: Rung,
    /// Tokens per sequence.
    pub context: usize,
    /// Sequences per step.
    pub batch: usize,
    pub steps: usize,
    pub learning_rate: f32,
    pub warmup_steps: usize,
    pub weight_decay: f32,
    /// Draws every batch and the initial weights; the run is reproducible from
    /// it alone, per `docs/randomness-and-entropy.md`.
    pub seed: u64,
    pub report_every: usize,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            rung: Rung::Drivel,
            context: 128,
            batch: 16,
            steps: 2000,
            learning_rate: 3e-3,
            warmup_steps: 100,
            weight_decay: 0.01,
            seed: 0,
            report_every: 20,
        }
    }
}

impl TrainingConfig {
    /// Linear warmup into cosine decay.
    ///
    /// Warmup exists because Adam's second-moment estimate is meaningless for
    /// the first few dozen steps — stepping at full rate into that produces the
    /// early divergence that gets misdiagnosed as "learning rate too high".
    pub fn learning_rate_at(&self, step: usize) -> f32 {
        if step < self.warmup_steps {
            return self.learning_rate * step as f32 / self.warmup_steps as f32;
        }

        let remaining = self.steps.saturating_sub(self.warmup_steps).max(1);
        let progress = (step - self.warmup_steps) as f32 / remaining as f32;

        0.5 * self.learning_rate * (1.0 + (core::f32::consts::PI * progress).cos())
    }

    /// Tokens consumed per step.
    pub const fn tokens_per_step(&self) -> usize {
        self.context * self.batch
    }

    /// The batch for `step`: `batch` windows of `context` tokens, each paired
    /// with the same window shifted one token later.
    ///
    /// Derived from `(seed, step)` rather than a running RNG so any step's batch
    /// can be reproduced without replaying the ones before it — the same
    /// property that makes a babble corpus addressable by index.
    pub fn batch_at(&self, tokens: &[u16], step: usize) -> Vec<(Vec<u16>, Vec<u16>)> {
        let span = self.context + 1;
        let last_start = tokens.len().saturating_sub(span).max(1);

        (0..self.batch)
            .map(|slot| {
                let draw = splitmix64(
                    self.seed
                        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
                        .wrapping_add(step as u64)
                        .wrapping_mul(31)
                        .wrapping_add(slot as u64),
                );
                let start = (draw % last_start as u64) as usize;
                let window = &tokens[start..(start + span).min(tokens.len())];

                (
                    window[..self.context.min(window.len() - 1)].to_vec(),
                    window[1..].to_vec(),
                )
            })
            .collect()
    }
}

/// A mixing function good enough to spread consecutive `(seed, step)` pairs.
///
/// Uniqueness/statistical category, not security — see
/// `docs/randomness-and-entropy.md`.
fn splitmix64(mut state: u64) -> u64 {
    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// What the loop reports about itself, every `report_every` steps.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub step: usize,
    pub steps: usize,
    pub loss: f32,
    /// Exponentially smoothed, because a single batch's loss is too noisy to
    /// see a trend in.
    pub smoothed_loss: f32,
    pub learning_rate: f32,
    /// L2 norm of the gradient. A spike here precedes a loss spike, and a
    /// collapse to zero is a dead model — both invisible in the loss alone.
    pub gradient_norm: f32,
    pub tokens_per_second: f64,
    pub elapsed: Duration,
    pub remaining: Duration,
}

impl Progress {
    /// One line, fixed-width, suitable for a terminal or a log.
    pub fn line(&self) -> String {
        format!(
            "step {:>6}/{:<6} loss {:>7.4} (avg {:>7.4})  lr {:.2e}  |g| {:>7.3}  {:>7.0} tok/s  {}  eta {}",
            self.step,
            self.steps,
            self.loss,
            self.smoothed_loss,
            self.learning_rate,
            self.gradient_norm,
            self.tokens_per_second,
            clock(self.elapsed),
            clock(self.remaining),
        )
    }

    /// Tab-separated, for the durable curve beside the checkpoint.
    pub fn row(&self) -> String {
        format!(
            "{}\t{:.6}\t{:.6}\t{:.6e}\t{:.6}\t{:.1}\t{:.1}",
            self.step,
            self.loss,
            self.smoothed_loss,
            self.learning_rate,
            self.gradient_norm,
            self.tokens_per_second,
            self.elapsed.as_secs_f64(),
        )
    }

    /// Header matching [`row`](Self::row).
    pub const HEADER: &'static str =
        "step\tloss\tsmoothed_loss\tlearning_rate\tgradient_norm\ttokens_per_second\telapsed";
}

fn clock(duration: Duration) -> String {
    let total = duration.as_secs();
    format!("{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
}

/// Train a model, reporting progress as it goes.
///
/// `observe` is called every `report_every` steps and at the last step. It is a
/// callback rather than a hardcoded `println!` so the same loop serves the CLI,
/// a test, and any future consumer that wants the numbers rather than the
/// prose.
pub fn train<G: Gemm + Sync>(
    tokens: &[u16],
    vocab: usize,
    config: TrainingConfig,
    gemm: &G,
    mut observe: impl FnMut(Progress),
) -> Model {
    let model_config = config.rung.config();
    let mut weights = pseudo_random_weights(model_config.param_count(vocab), config.seed);
    let mut optimizer = AdamW::new(
        weights.len(),
        AdamWConfig {
            learning_rate: config.learning_rate,
            weight_decay: config.weight_decay,
            ..AdamWConfig::default()
        },
    );

    let started = Instant::now();
    let mut smoothed = f32::NAN;

    for step in 0..config.steps {
        let model = Model::new(model_config, vocab, weights.clone())
            .expect("weight count is derived from the same config");
        let batch = config.batch_at(tokens, step);

        // Sequences in a batch are independent, so they run concurrently. The
        // win is large because the per-sequence cost is dominated by attention,
        // which is scalar Rust rather than a `Gemm` call and therefore gets no
        // acceleration at all — spreading it across cores is the only lever it
        // has. Results are summed in index order, so the gradient is identical
        // to the sequential one regardless of core count.
        let results: Vec<(f32, Vec<f32>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(inputs, targets)| {
                    let model = &model;
                    scope.spawn(move || loss_and_gradient(model, inputs, targets, gemm))
                })
                .collect();

            handles
                .into_iter()
                .map(|handle| handle.join().expect("a sequence's gradient panicked"))
                .collect()
        });

        let mut gradient = vec![0.0; weights.len()];
        let mut loss = 0.0;
        for (sequence_loss, sequence_gradient) in &results {
            loss += sequence_loss / batch.len() as f32;
            for (slot, value) in gradient.iter_mut().zip(sequence_gradient) {
                *slot += value / batch.len() as f32;
            }
        }

        optimizer.set_learning_rate(config.learning_rate_at(step));
        optimizer.step(&mut weights, &gradient);

        smoothed = if smoothed.is_nan() {
            loss
        } else {
            0.9 * smoothed + 0.1 * loss
        };

        let last = step + 1 == config.steps;
        if step % config.report_every == 0 || last {
            let elapsed = started.elapsed();
            let done = step + 1;
            let per_step = elapsed.as_secs_f64() / done as f64;

            observe(Progress {
                step: done,
                steps: config.steps,
                loss,
                smoothed_loss: smoothed,
                learning_rate: config.learning_rate_at(step),
                gradient_norm: gradient.iter().map(|g| g * g).sum::<f32>().sqrt(),
                tokens_per_second: config.tokens_per_step() as f64 * done as f64
                    / elapsed.as_secs_f64().max(f64::EPSILON),
                elapsed,
                remaining: Duration::from_secs_f64(per_step * (config.steps - done) as f64),
            });
        }
    }

    Model::new(model_config, vocab, weights).expect("weight count is unchanged by training")
}

/// Draw `max_tokens` tokens from `model`, continuing `prompt`.
///
/// **Unconstrained** — no oracle mask. That is the whole point of the
/// grammar-learnability probe: masked output parses by construction and would
/// measure nothing about the model. See `plans/drivel.md`.
///
/// Sampled rather than greedy, because greedy decoding yields exactly one
/// program however many times you run it, and `parse%` needs a distribution.
/// `temperature` scales the logits before the draw; the `seed` makes the whole
/// sample reproducible.
///
/// Cost is `O(max_tokens²)` — every step re-runs the whole prefix, since there
/// is no KV cache yet. Fine for a few hundred samples; the cache is the fix when
/// it stops being.
pub fn sample<G: Gemm>(
    model: &Model,
    gemm: &G,
    prompt: &[u16],
    max_tokens: usize,
    temperature: f32,
    seed: u64,
) -> Vec<u16> {
    let vocab = model.vocab();
    let mut tokens = prompt.to_vec();

    for step in 0..max_tokens {
        let logits = model.forward_with(&tokens, gemm);
        let last = &logits[logits.len() - vocab..];

        let mut probabilities: Vec<f32> = last.iter().map(|z| z / temperature).collect();
        softmax_in_place(&mut probabilities);

        let draw = splitmix64(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ step as u64);
        tokens.push(pick(&probabilities, draw));
    }

    tokens
}

/// Inverse-CDF sample from `probabilities` using the low 24 bits of `draw`.
///
/// Falls back to the last index only when floating-point error leaves the
/// cumulative sum a hair under the target, which is a rounding artifact rather
/// than a real outcome.
fn pick(probabilities: &[f32], draw: u64) -> u16 {
    let target = (draw >> 40) as f32 / 16_777_216.0;
    let mut cumulative = 0.0;

    for (index, probability) in probabilities.iter().enumerate() {
        cumulative += probability;
        if cumulative >= target {
            return index as u16;
        }
    }

    (probabilities.len() - 1) as u16
}

fn softmax_in_place(row: &mut [f32]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use kvetch_model::{ModelConfig, NaiveGemm};

    fn schedule() -> TrainingConfig {
        TrainingConfig {
            steps: 1000,
            warmup_steps: 100,
            learning_rate: 1.0,
            ..TrainingConfig::default()
        }
    }

    #[test]
    fn sampling_is_reproducible_and_stays_inside_the_vocab() {
        let config = ModelConfig {
            d_model: 8,
            layers: 1,
            heads: 2,
            ffn: 16,
        };
        let vocab = 11;
        let model = Model::new(config, vocab, pseudo_random_weights(config.param_count(vocab), 3))
            .expect("weight count matches");

        let drawn = sample(&model, &NaiveGemm, &[1, 2], 6, 1.0, 17);

        assert_eq!(drawn.len(), 8, "prompt plus one token per step");
        assert_eq!(&drawn[..2], &[1, 2], "the prompt must survive verbatim");
        assert!(
            drawn.iter().all(|&token| (token as usize) < vocab),
            "sampled a token outside the vocab: {drawn:?}"
        );
        assert_eq!(
            drawn,
            sample(&model, &NaiveGemm, &[1, 2], 6, 1.0, 17),
            "same seed must give the same sample"
        );
        assert_ne!(
            drawn,
            sample(&model, &NaiveGemm, &[1, 2], 6, 1.0, 18),
            "a different seed should explore elsewhere"
        );
    }

    #[test]
    fn the_learning_rate_warms_up_then_decays_to_almost_nothing() {
        let config = schedule();

        assert!(config.learning_rate_at(0) < 0.02, "should start near zero");
        assert!(
            (config.learning_rate_at(100) - 1.0).abs() < 1e-3,
            "should reach full rate at the end of warmup"
        );
        assert!(
            config.learning_rate_at(50) > config.learning_rate_at(10),
            "should climb through warmup"
        );
        assert!(
            config.learning_rate_at(999) < 0.01,
            "should decay to almost nothing by the end"
        );
    }

    #[test]
    fn batches_are_reproducible_from_the_seed() {
        let tokens: Vec<u16> = (0..500u16).collect();
        let config = TrainingConfig {
            context: 8,
            batch: 4,
            seed: 99,
            ..TrainingConfig::default()
        };

        let first = config.batch_at(&tokens, 7);
        let again = config.batch_at(&tokens, 7);
        let elsewhere = config.batch_at(&tokens, 8);

        assert_eq!(first, again, "same step must yield the same batch");
        assert_ne!(first, elsewhere, "different steps must differ");
        assert_eq!(first.len(), 4, "one sequence per batch slot");
        assert!(
            first
                .iter()
                .all(|(inputs, targets)| inputs.len() == 8 && targets.len() == 8),
            "every sequence is one context long"
        );
        assert!(
            first
                .iter()
                .all(|(inputs, targets)| inputs[1..] == targets[..7]),
            "targets must be inputs shifted by one — the next-token objective"
        );
    }
}
