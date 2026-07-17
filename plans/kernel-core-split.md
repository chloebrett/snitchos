# Splitting kernel-core into per-concern crates

The original carve-out plan ([legacy/kernel-core-carveout.md](legacy/kernel-core-carveout.md))
closed with: *"No splitting the kernel binary further. Just one new crate. If
later we want `kernel-trap-core` / `kernel-tracing-core` separation, fine — but
speculative now."*

This is that later. `kernel-core` was a staging area: a grab-bag whose only
meaning was "the host-testable bits". It has grown to 27 modules, and the name
has stopped describing anything. This plan replaces it with five crates that each
name a concept, and then deletes it.

## why: factoring, not speed

**This is an architecture change, not a performance one.** We measured the
performance case first and it does not carry the plan — see "the cost we're
accepting" below. The reason to do it is that crate boundaries are *enforced*
where module boundaries are not.

Today the dependency graph is clean **by discipline**. Nothing stops someone
adding `use crate::sched::TaskId` to `mmu.rs` tomorrow; it would compile, review
would likely miss it, and the layering would quietly rot. As separate crates, that
same edit requires a `Cargo.toml` change — a visible, reviewable decision — and a
cycle becomes a hard compile error rather than a judgement call.

That is the same argument this codebase has already made twice, and it is the
house position:

- `kernel` vs `kernel-core` itself — separate the platform-pure logic so the
  boundary is structural.
- `kernel::sync` + the `disallowed_types` clippy lint — one chokepoint, enforced
  by tooling rather than by remembering.
- and from [itest-harness-extraction.md](itest-harness-extraction.md): *"We're
  going to want this even if no one else ever uses the harness crate — **the
  boundary is the discipline.**"*

A second, quieter payoff: three of the five crates turn out to need **no
dependencies at all** (below). Making that visible is itself the point — it says
most of kernel-core has nothing to do with the wire format, which is not
discoverable today.

## the dependency graph is already a DAG

Grepping for `crate::` suggests a tangle (`cap ↔ notify ↔ ipc ↔ reap`), but
almost every hit is a doc-comment intra-doc link or a fully-qualified path. The
actual `use` statements across the whole crate were:

```
cap.rs    → ipc::EndpointId, notify::NotificationId, sched::TaskId
ipc.rs    → sched::TaskId
notify.rs → sched::TaskId
reap.rs   → sched::TaskId
stack.rs  → mem::mmu::PAGE_SIZE      (now kernel_mem::mmu::PAGE_SIZE)
intern.rs → sink::FrameSink
sched.rs  → (nothing)
```

That is the whole coupling. **There are no cycles.** This is *why* the split is
cheap — and also why it must be enforced structurally: a property this clean,
held only by discipline, is exactly the kind that erodes without anyone deciding
to erode it.

Note what this kills: an earlier draft of this plan proposed a `kernel-ids` crate
to break `cap → ipc/notify/sched` on three ID newtypes. **The conceptual grouping
below dissolves that need** — those five modules land in one crate, so the
coupling is internal and the junk-drawer crate never gets created. Grouping by
concept beat grouping by dependency-convenience.

## what moves

