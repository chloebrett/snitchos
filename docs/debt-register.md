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
- **#2 — Userspace names its own metrics.** The complaint was a layering
  inversion: the kernel owned the *names*. Fixed exactly as the entry specified —
  `RegisterMetric` (=16) is live and cap-mediated (needs a `TelemetrySink`),
  copying a name out of user memory into a per-process `MetricTable`; `EmitMetric`
  resolves a handle against *that* table alone, so a process can only emit to
  metrics it named. The entry's own motivating example is resolved in code that
  cites it: the FS server calls `register_gauge("snitchos.fs.denied")` itself
  (`user/fs/src/lib.rs`), and `kernel/src/trap/user.rs` records that it's "a plain
  bootstrap sink like every other IPC program … so the kernel no longer
  special-cases its telemetry."

  Two things that look like residue but aren't, recorded so this doesn't get
  re-filed:

  - **The ~56 names still hardcoded in `heartbeat.rs` are the kernel's *own*
    metrics** (`snitchos.heartbeat.count`, `snitchos.intern.strings_used`, the
    frame/heap/sched counters). That isn't the inversion — no userspace knows the
    intern table or the frame allocator exists, so there is nobody else to name
    them. "Mechanism in the kernel, meaning in userspace" applies to *userspace's*
    meanings.
  - **"The intern table lives in kernel memory and userspace can pressure it"
    isn't about names.** The kernel also holds `Mutex<CapTable>`, `Vec<Box<Task>>`,
    16 KiB kernel stacks and page-table frames per process, all from the kernel
    heap. The name table is one bounded item on that list — and the best-behaved
    one (16/process, reclaimed on exit by the span/metric name GC). Singling it
    out is arbitrary. The real target, if we ever want it, is an seL4-style
    **untyped-memory discipline**: the kernel allocates nothing, userspace hands
    it caps to memory it already owns and the kernel retypes them into kernel
    objects, so quotas become unnecessary rather than tuned and every object has
    an exact payer. That's a foundational redesign touching every kernel object
    and the `init` bootstrap — it wants its own entry, honestly scoped, not a
    leftover bullet here. (It would also make "who paid for this kernel object" a
    first-class observable, which is unusually on-brand.)

  Remaining nit, deliberately not opened as an entry: the kernel still maps metric
  *kinds* (`syscall/metric.rs::metric_kind_from_usize` → `Counter`/`Gauge`/
  `Histogram`). That is a passthrough to the wire enum — no aggregation, no rates,
  no interpretation — so the kernel transports the kind without acting on it. If
  "the kernel shouldn't know a gauge from a counter" ever bites, that's a small
  separate item.

- **#15 — `xtask mutants` is scopable.** `run_mutants` passed a workspace-wide
  `--features protocol/std,stitch/testing` for all ten crates at once. That
  survived only because an unscoped baseline builds every `-p` together;
  cargo-mutants narrows `cargo test` to the mutant's owning package, so any
  `-f`/`-p` filter died with *"the package 'kernel-proc' does not contain this
  feature: stitch/testing"* — i.e. the mandated MUTATE step worked only in its
  slowest, least useful mode. Now one invocation per crate from a
  `MUTANT_CRATES: &[(&str, &[&str])]` table (mirroring `UNIT_TEST_CRATES`), each
  carrying the features its *own* tests need, so the flag is always valid for the
  package it's applied to. `mutants [crate]` scopes it, matching the
  `audit <crate>` convention:
  `cargo xtask mutants kernel-proc -- -f kernel-proc/src/elf.rs`. An unknown name
  exits 2 listing the known crates. Trade-off accepted: a full unscoped sweep now
  pays ten baselines instead of one — mutation testing everything was already the
  slow path, and the scoped run is the one people actually use.

- **#12 — `elf::parse` now bounds the image's declared memory.** `mem_size` was
  unbounded, so an image declaring `2^60` made `page_perms` build a ~2^48-entry
  `BTreeMap` and hang — worse than the panic the module's trust-boundary contract
  already ruled out, and live the moment v0.10 loads an untrusted image.
  `MAX_IMAGE_MEM_SIZE` (64 MiB) now caps the **sum** of every `PT_LOAD`'s
  `mem_size` (`ElfError::ImageTooLarge`). Summing is what makes it a bound:
  `e_phnum` is a `u16`, so a per-segment limit would let 65535 segments multiply
  straight back to absurd. The running total is a `checked_add` — a legal first
  segment plus a `u64::MAX` second wraps a plain `+` back to a small value and
  slips through (tested). The bound is deliberately 4× looser than `user.ld`'s 16
  MiB region: it's a sanity bound keeping the page map ~16k entries, not a layout
  rule that breaks when the layout moves. `const _: () = assert!(MAX_IMAGE_MEM_SIZE
  >= 16 * 1024 * 1024)` ties it to the linker script at *compile* time — a tighter
  bound would reject real programs, and that surfaces as a boot panic rather than
  a red test. Mutation-clean (0 missed); `init` boots unaffected.

