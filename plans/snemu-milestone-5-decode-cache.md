# snemu M5 — JIT Tier 1: the decode cache

**Status: increment 1 SHIPPED + verified.** A per-hart decode cache behind an
on/off flag, proven byte-identical to the interpreter across the taxonomy, giving
~1.1–1.15× today. The 2–4× the tier ladder predicts needs increment 2 (pre-decode
the *operation*, not just the fetch).

Design context: the tier ladder in [docs/snemu-design.md](../docs/snemu-design.md)
(*JIT: the tier ladder*). Measured against the M4 spine
([plans/snemu-milestone-4-measurement.md](snemu-milestone-4-measurement.md)).

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

## Increment 2 (next) — pre-decode the operation → the real 2–4×

Cache a **dispatch-ready decoded form** (an `Op` enum with fields already
extracted), not just `raw`, so `execute` skips the opcode `match` + field
re-extraction. Restructures `execute` into decode-to-`Op` + execute-`Op`. This is
where the tier's headline 2–4× lives; increment 1 built the cache + flag +
invalidation + the verification harness it rides on.

## CLI

- `cargo xtask snemu-bench --decode-cache` — measure with the cache on (A/B vs the
  default off). Composes with `--taxonomy` / `--baseline`.
- `cargo xtask snemu-bench --verify-cache` — the faithfulness gate.
