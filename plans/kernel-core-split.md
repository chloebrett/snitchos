# Splitting kernel-core into per-concern crates

The original carve-out plan ([legacy/kernel-core-carveout.md](legacy/kernel-core-carveout.md))
closed with: *"No splitting the kernel binary further. Just one new crate. If
later we want `kernel-trap-core` / `kernel-tracing-core` separation, fine — but
speculative now."*

This is that later. `kernel-core` is now 10.7k lines / 499 tests across 27
modules, and it is a single compilation unit: a one-line change to any file
recompiles all of it.

## why — and what the win actually is

Measured first, because the obvious framing is wrong.

| action after touching one file | time | CPU |
|---|---|---|
| `cargo build -p kernel-core` (lib only) | 5.8s | 28% |
| `cargo test -p kernel-core --no-run` | 12.2s | 43% |
| running the 499 tests | **0.01s** | — |

**The tests are already free.** 499 of them run in 10 milliseconds. There is no
test runtime to recover, and no split will produce any. Anyone reading this plan
expecting "faster tests" should stop here: the tests are not slow.

The 12.2s is compile + link, and it is 12.2s whether you touch `cap.rs` or
`mmu.rs` — the crate is the compilation unit. Two things are actually wrong:

1. **No granularity.** Iterating on `mmu` (74 tests) costs the same as iterating
   on all 499. You cannot pay only for the part you're working on.
2. **Idle cores.** CPU sits at 28–43%. One crate largely serializes; six
   independent crates compile in parallel.

So the honest prize is a *focused* loop — `cargo test -p kernel-mem` recompiling
2445 lines instead of 10.7k — plus parallelism on the full build. See "what this
does not fix" before committing to the whole sequence.

## the dependency graph is already a DAG

This was the surprise. Grepping for `crate::` suggests a tangle
(`cap ↔ notify ↔ ipc ↔ reap`), but almost every hit is a doc-comment intra-doc
link or a fully-qualified path. The actual `use` statements across the entire
crate are:

```
cap.rs    → ipc::EndpointId, notify::NotificationId, sched::TaskId
ipc.rs    → sched::TaskId
notify.rs → sched::TaskId
reap.rs   → sched::TaskId
stack.rs  → mem::mmu::PAGE_SIZE
intern.rs → sink::FrameSink
sched.rs  → (nothing)
```

That is the whole coupling. **There are no cycles.** `mem/`, `virtio`, `elf`,
`fwcfg`, `workloads/`, and `obs/` import *nothing* from the rest of the crate —
their `use super::*` lines are all inside `mod tests`. The `mem/` / `obs/` /
`user/` / `workloads/` directory grouping already added for readability turns out
to be a near-perfect crate boundary map.

The coupling that does exist is three ID newtypes. That makes `kernel-ids` the
keystone: extract `TaskId`, `EndpointId`, `NotificationId` and four of the seven
edges above vanish, including both edges out of `cap`.

## the facade trick — why this is cheap and reversible

`kernel-core/src/lib.rs` already re-exports its directory modules at the crate
root:

```rust
pub use mem::{frame, heap, heap_smoke, mmu};
```

The kernel therefore says `kernel_core::mmu::…`, not `kernel_core::mem::mmu::…`.
When `mem/` becomes its own crate, that line becomes:

```rust
pub use kernel_mem::{frame, heap, heap_smoke, mmu};
```

and **the public path `kernel_core::mmu` is unchanged**. The kernel's 35
`kernel_core::{mmu,frame,heap,heap_smoke}` call sites need zero edits. Each
extraction is a pure code move behind a stable facade, revertible by moving the
directory back.

`kernel` is the only real consumer (the `learning/` toys reference kernel-core in
*comments* only, despite the name matches — they deliberately reimplement these
structures with `Vec` backing). Blast radius is one crate.

## first: what should move *in*

Before splitting kernel-core up, we swept `kernel/` for logic that should have
been in kernel-core all along. **The verdict is that the carve-out discipline has
been applied thoroughly** — 2665 lines of `kernel/` carry zero `unsafe`/`asm!`,
and nearly all of it is *glue by design*: `syscall/ipc.rs` reads a `TrapFrame`,
locks `proc.caps`, and calls `kernel_core::cap::invoke_send`; `sched/process.rs`
says outright that "the capability machinery is pure and host-tested in
`kernel_core::cap`; this module only decides where the table lives".

The lesson: **`unsafe`-freedom is not the test for what belongs in kernel-core.**
Glue that locks a singleton and threads a `TrapFrame` contains no `unsafe` and
still cannot run on the host. The real test is *does it touch statics,
`TrapFrame`, or MMIO* — and by that test the boundary is already clean.

