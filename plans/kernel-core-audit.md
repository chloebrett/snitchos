# kernel-core crate audit (2026-06-08)

**Scope:** `kernel-core` — host-testable kernel logic (pure data, no asm/MMIO/CSR).
`publish = false`; sole consumer is the `kernel` binary. `#![forbid(unsafe_code)]`.
~6k LOC across 15 modules (mass: `mmu.rs` 859, `frame.rs` 344, `intern.rs`/`bootargs.rs` 238 each).

**Re-run with `cargo xtask audit kernel-core` (102 `pub` items).** The tool agrees
with the hand-pass and **adds two demote candidates it missed** (`branch_pte`,
`split_va` — items 5–6). Five `ext=0` items total: 2 test-only (`alloc_contiguous`,
`count_in_use`) and 3 internal-only demote candidates (`PRE_INIT_BYTES`,
`branch_pte`, `split_va`). The hand-only finding `is_empty` (item 3) does **not**
appear in the tool's candidates — `ext=54` because it collides with the universal
`std` `is_empty` method name; the tool can't see that one, so it stays a
manual finding.

**Verdict: clean, with one real deletion.** No dead modules, no TODO/FIXME/HACK/stub
markers, no `#[allow(dead_code)]`, every `#[allow]` carries an inline justification,
no Cargo features to rot. One genuine finding: **`spin` is an unused dependency**
(below) — surfaced by the tool, which my first hand-pass missed (I'd assumed all
three deps load-bearing). The rest are visibility nits and one reserved-surface note.

## Findings

| # | Dim | Sev | Finding | Evidence | Recommendation |
|---|-----|-----|---------|----------|----------------|
| 0 | H | med | **`spin` is an unused dependency** | `cargo machete kernel-core` flags it; `grep -rw spin kernel-core/src` hits only a doc comment (`preinit.rs:7`) and the unrelated word "spin" (`bootargs.rs:44`) — no `spin::` use | ✅ **DONE.** Removed `spin = "0.10"` from `kernel-core/Cargo.toml`. Host tests + riscv kernel build green; `cargo machete` now reports `none`. |
| 1 | C | low | `Bitmap::alloc_contiguous` has zero production callers — only tests | `frame.rs:264-303` (tests only); no hit in `kernel/src` | **KEEP.** Reserved DMA surface by design (`plans/v0.4-memory-step-4-kernel-heap.md:38` chose explicit `alloc_contiguous(n)`; step-3 notes DMA needs contiguous frames). Rule 6: contract ahead of consumer. Optional: one-line doc comment saying "reserved for DMA, no caller yet". |
| 2 | A/F | low | `Bitmap::count_in_use` prod-unused; trivial derived accessor (`capacity - count_free`) | `frame.rs:167,176` tests only; design-doc'd at `plans/v0.4-memory-step-3-frame-allocator.md:123` | Keep as documented stats surface, or `#[cfg(test)]` it. Low value either way. **Left as-is** (documented stats surface). |
| 3 | F | low | `Runqueue::is_empty` is `pub`, zero non-test callers | tool: `ext=54` (`std::is_empty` collision — tool blind); demote attempt revealed `clippy::len_without_is_empty` fires | ⛔ **KEEP `pub` — NOT a demote candidate.** It's required public API: `Runqueue::len` is `pub`, and the lint demands a matching public `is_empty`. The sweep caught this (demote → `len_without_is_empty` warning). The hand-pass's "called internally (sched.rs:47)" was wrong — that line is `self.ready.is_empty()` (Vec). A 4th tool false-positive class: *required-by-a-lint, not by a caller*. |
| 4 | F | low | `PRE_INIT_BYTES` is `pub` but only referenced inside `preinit.rs` + tests | tool: `ext=0 int=3`; `preinit.rs:17,31,44` | ✅ **DONE.** Demoted to `pub(crate)`. |
| 5 | F | low | `mmu::branch_pte` is `pub` but no `kernel` caller — internal page-table helper | tool: `ext=0 int=3 test=3` (`mmu.rs:107`); `grep -rw branch_pte kernel/src` = 0 | ✅ **DONE.** Demoted to `pub(crate)`. Tool found it; hand-pass missed it. Sweep confirmed clean (plain-value return, not the positional/alias class). |
| 6 | F | low | `mmu::split_va` is `pub` but no `kernel` caller — internal VA-splitting helper | tool: `ext=0 int=3 test=7` (`mmu.rs:114`); `grep -rw split_va kernel/src` = 0 | ✅ **DONE.** Demoted to `pub(crate)`. Tool found it; hand-pass missed it. |

## Non-findings (checked, not debt)

