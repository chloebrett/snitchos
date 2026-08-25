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
    /// Steps between held-out loss reports. `0` turns the held-out pass off.
    ///
    /// A training loss cannot tell learning from memorising, and at these rung
    /// sizes memorising is the expected failure — drivel has a million
    /// parameters and the real corpus is a couple of million tokens. The
    /// held-out loss is the only number in the run that answers the question.
    pub eval_every: usize,
    /// Sequences in the held-out batch.
    ///
    /// Larger than a training batch because it is measured rarely, and a noisy
    /// held-out point is worse than an expensive one.
    pub eval_batch: usize,
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
            eval_every: 0,
            eval_batch: 64,
        }
    }
}

/// Folded into the seed so the held-out windows cannot coincide with a training
/// batch's. Arbitrary, and fixed forever: changing it changes which windows a
/// run measures itself on, which makes old curves incomparable.
const HELD_OUT_SALT: u64 = 0x5e1f_2b7d_9c04_a361;

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
        self.windows(tokens, self.batch, base_for(self.seed, step))
    }

    /// The held-out batch — **fixed for the whole run**, not drawn per step.
    ///
    /// Two consecutive points on the held-out curve must differ by the model
    /// alone. Re-drawing the windows each time would move the curve for two
    /// reasons at once, which is precisely the ambiguity the held-out loss
    /// exists to remove.
    pub fn held_out_batch(&self, tokens: &[u16]) -> Vec<(Vec<u16>, Vec<u16>)> {
        self.windows(tokens, self.eval_batch, base_for(self.seed ^ HELD_OUT_SALT, 0))
    }

    fn windows(&self, tokens: &[u16], count: usize, base: u64) -> Vec<(Vec<u16>, Vec<u16>)> {
        let span = self.context + 1;
        let last_start = tokens.len().saturating_sub(span).max(1);

        (0..count)
            .map(|slot| {
                let draw = splitmix64(base.wrapping_add(slot as u64));
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

/// Where a batch's window draws start, from the seed and the step alone.
fn base_for(seed: u64, step: usize) -> u64 {
    seed.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(step as u64)
        .wrapping_mul(31)
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
    /// Loss on data the run never trains on, measured every `eval_every` steps.
    /// `None` on the rows between measurements, and for a run with no split.
    pub held_out_loss: Option<f32>,
}

impl Progress {
    /// One line, fixed-width, suitable for a terminal or a log.
    pub fn line(&self) -> String {
        format!(
            "step {:>6}/{:<6} loss {:>7.4} (avg {:>7.4})  lr {:.2e}  |g| {:>7.3}  {:>7.0} tok/s  {}  eta {}{}",
            self.step,
            self.steps,
            self.loss,
            self.smoothed_loss,
            self.learning_rate,
            self.gradient_norm,
            self.tokens_per_second,
            clock(self.elapsed),
            clock(self.remaining),
            self.held_out_loss.map_or(String::new(), |loss| format!("  held-out {loss:>7.4}")),
        )
    }

    /// Tab-separated, for the durable curve beside the checkpoint.
    ///
    /// The held-out column is always present and empty when unmeasured, because
    /// a row with a different field count is a row every reader has to guess at.
    pub fn row(&self) -> String {
        format!(
            "{}\t{:.6}\t{:.6}\t{:.6e}\t{:.6}\t{:.1}\t{:.1}\t{}",
            self.step,
            self.loss,
            self.smoothed_loss,
            self.learning_rate,
            self.gradient_norm,
            self.tokens_per_second,
            self.elapsed.as_secs_f64(),
            self.held_out_loss.map_or(String::new(), |loss| format!("{loss:.6}")),
        )
    }

    /// Header matching [`row`](Self::row).
    pub const HEADER: &'static str =
        "step\tloss\tsmoothed_loss\tlearning_rate\tgradient_norm\ttokens_per_second\telapsed\theld_out_loss";
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
    held_out: &[u16],
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
        let (loss, gradient) = mean_loss_and_gradient(&model, &batch, weights.len(), gemm);

        let last = step + 1 == config.steps;
        let reporting = step % config.report_every == 0 || last;
        // Measured before the optimizer steps, so the held-out loss and the
        // training loss on the same row describe the same weights.
        let held_out_loss = (reporting
            && config.eval_every > 0
            && !held_out.is_empty()
            && (step % config.eval_every == 0 || last))
            .then(|| {
                let batch = config.held_out_batch(held_out);
                mean_loss_and_gradient(&model, &batch, weights.len(), gemm).0
            });

        optimizer.set_learning_rate(config.learning_rate_at(step));
        optimizer.step(&mut weights, &gradient);

        smoothed = if smoothed.is_nan() {
            loss
        } else {
            0.9 * smoothed + 0.1 * loss
        };

        if reporting {
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
                held_out_loss,
            });
        }
    }

    Model::new(model_config, vocab, weights).expect("weight count is unchanged by training")
}

