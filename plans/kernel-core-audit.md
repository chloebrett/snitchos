# kernel-core crate audit (2026-06-08)

**Scope:** `kernel-core` — host-testable kernel logic (pure data, no asm/MMIO/CSR).
`publish = false`; sole consumer is the `kernel` binary. `#![forbid(unsafe_code)]`.
~6k LOC across 15 modules (mass: `mmu.rs` 859, `frame.rs` 344, `intern.rs`/`bootargs.rs` 238 each).

**Verdict: clean, with one real deletion.** No dead modules, no TODO/FIXME/HACK/stub
markers, no `#[allow(dead_code)]`, every `#[allow]` carries an inline justification,
no Cargo features to rot. One genuine finding: **`spin` is an unused dependency**
(below) — surfaced by `cargo xtask audit kernel-core`, which my first hand-pass
missed (I'd assumed all three deps load-bearing). The rest are visibility nits and
one reserved-surface note.

## Findings

| # | Dim | Sev | Finding | Evidence | Recommendation |
|---|-----|-----|---------|----------|----------------|
| 0 | H | med | **`spin` is an unused dependency** | `cargo machete kernel-core` flags it; `grep -rw spin kernel-core/src` hits only a doc comment (`preinit.rs:7`) and the unrelated word "spin" (`bootargs.rs:44`) — no `spin::` use | **Remove `spin = "0.10"` from `kernel-core/Cargo.toml`.** Safe deletion (`publish = false`; it compiles+tests without it). The crate's locking goes through `kernel::sync` in the *kernel* crate; kernel-core itself doesn't lock. |
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

Net ≈ −1 line + a dependency. Item 0 deletes one `Cargo.toml` line and drops `spin`
from the kernel-core build (the only standalone win — worth doing now). Items 3–4
are `pub`→`pub(crate)` edits (no deletion); item 2 at most removes a 1-line
accessor — fold those into the next `frame`/`sched`/`preinit` touch.
