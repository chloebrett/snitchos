//! `AdamW`: Adam with **decoupled** weight decay.
//!
//! The decoupling is the whole point of the variant. Plain Adam with L2
//! regularization folds decay into the gradient, where Adam's own per-parameter
//! scaling then divides it back out unevenly — parameters with large gradient
//! history get decayed less, which is the opposite of intended. `AdamW` applies
//! decay directly to the weight, so it means the same thing everywhere.

/// Hyperparameters. Defaults are the standard ones; only `learning_rate` and
/// `weight_decay` are normally worth touching.
#[derive(Debug, Clone, Copy)]
pub struct AdamWConfig {
    pub learning_rate: f32,
    /// Decay rate of the gradient's running mean.
    pub beta1: f32,
    /// Decay rate of the running mean of *squared* gradients.
    pub beta2: f32,
    /// Guards the division when a parameter has seen no gradient.
    pub epsilon: f32,
    pub weight_decay: f32,
}

impl Default for AdamWConfig {
    fn default() -> Self {
        Self {
            learning_rate: 1e-3,
            beta1: 0.9,
            beta2: 0.999,
            epsilon: 1e-8,
            weight_decay: 0.01,
        }
    }
}

/// Per-parameter optimizer state: two running means, and the step count that
/// bias-corrects them.
pub struct AdamW {
    config: AdamWConfig,
    momentum: Vec<f32>,
    velocity: Vec<f32>,
    steps: u32,
}

impl AdamW {
    pub fn new(parameters: usize, config: AdamWConfig) -> Self {
        Self {
            config,
            momentum: vec![0.0; parameters],
            velocity: vec![0.0; parameters],
            steps: 0,
        }
    }

    /// Apply one update in place.
    ///
    /// Both running means start at zero and are therefore biased toward it for
    /// the first several steps. The corrections divide that bias out, which is
    /// what makes the very first step a full learning-rate move rather than
    /// `(1 − β₁)` of one — a slow start that otherwise reads as a badly chosen
    /// learning rate.
    pub fn step(&mut self, weights: &mut [f32], gradient: &[f32]) {
        let AdamWConfig {
            learning_rate,
            beta1,
            beta2,
            epsilon,
            weight_decay,
        } = self.config;

        self.steps += 1;
        let correct1 = 1.0 - beta1.powi(self.steps as i32);
        let correct2 = 1.0 - beta2.powi(self.steps as i32);

        for index in 0..weights.len() {
            let slope = gradient[index];

            self.momentum[index] = beta1 * self.momentum[index] + (1.0 - beta1) * slope;
            self.velocity[index] = beta2 * self.velocity[index] + (1.0 - beta2) * slope * slope;

            let mean = self.momentum[index] / correct1;
            let variance = self.velocity[index] / correct2;

            weights[index] -= learning_rate
                * (mean / (variance.sqrt() + epsilon) + weight_decay * weights[index]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimize `(w − target)²`, whose gradient is `2(w − target)`.
    ///
    /// A convex problem with a known answer: an optimizer that cannot find it
    /// is broken in a way no amount of transformer debugging would reveal.
    fn descend(steps: usize, target: f32, config: AdamWConfig) -> f32 {
        let mut weights = vec![0.0];
        let mut optimizer = AdamW::new(1, config);

        for _ in 0..steps {
            let gradient = vec![2.0 * (weights[0] - target)];
            optimizer.step(&mut weights, &gradient);
        }

        weights[0]
    }

    #[test]
    fn adamw_finds_the_minimum_of_a_convex_problem() {
        let settled = descend(
            400,
            3.0,
            AdamWConfig {
                learning_rate: 0.1,
                weight_decay: 0.0,
                ..AdamWConfig::default()
            },
        );

        assert!(
            (settled - 3.0).abs() < 1e-2,
            "expected to settle at 3.0, got {settled}"
        );
    }

    #[test]
    fn weight_decay_pulls_toward_zero_when_the_gradient_does_not() {
        // Decoupled decay is the whole point of AdamW: it shrinks weights even
        // where the loss is indifferent, which coupling it into the gradient
        // (plain Adam with L2) would not do consistently under adaptive scaling.
        let mut weights = vec![1.0];
        let mut optimizer = AdamW::new(
            1,
            AdamWConfig {
                learning_rate: 0.1,
                weight_decay: 0.5,
                ..AdamWConfig::default()
            },
        );

        optimizer.step(&mut weights, &[0.0]);

        assert!(
            weights[0] < 1.0,
            "decay should shrink an ungradiented weight, got {}",
            weights[0]
        );
    }

    #[test]
    fn bias_correction_makes_the_first_step_full_sized() {
        // Without it the first step is scaled by (1 − β₁) ≈ 0.1 and training
        // crawls out of the gate — a slow-start bug that looks like a bad
        // learning rate.
        let mut weights = vec![0.0];
        let mut optimizer = AdamW::new(
            1,
            AdamWConfig {
                learning_rate: 0.1,
                weight_decay: 0.0,
                ..AdamWConfig::default()
            },
        );

        optimizer.step(&mut weights, &[1.0]);

        assert!(
            (weights[0] + 0.1).abs() < 1e-3,
            "first step should be ~one learning rate, got {}",
            weights[0]
        );
    }
}
