# Technical / architectural debt register

A living backlog of elegance / architectural debt surfaced during the v0.10
work. Each item is independently actionable. Ordered by leverage, not urgency.

Delete an item when it's done; add one when you find it. This is a register, not
a plan — see `plans/` for active implementation tracks.

---

## Done

- **#1 — Program/workload registry.** The six parallel enumerations of
  userspace programs (build.rs embeds, ELF statics, 18 `*_main_entry` fns,
  the spawn match, the heartbeat no-storm arm) collapsed into: a `USER_PROGRAMS`
  manifest loop in `build.rs`, a `ProgramSpec` table + one generic
  `program_entry` (carried via a new per-task scheduler `arg` word), a
  `WorkloadKind → LAYOUTS` table, and the `is_storm()` heartbeat guard. Adding a
  program is now a manifest row + an ELF static + a spec + a layout row.
- **#4 — Shared FS test markers.** The `0x57A7`-style sentinels duplicated
  between `user/fs` and the itest scenarios moved into `fs_proto::markers`.
- **#5 — `FsError::Unsupported` overloading.** `fs-server` mapped copy / mint /
  decode failures to `Unsupported` (which means "op not implemented"). Added
  `FsError::Internal` (wire status 8) for genuine internal/transport failures.
- **#3 — Deferred-counter abstraction.** Introduced `kernel::counter::DeferredCounter`
  (atomic + wire name + interned `StringId`) and a `COUNTERS` registry. Converted
  26 counters across 9 subsystems (frame/heap/sched/ipc/demo_tasks/workload/ipi/
  mmu/secondary) from bare `AtomicU64` to `DeferredCounter`; the heartbeat's 5
  counter-draining functions collapsed into one `counter::drain_all()` call. Gauges
  (sampled state), histograms, the `Acquire`-ordered `workload.samples_consumed`
  oracle, and the storm counters stay bespoke. Adding a counter is now a
  `DeferredCounter` declaration + a registry row.

---

## High-leverage

### #2 — Push the observability vocabulary out of the kernel *(architectural, large)*