Five crates, grouped by what they *mean* — not by directory, and not by
extraction-order convenience. Line/test counts are exact (they sum to
kernel-core's 393 remaining tests).

| crate | modules | lines | tests | deps |
|---|---|---|---|---|
| `kernel-mem` ✅ | mmu, frame, heap, heap_smoke | 2445 | 121 | **none** |
| `kernel-obs` ✅ | intern, span, preinit, sink, batch_ring, panic_log, clock | 1356 | 45 | protocol, postcard |
| `kernel-devices` ✅ | virtio, fwcfg, ramfb, framebuffer, console | 1469 | 62 | **none** |
| `kernel-boot` | bootargs, workload, trap | 931 | 69 | **none** |
| `kernel-proc` | sched, cap, ipc, notify, reap, elf, stack, metric, span_name | 4377 | 217 | protocol, abi, kernel-mem |

The concepts:

- **`kernel-mem`** — memory bookkeeping. Arithmetic over caller-owned storage.
- **`kernel-obs`** — how the kernel talks about itself. The only group that
  touches the wire format.
- **`kernel-devices`** — device *protocol* state machines. No MMIO (that stays in
  `kernel/`); this is the sequencing logic behind it.
- **`kernel-boot`** — boot-time decisions: which workload, what the trap cause was.
- **`kernel-proc`** — tasks, their authority, their lifecycle. `elf` is here
  because loading is a process concern, not a memory one; `stack` likewise (it
  needs `mmu::PAGE_SIZE`, which is the single inter-crate edge).

**Three of five are dependency-free** — verified, not assumed: `virtio`, `fwcfg`,
`ramfb`, `framebuffer`, `console`, `bootargs`, `workload`, and `trap` import no
`protocol`/`abi`/`postcard` at all. They never wait on the `protocol` → `serde`
→ `syn` proc-macro chain, which is the workspace's real cold-build pole (~60s).

### what stays put

- Everything in `kernel/` — asm, MMIO, CSRs, statics. Untouched by the moves.

(An earlier draft said `tests/loom_tx.rs` should "stay with the last crate
standing". Wrong: it models `virtio::stage_and_emit`, so it moved with
`kernel-devices` in step 3, taking the loom dev-dependency with it. A test
belongs with the code it tests.)

## the endpoint: kernel-core dissolves

The five crates above account for **everything** currently in kernel-core.
Nothing is left over. So the plan ends with `kernel-core` empty, and it gets
deleted.

Sequencing — the facade makes each split two small known-good increments instead
of one big one:

1. **Motion.** `git mv` + a `pub use kernel_x::{…}` line in kernel-core's
   `lib.rs`. The public path `kernel_core::mmu` is unchanged, so **zero edits to
   any `kernel/` source file**. Revertible by moving the directory back.
2. **Rename** (one sweep, at the end). Delete the `pub use` lines, repoint the
   ~200 call sites at `kernel_mem::`/`kernel_obs::`/…, add the deps to
   `kernel/Cargo.toml`, delete `kernel-core`.

Removal cannot break anything silently: while a facade exists **both** paths
compile, and the moment the `pub use` goes, the compiler names every site needing
an update. There is no way to half-finish it and not know.

Why the umbrella must not survive: a `kernel-core` that re-exports all five
*depends on* all five, so it rebuilds whenever anything changes — reintroducing
the exact rebuild-everything hub the split exists to break, plus a sixth
compilation unit and a sixth test binary containing no code. Every cost, no
benefit. **If it isn't carrying code, it shouldn't be a crate.**

It also has no vocabulary role to fall back on: `protocol` already owns
`StringId`/`Frame` and `snitchos-abi` owns the rights bits, and the sub-crates
consume both directly. "The host-testable bits" stops being a distinction once
all five are host-testable by construction.

**This decision is deferrable.** Do the four remaining splits, live with the
facades, and settle the umbrella question with evidence at the end. The expected
answer is delete.

## the cost we're accepting

Stated plainly, because the measurements are real and they argue the other way.
Measured on a quiet tree (earlier 55–60s readings were cargo build-lock
contention from a concurrent build, not real cost):

| metric | before | after `kernel-mem` | |
|---|---|---|---|
| touch `mmu.rs` → `cargo test -p kernel-mem --no-run` | 11.9s | **6.3s** | −47% |
| touch `cap.rs` → `cargo test -p kernel-core --no-run` | 11.9s | 11.6s | −2.5% |
| touch `mmu.rs` → rebuild **both** crates' tests | 11.9s | **17.6s** | **+48%** |
| CPU during rebuild | 43% | 45% | — |

Read those honestly:

- **Test runtime is not a factor and never was.** kernel-core's 499 tests ran in
  **0.01s**. Nobody is waiting on tests; the whole cost is compile + link.
- **Build cost is not proportional to line count.** kernel-core shed 23% of its
  lines and 2.5% of its build time. There is a fixed **~5–6s per-test-binary
  floor** (codegen + link). A 2445-line dependency-free crate still costs 6.3s.
- **So more crates ≈ more floors.** Five crates means five test binaries. Row 3
  is the shape of it: +48% on any path that rebuilds everything. At 45% CPU there
  is not enough parallelism to absorb that.

**We are choosing enforced boundaries over build time, with eyes open.** The
mitigations are that the focused loop gets *better* (row 1, −47%), and that three
of the five crates are dependency-free so they parallelise well and skip the
proc-macro chain. If the full-suite cost becomes intolerable in practice, the
answer is to merge crates back — not to re-litigate the boundary.

## this is refactoring, not TDD

CLAUDE.md's RED-GREEN-MUTATE cycle governs *new behaviour*. The moves add none.
They are pure code motion under existing test cover, so there is no failing test
to write first.

The safety net is that moved tests move with their code and must still pass,
unchanged, from the new crate. The acceptance criterion for every step:

- the exact test count passes from the new crate (see the table)
- the totals still sum to 514 across the workspace
- `cargo build -p kernel --target riscv64gc-unknown-none-elf` builds clean
- `cargo xtask clippy` clean; `cargo xtask snemu-itest` green
- **zero diff outside the moved files, the two Cargo.tomls, and the facade line**

That last one is the real check. If a step needs to *edit* moved code, the
boundary is wrong — stop and reassess rather than bending the code to fit.

Mutation testing is not re-run per move: the tests and the code they cover are
byte-identical, so scores cannot move.

## remaining steps

Each follows the shape proven twice now: new `Cargo.toml` (+`[lints] workspace =
true`) → `lib.rs` (`#![no_std]`, `#![forbid(unsafe_code)]`, `extern crate alloc`)
→ `git mv` → workspace member → facade re-export → verify. Order barely matters
(they are independent); `kernel-proc` is last because it is the biggest and the
only one with real design work.

- ~~**Step 2: `kernel-obs`**~~ **DONE** (`57f5280`) — 45 tests, all 7 modules moved
  with `| 0` diffs (zero content edits). Correction to the plan: `panic_log` uses
  `postcard` in *production*, not just tests — it encodes `Frame::Log` without
  allocating, because the panic path cannot reach the intern table or the heap.
- ~~**Step 3: `kernel-devices`**~~ **DONE** — 62 tests, dependency-free as
  predicted, and the production code is pure `core` (only virtio's *test* reaches
  for `Vec`, so `extern crate alloc` is `cfg(test)`-gated — the device logic
  allocates nothing). `tests/loom_tx.rs` moved too; both loom models still pass
  from the new home, including the buggy-twin detector-liveness check. Two lines
  edited in it (the `kernel_core::` → `kernel_devices::` path and the stale run
  command in its doc comment) — an integration test names its crate externally,
  so this is unavoidable, not the boundary bending.
- **Step 4: `kernel-boot`** — 3 modules, ~71 tests, dependency-free.
- **Step 5: `kernel-proc`** — 9 modules, 217 tests. The residual, and the only
  genuine design work: it holds the `cap`/`ipc`/`notify`/`sched` cluster whose
  internal coupling is the reason `kernel-ids` isn't needed. Its one outbound edge
  is `stack → kernel_mem::mmu::PAGE_SIZE`.
- **Step 6: the facade sweep** — delete the `pub use` lines, repoint ~200 call
  sites, delete `kernel-core`. One mechanical commit, compiler-driven.

## risks and known weaknesses

- **`#![forbid(unsafe_code)]` per crate.** Does not inherit. Forgetting it
  silently weakens the invariant that makes the crate host-testable.
- **`[lints] workspace = true` per crate.** Also does not inherit. A missed
  opt-in means pedantic silently stops running on that crate.
- **`extern crate alloc`.** Each crate that uses `alloc` must declare it.
- **`git mv`, not delete+create** — `mmu.rs`'s history includes the v0.4
  higher-half findings; keep blame intact.
- **Facade churn on `pub use`.** Re-exporting modules keeps paths stable but
  rustdoc shows them as re-exports. Cosmetic, and temporary by design.
- **Prose churn at step 6.** CLAUDE.md, `docs/`, `posts/`, and older `plans/`
  all say `kernel_core::…`. The compiler won't catch those. Grep at the end.
- **The umbrella might feel useful at step 6** and get kept out of inertia. It
  shouldn't be — see "the endpoint" for why that is the worst of both worlds.

---

# what has already landed

## the sweep: what should move *in* (DONE)

Before splitting kernel-core up, we swept `kernel/` for logic that should have
been in kernel-core all along. **The carve-out discipline had been applied
thoroughly** — 2665 lines of `kernel/` carry zero `unsafe`/`asm!`, and nearly all
of it is *glue by design*: `syscall/ipc.rs` reads a `TrapFrame`, locks
`proc.caps`, and calls `kernel_core::cap::invoke_send`; `sched/process.rs` says
outright that "the capability machinery is pure and host-tested in
`kernel_core::cap`; this module only decides where the table lives".

**The durable lesson: `unsafe`-freedom is not the test for what belongs in
kernel-core.** Glue that locks a singleton and threads a `TrapFrame` contains no
`unsafe` and still cannot run on the host. The real test is *does it touch
statics, `TrapFrame`, or MMIO* — and by that test the boundary was already clean.

Explicitly checked and rejected: `maybe_preempt` / `kill_task` (glue over
`TaskDirectory`), `heap::extend` (delegates to `next_heap_top`),
`frame::init_from_dtb` (delegates to `release_unreserved`), `fwcfg.rs` /
`virtio_console.rs` (already trait-seam adapters), `workload.rs` (`Lcg`/`bin_of`
already extracted), `obs/counter.rs` (a host test would be testing `AtomicU64`),
`percpu::current_hartid` (its famous tp-truncation bug was *codegen*, not logic —
a host test would never have caught it).

The one real residue was the opposite shape to what we looked for: not an
unsafe-free file stranded in the kernel, but **pure arithmetic embedded inside an
`unsafe`-containing function** — which a file-level search cannot find.

Two smaller finds, still open, both in `kernel/src/mem/mmu.rs`:

- `MmioRegions` (`:33-66`, ~35 lines) — pure despite the name; `insert`
  aligns-then-dedups to 2 MiB and silently clamps at 16 entries. Tests would pin
  the align/compare order and the documented drop-on-overflow.
- `satp_for` / `root_from_satp` (`:456,465`, ~10 lines) — a round-trip test pins
  the mode-shift and PPN-mask constants together; they live 10 lines apart with
  nothing asserting they agree.

Both land in `kernel-mem`, so they are code-*changes* against an already-moved
crate — fine to do any time, just never mixed into a move.

## the ELF extraction + W^X guard (DONE)

Landed as `kernel_core::user::elf::{page_perms, copy_windows, CopyWindow,
PlanError, SegmentPerms::{union, is_wx}}`; `kernel/src/trap/user.rs::load` is now
alloc/map/copy only. 15 new host tests (499 → 514); clippy clean; itests green.

Deviations from the original sketch:

- **The planner returns `SegmentPerms`, not `PtePerms`.** `SegmentPerms`'s doc
  states it "stays decoupled from `mmu::PtePerms`" — deliberately. Returning
  `PtePerms` would have broken that *and* made `kernel-proc` depend on
  `kernel-mem` for perms. So the union and the W^X check happen in `SegmentPerms`
  space, and `perms_for` (`SegmentPerms → PtePerms`) stays kernel-side.
- **No new snitch mechanism.** Every other `LoadError` already `panic!`s, and the
  panic handler emits a `Log` frame (the `kernel-panic-emits-frame` itest). A
  `PlanError::WxViolation` rides that path, so "refuse + snitch" cost one enum
  variant.

**The bug was real, and was demonstrated, not argued.** `user.ld` aligned only
`.bss`, and `.data` is empty in every program today — so W^X held *by accident*.
Temporarily adding one initialised mutable static (`static mut X: u64 =
0xDEADBEEF` → `.data`) and reverting the alignment produced, from the real
toolchain:

```
PT_LOAD R-X @ 0x10000000 (1706 bytes)   ─┐
PT_LOAD R-- @ 0x100006B0 (600 bytes)    ─┼─ all three land in page 0x10000000
PT_LOAD RW- @ 0x10000908 (8 bytes)      ─┘   union = R+W+X
```

i.e. an RWX page holding all the program's code, mapped silently. (The linker
does **not** page-align new `PT_LOAD`s here — today's `.rodata` starts mid-page
at `0x100006B0`.) The guard refused it end-to-end under snemu:

