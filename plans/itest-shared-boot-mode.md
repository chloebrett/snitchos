# itest shared-boot mode

Let a group of integration scenarios that share an identical kernel
boot run **either** (a) against one shared boot — each scenario reading
the same recorded frame stream through its own cursor — **or** (b) on a
fresh kernel per scenario (today's model). Choose the mode at run time.

Goal: cut dev-loop wall-clock by collapsing redundant boots, **without**
losing the per-scenario isolation, flake-rate baselines, and
failure-signature capture that the separate-boot model gives us.

Non-goal: replacing the separate-boot model. Shared mode is an
additional, opt-in mode. The flake gate (`--repeat 10`) and baseline
updates keep running in separate mode.

## Why

41 scenarios today, but only ~16 *distinct* kernel boots. Two groups
dominate (audited 2026-06-13):

- **19** scenarios use `Harness::spawn` — the **identical default-demo
  kernel**. Boot / heartbeat / frame / heap / sched / smp-probe / ipi
  assertions all read frames from one boot.
- **9** scenarios use `spawn_with_workload(_, "userspace")` — the
  **identical `hello` run**. That single execution grants two caps,
  emits `telemetry=42`, opens `hello.work`, invokes a wrong-object
  handle (refused), invokes an ungranted handle (denied), yields, and
  exits. All nine scenarios' target frames come out of that one run.

So 28 of 41 scenarios re-boot QEMU to inspect disjoint frames from just
two kernels. In shared mode those 28 collapse to **2 boots**; the suite
goes from 41 boots to ~16.

The boot is the cost. Frame matching is a cheap scan of a decoded
`Vec<OwnedFrame>`. Today we pay the expensive part 41 times to do the
cheap part 41 times.

## The obstacle (why it isn't already possible)

`xtask/src/itest/harness.rs` is a **consume-once stream**. The reader
thread pushes decoded frames into an `mpsc::channel`; `wait_for` calls
`self.rx.recv_timeout` and each frame received is gone. The string table
(`StringId → name`) is built incrementally as frames drain.

Two scenarios can't share one `Harness`: the second starts wherever the
first drained the channel and misses the early frames (Hello,
ThreadRegisters) it needs.

Everything else is in our favour. Scenarios touch the kernel through a
tiny, uniform, **read-only** surface — `wait_for`, `assert_absent`,
`name_of`, `timebase_hz`. Nothing reaches into QEMU or mutates state.
That surface is the entire contract a cursor has to satisfy.

## Design: record-and-replay with per-scenario cursors

### Recorder (replaces the consume-once channel)

The reader thread appends every `OwnedFrame` to a shared, append-only
buffer instead of an mpsc channel:

```rust
struct Recorder {
    frames: Mutex<Vec<OwnedFrame>>,   // append-only; never drained
    grew: Condvar,                    // notified on each append + on close
    closed: AtomicBool,               // reader thread set this on EOF/disconnect
    strings: Mutex<StringTable>,      // built once over the full stream (monotonic)
    timebase_hz: AtomicU64,           // 0 until Hello; then the value
    // failure-capture bookkeeping (histogram, per-hart t, recent ring) moves here
}
```

The string table is **monotonic** — `StringRegister` only ever *adds*
ids, and ids are stable. So a complete table is correct for resolving
any historical frame's `name_id`. (One exception below.)

One Recorder owns one QEMU child + socket + reader thread + `Drop`
cleanup. That's the current `Harness`, minus the channel, plus the
buffer.

### View (the per-scenario cursor)

```rust
struct View<'r> {
    recorder: &'r Recorder,
    idx: usize,                       // this scenario's own position
    strings: StringTable,             // optional per-view incremental table (see exceptions)
    max_wait: (Duration, Duration),   // unchanged bookkeeping, now per-view
}
```

`View::wait_for(budget, pred)` scans `recorder.frames[idx..]`, advancing
`idx`; on reaching the end before a match it waits on `grew` up to the
deadline for more frames (or `closed`). `assert_absent`, `name_of`,
`timebase_hz` re-expressed against the same buffer. **The scenario-facing
API is identical** — only the receiver type changes (`&mut Harness` →
`&mut View`).

Independent `idx` per View means each scenario sees the whole stream
from position 0, regardless of what other Views consumed.

### How the two modes fall out

- **Separate (b)** — today's semantics. One Recorder/QEMU per scenario,
  one View at `idx 0`, run the fn. Per-scenario flake baseline +
  signature capture unchanged. **Flake-gate mode.**
- **Shared (a)** — group scenarios by declared workload string. One
  Recorder/QEMU per group; each scenario in the group gets its **own**
  View at `idx 0`; evaluate them sequentially on one worker (Vec scans
  are CPU-trivial). One boot per group: default-demo 19→1, userspace
  9→1. **Fast inner-loop / PR-check mode.**

The group's QEMU stays alive until the last View in the group returns
(or a group deadline trips), so liveness assertions ("kernel keeps
heartbeating after OOM") still hold.

The mode choice maps onto the two real use cases: shared for fast
feedback; separate for flake hunting and baselines, where isolation and
per-scenario flake rates actually matter. We keep the deflake machinery;
we just don't engage it in the fast mode.

## Declaring the boot in the catalog

Today the workload is a string buried inside each scenario fn
(`Harness::spawn_with_workload("urefuse", "userspace")`). To group by
boot, the workload must be **declared on the row**, and the fn must stop
spawning.

Extend the `scenarios!` row grammar with an optional workload token:

```
<profile> "<name>" <fn> [tags]? (workload "<name>")? ;
```

```rust
wfi "userspace-emits-telemetry" scenarios::userspace_emits_telemetry [userspace] workload "userspace";
wfi "boot-reaches-heartbeat"    scenarios::boot_reaches_heartbeat     [boot];          // no workload = default demo
```

`Scenario` gains `workload: Option<&'static str>`. The grouping key in
shared mode is exactly this field; `None` is its own group (default
demo). Singletons (storms, OOMs, tlb-shootdown) become groups of 1 —
shared mode boots them once, same as separate, no change.

Note: a scenario's label (the first arg to `spawn_with_workload`, e.g.
`"urefuse"`) was only ever a socket/log filename hint. It moves into the
runner, derived from the scenario name.

## Scenario signature migration

`fn() -> Result<(), String>` becomes `fn(&mut View) -> Result<(),
String>`. Per scenario this is mechanical:

1. Delete the `let mut h = Harness::spawn[_with_workload](...)?;` line.
2. Rename the receiver: `h.wait_for(...)` → `view.wait_for(...)` (and
   `h.name_of` / `h.timebase_hz` / `h.assert_absent`).

41 functions, rote. The workload string each used moves to its catalog
row (previous section).

## Exceptions (the actual risk surface)

1. **Same-workload-only sharing.** Only scenarios with the same
   `workload` field can share a boot. Enforced by the grouping key; no
   judgement needed.

2. **Superset requirement.** A shared workload must emit a superset of
   every grouped scenario's frames. True for default-demo and
   `userspace` today. Add a guard comment on the userspace group so a
   future scenario that needs a *different* `hello` program is forced to
   a new workload name rather than silently sharing.

3. **`pre_init_order` needs the incremental table.** It asserts
   `name_of(id).is_none()` to catch out-of-order registration — a
   complete table makes that vacuous. Fix: give each View an *optional*
   per-view incremental `StringTable` it builds as `idx` advances (the
   `strings` field above), used by this scenario instead of the
   Recorder's complete one. Cheap, and keeps the assertion meaningful in
   both modes. Alternatively tag it `separate-only` (below) — but the
   per-view table is the clean answer and most scenarios just ignore it.

4. **`separate-only` escape hatch.** A `Scenario` flag (or a `[…]` tag
   convention) marking scenarios that must not share a boot. Shared mode
   runs them as groups of 1. Reserved for anything order/incremental
   sensitive we don't want to adapt. Expected users: none after the
   per-view table; keep it as a safety valve.

5. **Infra-failure attribution coarsens in shared mode.** A QEMU death
   mid-stream fails every View in the group at once. That is correct (it
   is one failure), but it's why per-scenario flake baselines belong in
   separate mode. Document it; don't update baselines from shared runs.

## Runner + CLI

- `--shared` flag on `cargo xtask itest` (default off → separate, today's
  gate semantics preserved). Composes with `--tag` (`--tag userspace
  --shared` = the nine userspace assertions off one boot) and `--skip`.
- Shared path: partition `to_run` by `workload`, spawn one Recorder per
  group, fan groups across the existing worker pool (the unit of
  parallelism becomes the *group*), evaluate each group's Views
  sequentially within its worker.
- The Cpu/Wfi profile partition still applies at the group level: a group
  is Cpu-bound if any member is.
- Baseline writes + `--update-baseline` refuse (warn) in shared mode —
  flake rates from shared runs aren't comparable to the per-scenario
  baseline.

## Incremental delivery (TDD, each step green)

The harness has host unit tests (`cargo test -p itest-harness`) and the
scenarios run under QEMU. Recorder/View logic is host-testable without
QEMU by feeding a synthetic `Vec<OwnedFrame>`.

1. **Recorder + View, host-tested in isolation.** New module; feed it a
   hand-built frame vec + a "closed" signal. Test: two Views over one
   buffer each match independently from idx 0; `wait_for` blocks then
   wakes on append; `assert_absent` clean-elapses; timebase/name_of
   resolve. No QEMU, no scenario changes yet. (RED tests first.)
2. **Re-implement `Harness` on the Recorder, keep the old fn signature.**
   `Harness::spawn*` builds a Recorder + a single View internally and
   delegates `wait_for`/etc. to it. All 41 scenarios and the runner are
   untouched; the full QEMU suite must stay green. This proves the
   record-and-replay path is behaviour-preserving before any migration.
   Validate with `--repeat 10` (the commit gate).
3. **Add `workload` to the `scenarios!` row grammar + `Scenario`.**
   Host-test the macro expansion (extend the existing `scenarios_macro_*`
   tests). Catalog still compiles; workload field unused so far.
4. **Migrate scenarios to `fn(&mut View)` + move workloads to rows.** Do
   it one workload-group at a time, starting with userspace (9). After
   each group: full suite green in separate mode. Mechanical; the View
   API matches the old Harness API exactly.
5. **Per-view incremental table for `pre_init_order`** (exception 3).
6. **Runner: `--shared` + grouping.** Default off. Add a host test for
   the partition-by-workload grouping. Then: `cargo xtask itest --tag
   userspace --shared` boots once for all nine; compare wall-clock to the
   separate run; full-suite `--shared` green.
7. **Docs:** README flag + a short `docs/` note; a guard comment on the
   userspace group (exception 2).

Land 1–2 first and stop: that's the whole risk (does record-and-replay
preserve behaviour?) retired with zero scenario churn. 3–7 are additive.