The kernel still owns the *names* of metrics — a layering inversion (it's the
one place "mechanism in the kernel, meaning in userspace" isn't applied).

- ~60 metric names are hardcoded in `kernel/src/obs/heartbeat.rs` (the
  `define_metrics!` block).
- The **intern table lives in kernel memory** (`kernel/src/obs/tracing.rs`) and
  hosts *userspace-defined* span/metric names. The `Process::MAX_SPAN_NAMES`
  quota is a band-aid on that — userspace can pressure a kernel resource.

**End-state:** userspace names its own metrics via a `RegisterMetric` syscall
that copies a name from user memory and interns it — the kernel **already does
exactly this for span names** (`SpanOpen` under `user_range_ok`), so it's a copy
of an existing path. The kernel then transports opaque metric frames and knows
nothing of names or kinds. The v0.10 FS denial gauge had to be kernel-registered
(`snitchos.fs.denied`) precisely because this doesn't exist yet — that's the
motivating example. Protocol-level change; a milestone, not an afternoon. See
the `project_userspace_defined_metrics` memory.

---

## Done (cont.)

- **#6 — Fault-safe user-copy.** `copy_from_user` only bounds-checked the user
  range (`user_range_ok`), so an in-range-but-unmapped pointer faulted the kernel
  on the `SUM` deref. Added `kernel_mem::mmu::range_mapped` (host-tested page-walk,
  reusing `translate`) + the `kernel::mmu::user_range_readable` wrapper; the copy
  now refuses (`BadUserRange`) instead of faulting. Proven end-to-end by the
  `userspace-bad-ptr` itest (a new `bad-ptr` probe program passes an unmapped VA
  to `DebugWrite`; the kernel refuses and the process survives).

## Correctness gaps

### #12 — `elf::parse` doesn't bound `mem_size` *(security, small)*

`kernel-proc/src/elf.rs` validates `file_size <= mem_size` and that the file
range is in-bounds, but **nothing caps `mem_size`**. An image declaring
`mem_size = 2^60` makes `elf::page_perms` build a `BTreeMap` with ~2^48 entries
— the kernel hangs (or OOMs) before ever allocating a frame.

This contradicts the module's own stated contract: *"a trust boundary … every
field is validated and a malformed image yields an `ElfError`, never a panic."*
A hang is worse than a panic. Pre-existing (the old `load` had the same unbounded
`pages_of`), and surfaced by 4 mutation TIMEOUTs in `pages_of` — the mutants that
enlarge the page range don't fail, they just never finish, which is the same
shape as the bug.

Live the moment v0.10 loads an image from the filesystem rather than an embedded
one. Fix is a bound + a test; sits naturally beside the W^X guard already in
`page_perms`.

### #13 — `MmioRegions` + `satp` encode/decode are still kernel-side *(small)*

Both are pure and host-testable, found by the `kernel/` sweep during the
kernel-core split and never extracted (see `plans/legacy/kernel-core-split.md`):

- `MmioRegions` (`kernel/src/mem/mmu.rs:33-66`, ~35 lines) — aligns-then-dedups
  to 2 MiB and **silently clamps at 16 entries**. Tests would pin the
  align/compare order (flip it and two devices in one 2 MiB region burn two boot
  page-table slots) and the documented drop-on-overflow.
- `satp_for` / the PPN decode in `current_satp_root` (`:456,465`, ~10 lines) — a
  round-trip test would pin the mode-shift and PPN-mask constants *together*.
  They live 10 lines apart today with nothing asserting they agree; a wrong shift
  silently loads the wrong address space.

Both land in `kernel-mem`.

## Tooling gaps

### #14 — `cargo doc` isn't in the gate *(small)*

Broken intra-doc links rot silently: `kernel-obs/src/intern.rs` has two
(`[`register_or_lookup`]`, `[`release`]` — bare method links need `Self::`) that
have presumably been dead for a while, because nothing runs `cargo doc`. Adding
it to `xtask test` catches the class rather than the instances. Expect a first
pass to surface a backlog.

### #15 — `xtask mutants` can't be scoped to one crate *(small)*

`run_mutants` passes `--features protocol/std,stitch/testing` for the whole crate
set. cargo-mutants narrows to the owning package per mutant, so any `-f`/`-p`
filter fails the baseline with *"the package 'kernel-proc' does not contain this
feature: stitch/testing"*. Whole-workspace runs are fine; scoping to one file —
the normal way to use it during the mandated MUTATE step — is not. Worked around
today by invoking `cargo mutants -p <crate> -f <file>` directly.

### #16 — Userspace pinned to opt-1 to dodge a UB class *(latent, hard)*

`kernel/build.rs` builds the embedded userspace with
`--config profile.release.opt-level=1` because there's a latent opt≥2 UB class in
the userspace crates (talc OOM-loop → hang; confirmed in `snitchos-user`, at
least one more crate). The itest speedup is kernel-dominated, so the pin costs
~nothing — but it's a real bug being routed around, not fixed. Repro:
`cargo xtask snemu-itest --opt high`.

## Deferred placeholders (Tier 3)

### #7 — Capability generation / revocation

`Capability.generation` exists as the revocation hook but is dead-weight at 0
(`kernel-proc/src/cap.rs`); `Stale`-on-revoke is unbuilt.

### #8 — `kernel::sync` is one-flavor

No `lock` vs `lock_irqsave` split (`kernel/src/smp/sync.rs`); deferred until a
hot path proves it needs the distinction.

### #9 — `TX_STAGING` virtio staging hack

`virtio_console::send` stages frame bytes through a static buffer only because
`mmu::va_to_pa` handles a single VA range (`KERNEL_OFFSET`); a general
DMA-address translation would remove the staging.

### #10 — Hardcoded QEMU-`virt` MMIO + parked DTB walk

MMIO regions are hardcoded for QEMU `virt` in `kmain`; the DTB-driven
`collect_mmio_regions` is parked behind `#[expect(dead_code)]` (the pre-MMU DTB
crash under the higher-half link was never isolated).

### #11 — `Exit` vs `Yield` wire distinction

Tasks exit, but the wire only carries `Yield`-shaped context-switch frames
(noted in `kernel/src/sched/mod.rs`); a dedicated `Exit` reason is deferred.
