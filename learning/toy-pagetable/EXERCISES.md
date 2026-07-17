# toy-pagetable — exercises

Implement the Sv39 page-table walk. These build on each other, so do them
**in order** — you can't translate without splitting the VA first.

```bash
cd learning
cargo test -p toy-pagetable
cargo test -p toy-pagetable split_va     # just exercise 1
cargo test -p toy-pagetable translate    # just exercise 2
cargo test -p toy-pagetable map_         # just exercise 3
```

Maps to **`kernel-mem/src/mmu.rs`** — same bit layouts, same walk; only the
backing store differs (a `Vec` of tables instead of real frames).

---

## Exercise 1 — `split_va` (the 9/9/9/12 carve)

**Concept.** An Sv39 VA is three 9-bit table indices plus a 12-bit page offset.
9 bits because each table has 512 = 2⁹ entries; 12 bits because a page is
4 KiB = 2¹². 3 × 9 + 12 = 39 → "Sv39".

**Maps to:** `split_va` in `kernel-mem/src/mmu.rs` (identical).

**Done when:** the three `split_va_*` tests pass.

---

## Exercise 2 — `translate` (the READ walk) ★ the centerpiece

**Concept.** This is what the hardware MMU does on *every* memory access, and
the kernel never writes it (it relies on hardware). Start at the root, index by
VPN[2], follow the branch to the mid table, index by VPN[1], and so on until you
hit a **leaf**.

The subtle part — and the thing you were fuzzy on in the quiz — is that **a leaf
can appear at any level**, giving a huge page. How many low VA bits pass through
unchanged depends on *where* the leaf was found:

| Leaf at level | Page size | Offset bits | Mask |
|---|---|---|---|
| 0 | 4 KiB | 12 | `0xfff` |
| 1 | 2 MiB | 21 | `0x1f_ffff` |
| 2 | 1 GiB | 30 | `0x3fff_ffff` |

i.e. `offset_mask = (1 << (12 + 9 * level)) - 1`, and the answer is
`pte_addr(leaf) | (va & offset_mask)`. An invalid PTE anywhere before a leaf is
a **page fault** → `None`.

**Why it matters here:** the 1 GiB leaf (level 2) is exactly how the kernel's
linear map covers all of RAM with a single root PTE — `LINEAR_OFFSET` in
`kernel-mem/src/mmu.rs`. Implementing this is the "aha" for that design.

**Done when:** all five `translate_*` tests pass (4 KiB, 2 MiB, 1 GiB, and two
fault cases).

---

## Exercise 3 — `Mem::map_4kib` (the WRITE walk)

**Concept.** The inverse of translate: install a 4 KiB leaf, allocating
intermediate tables on the way down. This is the part the kernel *does* own.

Walk vpn2 then vpn1, and at each upper level:
- **branch** → descend into `pte_addr(pte)`.
- **leaf** → a huge page is in the way → `Err(AlreadyMapped)`.
- **invalid** → `alloc_table()` (→ `Err(OutOfFrames)` if `None`), write a
  `branch_pte` into the slot, descend into the new table.

At level 0: if the slot is already valid → `Err(AlreadyMapped)`; else write
`leaf_pte(pa, perms)`.

**Maps to:** `map` + `walk_or_install` in `kernel-mem/src/mmu.rs`. Note the
real one (and this) does **not** unwind partially-installed tables on
`OutOfFrames` — a documented simplification.

**Done when:** the `map_*` tests pass — and then the two payoff tests go green:
`map_then_translate_round_trips` and the proptest
`map_then_translate_round_trips_for_any_page_and_offset`, which exercises the
whole 39-bit VA space and shrinks any failure to a minimal `(va, pa, offset)`.

---

## Stretch goals (no tests)

1. **`map` huge pages.** Add `map_1gib(va, pa, perms)` that writes a leaf at
   level 2 (root) directly — the linear-map primitive. Then `translate` already
   handles reading it back.
2. **Permission-aware translate.** Make `translate` take an access kind
   (read/write/execute) and fault if the leaf lacks the matching R/W/X bit —
   what the hardware actually does, and the basis of W^X and user/supervisor
   protection.
3. **`unmap`.** Clear a leaf and, if its table becomes empty, free the table and
   clear the parent branch — the GC problem real kernels handle (and SnitchOS
   defers).
