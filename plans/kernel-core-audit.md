# kernel-core crate audit (2026-06-08)

**Scope:** `kernel-core` — host-testable kernel logic (pure data, no asm/MMIO/CSR).
`publish = false`; sole consumer is the `kernel` binary. `#![forbid(unsafe_code)]`.
~6k LOC across 15 modules (mass: `mmu.rs` 859, `frame.rs` 344, `intern.rs`/`bootargs.rs` 238 each).

**Verdict: clean.** No dead modules, no TODO/FIXME/HACK/stub markers, no
`#[allow(dead_code)]`, every `#[allow]` carries an inline justification, no unused
deps (spin, protocol, postcard all load-bearing), no Cargo features to rot. Findings
below are visibility nits and one reserved-surface note — nothing to delete.

## Findings

| # | Dim | Sev | Finding | Evidence | Recommendation |
|---|-----|-----|---------|----------|----------------|
| 1 | C | low | `Bitmap::alloc_contiguous` has zero production callers — only tests | `frame.rs:264-303` (tests only); no hit in `kernel/src` | **KEEP.** Reserved DMA surface by design (`plans/v0.4-memory-step-4-kernel-heap.md:38` chose explicit `alloc_contiguous(n)`; step-3 notes DMA needs contiguous frames). Rule 6: contract ahead of consumer. Optional: one-line doc comment saying "reserved for DMA, no caller yet". |
| 2 | A/F | low | `Bitmap::count_in_use` prod-unused; trivial derived accessor (`capacity - count_free`) | `frame.rs:167,176` tests only; design-doc'd at `plans/v0.4-memory-step-3-frame-allocator.md:123` | Keep as documented stats surface, or `#[cfg(test)]` it. Low value either way. |
| 3 | F | low | `Runqueue::is_empty` is `pub` but only called internally (`sched.rs:47`) + tests | `sched.rs:47,70-89` | Demote to `pub(crate)` (or keep — idiomatic companion to `len`). |
| 4 | F | low | `PRE_INIT_BYTES` is `pub` but only referenced inside `preinit.rs` + tests | `preinit.rs:17,31,44` | Demote to `pub(crate)`. |

## Non-findings (checked, not debt)

- **6 same-named module pairs** (`heap_smoke/workload/sched/frame/heap/trap.rs` in
  both `kernel-core` and `kernel`) — this is the intended carveout: pure logic in
  `-core` (host-tested), asm/MMIO twin in `kernel`. Not duplication.
- **`CapturingSink`** — appeared zero-ext, but is `#[cfg(test)] pub(crate) mod
  capture` (`sink.rs:17-18`). Correctly gated test double. Fine.
- **`workload.rs` `buckets`-as-param** — looks like a speculative knob (always
  `BUCKETS` in prod) but the doc comment explains it exists for test variation
  without recompiling the kernel. Honest, keep.

## Mass estimate

Net ≈ 0 lines. Items 3–4 are `pub`→`pub(crate)` edits (no deletion); item 2 at most
removes a 1-line accessor. Nothing here is worth a churn PR on its own — fold into
the next `frame`/`sched`/`preinit` touch.