Explicitly checked and rejected: `maybe_preempt` / `kill_task` (glue over
`TaskDirectory`), `heap::extend` (delegates to `next_heap_top`),
`frame::init_from_dtb` (delegates to `release_unreserved`), `fwcfg.rs` /
`virtio_console.rs` (already trait-seam adapters), `workload.rs` (`Lcg`/`bin_of`
already extracted), `obs/counter.rs` (a host test would be testing `AtomicU64`),
`percpu::current_hartid` (its famous tp-truncation bug was *codegen*, not logic —
a host test would never have caught it).

The one real residue is the opposite shape to what we looked for: not an
unsafe-free file stranded in the kernel, but **pure arithmetic embedded inside an
`unsafe`-containing function**.

### the ELF loader's page planning (`kernel/src/trap/user.rs:1016-1088`)

`load()` is page-planning arithmetic wrapped around exactly three effects
(`frame::alloc_zeroed`, `mmu::map_in`, `copy_nonoverlapping`). The planning parts
are pure and, tellingly, already speak kernel-core's vocabulary on *both* sides:
`perms_for(SegmentPerms) -> PtePerms` maps a `kernel_core::user::elf` type to a
`kernel_core::mem::mmu` type, yet lives in the kernel. `LoadPlan`, `LoadSegment`,
`SegmentPerms` are all already host-side. This looks like an oversight, not a
design call.

Worth extracting because host tests would pin four things QEMU currently can't:

- **W^X.** Two segments may share a page (R-X code + R rodata in the first page),
  and `load()` *unions* their perms per page with **no W^X guard anywhere in the
  codebase**. Nothing today asserts that a code page and a data page cannot union
  to `RWX`. That is a security property, currently unassertable.
- **`pages_of` end-rounding.** A bss tail landing exactly on a page boundary maps
  one page too few or too many — today observable only as a userspace fault.
- **Copy-window drift.** `dst` and `src` use different bases (`lo - page_va` vs
  `lo - file_lo`). For a non-page-aligned `vaddr`, an off-by-one copies the image
  shifted by a few bytes.
- **bss.** `file_size < mem_size` must skip the tail so the zeroed frame stays
  zero; a regression silently copies garbage into bss.

Shape: `elf::page_perms(&LoadPlan, page_size) -> BTreeMap<usize, PtePerms>` and
`elf::copy_windows(&LoadSegment, page_size) -> impl Iterator<Item = CopyWindow>`.
kernel-core already has `extern crate alloc`, so `BTreeMap` is fine and **no trait
seam or injection is needed**. ~45 lines move; `load()` shrinks to ~20 lines of
pure effect. Unlike the rest of this plan, this one adds real test coverage rather
than just recompiling faster — **it is the highest-value item in this document.**

Two smaller finds, both landing in `mem/mmu.rs` and therefore **sequenced against
step 1** (do them strictly before or after the `kernel-mem` move, never during —
mixing a code-move with a code-change breaks the zero-diff check below):

- `MmioRegions` (`kernel/src/mem/mmu.rs:33-66`, ~35 lines) — pure despite the
  name; `insert` aligns-then-dedups to 2 MiB and silently clamps at 16 entries.
  Tests would pin the align/compare order and the documented drop-on-overflow.
- `satp_for` / `root_from_satp` (`kernel/src/mem/mmu.rs:456,465`, ~10 lines) — a
  round-trip test pins the mode-shift and PPN-mask constants together; they live
  10 lines apart today with nothing asserting they agree.

**Recommended order: ELF extraction first** (it is independent of `mem/`, lands in
`elf`, and is the only item here that buys correctness), then step 1 below.

### as-built: the ELF extraction (DONE)

Landed as `kernel_core::user::elf::{page_perms, copy_windows, CopyWindow,
PlanError, SegmentPerms::{union, is_wx}}`; `kernel/src/trap/user.rs::load` is now
alloc/map/copy only. 15 new host tests (kernel-core 499 → 514); 114/114
snemu-itest; clippy clean.

Deviations from the sketch above:

- **The planner returns `SegmentPerms`, not `PtePerms`.** `SegmentPerms`'s doc
  states it "stays decoupled from `mmu::PtePerms`" — deliberately. Returning
  `PtePerms` would have broken that *and* made a future `kernel-elf` crate depend
  on `kernel-mem`. So the union and the W^X check happen in `SegmentPerms` space,
  and `perms_for` (the `SegmentPerms → PtePerms` map) stays kernel-side. **This
  keeps the step-7 `kernel-elf` crate a true leaf.**
- **No new snitch mechanism.** Every other `LoadError` already `panic!`s, and the
  panic handler emits a `Log` frame (the `kernel-panic-emits-frame` itest). A
  `PlanError::WxViolation` rides that path, so "refuse + snitch" cost one enum
  variant.

