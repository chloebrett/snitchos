// SMOKE TEST — remove once real kernel workloads drive heap metrics.
// Incrementally builds a prime-factor table: each call to `extend`
// factorizes a batch of integers and stores (n → smallest_prime_factor(n))
// in a BTreeMap. Periodic `evict_composites` frees non-prime entries,
// creating fragmentation holes and a sawtooth in `entry_count`.
//
// Pure logic: no statics, no locks, no unsafe. Tested on the host;
// driven from `kernel::heap_smoke` which holds the static and the lock.

use alloc::collections::BTreeMap;

pub struct FactorTable {
    table: BTreeMap<u64, u64>,
    next: u64,
}

impl Default for FactorTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FactorTable {
    pub fn new() -> Self {
        Self { table: BTreeMap::new(), next: 2 }
    }

    pub fn extend(&mut self, batch: usize) {
        for _ in 0..batch {
            let n = self.next;
            let spf = smallest_prime_factor(n, &self.table);
            self.table.insert(n, spf);
            self.next += 1;
        }
    }

    /// Remove all composite entries (spf ≠ n). Correctness is preserved:
    /// future `extend` calls only need primes ≤ √n, which remain.
    pub fn evict_composites(&mut self) {
        self.table.retain(|&k, v| k == *v);
    }

    pub fn entry_count(&self) -> usize {
        self.table.len()
    }

    pub fn prime_count(&self) -> usize {
        self.table.iter().filter(|&(&k, &v)| k == v).count()
    }

    pub fn next_candidate(&self) -> u64 {
        self.next
    }
}

fn smallest_prime_factor(n: u64, known: &BTreeMap<u64, u64>) -> u64 {
    for (&k, &v) in known.iter() {
        if k.checked_mul(k).is_none_or(|kk| kk > n) {
            break;
        }
        if k == v && n.is_multiple_of(k) {
            return k;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_table_is_empty() {
        let t = FactorTable::new();
        assert_eq!(t.entry_count(), 0);
        assert_eq!(t.prime_count(), 0);
        assert_eq!(t.next_candidate(), 2);
    }

    #[test]
    fn two_is_prime() {
        let mut t = FactorTable::new();
        t.extend(1);
        assert_eq!(t.entry_count(), 1);
        assert_eq!(t.prime_count(), 1);
    }

    #[test]
    fn four_has_spf_two() {
        let mut t = FactorTable::new();
        t.extend(3); // covers 2, 3, 4
        assert_eq!(t.prime_count(), 2); // 2 and 3
        assert_eq!(t.entry_count(), 3);
    }

    #[test]
    fn nine_has_spf_three() {
        let mut t = FactorTable::new();
        t.extend(8); // covers 2..=9
        // primes: 2, 3, 5, 7
        assert_eq!(t.prime_count(), 4);
        assert_eq!(t.entry_count(), 8);
    }

    #[test]
    fn next_candidate_advances_by_batch() {
        let mut t = FactorTable::new();
        t.extend(10);
        assert_eq!(t.next_candidate(), 12);
    }

    #[test]
    fn evict_removes_composites_keeps_primes() {
        let mut t = FactorTable::new();
        t.extend(9); // covers 2..=10; primes: 2,3,5,7
        assert_eq!(t.entry_count(), 9);
        t.evict_composites();
        assert_eq!(t.entry_count(), 4);
        assert_eq!(t.prime_count(), 4);
        assert_eq!(t.next_candidate(), 11); // unchanged
    }

    #[test]
    fn extend_after_evict_is_correct() {
        let mut t = FactorTable::new();
        t.extend(14); // covers 2..=15
        t.evict_composites(); // keeps 2,3,5,7,11,13
        t.extend(2); // factorize 16 and 17
        // 16 = 2*2*2*2 → spf 2; 17 is prime
        assert_eq!(t.entry_count(), 8); // 6 primes + 16 + 17
        assert_eq!(t.prime_count(), 7); // 2,3,5,7,11,13,17
    }

    #[test]
    fn prime_count_is_monotone_across_evictions() {
        let mut t = FactorTable::new();
        t.extend(50);
        let p1 = t.prime_count();
        t.evict_composites();
        assert_eq!(t.prime_count(), p1); // eviction removes only composites
        t.extend(50);
        assert!(t.prime_count() >= p1);
    }
}
