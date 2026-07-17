//! Guest physical memory: a flat byte array at the QEMU `virt` RAM base,
//! with width-typed little-endian accessors and out-of-range detection.

/// Physical base of guest RAM on the QEMU `virt` machine.
pub const RAM_BASE: u64 = 0x8000_0000;

/// Guest page size (Sv39 base page).
const PAGE: usize = 0x1000;

/// A guest physical access fell outside mapped RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusError {
    OutOfRange { addr: u64 },
}

/// Flat guest RAM, addressed by absolute guest physical address.
#[derive(Clone)]
pub struct Memory {
    ram: Vec<u8>,
    /// Highest byte offset (past-the-end) ever written, for right-sizing the machine:
    /// it's the guest's RAM footprint (the smallest RAM that would still fit it).
    /// Reset after the ELF/DTB load so it tracks only *guest execution* writes, not
    /// the loader placing the image (which puts the DTB near the top).
    high_water: u64,
    /// Debug/stress mode: a **deterministic** frame permutation. When `Some(k)`, a
    /// guest physical frame `f` is stored at physical frame `(f * k) mod N` — the
    /// guest is oblivious (every access is remapped uniformly through [`span`] and
    /// [`write_bytes`]), *except* that a width-typed access straddling a page
    /// boundary reads its upper half from the physically-next storage frame, which
    /// is no longer the guest's next frame. That makes "physically contiguous"
    /// almost never true, so it forces the page-straddle fetch/load hazard to fire
    /// on every straddling access instead of only when the guest allocator happens
    /// to fragment. `k` is coprime to `N` (a bijection) and fixed per size (no RNG),
    /// so runs stay deterministic. Off by default; opt in with `SNEMU_SCRAMBLE_FRAMES`.
    scramble: Option<u64>,
}

/// Greatest common divisor (Euclid), for choosing a frame multiplier coprime to N.
const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A frame multiplier for an `n`-frame machine: odd and coprime to `n` (so `f -> f*k
/// mod n` is a bijection), near `n/2` so adjacent guest frames land far apart in
/// storage. Deterministic in `n`. `None` when there's nothing to scramble (`n <= 2`).
fn scramble_multiplier(n: u64) -> Option<u64> {
    if n <= 2 {
        return None;
    }
    let mut k = (n / 2) | 1; // odd, mid-range for wide scatter
    while gcd(k, n) != 1 {
        k += 2; // stay odd; coprimes are dense, so this converges immediately
    }
    Some(k)
}

/// Generates a little-endian read/write pair backed by [`Memory::span`].
macro_rules! accessors {
    ($read:ident, $write:ident, $ty:ty, $len:literal) => {
        pub fn $read(&self, addr: u64) -> Result<$ty, BusError> {
            let span = self.span(addr, $len)?;
            // span() guarantees exactly $len bytes, so the conversion is total.
            let bytes = <[u8; $len]>::try_from(&self.ram[span]).unwrap();
            Ok(<$ty>::from_le_bytes(bytes))
        }

        pub fn $write(&mut self, addr: u64, value: $ty) -> Result<(), BusError> {
            let span = self.span(addr, $len)?;
            self.high_water = self.high_water.max(span.end as u64);
            self.ram[span].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }
    };
}

impl Memory {
    /// Fold guest RAM into `h` for the machine state hash. Only the written region
    /// `[0, high_water)` is hashed — everything above is zero by construction, so
    /// this captures all non-zero RAM at a fraction of the cost of the full array
    /// (the guest footprint is a few MiB of a much larger machine). `high_water`
    /// itself is folded in so a machine that merely *touched* a higher address (then
    /// zeroed it) still differs from one that never did.
    pub(crate) fn hash_state(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.high_water.hash(h);
        let written = (self.high_water as usize).min(self.ram.len());
        self.ram[..written].hash(h);
    }

    #[must_use]
    pub fn new(size: usize) -> Self {
        Self { ram: vec![0; size], high_water: 0, scramble: None }
    }

    /// Enable/disable the deterministic frame-scramble stress mode (see the
    /// [`scramble`](Self::scramble) field). Set it **before** loading the guest
    /// image so the loader's writes are scattered consistently.
    pub fn set_scramble(&mut self, on: bool) {
        self.scramble = on.then(|| scramble_multiplier((self.ram.len() / PAGE) as u64)).flatten();
    }

