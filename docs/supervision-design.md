# 🌳 Supervision-tree design

*Supervision is capability ownership viewed twice. The supervisor owns the durable objects; services borrow authority from it and are restartable precisely because of that. Every transition is a span.*

**Status: design only — not built, but now UNBLOCKED (manifest v2 shipped).** This is
redesign item **#6** from [redesign-from-scratch.md](redesign-from-scratch.md). v1
(crash-restart) is achievable entirely on shipped primitives (`Spawn`, `Wait`/`WaitAny`,
`EndpointCreate`, `Notify`, caps + `Revoke`, the clock); v2 (health + graceful shutdown)
names two new ones. No supervision code exists yet — but its hardest part does (below).

**The reframe (updated 2026-07-11, after manifest v2 shipped).** The supervisor is, in
the terms of [design-explorations-seven-questions.md](design-explorations-seven-questions.md)
Q1, a *manifest satisfier* — it reads each service's declared authority requirements and
grants them from its own caps. That was the stated blocker: build on the manifest-v2
`Slot` language, not the positional `delegated_handle(i)` contract. **Manifest v2 is now
shipped** ([manifest-design.md](manifest-design.md), plan archived in `plans/legacy/`),
and with it the re-grant mechanism this doc treats as the heart of restart:

- `hitch::satisfy(needs, have) -> Vec<Grant>` (pure, host-tested, mutation-clean) maps a
  child's declared `needs` to the supervisor's own caps — `Use` for an exact match,
  `Mint` to attenuate a wider cap down to a narrower slot, `Unsatisfied` to refuse.
- `user/fs/src/bin/satisfier.rs::process(child)` is the working userspace loop around it:
  read a child's `needs` off its manifest, `satisfy` against held caps, delegate the
  results **in slot order via the unchanged `Spawn`/`SpawnImage` handle array** (there is
  **no `BootInfo` page** — manifest v2 delivers by *manifest-as-index*: the child resolves
  authority by role via `bootstrap.get::<Endpoint>("fs")` against the macro-emitted
  `__SNITCH_SLOTS` table). Everywhere below that says "grant a cap" or references a
  `BootInfo` page, read: `process(child)` — re-run per incarnation.

So v1 supervision = **`process(child)` (built) + a `WaitAny` loop + `restart_decision`
(pure, not yet built) + the transition telemetry.** The silent-failure-prone part
(data-driven cap re-grant) is done and tested; restart is just calling `process` again
against the new incarnation.