/// Mean loss and gradient over a batch.
///
/// Sequences in a batch are independent, so they run concurrently. The win is
/// large because the per-sequence cost is dominated by attention, which is
/// scalar Rust rather than a `Gemm` call and therefore gets no acceleration at
/// all — spreading it across cores is the only lever it has. Results are summed
/// in index order, so the gradient is identical to the sequential one regardless
/// of core count.
///
/// The held-out pass wants only the loss and pays for a gradient it discards.
/// That is deliberate: a forward-only path would be a second implementation of
/// the forward pass, and two of those disagreeing is a far worse bug than the
/// handful of wasted milliseconds this costs every `eval_every` steps.
fn mean_loss_and_gradient<G: Gemm + Sync>(
    model: &Model,
    batch: &[(Vec<u16>, Vec<u16>)],
    parameters: usize,
    gemm: &G,
) -> (f32, Vec<f32>) {
    let results: Vec<(f32, Vec<f32>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = batch
            .iter()
            .map(|(inputs, targets)| {
                scope.spawn(move || loss_and_gradient(model, inputs, targets, gemm))
            })
            .collect();

        handles
            .into_iter()
            .map(|handle| handle.join().expect("a sequence's gradient panicked"))
            .collect()
    });

    let mut gradient = vec![0.0; parameters];
    let mut loss = 0.0;
    for (sequence_loss, sequence_gradient) in &results {
        loss += sequence_loss / batch.len() as f32;
        for (slot, value) in gradient.iter_mut().zip(sequence_gradient) {
            *slot += value / batch.len() as f32;
        }
    }

    (loss, gradient)
}

/// Mean held-out loss for `model` over `held_out`, exactly as `train` measures it.
///
/// Scoring a checkpoint after the fact — against different text, or a corpus it
/// was never trained on — is not something the training loop can do, because it
/// only ever scores the weights it is currently holding. This is that same pass,
/// exposed.
///
/// It shares `held_out_batch` and `mean_loss_and_gradient` with the loop rather
/// than reimplementing either. The discarded gradient is deliberate for the
/// reason given on `mean_loss_and_gradient`: a forward-only path would be a
/// second implementation of the forward pass, and the two disagreeing is a far
/// worse bug than the milliseconds.
///
/// The batch is a function of `config.seed`, `config.eval_batch` and
/// `config.context`, so two scores are comparable only when all three match —
/// and a score is comparable to a training run's curve only when they match it.
#[must_use]
pub fn score<G: Gemm + Sync>(
    model: &Model,
    held_out: &[u16],
    config: TrainingConfig,
    gemm: &G,
) -> f32 {
    let batch = config.held_out_batch(held_out);
    mean_loss_and_gradient(model, &batch, model.weights().len(), gemm).0
}