**The bug was real, and was demonstrated, not argued.** Temporarily adding one
initialised mutable static (`static mut X: u64 = 0xDEADBEEF` → `.data`) to `init`
and reverting the `.data` alignment produced, from the real toolchain:

```
PT_LOAD R-X @ 0x10000000 (1706 bytes)   ─┐
PT_LOAD R-- @ 0x100006B0 (600 bytes)    ─┼─ all three land in page 0x10000000
PT_LOAD RW- @ 0x10000908 (8 bytes)      ─┘   union = R+W+X
```

and the guard refused it end-to-end under snemu:

```
userspace load failed: Plan(WxViolation { page_va: 268435456 })   // 0x10000000
```

With `.data : ALIGN(0x1000)` restored, the same probe binary links its RW segment
at `0x10001000` and boots clean (560 frames). Both probes reverted.

**Bonus property:** the itest suite now guards the linker script. Removing the
alignment makes `init` unloadable, so the default boot panics and the whole suite
fails loudly — the intent `user.ld` has documented since v0.7a is finally
enforced by something.

Open follow-ups (deliberately not done here):

- **`mem_size` is unbounded** — `parse` validates `file_size <= mem_size` and the
  file range, but nothing caps `mem_size`. A malicious ELF declaring
  `mem_size = 2^60` makes `page_perms` build an enormous `BTreeMap` and hang.
  Pre-existing (the old `load` had the same unbounded `pages_of`), and surfaced
  by 4 mutation TIMEOUTs in `pages_of`. It contradicts the module's stated "a
  malformed image yields an `ElfError`, never a panic" trust-boundary claim, and
  matters for v0.10's untrusted images. Worth its own increment.
- **`WxViolation` prints its page in decimal** (`268435456`). A `Debug` impl
  formatting `page_va` as hex would make the panic line readable at a glance.

## what moves

Extraction order is by (independence × payoff). Each row is a leaf that depends
only on rows above it.

| # | crate | modules | lines | tests | depends on |
|---|---|---|---|---|---|
| 1 | `kernel-mem` | mmu, frame, heap, heap_smoke | 2445 | 121 | — |
| 2 | `kernel-ids` | TaskId, EndpointId, NotificationId | ~50 | few | — |
| 3 | `kernel-obs` | intern, span, preinit, sink, batch_ring | 1211 | 40 | — |
| 4 | `kernel-workloads` | bootargs, workload | 834 | 62 | — |
| 5 | `kernel-virtio` | virtio | 703 | 29 | — |
| 6 | `kernel-devices` | fwcfg, ramfb, framebuffer | 606 | 27 | — |
| 7 | `kernel-elf` | elf | 401 | 14 | — |
| 8 | `kernel-proc` (residual) | sched, cap, ipc, notify, reap, console, stack, metric, span_name | ~3300 | ~163 | ids, mem |

`kernel-core` survives as a **facade crate**: no code of its own, just
`pub use` re-exports keeping every existing `kernel_core::…` path working. If
step 8's residual proves to be the real tangle, it can stay as-is forever — the
leaves are where the value is.

### what stays put

- Everything in `kernel/` — asm, MMIO, CSRs, statics. Untouched.
- `kernel-core` itself, as the facade. Deleting it and repointing the kernel at
  seven crates is a separate, later, optional call.
- `tests/loom_tx.rs` — the `--cfg loom` model check. It stays with `kernel-core`
  until step 5 (virtio); moving it early would need the loom dev-dependency
  duplicated for no gain.

## this is refactoring, not TDD

CLAUDE.md's RED-GREEN-MUTATE cycle governs *new behaviour*. This plan adds none.
It is pure code motion under existing test cover, so the cycle does not apply and
there is no failing test to write first.

The safety net is that the moved tests move with their code and must still pass,
unchanged, from the new crate. The acceptance criterion for every step is
therefore identical:

- the same test count passes from the new crate (121 for step 1, etc.)
- `cargo test -p kernel-core` still passes 499 total across the workspace
- `cargo build -p kernel --target riscv64gc-unknown-none-elf` builds clean
- `cargo xtask clippy` clean
- **zero diff outside the moved files, the two Cargo.tomls, and the facade line**

That last one is the real check. If a step needs to *edit* moved code, the
boundary is wrong — stop and reassess rather than bending the code to fit.

Mutation testing is also not re-run per step: the tests and the code they cover
are byte-identical, so mutation scores cannot move. (`kernel` is already excluded
from `cargo xtask mutants` as bare-metal.)

## step 1: `kernel-mem` — the probe

**Do this one, then measure, then decide the rest.** It is the best test of the
whole premise: 23% of the crate, the largest module (`mmu`, 1575 lines / 74
tests), a true leaf needing *zero* import changes inside it, and exactly one
inbound edge (`stack.rs` → `PAGE_SIZE`).

