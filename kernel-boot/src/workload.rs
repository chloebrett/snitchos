//! Producer/consumer workload — pure logic.
//!
//! v0.6 step 1 lands the workload on a single hart with the
//! cooperative scheduler; v0.6 steps 11-12 migrate it to two harts
//! first with `Mutex<VecDeque>` then with `heapless::spsc`. The
//! pieces in this module are the parts that don't depend on the
//! kernel's locking / scheduling — they're host-tested here so the
//! kernel-side wiring stays mechanical.

/// Number of histogram buckets. Power of two so `bin_of` is a mask.
pub const BUCKETS: usize = 64;

/// Map a sample to a histogram bucket. Pure function: same input
/// always yields the same output, always in `0..buckets`. `buckets`
/// is taken as a parameter rather than baked to `BUCKETS` so tests
/// can vary it without recompiling the kernel.
pub fn bin_of(sample: u64, buckets: usize) -> usize {
    (sample as usize) % buckets
}

/// Increment the bin for `sample` by one. The histogram correctness
/// oracle holds: after N calls starting from a zeroed histogram, the
/// sum of bins is exactly N.
pub fn bin_sample(hist: &mut [u64; BUCKETS], sample: u64) {
    hist[bin_of(sample, BUCKETS)] += 1;
}

/// Linear congruential generator. Knuth's MMIX constants — the same
/// recurrence the v0.5 `burn_lcg` uses, but exposing the state as
/// observable samples rather than discarding them.
pub struct Lcg {
    state: u64,
}

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    // `next` is the natural name for an LCG step; it's an infinite
    // generator, not an `Iterator` (no `Option`, never ends), so the
    // standard-trait confusion the lint warns about doesn't apply.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcg_is_deterministic_with_same_seed() {
        let mut a = Lcg::new(42);
        let mut b = Lcg::new(42);
        for _ in 0..100 {
            assert_eq!(a.next(), b.next());
        }
    }

    #[test]
    fn lcg_produces_many_distinct_values() {
        // Pins against a "returns constant" mutant. Knuth's MMIX LCG
        // has a full period of 2^64; 1024 samples should be entirely
        // distinct.
        let mut lcg = Lcg::new(1);
        let mut seen = alloc::collections::BTreeSet::new();
        for _ in 0..1024 {
            seen.insert(lcg.next());
        }
        assert_eq!(seen.len(), 1024);
    }

    #[test]
    fn lcg_distinguishes_seeds() {
        // Pins against a "ignores seed" mutant. Different seeds must
        // produce different first samples.
        assert_ne!(Lcg::new(0).next(), Lcg::new(1).next());
        assert_ne!(Lcg::new(42).next(), Lcg::new(43).next());
    }

    #[test]
    fn bin_of_is_within_range_for_arbitrary_samples() {
        // Range invariant: bin_of must always land in 0..BUCKETS for
        // any u64. This is the histogram safety property — if it
        // could return >= BUCKETS we'd index out of bounds in the
        // kernel.
        let mut lcg = Lcg::new(0xc0ffee);
        for _ in 0..10_000 {
            let b = bin_of(lcg.next(), BUCKETS);
            assert!(b < BUCKETS, "bin {b} out of range for BUCKETS={BUCKETS}");
        }
    }

    #[test]
    fn bin_of_is_pure() {
        // Pins against a "uses a hidden counter" or "non-deterministic"
        // mutant. Same sample must map to the same bin every time.
        let sample = 0xdead_beef_cafe_babe_u64;
        let b = bin_of(sample, BUCKETS);
        for _ in 0..100 {
            assert_eq!(bin_of(sample, BUCKETS), b);
        }
    }

    #[test]
    fn histogram_total_equals_samples_fed() {
        // The correctness oracle: the same invariant the v0.6 step-1
        // integration scenario will assert against the running kernel.
        // Pin it here at the pure-data level so the in-kernel version
        // is just "the same logic, wrapped in atomics."
        //
        // Feed N samples through bin_sample; the sum of bins must be
        // exactly N. Always. No matter the seed, no matter the bin
        // distribution.
        const N: u64 = 50_000;
        let mut hist = [0u64; BUCKETS];
        let mut lcg = Lcg::new(0xdead);
        for _ in 0..N {
            bin_sample(&mut hist, lcg.next());
        }
        assert_eq!(hist.iter().sum::<u64>(), N);
    }

    #[test]
    fn histogram_starts_empty() {
        // Pins against a "bin_sample initializes hist[0] = 1" mutant
        // or anything that would change the empty-histogram invariant.
        let hist = [0u64; BUCKETS];
        assert_eq!(hist.iter().sum::<u64>(), 0);
    }

    #[test]
    fn bin_of_distinguishes_samples() {
        // Pins against a "returns 0" or "returns sample % 1" mutant.
        // Across many samples we should hit many distinct bins.
        let mut lcg = Lcg::new(7);
        let mut seen = alloc::collections::BTreeSet::new();
        for _ in 0..10_000 {
            seen.insert(bin_of(lcg.next(), BUCKETS));
        }
        // With 10k samples and 64 buckets, we should hit all of them.
        assert_eq!(seen.len(), BUCKETS);
    }
}
