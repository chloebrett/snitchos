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
- **#11 — `Exit` vs `Yield` is on the wire (entry was stale).** The entry claimed
  "the wire only carries `Yield`-shaped context-switch frames" — false. `exit_now`
  passes `SwitchReason::Exit` to `prepare_switch` (`sched/mod.rs:1194`), which
  emits it via `emit_context_switch` (`:1056`), and the `sched-task-exits-cleanly`
  itest asserts a `ContextSwitch{Exit}` reaches the wire, distinct from a yield.
  What the entry *should* have said, now the only residue: the **collector**
  doesn't read the reason (`state.rs` never matches `SwitchReason`), so an exit
  and a yield produce identical OTLP today. That's a host-side feature ("surface
  task lifecycle in the trace view"), not the kernel-wire debt this described —
  file it there if wanted.

## Correctness gaps

### #16 — ROOT-CAUSED 2026-07-29: the spawn/reap half was never UB at all.

**The diagnosis below (kept in full underneath) was wrong for the failure that
still reproduced, and wrong in the direction that made it look hard.** The only
failure left on the current tree was `memhog` losing its allocation to ordinary,
*correct* LLVM optimisation — not UB, and not in the kernel. (The entry bundled a
second, older FS-path symptom that no longer reproduces; the two are separated
below, because only one of them is actually closed.)

`memhog` reserves 4 MiB to make the kernel commit ~1024 frames, then exits so the
reaper can prove Exit reclaims them. It guarded the reservation like this:

```rust
let buf = Vec::<u8>::with_capacity(4 * 1024 * 1024);
// Read the capacity back so the reservation can't be optimized away.
exit_with((buf.capacity() != 0) as i32);
```

That guard does nothing. `capacity()` is a field load LLVM constant-folds back to
the requested size, so it never observes the *allocation*; and Rust's allocator
calls carry LLVM's allocator attributes, which make them removable when the
result is unused. So at opt≥2 the allocation is dead code and gets deleted.

**Measured, not inferred** — `rust-objdump -d` on `memhog` built at each level:

| build | `li a7, 0x4` (`MapAnon`) | `ecall` count |
|---|---|---|
| `--config profile.release.opt-level=1` | present at `0x1000089a` | 6 |
| `--config profile.release.opt-level=2` | **absent** | 5 |

The child never called `MapAnon`, so it committed no frames, so there was nothing
to reclaim and `snitchos.frames.freed_total` never reached the scenario's 5000.

**The failure was misread, and the misreading is the lesson.** The register said
"a userspace task is stuck in a loop" and prescribed finding the hot userspace PC.
But `cargo xtask itest --opt hi spawn-reclaims-memory` fails on its **second**
assertion — the first, `reaper.done`, passes, which means the reaper completed all
15 spawn/wait cycles. Nothing was spinning. `snemu profile --opt hi --user-detail`
agrees: no `[user:…]` bucket appears in the top 30 at all, and userspace accounts
for under 1% of 407M instructions. The 91M-instret observation was the kernel
context-switching and serializing telemetry, not a userspace spin.

**Fix**: `core::hint::black_box(buf.as_ptr())` — make the pointer escape.
One line, in a test program; no kernel change.

**Result after the fix** (each run rebuilds the embedded userspace at that level):

| run | before | after |
|---|---|---|
| `itest --opt hi spawn-reclaims-memory` | FAIL (62.0s) | pass (2.0s) |
| `itest --opt hi` (whole suite) | — | **130/130** |
| `itest --opt max` (whole suite) | — | **130/130** |
| `itest --engine qemu --opt max spawn-reclaims-memory` | documented `hart_stalled` | pass (0.8s) |

The QEMU row is the one that closes it: QEMU was the oracle the old entry used to
argue "genuine UB, not a snemu artifact," and it now passes at opt-3.

**There were two distinct symptoms under this one entry, and they need separating
— only one is closed by the above.**

- **spawn/reap — CLOSED.** `spawn-reclaims-memory` is the `memhog` dead allocation,
  root-caused and fixed above. This is also the one the original bisect could not
  explain: pinning `snitchos-user` alone fixed the FS scenarios but left this
  failing, which is what produced the "at least one more UB in another crate"
  conclusion. There was no other UB — the second crate was `user/hello`, and its
  "UB" was a test program whose allocation was legal to delete.