1. `kernel-mem/Cargo.toml` — `edition = "2024"`, `[lints] workspace = true`.
   Deps: none (mem imports nothing internal; confirm `protocol`/`abi` aren't
   needed once moved).
2. `kernel-mem/src/lib.rs` — `#![no_std]`, `#![forbid(unsafe_code)]`,
   `extern crate alloc;`, `pub mod {mmu, frame, heap, heap_smoke};`
3. `git mv kernel-core/src/mem/*.rs kernel-mem/src/`, delete `kernel-core/src/mem.rs`.
4. Workspace `members`: add `kernel-mem`.
5. `kernel-core/Cargo.toml`: add `kernel-mem = { path = "../kernel-mem" }`.
6. `kernel-core/src/lib.rs`: `mod mem;` → `pub use kernel_mem::{frame, heap, heap_smoke, mmu};`
7. `kernel-core/src/stack.rs`: `use crate::mem::mmu::PAGE_SIZE;` →
   `use kernel_mem::mmu::PAGE_SIZE;` (or leave it — the facade re-export means
   `crate::mmu::PAGE_SIZE` also resolves).

**Then measure and record here, before step 2:**

| metric | before | after |
|---|---|---|
| `cargo test -p kernel-mem --no-run` (touch mmu.rs) | n/a (12.2s for all) | ? |
| `cargo test -p kernel-core --no-run` (touch cap.rs) | 12.2s | ? |
| full cold `cargo test --no-run` for both | ? | ? |
| peak CPU on full build | 43% | ? |

**The decision rule:** if the focused `-p kernel-mem` loop doesn't land
meaningfully under 12.2s, steps 2–8 won't help either — stop and delete this
plan. One afternoon spent learning that beats seven.

## steps 2–8

Deliberately not elaborated until step 1's numbers exist. Each follows the same
shape as step 1 (new crate → `git mv` → facade re-export → verify), in the table
order above. Two notes for when we get there:

- **Step 2 (`kernel-ids`) is the keystone**, not a leaf win — ~50 lines and few
  tests, worth nothing on its own. Its value is unblocking a clean `cap` / `ipc`
  / `notify` / `reap` separation in step 8. Skip it if step 8 is skipped.
- **Step 8's residual is the only genuine design work.** Everything above it is
  mechanical.

## what this does not fix

Stated plainly so the plan isn't oversold:

- **Test runtime.** 0.01s. Nothing to win, ever.
- **The full-crate rebuild after touching `mmu.rs`.** Cargo rebuilds dependents
  on any fingerprint change, so touching `kernel-mem` still rebuilds `kernel-core`
  and `kernel`. The win is the *focused* `-p kernel-mem` loop, not this path.
- **Cold full builds may get slightly slower.** Seven crates means seven test
  binaries to link, and linking is a real share of the 12.2s (the test build costs
  6.4s more than the lib build). Parallelism should more than pay for it given the
  idle cores — but that's a hypothesis, and it's exactly what step 1 measures.
- **The actual cold-build pole, if you're feeling `cargo xtask test`.** kernel-core
  compiles in 1.2s release; its *dependency chain* (`syn`, `serde_derive`,
  `hitch-derive` via `protocol`) costs ~60s cold. If the pain is a cold full-workspace
  build, this plan is rearranging deck chairs and the proc-macro chain is the target.

## risks and known weaknesses

- **Facade churn on `pub use`.** Re-exporting a module (`pub use kernel_mem::mmu`)
  rather than items keeps paths stable, but rustdoc will now show `mmu` as a
  re-export. Cosmetic; note it if the docs read badly.
- **`#![forbid(unsafe_code)]` per crate.** Each new crate must re-declare it —
  it doesn't inherit. Easy to forget, and forgetting silently weakens the
  invariant that makes kernel-core host-testable. Add it in step 1 of each move.
- **`[lints] workspace = true` per crate.** Same — new crates don't inherit
  workspace lints without opting in. A missed opt-in means pedantic silently
  stops running on 2445 lines.
- **`extern crate alloc`.** `mem/` uses `alloc`; confirm the new crate declares it.
- **Crate-count tax on `cargo xtask clippy`.** It lints host crates for host and
  kernel for riscv. Seven new members shouldn't need changes (it's workspace-wide),
  but verify rather than assume.
- **`git mv` preserving blame.** Use `git mv` (not delete+create) so `mmu.rs`'s
  history — which includes the v0.4 higher-half findings — survives.

## done state (step 1)

- `kernel-mem` exists as a workspace member; 121 tests pass via `cargo test -p kernel-mem`.
- `cargo test -p kernel-core` still reports 499 total.
- `cargo build -p kernel --target riscv64gc-unknown-none-elf` clean, **with no
  changes to any `kernel/` source file**.
- Measurement table above filled in, and a go/no-go recorded for steps 2–8.

---
*Delete this file when the plan is complete or abandoned.*