/// Draw `max_tokens` tokens from `model`, continuing `prompt`.
///
/// **Unconstrained** — no oracle mask. That is the whole point of the
/// grammar-learnability probe: masked output parses by construction and would
/// measure nothing about the model. See `plans/legacy/drivel.md`.
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

    /// The held-out batch is drawn once for the whole run, not per step. A
    /// held-out loss exists to be compared with the one before it, and a curve
    /// whose every point sampled different windows would move for two reasons at
    /// once — the model changing and the sample changing — which is exactly the
    /// confusion it was added to remove.
    #[test]
    fn the_held_out_batch_is_fixed_and_never_a_training_batch() {
        let tokens: Vec<u16> = (0..500u16).collect();
        let config = TrainingConfig {
            context: 8,
            batch: 4,
            eval_batch: 4,
            seed: 99,
            ..TrainingConfig::default()
        };

        let held_out = config.held_out_batch(&tokens);

        assert_eq!(held_out, config.held_out_batch(&tokens), "must not move between calls");
        assert_eq!(held_out.len(), 4, "one sequence per eval_batch slot");
        for step in 0..8 {
            assert_ne!(
                held_out,
                config.batch_at(&tokens, step),
                "held-out windows collided with training step {step}"
            );
        }
    }

    /// A tiny run: the smallest rung over a nearly-empty vocab, two steps. Enough
    /// to exercise the reporting path without paying for a real model.
    fn tiny(eval_every: usize) -> TrainingConfig {
        TrainingConfig {
            context: 8,
            batch: 1,
            eval_batch: 1,
            steps: 2,
            report_every: 1,
            eval_every,
            ..TrainingConfig::default()
        }
    }

    #[test]
    fn no_held_out_loss_is_reported_when_evaluation_is_off() {
        let tokens: Vec<u16> = (0..64u16).map(|token| token % 16).collect();
        let mut reported = Vec::new();

        train(&tokens, &tokens, 16, tiny(0), &NaiveGemm, |progress| {
            reported.push(progress.held_out_loss);
        });

        assert_eq!(reported.len(), 2, "one report per step");
        assert!(reported.iter().all(Option::is_none), "reported {reported:?} with evaluation off");
    }

    #[test]
    fn a_held_out_loss_is_reported_on_the_evaluation_cadence() {
        let tokens: Vec<u16> = (0..64u16).map(|token| token % 16).collect();
        let mut reported = Vec::new();

        train(&tokens, &tokens, 16, tiny(1), &NaiveGemm, |progress| {
            reported.push(progress.held_out_loss);
        });

        assert!(reported.iter().all(Option::is_some), "missing a held-out loss: {reported:?}");
        assert!(
            reported.iter().flatten().all(|loss| loss.is_finite() && *loss > 0.0),
            "a held-out loss should be a real positive number: {reported:?}"
        );
    }

    /// Scoring a checkpoint after the fact must return the *same number* the
    /// training loop reported for those weights — not merely a plausible one.
    ///
    /// This is the whole reason `score` exists rather than a hand-rolled
    /// forward-only loss: a second implementation that disagrees would not
    /// error, it would quietly report a different metric, and every comparison
    /// built on it would be wrong in a way nothing downstream could catch.
    ///
    /// `train` measures the held-out loss *before* the optimizer steps, so the
    /// first reported value describes the initial weights — which
    /// `pseudo_random_weights` reproduces exactly from the seed.
    #[test]
    fn scoring_a_model_returns_the_trainers_own_held_out_loss() {
        let tokens: Vec<u16> = (0..64u16).map(|token| token % 16).collect();
        let config = tiny(1);
        let mut reported = Vec::new();

        train(&tokens, &tokens, 16, config, &NaiveGemm, |progress| {
            reported.push(progress.held_out_loss);
        });

        let model_config = config.rung.config();
        let model = Model::new(
            model_config,
            16,
            pseudo_random_weights(model_config.param_count(16), config.seed),
        )
        .expect("weight count is derived from the same config");

        assert_eq!(
            score(&model, &tokens, config, &NaiveGemm),
            reported[0].expect("the first report carries a held-out loss"),
            "the scorer must be the trainer's own held-out pass, bit for bit"
        );
    }

    /// An empty held-out set is a run without a split, not a crash — and it must
    /// not report a loss it did not measure.
    #[test]
    fn an_empty_held_out_set_reports_nothing() {
        let tokens: Vec<u16> = (0..64u16).map(|token| token % 16).collect();
        let mut reported = Vec::new();

        train(&tokens, &[], 16, tiny(1), &NaiveGemm, |progress| {
            reported.push(progress.held_out_loss);
        });

        assert!(reported.iter().all(Option::is_none), "reported {reported:?} with nothing held out");
    }

    /// The curve is read by tools that split on tabs, so a row missing its
    /// held-out value must still carry the column.
    #[test]
    fn every_curve_row_has_one_field_per_header_column() {
        let columns = Progress::HEADER.split('\t').count();
        let base = Progress {
            step: 1,
            steps: 2,
            loss: 1.0,
            smoothed_loss: 1.0,
            learning_rate: 1e-3,
            gradient_norm: 0.5,
            tokens_per_second: 1.0,
            elapsed: Duration::from_secs(1),
            remaining: Duration::from_secs(1),
            held_out_loss: None,
        };

        assert_eq!(base.row().split('\t').count(), columns, "row without a held-out loss");
        assert_eq!(
            Progress { held_out_loss: Some(2.0), ..base }.row().split('\t').count(),
            columns,
            "row with a held-out loss"
        );
    }
}
