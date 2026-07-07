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
/// *slower* than the page walk it was meant to save). A cached entry is a
/// translated instruction, valid until the guest changes translations; the hart
/// flushes on a `satp` write and on `sfence.vma`, both via an O(1) **epoch bump**
/// (a slot counts only if its epoch matches the current one). Keeping `satp`
/// invalidation out of the lookup means the fast path never re-reads the CSR file
/// — the hot path is a single array probe. Tracks hit/miss counts for the
/// hot-block metrics (M4 step 5) and to prove the fast path engaged.
#[derive(Clone)]
pub(crate) struct DecodeCache {
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

    /// Look up `pc`. Returns the cached [`Decoded`] on a hit — the slot's epoch is
    /// current AND its tag matches this exact PC (not an aliasing neighbour that
    /// shares its index) — bumping hits; else `None`, bumping misses, and the
    /// caller does the slow fetch+expand and [`insert`](Self::insert)s. No `satp`
    /// check here: the hart flushes on any translation change, so a live slot is
    /// by construction valid for the current address space.
    pub(crate) fn get(&mut self, pc: u64) -> Option<Decoded> {
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
        assert_eq!(cache.get(0x1000), None, "cold lookup misses");
        cache.insert(0x1000, A);
        assert_eq!(cache.get(0x1000), Some(A), "warm lookup hits");
    }

    #[test]
    fn a_flush_drops_every_entry() {
        // The invalidation hook (satp write / sfence.vma): after a flush, a
        // previously-warm PC misses.
        let mut cache = DecodeCache::default();
        cache.insert(0x2000, B);
        assert_eq!(cache.get(0x2000), Some(B));
        cache.flush();
        assert_eq!(cache.get(0x2000), None, "flushed");
        // ...and the cache is usable again afterwards (new epoch, fresh inserts).
        cache.insert(0x2000, A);
        assert_eq!(cache.get(0x2000), Some(A));
    }

    #[test]
    fn two_pcs_sharing_a_slot_evict_each_other() {
        // Direct-mapped: PCs that land on the same index alias. The tag check
        // means the evicted one misses (never returns the wrong instruction) and
        // the resident one hits. `SLOTS << 1` in PC space is exactly one index
        // period apart.
        let mut cache = DecodeCache::default();
        let p1 = 0x8000;
        let p2 = p1 + ((super::SLOTS as u64) << 1); // same index, different tag
        cache.insert(p1, A);
        assert_eq!(cache.get(p1), Some(A));
        cache.insert(p2, B); // aliases p1's slot, evicting it
        assert_eq!(cache.get(p2), Some(B), "resident hits");
        assert_eq!(cache.get(p1), None, "evicted neighbour misses, never mis-hits");
    }

    #[test]
    fn hits_count_only_the_fast_path() {
        let mut cache = DecodeCache::default();
        let _ = cache.get(0x1000); // miss
        cache.insert(0x1000, A);
        let _ = cache.get(0x1000); // hit
        let _ = cache.get(0x1000); // hit
        assert_eq!(cache.hits(), 2);
    }
}