- **#14 — Broken intra-doc links are gated.** Rustdoc resolves `[`link`]`s but
  only *warns* on a broken one, and nothing ran rustdoc — so they rot invisibly.
  `run_unit_tests` now runs `cargo doc --no-deps` across every crate with
  `-D rustdoc::broken_intra_doc_links` (same two-target host/riscv split as
  clippy, reusing `unit_test_plan`/`riscv_only_plan`). Scoped to *broken* links,
  not `-D warnings`: the private-intra-doc-link class (a public item linking to a
  private one — real target, just doesn't render) is cosmetic and fires ~10× in
  `snemu`; the rot worth gating is a link to a symbol that *doesn't exist*.
  Clearing the backlog to turn it on found the real prize: **`[`span_start`]` and
  `[`span_open_owned`]` had outlived the functions by several renames**
  (`span_start_id`, `span_open_bounded`) — the prose lied and nothing noticed.
  Also four `crate::`-relative links the kernel-core split dangled (their modules
  moved to other crates), plus mechanical `VPN[2]`-parsed-as-a-link cases.
- **#13 — `MmioRegions` + the `satp` encode/decode are host-tested.** Both moved
  to `kernel-mem`; `kernel/src/mem/mmu.rs` keeps only what touches hardware (the
  CSR read/write and the boot-table construction). 9 new tests. The find that
  justified it on its own: **`satp_for` was open-coded a second time inside
  `mmu::enable`** — the same mode-shift/PPN encode written twice, either of which
  could have been fixed without the other. One host-tested source now.
  `root_from_satp` is the named inverse (was an anonymous `PPN_MASK` inside
  `current_satp_root`), and the round-trip test pins the two constants *together*
  — they sat 10 lines apart with nothing asserting they agreed, and a mismatch
  silently activates the wrong address space rather than failing. `MEGAPAGE_SIZE`
  is derived as `512 * PAGE_SIZE` (the table geometry) rather than a `2 * 1024 *
  1024` literal, so it can't drift. `MmioRegions::insert` aligns *then* compares
  (two devices in one megapage → one boot leaf, which is exactly QEMU `virt`'s
  UART + virtio-mmio slots), and its silent drop past 16 is now pinned by a test
  instead of promised by a comment — it's silent by design, since it runs pre-MMU
  where there is nowhere to report. One documented equivalent mutant (`|`→`^` in
  `satp_for`: the mode and PPN fields are disjoint, so no test can tell them
  apart).
- **#7 — Capability generation is load-bearing; revocation shipped.** The entry
  said `generation` was "dead-weight at 0" and `Stale`-on-revoke "unbuilt" —
  both stale. `CapTable::consume` bumps the generation (the single-use reply-cap
  path), `revoke_by_cap_id` bumps it to reclaim a grant in *another* process's
  table, and `CapError::Stale` is what a dead handle resolves to. On top of that
  primitive: the transitive `Revoke` syscall (=28, by handle),
  `CapEvent::Revoked`, `sched::revoke_descendants_of`'s cross-table
  derivation-tree walk over `parent_cap_id`, `Endpoint::revoke_derived`, and the
  `revoke-reclaims-a-minted-cap` itest. The Stitch shell's `hold`/`grant`/`revoke`
  verbs close grant→use→reclaim end to end.
- **#6 — Fault-safe user-copy.** `copy_from_user` only bounds-checked the user
  range (`user_range_ok`), so an in-range-but-unmapped pointer faulted the kernel
  on the `SUM` deref. Added `kernel_mem::mmu::range_mapped` (host-tested page-walk,
  reusing `translate`) + the `kernel::mmu::user_range_readable` wrapper; the copy
  now refuses (`BadUserRange`) instead of faulting. Proven end-to-end by the
  `userspace-bad-ptr` itest (a new `bad-ptr` probe program passes an unmapped VA
  to `DebugWrite`; the kernel refuses and the process survives).

## Correctness gaps

### #16 — Userspace pinned to opt-1 to dodge a UB class *(latent, hard)*

`kernel/build.rs` builds the embedded userspace with
`--config profile.release.opt-level=1` because there's a latent opt≥2 UB class in
the userspace crates (talc OOM-loop → hang; confirmed in `snitchos-user`, at
least one more crate). The itest speedup is kernel-dominated, so the pin costs
~nothing — which is exactly why it stays. The pin is the workaround; the UB is
the debt. Repro: `cargo xtask itest --opt high`.

## Deferred placeholders (Tier 3)

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