- **FS path — NOT reproducing, but not disproven either.** The `build.rs` comment's
  version (talc's OOM handler looping on 68 KiB regions until the per-process cap,
  then hanging in the panic handler) was a real observation, bisected at the time to
  `snitchos-user`. The arithmetic checks out — `MmapOnOom` maps
  `size.next_multiple_of(PAGE_SIZE) + MIN_MAP`, which is exactly 68 KiB for a small
  layout — and talc's `malloc` really is
  `loop { get_sufficient_chunk(…) | handle_oom(…)? }`, so the loop is structurally
  possible. It simply does not happen today: every FS scenario is green at opt-2 and
  opt-3. Most likely fixed by unrelated work in the ~3 weeks since the pin
  (2026-07-10) and never re-checked. **Treat as latent, not dead** — if it ever
  returns, the loop above is where to look, and the tell is a flood of 68 KiB
  `MapAnon`s.
- *"`supervised-ipc-client-cap-survives` also fails."* Passes at opt-2 and is green
  in both full-suite runs. It shares no code with `memhog`; presumably the same
  silent fix as the FS half.

**A hypothesis eliminated along the way, worth not re-testing.** The natural
suspect was an under-declared inline-asm register — the same class as the kernel's
SBI `a1` clobber, which is release-only and opt-sensitive, and there are 22
grandfathered hand-rolled `ecall` wrappers. Checked exhaustively: every handler
that writes `a1`–`a6` back (`handle_receive`, `handle_call`, `handle_wait_any`,
`handle_span_open`, `handle_wait_notify`, `handle_spawn`/`_on`/`_image`) has a
wrapper that either routes through the all-`inlateout` `ecall` helper or declares
that register `inlateout` explicitly. No false promise anywhere. The raw-ecall
ratchet is doing its job.

**What is left, and it is a decision rather than a bug.** The pin still stands
(`kernel/build.rs:178`, default `"1"`), and is now unjustified by any *evidence*.
It is not, however, a one-line removal, and the reasons are worth having written
down before someone deletes the line on a Friday.

### Who the pin actually affects

Narrower than it looks, and I overstated this in the first draft of this entry.
The pin is inside `if profile == "release"`, so it only touches release kernel
builds. Concretely:

| path | regime | pinned? |
|---|---|---|
| `cargo xtask itest` (default) | `Mid` | **yes** — this is the pin's real consumer |
| `cargo xtask itest --opt hi` / `--opt max` | `Hi` / `Max` | no (overridden to 2 / 3) |
| `cargo xtask boot`, `snemu boot` | `Low` (debug) | no |
| **`cargo xtask image` (the VF2 board image)** | `Low` (debug) | **no** |

That last row is the correction. `image()` calls `qemu::build_kernel`, which is
`build_kernel_profiled(features, OptLevel::Low)` — a *debug* build. So the board
has never taken the pinned path, and "unpinning changes what ships to the VF2" was
wrong. The board is out of scope for this decision **today**; it re-enters the
moment anyone builds a release image, which is also the moment opt-3 userspace
becomes new-on-hardware. (That the board runs a debug image is its own standing
issue — it is what hid the SBI `a1` clobber for weeks. See post 68.)

### Preconditions for unpinning

1. **Make `Mid` force opt-1 explicitly, before removing the default.** `Mid` is
   currently defined by *inheriting* `build.rs`'s pin —
   `userspace_opt_override()` returns `None` for `Low`/`Mid`. Delete the pin and
   `Mid` silently becomes opt-3, i.e. `Max`, and the ladder collapses from four
   rungs to three. That would throw away the exact discrimination that closed this
   entry: "opt-2 already broke it" vs "only opt-3 breaks it" is what made the
   `memhog` bisect cheap. Fix: `OptLevel::Mid => Some("1")`, so the ladder stays
   monotonic Low(0) → Mid(1) → Hi(2) → Max(3) and the pin becomes a *selectable
   regime* rather than an invisible default. Do this first, as its own change; it
   is behaviour-preserving and independently correct.
