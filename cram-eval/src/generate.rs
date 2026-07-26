//! The generative half of the eval: sample from a rung, then ask the real
//! parser what came out.
//!
//! Separate from [`Predictor`](crate::Predictor) on purpose. Scoring needs a
//! distribution and generation needs a sampler, and only the first is the gate —
//! keeping them apart means a rung that can be scored but not sampled (or the
//! reverse) is representable rather than a hole in one trait.

/// A rung that can produce a program.
pub trait Generator {
    fn name(&self) -> &str;

    /// One program, reproducible from `seed`.
    fn sample(&self, seed: u64) -> String;

    /// Does this rung stop when it runs out of token budget, rather than when
    /// the program is finished?
    ///
    /// This is what makes "as sampled" and "complete items" two different
    /// numbers. A model asked for 96 tokens usually stops mid-construct, so
    /// judging its raw output conflates the harness's budget with the model's
    /// competence — hence the second cut. babble stops *at* a program boundary
    /// by construction, so cutting its output back would chop a legal program's
    /// last line and report a false failure.
    fn stops_at_budget(&self) -> bool {
        false
    }
}

/// babble generating whole programs — the corpus hat.
pub struct Babble;

impl Generator for Babble {
    fn name(&self) -> &str {
        "babble"
    }

    fn sample(&self, seed: u64) -> String {
        babble::generate(seed)
    }
}

/// How much of what a rung generated is legal Stitch.
///
/// **Not a babble comparison.** babble is 100% by construction — the mask
/// guarantees it — so a trained rung can only tie or lose here. The number is
/// meaningful against *other trained rungs* measured the same way, and as
/// evidence that a model learned something structural at all.
pub struct ParseRate {
    pub rung: String,
    pub samples: usize,
    /// Parsed exactly as sampled. A sample cut off mid-construct counts as a
    /// failure here.
    pub as_sampled: usize,
    /// Parsed after cutting back to the last complete top-level item. The gap
    /// between the two is the cost of the token budget, not a property of the
    /// model — so both are reported and neither can be quoted as the other.
    pub complete_items: usize,
    /// Actual output, kept beside the number.
    ///
    /// Not decoration. A bare `0.0%` once sent someone hunting through a
    /// backward pass that was correct all along; three samples showed the
    /// corpus separator in the output and turned it into an obvious bug in
    /// seconds. Every eval this ladder grows prints samples beside its number.
    pub examples: Vec<Example>,
}

pub struct Example {
    pub seed: u64,
    pub text: String,
    pub parses: bool,
}

impl ParseRate {
    #[must_use]
    pub fn as_sampled_pct(&self) -> f64 {
        percent(self.as_sampled, self.samples)
    }

    #[must_use]
    pub fn complete_items_pct(&self) -> f64 {
        percent(self.complete_items, self.samples)
    }
}

fn percent(count: usize, total: usize) -> f64 {
    if total == 0 { 0.0 } else { 100.0 * count as f64 / total as f64 }
}

/// How many examples a report carries. Three was enough to diagnose the
/// separator bug at a glance; more is scrolling.
const EXAMPLES: usize = 3;

/// Sample `count` programs and measure how many parse.
#[must_use]
pub fn parse_rate<G: Generator + ?Sized>(rung: &G, count: usize) -> ParseRate {
    let mut as_sampled = 0;
    let mut complete_items = 0;
    let mut examples = Vec::new();

    for seed in 0..count as u64 {
        let text = rung.sample(seed);

        let (sampled_ok, whole_ok, shown) = if rung.stops_at_budget() {
            // Exactly the two cuts `parse-rate` used, so the numbers already
            // recorded for drivel (85.0% / 91.0%) stay comparable to anything
            // measured from here on.
            let raw = text.rsplit_once('\n').map_or(text.as_str(), |(head, _)| head);
            let whole = text.rsplit_once("\n\n").map_or(raw, |(head, _)| head).trim_end();
            (
                stitch::parser::parse_program(raw).is_ok(),
                !whole.is_empty() && stitch::parser::parse_program(whole).is_ok(),
                raw.to_string(),
            )
        } else {
            // A rung that finishes what it starts is judged on what it emitted.
            let ok = stitch::parser::parse_program(&text).is_ok();
            (ok, ok, text)
        };

        as_sampled += usize::from(sampled_ok);
        complete_items += usize::from(whole_ok);

        if examples.len() < EXAMPLES {
            examples.push(Example { seed, text: shown, parses: sampled_ok });
        }
    }

    ParseRate {
        rung: rung.name().to_string(),
        samples: count,
        as_sampled,
        complete_items,
        examples,
    }
}