**On the differential-oracle prerequisite — the oracle already ships.** The 2026-07-05
sequencing filed #6 behind the Tier-2 differential oracle, because cap re-grant is
silent-failure-shaped (a restarted service that doesn't receive its re-delegated cap
leaves a client hanging with no error). **Resolved (2026-07-11):** the oracle for *this*
invariant is `cap_list` (syscall 27, shipped, `snitchos_user::cap_list`, backs the shell's
`hold`): a process enumerates its **own** cap table. That is the snitch-on-the-snitch —
the kernel's `CapEvent::Transferred` is the supervisor's *claim* ("re-granted `fs` to
incarnation 3"); `cap_list` from the restarted service is the holder's *independent report*
of what it actually holds. Cross-checking them is precisely axis-3's "verify the restarted
service holds the caps the supervisor claims it granted," at cap-holding granularity, with
both halves already on the wire. The snemu-scale frame-diffing oracle is a *separate,
general* thing #6 does not need. So v1 is not gated on new oracle work — it uses `cap_list`
in its acceptance itest (step 4).

Related: [manifest-design.md](manifest-design.md) (the `Slot` language this consumes —
**shipped**), [capability-system-design.md](capability-system-design.md),
[ipc-design.md](ipc-design.md), [notification-design.md](notification-design.md),
[observability-design.md](observability-design.md), and the actor-model reframe in
[userland-text-streams-and-the-actor-model-design.md](userland-text-streams-and-the-actor-model-design.md).

---

## The problem: supervision is ad-hoc today

"Supervision" in SnitchOS today is one-off loops hardcoded into bespoke programs:

- `init` (`user/hello/src/bin/init.rs`) `EndpointCreate`s an endpoint, `Spawn`s the
  FS server + a client, and `WaitAny`s forever.
- `reaper` (`workload=spawn-reap`) spawns + `Wait`s a `memhog` child 30× in a loop.
- `supervisor` (`workload=wait-any`) spawns a `spinner` + a `spawnee`, `WaitAny`s
  once, emits the reaped id + status.

Each encodes *what runs, in what order, and what to do when it dies* as **code**.
There is no notion of a service, no dependency ordering, no restart policy, no
health, no shutdown. #6 makes that knowledge **data** and `init` a generic
**engine** over it.

## Core principle: supervision = capability ownership, viewed twice

This is the insight that makes SnitchOS's supervision more than a systemd clone,
and it turns a current wart into the mechanism.

When a service dies and restarts, the new instance is a **fresh process with a
fresh `CapTable`**. Two problems follow: the new instance needs its authority
back, and clients holding caps to the *old* instance must not be orphaned. The
clean answer is an invariant:

> **The supervisor owns the durable objects; services borrow authority from it for
> the lifetime of one incarnation.**

Concretely — today `init` `EndpointCreate`s the FS endpoint and delegates
`RECV | MINT` to the server, and [the v0.13 notes](redesign-from-scratch.md) flag
that init "over-holds `RECV`" as a wart. Under supervision that over-hold becomes
**the feature**: because *init* owns the endpoint object, when the FS server
crashes, init re-spawns it and re-delegates `RECV` to the new instance — and every
client's minted `SEND` cap still names the **same endpoint object**, so clients
survive the restart transparently. Services are restartable *because* their
authority is granted by the supervisor and outlives them. Capability ownership and
supervision are the same tree.

This also composes: a supervisor can supervise sub-supervisors (each owning its
subtree's durable objects), giving the recursive supervision tree. v1 is a single
level under `init`; the recursion is the general form.

---

## Three decisions this design pins

Everything else follows from these. Recommended choices, with rationale.

### D1 — Restart strategy set: one-for-one + rest-for-one (defer one-for-all)

The Erlang taxonomy for "a child died, what do we restart":

- **one-for-one** — restart just the dead child. The default; correct when services
  are independent.
- **rest-for-one** — restart the dead child *and everything started after it* (its
  dependents in start order). Correct when a downstream service can't survive its
  upstream restarting (it re-delegated caps, re-opened connections).
- **one-for-all** — restart the whole group on any death. Rarely needed once caps
  make dependencies explicit; **deferred**.

**Choice:** ship one-for-one + rest-for-one. rest-for-one falls out of the
dependency order we already compute, and it's exactly what the cap-re-grant story
needs (a dependent that cached a delegated handle to the old incarnation must
restart to pick up the new one). one-for-all adds a strategy without a motivating
case yet.

### D2 — Readiness: "spawned" by default, opt-in "signaled-ready"

A dependent must not start until its dependency is *usable*, not merely *spawned*.
Two tiers:

- **spawned** (default) — the dependency is considered ready the instant `Spawn`
  returns a task id. Fine for services with no startup work.
- **signaled-ready** (opt-in) — the service does startup work, then `Signal`s a
  readiness [notification](notification-design.md) the supervisor holds the `WAIT`
  end of. The supervisor blocks on it before starting dependents. This is
  systemd's `Type=notify`, and SnitchOS already has the exact primitive
  (`NotifyCreate`/`Signal`/`WaitNotify`, v0.12).

**Choice:** support both; `spawned` is the default, `signaled-ready` is a per-service
opt-in flag that names a readiness notification the supervisor creates and delegates
the `SIGNAL` end of. No new mechanism required.

### D3 — Cap re-grant model: supervisor-owns-endpoints (the invariant above)

Durable objects (endpoints, notifications, memory regions) are created **once** by
the supervisor and owned by it. A service receives *delegated handles* each
incarnation. On restart, the supervisor re-runs the same delegation against the new
`CapTable`. This is what makes D1's rest-for-one and D2's readiness coherent, and it
is the concrete resolution of the init over-hold wart.

Corollary: a service must **not** `EndpointCreate` an object other services depend
on — that object would die with it. Services may still create *private* objects
(their own scratch notifications). The supervisor owns everything that crosses a
service boundary.

---

## The service spec (the data model)

Pure data, host-testable, living in `kernel-core` alongside `sched::Runqueue` and
`bootargs` (the same "policy logic with no MMIO/CSRs" tier). Sketch:

```rust
struct ServiceSpec {
    name: &'static str,
    program: ProgramRef,          // Spawn registry id today; an FS path after #1
    needs: &'static [Slot],       // the child's manifest Slots the supervisor satisfies
    deps: &'static [ServiceId],   // must be Ready before this one starts
    readiness: Readiness,         // Spawned | SignaledReady(NotifyId)
    restart: RestartPolicy,       // Never | OnFailure | Always
    limits: RestartLimits,        // intensity: max_restarts within window
}

enum ProgramRef { Registry(usize), /* File(Path) after #1 */ }
enum Readiness  { Spawned, SignaledReady }
enum RestartPolicy { Never, OnFailure, Always }
struct RestartLimits { max_restarts: u32, window: Duration }  // Duration ← the clock work
```

`needs` is **not a supervision-specific type** — it is the child's manifest `Slot`
list (the shipped `Slot { name, object, rights }`; the richer `protocol`/`optional`/
`constraints` fields are deferred, see manifest-design § Deferred), the same
authority-requirement language the shell's `~>` grant step, checkpoint petitions, and
Stitch's `uses` row all consume. The supervisor is one *satisfier* among several — and
the mechanism is already built: `hitch::satisfy` maps each `Slot` to one of *its own*
caps (minting an attenuated/badged child where the Slot asks for less), and the
delegation rides the `Spawn` handle array in slot order, so the child reads authority by
role — `bootstrap.get::<Endpoint>("fs")` — never a positional `delegated_handle(i)`. This
is `satisfier.rs::process(child)` verbatim; supervision reuses it, it does not reinvent it.

---

## Lifecycle state machine

Per service. The supervisor drives each service through this; the whole set is the
supervision tree.

The diagram lives in its own doc so it can be rendered to SVG:
**[supervision-lifecycle.md](supervision-lifecycle.md)** (a hand-drawn diagram,
`cargo xtask diagram svg`).

`Exited` vs `Failed` is decided by the `i32` from `Wait`/`WaitAny`: **0 = clean,
non-zero = failure** (the honest exit-code plumbing from the #5 work — `process::exit(code)`
now carries the code, so `exit_with(134)` on abort is distinguishable). A future
supervisor-initiated `Kill` adds a third outcome.

## Dependency ordering

The `deps` edges form a DAG. Two pure functions in `kernel-core`:

```rust
fn startup_order(specs: &[ServiceSpec]) -> Result<Vec<ServiceId>, DependencyError>;
// topological sort; DependencyError::Cycle names the offending nodes.
// Teardown order is the reverse.

fn restart_set(strategy, failed: ServiceId, order: &[ServiceId]) -> Vec<ServiceId>;
// one-for-one → [failed]; rest-for-one → failed + everything after it in `order`.
```

Both are trivially unit-testable with no QEMU — the TDD sweet spot, same as the
scheduler's aging math.

## Restart policy, backoff, and intensity

The decision is a pure function of policy, exit outcome, and history:

```rust
fn restart_decision(
    policy: RestartPolicy,
    outcome: ExitOutcome,      // Clean | Failed(i32)  (later: Killed)
    history: &RestartHistory,  // recent restart timestamps
    limits: RestartLimits,
    now: Instant,
) -> RestartAction;

enum RestartAction { Restart { after: Duration }, Stop, Escalate }
```

- **Backoff:** exponential with a cap — `after = min(base · 2^consecutive_failures,
  cap)`. A clean-exit restart (policy `Always`) uses no/low backoff; a failure loop
  backs off.
- **Intensity (restart storm guard):** if restarts within `window` exceed
  `max_restarts`, return `Escalate` instead of looping. This is **not optional
  polish** — without it a crash-looping service is a busy-loop that floods the
  telemetry channel and starves everything else.
- **Escalation:** for `init` (the root) `Escalate` means log a fatal supervision
  event and halt (a crashed root service the system can't run without is a genuine
  panic). For a sub-supervisor it means the sub-supervisor itself `Exit`s failed,
  and *its* parent applies *its* policy — the recursion.

## Cap re-grant on restart (the mechanism)

On every `Starting` transition (first start or restart):

1. Supervisor has already created the service's durable objects **once** (at tree
   construction) and holds them.
2. Supervisor `satisfy`s the child's `needs` (§ manifest `Slot`s) from its own caps —
   minting attenuated/badged children where a Slot asks for less — and delegates the
   results in slot order via the `Spawn`/`SpawnImage` handle array. This is
   `satisfier.rs::process(child)`, unchanged. Same objects each incarnation.
3. On death + restart, step 2 re-runs against the **new** child table. Same objects,
   new incarnation. Clients holding minted caps to those objects are untouched.
4. `rest-for-one` restarts dependents so any that cached a delegated handle to the
   dead incarnation re-acquire the live one.

The old incarnation's private caps die with it (address space + table reclaimed on
`Exit` — the v0.12 reap path). No `Revoke` needed for those; `Revoke` is for
*proactively* reclaiming authority from a *live* misbehaving service (a policy hook,
not part of the restart happy path).