2. **Decide what exercises the level nobody defaults to.** There is no CI in this
   repo (no `.github/workflows`) — the gate is whatever a human runs. Today the
   default gate run is `Mid`, so opt-1 is exercised constantly and opt-2/3 only
   on demand. Unpinning inverts that: opt-3 becomes continuously exercised and
   **opt-1 stops being run at all**, so `Mid` rots the way any untested regime
   does. Either accept that and say so, or keep a periodic `--opt mid` pass.
3. **Guard the symptom that is latent rather than dead.** The FS-path talc OOM
   flood (above) was real, was bisected to `snitchos-user`, and simply does not
   reproduce now — nobody found or fixed it, it stopped happening. Nothing watches
   for its return, and the pin is currently the thing that would mask it. Its tell
   is a flood of 68 KiB `MapAnon`s, so the cheap standing guard is a bound on
   per-process `MapAnon` count in the FS scenarios. Worth having *before* removing
   the workaround, not after.
4. **Re-verify on the board when, and only when, a release image exists.** Not a
   blocker now (see the table). It becomes one the same day `image()` stops
   building debug.

Steps 1 and 3 are small and independently worth doing. Step 2 is the actual
decision. The `--config` line in `kernel/build.rs` is the last thing to touch, not
the first.

<details>
<summary>The original entry, kept because the misdiagnosis is instructive</summary>

### #16 — Userspace pinned to opt-1 to dodge a UB class *(latent, hard)*

`kernel/build.rs` pins the embedded userspace to `opt-level=1`
(`profile.release.opt-level`, default `1`, overridable via
`SNITCHOS_USERSPACE_OPT`) because there's a latent opt≥2 UB in the userspace
crates. The itest speedup is kernel-dominated, so the pin costs ~nothing — which
is exactly why it stays. The pin is the workaround; the UB is the debt. Repro:
`cargo xtask itest --opt hi spawn-reclaims-memory` (`hi` = opt-2, the minimal
level that reproduces; `max` = opt-3).

**Narrowed 2026-07-18.** Exactly **two** scenarios fail:
`spawn-reclaims-memory` and `supervised-ipc-client-cap-survives` (both
userspace-heap-heavy) — the rest of the 120 boot and pass. It is **not** a total
"nothing boots" failure; that description belonged to snemu `--scramble`, a
different thing.

**It is genuine userspace UB, not a snemu artifact — confirmed on the QEMU
oracle.** `spawn-reclaims-memory --opt max --engine qemu` boots ("I am alive",
"entering heartbeat") then `hart_stalled`. QEMU has real RISC-V semantics and
never had snemu's memory-straddle bug (fixed in `657afa0`, which `--scramble`
stress-tests), so a hang there rules out the emulator. The straddle fix landing a
week after the pin (2026-07-10 pin, 2026-07-17 fix) was coincidence, not cause —
tested and rejected, not assumed.

**It's a spin, not a fault** (2026-07-18): `snemu boot --workload spawn-reap` at
opt≥2 runs 91M instret with the kernel still heartbeating and *no* `scause`/
`sepc`/panic — a userspace task is stuck in a loop, not faulting. So fault-report
tooling won't help; the locator is the profiler.

**opt-2 already reproduces it** — so bisect opt-1→opt-2 (few transforms) not
opt-1→opt-3 (noisy). Tooling now supports this directly: `--opt hi` (opt-2) /
`--opt max` (opt-3) are first-class levels, and `cargo xtask snemu profile --opt
hi --user-detail --workload spawn-reap` splits userspace per-PC so the spin-loop's
address surfaces. Next step: read the hot PC off that profile → objdump the owning
program → compare that one function's codegen at opt-1 vs opt-2. "Terminates at
-O1, spins at -O2" is the classic signature of UB the optimiser exploits, so the
source is likely fixable (pin comes off), not a compiler bug to route around.

</details>

**Postscript on the method.** The prescribed next step — objdump the owning program
at opt-1 vs opt-2 — was exactly right, and is what found it. What sent it wrong for
three weeks was the *symptom* it was pointed at: "spins at -O2" was inferred from an
instret count rather than read off the frame stream or the profile, and the profile,
once actually run, said the opposite. The classifier ("UB the optimiser exploits")
was then chosen to fit the wrong symptom, and it made the work look like a hunt for
unsound code when a diff of two disassemblies would have closed it. Read the wire
before believing the summary — the same lesson `plans/repl-completion.md` records
for the FP wedge, arrived at independently.

