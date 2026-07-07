//! Tier-1 JIT — a per-hart **decode cache** (snemu milestone M5).
//!
//! The pure interpreter re-does the full fetch pipeline for *every* executed
//! instruction: a Sv39 page walk to translate the PC, a byte fetch, and (for the
//! compressed set) an expansion — before it even dispatches. In a loop that runs
//! the same instructions millions of times, that work is pure waste. This cache
//! stores the fetch+expand result keyed by virtual PC, so a re-executed
//! instruction skips straight to dispatch.
//!
//! **Data, not code:** the cache is a plain map of decoded structs — no generated
//! machine code, no executable memory. So it stays portable, `no_std`-friendly in
//! spirit, and nests. It changes only *how fast* an instruction runs, never
//! *what* runs, so instret and telemetry are byte-identical to the interpreter —
//! which is why snemu keeps it behind a flag (the interpreter is the oracle) and
//! a differential check asserts the equivalence.
//!
//! **Correctness rides the guest's own TLB-coherence contract.** A cached entry
//! is a *translated* instruction, valid only while its translation is: for one
//! address space (`satp`) and until the guest invalidates translations. A `satp`
//! change flushes ([`get`](DecodeCache::get) detects it); an `sfence.vma` flushes
//! explicitly ([`flush`](DecodeCache::flush)). Self-modifying code that rewrites
//! an already-cached page without an `sfence` would go stale — not something the
//! kernel/itest workloads do, and the differential check would catch it.

/// Direct-mapped cache index bits: 2^15 = 32768 slots. Sized so the kernel's hot
/// code (a few thousand instructions) rarely collides, while a full cache clones
/// cheaply for the snapshot/fork harness. Indexed by `(pc >> 1)` — instructions
/// are ≥2-byte aligned, so the low bit carries no information.
const INDEX_BITS: u32 = 15;
const SLOTS: usize = 1 << INDEX_BITS;
const INDEX_MASK: u64 = (SLOTS as u64) - 1;

/// A decoded instruction ready for the executor: the 32-bit form it consumes
/// (compressed instructions stored already-expanded, as the interpreter runs
/// them) plus the length to advance the PC by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Decoded {
    pub raw: u32,
    pub ilen: u64,
}

/// One direct-mapped slot: the full PC as a tag (to detect the aliasing two PCs
/// that share an index), the epoch it was written in (for O(1) flush), and the
/// decoded instruction.
#[derive(Clone, Copy, Default)]
struct Slot {
    epoch: u64,
    tag: u64,
    decoded: Decoded,
}

/// A per-hart decode cache — a **direct-mapped array**, not a hash map: a PC
/// indexes straight into `slots` with a shift+mask (no hashing, which measured
/// *slower* than the page walk it was meant to save). Valid for one address
/// space (`satp`) and until the guest invalidates translations; both flush via
/// an **epoch bump** (O(1) — a slot counts only if its epoch matches the current
/// one), so the frequent boot-time `sfence`/`satp` churn stays cheap. Tracks
/// hit/miss counts for the hot-block metrics (M4 step 5) and to prove the fast
/// path engaged.
#[derive(Clone)]
pub(crate) struct DecodeCache {
    /// The address space the live entries belong to; a change bumps `epoch`.
    satp: u64,
    /// Slots written with an earlier epoch are stale. Starts at 1 so the
    /// zero-initialised slots (epoch 0) are invalid from birth.
    epoch: u64,
    slots: Box<[Slot]>,
    hits: u64,
    misses: u64,
}

impl DecodeCache {
    pub(crate) fn new() -> Self {
        Self {
            satp: 0,
            epoch: 1,
            slots: vec![Slot::default(); SLOTS].into_boxed_slice(),
            hits: 0,
            misses: 0,
        }
    }

    #[inline]
    fn index(pc: u64) -> usize {
        ((pc >> 1) & INDEX_MASK) as usize
    }

    /// Look up `pc` in address space `satp`. A `satp` change flushes first (the
    /// slots map different physical code). Returns the cached [`Decoded`] on a
    /// hit — the slot's epoch is current AND its tag matches this exact PC (not an
    /// aliasing neighbour) — bumping hits; else `None`, bumping misses, and the
    /// caller does the slow fetch+expand and [`insert`](Self::insert)s.
    pub(crate) fn get(&mut self, satp: u64, pc: u64) -> Option<Decoded> {
        if satp != self.satp {
            self.satp = satp;
            self.epoch += 1;
        }
        let slot = &self.slots[Self::index(pc)];
        if slot.epoch == self.epoch && slot.tag == pc {
            self.hits += 1;
            Some(slot.decoded)
        } else {
            self.misses += 1;
            None
        }
    }

    /// Record the slow-path decode of `pc`, evicting whatever aliased its slot.
    pub(crate) fn insert(&mut self, pc: u64, decoded: Decoded) {
        let epoch = self.epoch;
        self.slots[Self::index(pc)] = Slot { epoch, tag: pc, decoded };
    }

    /// Invalidate every slot in O(1) — the guest invalidated translations
    /// (`sfence.vma`). The `satp` marker is left as-is; the next `get` re-checks
    /// it.
    pub(crate) fn flush(&mut self) {
        self.epoch += 1;
    }

    #[cfg(test)]
    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }
}

impl Default for DecodeCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeCache, Decoded};

    const A: Decoded = Decoded { raw: 0x0000_0013, ilen: 4 }; // nop (addi x0,x0,0)
    const B: Decoded = Decoded { raw: 0x0010_0093, ilen: 4 }; // addi x1,x0,1

    #[test]
    fn a_miss_then_insert_then_hit_returns_the_decoded_instruction() {
        let mut cache = DecodeCache::default();
        assert_eq!(cache.get(0, 0x1000), None, "cold lookup misses");
        cache.insert(0x1000, A);
        assert_eq!(cache.get(0, 0x1000), Some(A), "warm lookup hits");
    }

    #[test]
    fn a_satp_change_flushes_stale_entries() {
        // Entries belong to one address space. Switching satp (a context switch)
        // must not return the previous space's code for the same VA.
        let mut cache = DecodeCache::default();
        cache.insert(0x1000, A); // satp still 0 (its default)
        assert_eq!(cache.get(0, 0x1000), Some(A));
        assert_eq!(cache.get(1, 0x1000), None, "new satp sees no stale entry");
        // ...and the old entry is gone even back under satp 0.
        assert_eq!(cache.get(0, 0x1000), None);
    }

    #[test]
    fn an_sfence_flush_drops_entries_but_keeps_the_address_space() {
        // sfence.vma invalidates translations in the *current* space; entries go,
        // but a subsequent same-satp lookup shouldn't be treated as a space change.
        let mut cache = DecodeCache::default();
        let _ = cache.get(7, 0x2000); // establish satp=7
        cache.insert(0x2000, B);
        assert_eq!(cache.get(7, 0x2000), Some(B));
        cache.flush();
        assert_eq!(cache.get(7, 0x2000), None, "flushed");
    }

    #[test]
    fn hits_count_only_the_fast_path() {
        let mut cache = DecodeCache::default();
        let _ = cache.get(0, 0x1000); // miss
        cache.insert(0x1000, A);
        let _ = cache.get(0, 0x1000); // hit
        let _ = cache.get(0, 0x1000); // hit
        assert_eq!(cache.hits(), 2);
    }
}