**Restart is checkpoint-restore's twin.** Steps 1–3 — re-satisfy a manifest against a
fresh `CapTable` — are *exactly* the operation axis 6 (checkpoint) calls "restore":
[cross-cutting-axes-brainstorm.md](cross-cutting-axes-brainstorm.md) #6 defines restore
as "re-run the delegation decision to re-satisfy the manifest." Restart and restore
are the same re-delegation primitive under two triggers (a crash vs an image load),
and should share one implementation — a `satisfy(manifest, into: &mut CapTable)` that
both call. Building supervision's re-grant as a bespoke path would fork code that
checkpoint has to re-converge later.

---

## Observability model — the payoff

This is the SnitchOS reason to build it. Every transition is already a wire event;
supervision makes the **structure** first-class.

- **Per-service umbrella span** covering *all* incarnations of a service, with each
  incarnation a **child span** under it. So Tempo shows "FS server, incarnation 3,
  restarted after a crash" correlated with incarnations 1–2 — restart continuity the
  raw `ThreadRegister`s (new task id per incarnation) can't express alone.
- **Metrics:** `snitchos.svc.<name>.restarts_total`, `.state` (an enum gauge),
  `.uptime_ticks`, `.backoff_ticks`. Per service, so a crash loop is a visible rising
  line before it trips intensity.