## Deferred placeholders (Tier 3)

### #17 — The canon's native tests never run on target

`test`/`expect` shipped and the canon carries 89 native tests (plus 279 across
`examples/stitch/`), but every one of them runs on the **host** — `canon.rs`
drives `stitch::test_runner` under `cargo xtask test` and nothing else does.

The gap is a claim, not a crash. The canon stratum's justification in
[generative-ladder.md](generative-ladder.md) is that these programs are
*validated by use* — shipped in `fs-image/`, run in itests, continuously
re-validated by the whole gate. That holds for the programs and **not for their
test suites**, which no booted kernel has ever executed. So a canon suite could
depend on host-only behaviour (or on a native the target lacks) and stay green
forever.

Closing it is increment 9 of
[../plans/stitch-native-tests.md](../plans/stitch-native-tests.md): a span per
test and an event per assertion (the collector already decodes those frames, so
the runner becomes a collector and results land beside the kernel's own spans), a
`stitch test` verb, and one itest scenario running the canon's suites under a
booted kernel. The runner was deliberately built as a pure function over parsed
items — no I/O, no printing, no exit — precisely so it can run where there is no
stdout.

Measured cost of the tests that already ship in the image (`prelude.st`, which is
parsed at every program start, on target too): source 3505 → 8456 bytes, parse
517µs → 725µs (+40%), `build_env` unchanged at ~135µs — `Item::Test` lowers to a
`CoreItem::Test` that binds no name, so the cost is parse-only. Stripping tests
from the metal build is therefore an optimisation, not a prerequisite.

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

### #18 — No console mode for "text, but the kernel keeps quiet"

The board's `hb {count}` liveness pulse interleaves with any workload that owns the
console. Measured on hardware 2026-07-29: typing `// hello` at the `stitch-drivel`
prompt rendered as `// he` + `hb 7` + `llo`.

`heartbeat.rs` already predicts this and prescribes `console=frames` — which is
**wrong advice on the board**, because the collector has no serial source (it takes a
QEMU socket, `--replay`, or `--udp`), so frames mode over a UART is unreadable in a
terminal. Text mode is the only usable mode for an interactive workload on hardware,
and it is the one that shreds it.

Decided but unimplemented: a third `ConsoleMode::Quiet` — human text still flows, the
kernel emits no chatter of its own — leaving `Text` as the bring-up mode that keeps the
pulse. Alternatives considered and rejected: an `hb=off` bootarg (another knob), and
suppressing automatically when the selected workload owns the console (loses the pulse
during bring-up of exactly those workloads). Roughly a variant plus a parser arm in
`kernel-boot` and the two match sites in `kernel/src/device/console.rs`.

The demo was unblocked with a temporary `if false &&` on the print, since reverted —
do not reach for that again, it silences the pulse on every `vf2` build in every mode.

### #19 — `cargo xtask image` has no `--opt`, so board images are always debug

`image()` calls `qemu::build_kernel`, which is hardcoded to `OptLevel::Low` — a debug
kernel with the `build.rs` default opt-1 userspace. Every syscall and trap around a
workload runs unoptimised, which is felt directly by anything compute-bound on the
board: a drivel Tab completion is a transformer forward pass wrapped in debug-build
kernel overhead.

An opt-3 image can be produced by hand (`SNITCHOS_USERSPACE_OPT=3 cargo build -p kernel
--target riscv64gc-unknown-none-elf --release --features vf2,<workload features>` then
`rust-objcopy -O binary … snitchos.img`) — done 2026-07-29, 7.78 MB → 6.13 MB — but it
is **not reproducible through the tool, and any later `cargo xtask image` silently
overwrites it with a debug build.**

The fix is an `--opt` flag threaded to `build_kernel_profiled`, mirroring the ladder
`itest` already has. It sits naturally beside `image_features`. Note this would make
release `vf2` images routine, and that regime is where both the `tp`-truncation and the
SBI `a1`-clobber bugs lived — the latter *hidden* precisely because board images were
debug builds. Worth landing deliberately rather than as a convenience.

