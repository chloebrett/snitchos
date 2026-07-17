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