    /// Map a raw RAM offset (guest physical minus [`RAM_BASE`]) to a storage offset,
    /// applying the frame permutation when scramble is on. The page offset is
    /// preserved; only the frame index is permuted — so a width-typed access still
    /// reads *contiguous* storage bytes (which is exactly what surfaces the
    /// page-straddle hazard: the upper half of a boundary-crossing access lands in
    /// `permute(f)+1`, not the guest's `permute(f+1)`).
    fn storage_offset(&self, raw: usize) -> usize {
        match self.scramble {
            None => raw,
            Some(k) => {
                let n = (self.ram.len() / PAGE) as u64;
                let sframe = ((raw / PAGE) as u64).wrapping_mul(k) % n;
                sframe as usize * PAGE + raw % PAGE
            }
        }
    }

    /// Copy `bytes` into RAM starting at `addr` (the ELF loader, or a guest bulk
    /// write like a native `memset`/virtio DMA). Under scramble this splits at guest
    /// page boundaries so each guest frame lands in **its own** permuted storage
    /// frame — a single contiguous copy would scatter a multi-page blob wrong and
    /// corrupt the guest image for reasons unrelated to the straddle hazard.
    pub(crate) fn write_bytes(&mut self, addr: u64, bytes: &[u8]) -> Result<(), BusError> {
        if self.scramble.is_none() {
            let span = self.span(addr, bytes.len())?;
            self.high_water = self.high_water.max(span.end as u64);
            self.ram[span].copy_from_slice(bytes);
            return Ok(());
        }
        let mut done = 0;
        while done < bytes.len() {
            let a = addr + done as u64;
            let chunk = (PAGE - a as usize % PAGE).min(bytes.len() - done);
            let span = self.span(a, chunk)?; // chunk stays within one guest page
            self.high_water = self.high_water.max(span.end as u64);
            self.ram[span].copy_from_slice(&bytes[done..done + chunk]);
            done += chunk;
        }
        Ok(())
    }

    /// Guest RAM footprint: the highest byte offset ever written (past-the-end),
    /// i.e. the smallest machine that would still hold everything the guest touched.
    #[must_use]
    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    /// Reset the write high-water — called after the ELF/DTB load so the mark tracks
    /// only guest-execution writes (the loader placed the DTB near the top of RAM).
    pub(crate) fn reset_high_water(&mut self) {
        self.high_water = 0;
    }

    accessors!(read_u8, write_u8, u8, 1);
    accessors!(read_u16, write_u16, u16, 2);
    accessors!(read_u32, write_u32, u32, 4);
    accessors!(read_u64, write_u64, u64, 8);