- **State-transition events:** each `Pending→Starting→Ready→…` edge is a span event
  or a `CapEvent`-style attributed frame, carrying the reason (exit status, backoff,
  escalation).
- **The tree itself:** nodes = services (colored by `state`), edges = `deps` +
  cap-delegation (already `CapEvent::Granted`/`Transferred` on the wire). A **live
  supervision tree in Grafana** is then just a query over existing frames.

The devlog money shot is the same shape as the v0.5 "follow a trace across a context
switch" post: **watch a service crash and get restarted, and the trace proves it** —
authority flowing down the tree, restarts nested under the service's umbrella span.

---

## Primitives: have vs missing

**v1 (crash-restart) is achievable on shipped primitives:**

| Need | Primitive | Status |
|---|---|---|
| Launch + delegate caps | `Spawn` (15), `SpawnImage` (26) | ✅ |
| Crash detection + status | `Wait` (18), `WaitAny` (24) → `i32` | ✅ |
| Supervisor-owned durable endpoints | `EndpointCreate` (25) | ✅ |
| Readiness / heartbeat | `NotifyCreate`/`Signal`/`WaitNotify` (21–23) | ✅ |
| Backoff timing | `ClockNow` (20) / `ClockFreq` (29) → `Duration` | ✅ (v#5) |
| Proactive authority reclaim | `Revoke` (28) | ✅ |
| Every transition observable | span + `CapEvent` + metric frames | ✅ |

**v2 (health + graceful shutdown) needs two new primitives:**

1. **Supervisor-initiated `Kill(child)`.** Today a process only `Exit`s *itself* —
   there is no way to terminate another. Needed to (a) shut a service down gracefully
   in reverse-dep order, and (b) restart a *hung* (alive-but-wedged) service. A
   capability-shaped kill (the supervisor holds a "lifecycle" cap over its children)
   fits the model.
2. **Timed wait / deadline.** `Wait`/`WaitAny` block indefinitely. Detecting
   "hung, not dead" needs `WaitAny`-with-deadline (or a readiness-heartbeat that
   times out). The clock work gives the *time source*; the *blocking-with-deadline*
   syscall is missing.

Both v2 primitives must land **cap-mediated, not ambient** — per the adopted
ambient-diet policy (seven-questions Q3 #3 / coda #6: *new syscalls default
capability-gated*). `Kill` is authorized by a **lifecycle cap** the supervisor holds
over each child (born at `Spawn`, so a process can only kill what it launched); the
timed wait is child-scoped by caller already, and should take a deadline argument on
`WaitAny` rather than becoming a second ambient call. Adding these as ambient syscalls
would reopen exactly the drift the policy was adopted to stop.

So the honest split: **v1 = crash-restart** (Wait-based, one-for-one + rest-for-one,
backoff + intensity, supervisor-owned endpoints, every transition a span) — fully
doable now. **v2 = health checks + graceful shutdown** — gated on `Kill` + timed wait.
Don't oversell v1 as full lifecycle management.

---

## Where the code lives + increment plan

**Prerequisites: satisfied.** Manifest v2 (the `Slot` language + the shared
`hitch::satisfy` primitive + `satisfier.rs::process` as the delegation loop) is shipped.
The differential oracle is **not** a v1 gate — v1's step-4 itest asserts the one
silent-failure invariant (a client's minted cap survives a restart) directly (see the
reframe at the top). So the steps below are buildable now, on shipped primitives.

Mirrors the project's kernel-core-vs-userspace split: **pure policy in `kernel-core`
(host-tested), mechanism in a userspace engine.**

1. **`kernel-core::supervision`** — `ServiceSpec`, `startup_order` (topo + cycle
   detection), `restart_decision` (policy + backoff + intensity), `restart_set`
   (D1). All pure, all `cargo test -p kernel-core`. TDD each.
2. **Generic supervisor engine** — a new `supervisor` root (**decided 2026-07-11**: new
   root, not evolving `init` — `init` is load-bearing for the default boot + several
   itests, so a `workload=supervised-*` root is additive and safe to iterate; `init`
   becomes the supervisor later, once proven). It reads a service table, creates durable
   objects,
   brings services up in `startup_order` (respecting `readiness`), runs a `WaitAny`
   loop, and on each exit consults `restart_decision` → re-spawn + re-delegate or
   escalate. **The re-delegate step is `satisfier.rs::process(child)`, already built** —
   the engine's new code is the table walk, the `WaitAny` loop, and the policy calls.
3. **Supervision telemetry** — umbrella span per service, incarnation child spans,
   `restarts_total` + `state` metrics, transition events.
4. **Acceptance itest** (`workload=supervised-crash-loop`) — **first graph (decided
   2026-07-11): the FS server as the supervised service + a client holding a minted cap**,
   reusing today's services rather than inventing new ones. The FS server exits non-zero
   on a schedule (injected crash); assert it restarts `N` times, that backoff spacing grows,
   that intensity trips `Escalate` after the cap, and — the oracle for the silent
   failure — that the **restarted service's own `cap_list` still contains the re-granted
   cap** (by object/rights/name), cross-checked against the supervisor's `CapEvent`
   grant (proves D3 by direct possession, not an inferred "the call worked"). Reuses the
   honest exit-code path from #5 and `cap_list` (27) for the snitch-on-the-snitch.
5. **v2 later** — `Kill` + timed wait, then graceful shutdown (reverse-dep teardown)
   and hung-service detection.

## Relationship to the rest of the design (what #6 sits on)

Re-read after [cross-cutting-axes-brainstorm.md](cross-cutting-axes-brainstorm.md) and
[design-explorations-seven-questions.md](design-explorations-seven-questions.md):
supervision is not a standalone feature but a **consumer** sitting on several designs.

- **Manifest v2 (Q1) — SHIPPED, the blocker is gone.** The supervisor *is* a manifest
  satisfier; its `needs` list is the manifest `Slot` language and its re-grant is
  `hitch::satisfy` + `satisfier.rs::process`. That satisfier now exists, so supervision
  **falls out as one satisfier plus a restart loop** — the re-grant is not new code.
- **Checkpoint (axis 6) — a shared primitive.** Restart == restore (§ Cap re-grant on
  restart). One `satisfy` implementation serves both; don't fork it.
- **The differential oracle (axis 3) — already shipped for this invariant (2026-07-11).**
  Cap re-grant is a *silent-failure* operation: if a restarted service doesn't actually
  receive a re-delegated cap, a client just hangs against a dead endpoint with no error.
  The coda's rule is "for any feature whose wrongness is silent, build its oracle first" —
  and it's built: `cap_list` (27) lets the restarted service report its *own* caps, and
  cross-checking that against the supervisor's `CapEvent` grant is the snitch-on-the-snitch
  axis 3 named. Both halves are on the wire today; step 4 just uses them. The snemu-scale
  frame-diffing oracle is the *general* form — worthwhile, but not a prerequisite here.
- **Budgets (axis 5) — the general form of restart intensity.** The intensity storm
  guard is a crude, hand-rolled budget; when axis 5 lands it should become a `Budget`
  policy. Hazard to design against now: `MapAnon` is ambient and unmetered, so a
  service that leaks memory *per incarnation* turns an unbounded restart loop into a
  leak amplifier — which is why intensity is load-bearing, and a case the budget axis
  closes properly.
- **Replay (axis 1) — the supervisor is a nondeterminism source.** `WaitAny` return
  order and the backoff clock reads are scheduling-/time-visible inputs (axis 2's
  documented `WaitAny`-order carve-out). A supervisor is inherently a nondet process;
  for replay its `WaitAny` order and clock draws are recorded-input sites, not blockers.
- **Programs-as-files (#1) — still applies.** `ProgramRef::File(path)` instead of a
  `Registry(id)` once #1 lands, so the service table is data all the way down and
  adding a service is not a kernel edit.

Net: the mechanics in this doc stand, and as of 2026-07-11 **#6 is unblocked** — the
`Slot` language and the shared `satisfy` primitive it needed are shipped (`hitch::satisfy`
+ `satisfier.rs::process`), and the restart-cap-survival check is a v1 itest assertion, not
a dependency on the full differential oracle. v1 = **`process` + `WaitAny` loop + pure
`restart_decision` + telemetry**, buildable now; v2 (health + graceful shutdown) still
waits on `Kill` + timed-wait syscalls.

## Open questions

- **Escalation at the root.** Is a root-service intensity breach always a system
  halt, or can `init` run in a degraded mode (some services down)? Leaning: halt for
  services marked `critical`, degrade for the rest — but that adds a `critical` flag.
- **Readiness timeout without v2's timed wait.** A `signaled-ready` service that
  never signals blocks bring-up forever in v1 (no deadline). Acceptable for v1
  (it's a boot-time bug, loudly stuck), but note it.
- **Dependency-failure semantics.** If a dependency hits `GaveUp`, do its dependents
  stop too? rest-for-one handles *restart*; permanent failure propagation is a
  separate policy (probably: dependents of a `GaveUp` service also stop). Decide
  when v1's escalation lands.
- **One durable-object registry or per-service?** Where exactly the supervisor tracks
  "which objects belong to which service" — a flat table vs nested — affects the
  recursive (sub-supervisor) case. Flat is fine for single-level v1.
