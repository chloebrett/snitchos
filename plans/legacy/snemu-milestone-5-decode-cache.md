# snemu M5 — JIT Tier 1: the decode cache

**Status: increments 1–3 SHIPPED + verified.** A per-hart decode cache behind an
on/off flag, proven byte-identical to the interpreter across the taxonomy. The
measure-first chain, each step guided by the M4 spine:

| step | change | interp (off) | cache (on) |
|---|---|---|---|
| — | baseline interpreter | 18.3 | — |
| inc 1 | direct-mapped cache | 18.3 | 21.0 |
| inc 2 | drop per-instr `satp` read | 18.3 | 26.4 |
| **inc 3** | **CSR file: BTreeMap → flat array** | **41.4** | **52.8** |

**Headline: snemu 18.3 → 52.8 MIPS = 2.9× self-speedup; vs QEMU on the boot
milestone flipped from ~0.96× (parity) to ~1.6×.** The dominant cost was never
instruction decode — it was per-instruction CSR reads (`pending_interrupt` probes
`sip`/`sie`/`sstatus` every step through a `BTreeMap`). Fixing the CSR *storage*
(inc 3) helped the interpreter itself 2.25×, more than the whole decode cache.
The op pre-decode (the *original* inc-3 idea) is deferred — dispatch is cheap
jump-table matching, not the cap; revisit only if a JIT tier needs it.

Design context: the tier ladder in [docs/snemu-design.md](../../docs/snemu-design.md)
(*JIT: the tier ladder*). Measured against the M4 spine
([plans/snemu-milestone-4-measurement.md](../snemu-milestone-4-measurement.md)).

## What shipped (increment 1)

- **`snemu::decode_cache::DecodeCache`** — a per-hart cache of the fetch+expand
  result (`Decoded { raw, ilen }`) keyed by virtual PC. On a hit, `Hart::step`
  skips the whole fetch pipeline (Sv39 walk + byte read + compressed expand) and
  goes straight to `execute`. `execute` still reads live `pc`/registers, so
  behaviour is identical.
- **Behind a flag** (`Machine::set_decode_cache`, `Cpu::set_decode_cache`; default
  OFF). The interpreter stays the oracle. This is deliberate: it forces clean
  factoring and makes the fast path A/B-measurable and differentially verifiable.
- **Correctness = the guest's TLB-coherence contract.** A cached entry is a
  *translated* instruction, valid for one `satp` and until the guest invalidates
  translations. Flush on `satp` change (detected in `get`) and on `sfence.vma`
  (hooked in `priv_op`). SMC that rewrites a cached page without an `sfence` would
  go stale — the kernel/itest workloads don't, and `--verify-cache` would catch it.

## Two findings the M4 spine surfaced immediately

1. **The naive `HashMap<u64, _>` was SLOWER than the interpreter** (15 vs 18.3
   MIPS): SipHash on a PC every instruction costs more than the page walk it
   saves. Fixed with a **direct-mapped array + epoch invalidation** — index =
   `(pc >> 1) & mask`, no hashing; a slot counts only if its epoch matches, so
   `satp`/`sfence` flush is an O(1) epoch bump (crucial — boot churns these).
   Result: **21.0 vs 18.3 MIPS ≈ 1.15×** on the default boot.
2. **The win is modest (~1.1–1.15×), flat across the taxonomy.** Expected: the
   cache only skips *fetch*; `execute` still re-extracts fields and re-dispatches
   on `raw` every time. The flat profile (M4 finding) already said cost is
   dispatch-bound — which is what increment 2 attacks.

## Verification

- **Unit:** `the_decode_cache_changes_nothing_but_speed` (cpu.rs) — a toy loop run
  cache-off vs cache-on yields identical instret/pc/regs, and the fast path
  engages (hits > 0).
- **Integration:** `cargo xtask snemu-bench --verify-cache` — each taxonomy
  workload run both ways must emit **byte-identical telemetry**. Passes: demo
  (134), mutex-storm (254), heap-oom (1281), syscall-hog (2335 frames) — covering
  satp switches, userspace, storms. instret identical on every A/B bench run.

## Increment 2 (SHIPPED) — the fast path is a single array probe

Profiling the increment-1 cache pointed at a bigger, cheaper win than the planned
op pre-decode: the fast path read `satp` from the CSR file **every instruction**
(a `BTreeMap<u16,u64>` lookup) just to detect address-space changes. Removed it:

- The cache is now **satp-agnostic** — `get(pc)` is a single direct-mapped array
  probe, no CSR read. Correctness moves to **flush-on-write**: the hart flushes
  the cache when the guest writes `satp` (hook in `csr_access`) or runs
  `sfence.vma` (hook in `priv_op`). Both are rare (context switches / TLB
  invalidations), so a live slot is valid for the current address space by
  construction.
- Result: **21.0 → 26.4 MIPS** (default boot), i.e. **1.15× → 1.44×** over the
  interpreter; ~1.4× flat across the taxonomy; startup 0.123 → 0.081 s.
- Still byte-identical telemetry across the taxonomy (`--verify-cache`), and the
  new eviction/flush behaviour is unit-tested (aliasing eviction; flush-then-reuse).

## Increment 3 (SHIPPED) — CSR file: BTreeMap → flat array

Inc 2 flagged `pending_interrupt`'s per-instruction CSR reads as the next lever;
the data confirmed it emphatically. `Csr` stored 10 registers in a
`BTreeMap<u16,u64>`, and the interrupt check probes `sip`/`sie`/`sstatus` on
*every* step — tree comparisons + pointer-chasing, per instruction.

- Replaced the `BTreeMap` with a `[u64; N]` flat array indexed by a jump-table
  `Csr::slot(addr)` match. API unchanged (`read`/`write` signatures identical), so
  nothing outside `csr.rs` moved. `Clone` (the snapshot/fork primitive) is now a
  cheap array copy too.
- **Interpreter 18.3 → 41.4 MIPS (2.25×); cache 26.4 → 52.8 (2.0×).** Bigger than
  the entire decode cache — the cap was CSR *storage*, not decode.
- Guarded by a round-trip test over every modeled CSR (protects the address→slot
  map from drift/aliasing); `--verify-cache` still byte-identical across the
  taxonomy.

**The lesson (twice now):** the tier ladder predicted decode was the bottleneck;
measurement said per-instruction `BTreeMap` CSR access was. Measure-first beat the
textbook.

## Deferred — pre-decode the operation (the textbook Tier-1 finish)

Cache a dispatch-ready `Op` enum so `execute` skips the opcode `match` + field
re-extraction. Deprioritised: after inc 3, dispatch is cheap jump-table matching,
not the cap. The harness (`--verify-cache`, A/B bench) is in place if a future
tier makes it worthwhile.

## CLI

- `cargo xtask snemu-bench --decode-cache` — measure with the cache on (A/B vs the
  default off). Composes with `--taxonomy` / `--baseline`.
- `cargo xtask snemu-bench --verify-cache` — the faithfulness gate.