```
userspace load failed: Plan(WxViolation { page_va: 268435456 })   // 0x10000000
```

With `.data : ALIGN(0x1000)` added, the same probe binary links its RW segment at
`0x10001000` and boots clean (560 frames). Both probes reverted.

**Bonus property:** the itest suite now guards the linker script. Remove the
alignment and `init` won't load, so the default boot panics and the suite fails
loudly — the intent `user.ld` has documented since v0.7a is finally enforced by
something.

Open follow-ups:

- **`mem_size` is unbounded** — `parse` validates `file_size <= mem_size` and the
  file range, but nothing caps `mem_size`. A malicious ELF declaring
  `mem_size = 2^60` makes `page_perms` build an enormous `BTreeMap` and hang.
  Pre-existing (the old `load` had the same unbounded `pages_of`), surfaced by 4
  mutation TIMEOUTs in `pages_of`. It contradicts the module's stated "a
  malformed image yields an `ElfError`, never a panic" trust-boundary claim, and
  matters for v0.10's untrusted images. Worth its own increment.
- **`WxViolation` prints its page in decimal** (`268435456`). A `Debug` impl
  formatting `page_va` as hex would make the panic line readable at a glance.

## step 1: `kernel-mem` (DONE)

Landed exactly as scripted, and became the template for steps 2–5.
Dependency-free (`mem/` imports only `alloc`); 121 tests pass from the new crate,
kernel-core keeps 393 — **514 total, unchanged**. The facade held: **the kernel
built with zero edits to any `kernel/` source file**, and all four modules moved
as tracked renames, so blame survives. Total hand-written diff: one import
(`stack.rs`), one facade line, two Cargo.tomls, one workspace member.

Measurements are in "the cost we're accepting" above — they are the reason this
plan is now justified on factoring grounds rather than speed.

---
*On completion, `git mv` this file to `plans/legacy/` — do not delete it.*