    /// The byte range for a `len`-wide access at `addr`, or `OutOfRange` if it
    /// would fall outside mapped RAM. Guest-visible bounds are checked on the *raw*
    /// address (so scramble can't change what the guest sees as mapped); the
    /// returned range is in *storage* space, permuted when scramble is on. The base
    /// frame is permuted once and the range stays contiguous — a width-typed access
    /// that crosses a page boundary therefore reads its tail from the wrong storage
    /// frame, which is the whole point of the mode.
    fn span(&self, addr: u64, len: usize) -> Result<std::ops::Range<usize>, BusError> {
        let raw = usize::try_from(addr.wrapping_sub(RAM_BASE))
            .ok()
            .filter(|_| addr >= RAM_BASE)
            .ok_or(BusError::OutOfRange { addr })?;
        let raw_end = raw.checked_add(len).ok_or(BusError::OutOfRange { addr })?;
        if raw_end > self.ram.len() {
            return Err(BusError::OutOfRange { addr });
        }
        let start = self.storage_offset(raw);
        let end = start.checked_add(len).ok_or(BusError::OutOfRange { addr })?;
        if end > self.ram.len() {
            return Err(BusError::OutOfRange { addr });
        }
        Ok(start..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_then_reads_a_byte_at_the_ram_base() {
        let mut mem = Memory::new(0x1000);
        mem.write_u8(RAM_BASE, 0xab).unwrap();
        assert_eq!(mem.read_u8(RAM_BASE).unwrap(), 0xab);
    }

    #[test]
    fn multi_width_round_trips() {
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x1234).unwrap();
        assert_eq!(mem.read_u16(RAM_BASE).unwrap(), 0x1234);
        mem.write_u32(RAM_BASE, 0xdead_beef).unwrap();
        assert_eq!(mem.read_u32(RAM_BASE).unwrap(), 0xdead_beef);
        mem.write_u64(RAM_BASE, 0x0123_4567_89ab_cdef).unwrap();
        assert_eq!(mem.read_u64(RAM_BASE).unwrap(), 0x0123_4567_89ab_cdef);
    }

    #[test]
    fn out_of_range_access_is_an_error() {
        let mut mem = Memory::new(0x1000);
        let below = RAM_BASE - 1;
        let past_end = RAM_BASE + 0x1000;
        assert_eq!(mem.read_u8(below), Err(BusError::OutOfRange { addr: below }));
        assert_eq!(
            mem.write_u8(past_end, 0),
            Err(BusError::OutOfRange { addr: past_end })
        );
        // A width that starts in range but straddles the end is rejected whole.
        let straddle = RAM_BASE + 0x1000 - 2;
        assert_eq!(
            mem.read_u32(straddle),
            Err(BusError::OutOfRange { addr: straddle })
        );
    }

    #[test]
    fn stores_little_endian() {
        let mut mem = Memory::new(0x1000);
        mem.write_u32(RAM_BASE, 0xdead_beef).unwrap();
        assert_eq!(mem.read_u8(RAM_BASE).unwrap(), 0xef);
        assert_eq!(mem.read_u8(RAM_BASE + 1).unwrap(), 0xbe);
        assert_eq!(mem.read_u8(RAM_BASE + 2).unwrap(), 0xad);
        assert_eq!(mem.read_u8(RAM_BASE + 3).unwrap(), 0xde);
    }

    const PAGE_U64: u64 = PAGE as u64;

    /// The scramble permutation is a **bijection**: no two guest frames alias the
    /// same storage frame, so distinct per-frame writes all survive. (If two frames
    /// collided, a later write would clobber an earlier one and the read-back would
    /// mismatch.) This is what keeps the guest oblivious to the remapping.
    #[test]
    fn scramble_is_a_bijection_over_frames() {
        let mut mem = Memory::new(16 * PAGE);
        mem.set_scramble(true);
        for f in 0..16u64 {
            mem.write_u8(RAM_BASE + f * PAGE_U64, f as u8 + 1).unwrap();
        }
        for f in 0..16u64 {
            assert_eq!(mem.read_u8(RAM_BASE + f * PAGE_U64).unwrap(), f as u8 + 1);
        }
    }

    /// A multi-page bulk `write_bytes` (the ELF loader path) followed by per-frame
    /// aligned reads round-trips under scramble — proving bulk writes scatter
    /// **per page**, so the guest image loads correctly. Without this, scramble
    /// would corrupt the image and any failure would be a loader artifact, not the
    /// straddle hazard we want to expose.
    #[test]
    fn scramble_preserves_multipage_bulk_write() {
        let mut mem = Memory::new(16 * PAGE);
        mem.set_scramble(true);
        let mut blob = vec![0u8; 8 * PAGE];
        for f in 0..8 {
            blob[f * PAGE] = f as u8 + 1; // tag each page with its frame index
        }
        mem.write_bytes(RAM_BASE, &blob).unwrap();
        for f in 0..8u64 {
            assert_eq!(mem.read_u8(RAM_BASE + f * PAGE_U64).unwrap(), f as u8 + 1);
        }
    }

    /// Determinism (the property the whole snapshot/oracle discipline rests on):
    /// two machines of the same size get the *same* permutation, and a clone
    /// carries it — no RNG, no host-state dependence.
    #[test]
    fn scramble_is_deterministic_and_clones() {
        let mut a = Memory::new(64 * PAGE);
        a.set_scramble(true);
        a.write_u32(RAM_BASE + 5 * PAGE_U64, 0xcafe_f00d).unwrap();
        let mut b = Memory::new(64 * PAGE);
        b.set_scramble(true);
        b.write_u32(RAM_BASE + 5 * PAGE_U64, 0xcafe_f00d).unwrap();
        assert_eq!(a.read_u32(RAM_BASE + 5 * PAGE_U64).unwrap(), 0xcafe_f00d);
        let c = a.clone();
        assert_eq!(c.read_u32(RAM_BASE + 5 * PAGE_U64).unwrap(), 0xcafe_f00d);
    }
}