## Risks / open questions

- **Shared-group deadline.** One group, many Views with different stream
  appetites (early frames vs the 200-sample threshold). The group QEMU
  must outlive the slowest View. Use `max(per-view budgets)` as the group
  deadline, or keep QEMU alive until all Views return. Lean: keep alive
  until all return or a hard group cap trips.
- **A shared View that wedges** shouldn't hang the group. Each
  `wait_for` keeps its own budget, so a stuck View fails on its deadline
  and the group proceeds — same as today, per-call.
- **Memory.** A full boot's frame stream buffered as `OwnedFrame`s.
  Bounded by run length; a default-demo boot is well under a few MB.
  Trivial. (The storm workloads emit the most; they're singletons.)
- **Does shared mode ever hide a bug separate mode would catch?** Yes in
  principle: a bug that only manifests under per-scenario *boot* timing
  (e.g. a boot-ordering flake) won't reproduce when 9 assertions share
  one boot. That's exactly why the flake gate stays in separate mode.
  Shared mode is for fast confirmation, not for flake hunting.

## Payoff

- Boots: 41 → ~16 in shared mode (default-demo 19→1, userspace 9→1).
- Wall-clock dominated by the slowest group's boot+drain, groups in
  parallel — a large cut for the inner loop and PR checks.
- Orthogonal to (and stacks with) the **10 Hz itest-timer** idea from the
  suite audit, which cuts each *boot's* time-to-threshold ~10×. Shared
  mode cuts boot *count*; the faster timer cuts boot *duration*.
- Separate mode unchanged: the flake gate and baselines keep their
  per-scenario isolation.

## Effort

Medium, less than it looks. Steps 1–2 (the Recorder/View + behaviour-
preserving Harness re-impl) are the conceptual core and are ~100 lines +
host tests. Step 4 is rote across 41 fns. Steps 3/6 are small. The novel
part — cursors over a shared append-only buffer — is a well-trodden
pattern. A focused day, not a weekend.