- **6 same-named module pairs** (`heap_smoke/workload/sched/frame/heap/trap.rs` in
  both `kernel-core` and `kernel`) — this is the intended carveout: pure logic in
  `-core` (host-tested), asm/MMIO twin in `kernel`. Not duplication.
- **`CapturingSink`** — appeared zero-ext, but is `#[cfg(test)] pub(crate) mod
  capture` (`sink.rs:17-18`). Correctly gated test double. Fine.
- **`workload.rs` `buckets`-as-param** — looks like a speculative knob (always
  `BUCKETS` in prod) but the doc comment explains it exists for test variation
  without recompiling the kernel. Honest, keep.

## Applied (privatization sweep + dep removal)

Ran the compiler-backed sweep (the procedure from
`plans/audit-revisit-post-xtask-audit.md`). For kernel-core the verifying rebuild
must hit **both** targets — `cargo test -p kernel-core` (host) and `cargo xtask
build` (riscv kernel) — plus `cargo clippy -p kernel-core`. Result:

- **`spin` dependency removed** (item 0).
- **3 demotes** `pub`→`pub(crate)`: `PRE_INIT_BYTES`, `branch_pte`, `split_va`
  (items 4–6). All clean on both targets.
- **`is_empty` reverted to `pub`** (item 3) — the sweep's clippy pass fired
  `len_without_is_empty`; it's required public API, not dead. A 4th false-positive
  class for `xtask audit`, now logged in the skill.

Public surface 102 → 99 `pub` items; `cargo machete` clean; 138 host tests + riscv
build + clippy all green. Items 1 (reserved DMA) and 2 (documented stats accessor)
left as-is by design.

## Architecture / abstraction / complexity (dims C–F)

The tool doesn't cover these — read `mmu.rs` (the 859-line module) and scanned the
rest. **Verdict: structurally healthy.** Clippy pedantic flags no `too_many_lines`
/ `too_many_arguments` / cognitive-complexity; the longest *production* function is
41 lines (`map_2mib`) — the only longer items are tests. The `PtMem` trait is a
clean port: the page-table walk is pure and host-testable, with the `unsafe` deref
isolated in the kernel's `KernelPtMem` impl. `MapError` is a proper error enum.
No god functions, no arg-list bloat.

**Non-finding — `map` vs `remap` look duplicated but are honest divergence.** Same
shape (split_va → walk → write leaf) but genuinely different contracts: `map` uses
`walk_or_install` (allocates, rejects existing → `AlreadyMapped`, can `OutOfFrames`);
`remap` uses `walk_existing` (never allocates, requires a leaf → `NotMapped`,
overwrites). Collapsing them needs a walk-strategy enum + conflict-policy param —
*more* complexity than two clear 8-line functions. The shared primitives
(`split_va`, `leaf_pte`, `walk_*`) are already extracted. Leave it.

### Abstraction opportunities (offered, not imposed — your call)

| Opp | What | Benefit | Cost |
|---|---|---|---|
| **A. `Pte(u64)` newtype** | PTEs are raw `u64` today, manipulated by 6 free fns: `leaf_pte`, `branch_pte`, `pte_is_branch`, `pte_is_leaf`, `branch_pte_child_pa`, `pa_to_pte_ppn`. Wrap as `Pte(u64)` with `Pte::leaf(pa,perms)` / `Pte::branch(child_pa)` / `.is_leaf()` / `.child_pa()`. | **Recommended.** Everything in `mmu` is `u64` (PTE, PA, perms-bits) — easy to pass the wrong one; a newtype makes that a compile error and co-locates the spec encode/decode with its invariants. **Matches the crate's own `PtePerms(u64)` precedent** — it's finishing a pattern already started. | Newtype + methods; `PtMem::read_entry/write_entry` change to `Pte` (touches trait + kernel `KernelPtMem` + tests). Medium. |
| **B. name `split_va`'s tuple** | Returns `(usize,usize,usize,usize)` = `(vpn2,vpn1,vpn0,offset)`, destructured positionally everywhere. A `struct Sv39Va { vpn: [usize;3], offset }` names them. | Smaller win — a vpn1/vpn0 swap is currently a silent bug the type system won't catch; `vpn: [usize;3]` also suits the level-indexed walk loop. | Low effort, but the tuple is always destructured immediately, so payoff is modest. |

Both are TDD-able refactors behind the existing `mmu` tests (host) + the riscv
build. A is the one I'd actually do; B is optional polish. **Ask before applying.**

## Mass estimate

Net so far: −1 dependency, −1 `Cargo.toml` line, 3 `pub`→`pub(crate)` demotes, 0
deletions, 0 behavior change. Abstraction A (`Pte` newtype) would be net-neutral on
lines (free fns → methods) but a real safety gain; B is ~+10 lines.
