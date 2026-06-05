//! Producer/consumer workload — pure logic.
//!
//! v0.6 step 1 lands the workload on a single hart with the
//! cooperative scheduler; v0.6 steps 11-12 migrate it to two harts
//! first with `Mutex<VecDeque>` then with `heapless::spsc`. The
//! pieces in this module are the parts that don't depend on the
//! kernel's locking / scheduling — they're host-tested here so the
//! kernel-side wiring stays mechanical.

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
}
