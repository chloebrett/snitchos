# Seven questions — design explorations

*Status: exploration / handoff note, 2026-07-04. Follow-on to
[cross-cutting-axes-brainstorm.md](cross-cutting-axes-brainstorm.md). Each section
explores one of the seven "ask the strong model" questions, grounded in three
code audits run for this exercise (cross-hart shared state; the full syscall/cap
ABI; the wire format and its durability) plus the Hitch, Stitch, IPC, and
capability design docs. Recommendations are marked **⟶**; genuine user decisions
are marked **⚖**.*

---

## Q1. The authority-description language

### What exists (more than expected)

The manifest already ships: `Manifest { input: Option<TypeSchema>, output:
TypeSchema, uses: Vec<String> }` (`hitch/src/lib.rs:167`), emitted by
`#[entry(in, out, uses)]` into `.snitch.iface`, seeded to the `user.iface`
xattr, served over `GetXattr`, and structurally typechecked for Stitch↔Stitch
`~>`. But the `uses` row is **bare strings** — a soft hint that drives no kernel
grant. Meanwhile the actual startup ABI is positional (`delegated_handle(i) = 2
+ i`, max 16, copy-semantics all-or-nothing), and the capability doc separately
designed a `BootInfo` many-cap startup page (auxv-shaped) that was never built.

Five consumers need one language: spawn-time requirements (redesign #2), shell
`~>` grant decisions, checkpoint petitions (axis 6), IFC declassifier grants
(axis 4), and Stitch `uses` rows. The design question is what `uses` becomes.

### Design: the slot

**⟶ Promote `uses` from `Vec<String>` to a list of typed authority slots:**

```
Slot {
  name:      Str,                  // the CHILD's local role name: "fs", "clock"
  object:    ObjectKind,           // Endpoint | Notification | TelemetrySink | …
  rights:    Rights,               // requested mask (SEND, RECV|MINT, …)
  protocol:  Option<TypeSchema>,   // endpoints: the request→response shape, structural
  optional:  Bool,                 // required (spawn fails unsatisfied) vs optional
  constraints: List<(Str, Value)>, // extensible: badge, label clearance, budget size
}
```

Load-bearing choices inside that shape:

- **Names are local roles, not global service names.** A slot says "I need *an*
  endpoint speaking this protocol under the name `fs`," never "connect me to
  `/services/fs`." Global naming is the satisfier's business (the shell/init);
  keeping it out of the manifest is what makes the same program runnable against
  a proxy, a sandbox subtree, or a test double with no manifest change — the
  interposition axis depends on this property.
- **Protocol compatibility is structural** (a `TypeSchema` over the
  request/response sum), matching the shipped `~>` check and staying
  cross-language. An endpoint's protocol type is the natural home for the typed
  channels the actor-model doc wants.
- **`constraints` is the extension point** the other axes plug into: axis 5 adds
  `("budget.ticks", n)`, axis 4 adds `("clearance", label)`. The manifest schema
  itself never changes for them.

### Satisfaction semantics

**⟶ Satisfaction is strictly userspace; the kernel keeps the mechanism it has.**
The satisfier (init, the shell, any parent) reads the manifest, decides which of
*its own* caps satisfy each slot — minting attenuated/badged children first
where appropriate — and calls `Spawn` with the handle array exactly as today.
What changes is the *child-facing* half: instead of the positional convention,
the parent also passes a **BootInfo page** — a hitched list of `(name, handle)`
pairs — and the runtime exposes `bootstrap.get::<Endpoint>("fs")`. This
implements the cap doc's designed-but-unbuilt BootInfo and kills the positional
contract in one move; the kernel never learns what names mean
(mechanism-not-meaning, consistent with the userspace-defined-metrics
direction).

Required slots are all-or-nothing (matching today's delegation semantics);
optional slots may be absent and the program degrades. The satisfier emits the
grant record — which slot was satisfied by which `cap_id` — as telemetry, so the
wire shows *named* delegation ("granted `fs` ⟵ cap 41") rather than anonymous
transfers. The cap-id spine makes this a pure annotation.

### The checkpoint extension (axis 6)

A checkpoint's petition = the original manifest **plus** runtime-acquired
holdings. The manifest slots re-satisfy by name; runtime-acquired caps (badged
file caps from the FS, reply-path transfers) have no slot, so they serialize as
**provenance records** — derivation chains via `parent_cap_id`, which the spine
already stores — and the restoring authority decides policy per record
(re-petition the same server by protocol + badge, or drop). This is the honest
research edge: named slots restore cleanly; anonymous acquisitions restore only
as well as their provenance is interpretable. Worth writing up as such.

**⚖ Decisions for you:** (a) confirm local-role naming over any global service
namespace; (b) whether the grant record is satisfier-emitted telemetry (my
recommendation — kernel stays name-blind) or a new kernel frame; (c) whether
optional slots are worth having in v1 or everything starts required.

---

## Q2. Do the three schemas unify? Plus the wire-durability problem

### Unify the model, not the encodings

Three type systems exist: `protocol::Frame` (hand-rolled postcard),
Hitch (`TypeSchema`/`Value`, its own TLV codec), and the syscall ABI
(positional registers + `Pod` structs like `CapDesc`). The Hitch doc already
settles the direction — one algebraic model, encodings chosen per consumer, the
known-schema principle keeping hot paths packed. Endorsed; the audits add one
concrete move:

**⟶ Derive a Hitch `TypeSchema` for `Frame` itself** (`#[derive(Schema)]` on the
protocol enum). Three payoffs: (1) generic consumers — a future `frames` shell
command, the nushell-style renderer — can unhitch telemetry without hand-written
decode tables; (2) the schema's hash becomes a machine-derived **protocol
fingerprint**, superseding the hand-bumped `PROTOCOL_VERSION` for artifact
stamping; (3) `Frame` stops being outside the model without touching its
encoding. Postcard stays on the wire — the kernel path is exactly the "packed
hitch against a known schema" the known-schema principle blesses. Migrating the
hot path to a self-describing encoding would buy nothing and cost bytes.

### The durability problem is real and currently unguarded

The wire audit found: `PROTOCOL_VERSION = 4` is transmitted in `Hello` and
**ignored by every consumer** (collector destructures it away; harness reads
only `timebase_hz`); the append-only discipline lives entirely in comments; all
encoding tests are roundtrips, so **a mid-enum variant insert passes the whole
suite** (encoder and decoders rebuild together — the failure mode is silent
skew against anything *not* rebuilt); and no raw wire bytes are persisted
anywhere today (capture.json is decoded, lossy, level-gated strings).

Today that's latent — there is no stored-bytes corpus to strand. **Axis 1
converts it to load-bearing**: replay recordings are persisted raw streams meant
to be re-executable years later. Before the first recording exists:

**⟶ Three cheap hedges, ~a day total:**
1. **Golden-bytes snapshot test** — encode one exemplar of every `Frame` variant
   (and each supporting enum arm), `insta`-snapshot the exact bytes. This is the
   only thing that makes the append-only rule *enforced* rather than social.
2. **Version enforcement** — collector and itest harness reject (loudly) on
   `Hello.protocol_version` mismatch. The field was added for exactly this and
   checks nothing.
3. **Define the capture container now** — any persisted raw stream starts with a
   hitched header: magic, `protocol_version`, schema hash (once derived), git
   rev, timebase. Retroactively versioning a corpus is miserable; versioning an
   empty corpus is free.

Manifest versioning (Hitch open fork #3) is the same problem one layer up —
manifests persist in xattrs/ELF notes. **⟶ Same policy**: TLV tags append-only +
golden-bytes test + a version byte in the manifest header next to the existing
4-byte length.

**⚖ Decision:** postcard-with-guards (recommended) vs re-encoding `Frame` as
hitch-packed. Also whether the derived-schema hash should *replace* the manual
version constant or sit beside it (recommend beside: version = intent, hash =
fact; disagreement between them is itself a useful failure).

---

## Q3. The fatal-flaw review

Headline: **no fatal flaw, but a consistent pattern — the documented invariants
are ahead of the enforced ones, and the new axes are about to promote three of
those gaps from latent to load-bearing.** Ranked:

### 1. The wire contract has no teeth (see Q2)
Version ignored, discipline-by-comment, roundtrip-only tests, one silently
lossy path. Surfaces: the first replay capture that outlives a protocol bump;
or any out-of-tree consumer. Hedge: Q2's three fixes. *Promoted by axis 1.*

### 2. The telemetry channel's loss/liveness contract is accidental
Three distinct behaviors coexist: pre-init overflow **drops and counts**
(`Dropped` covers only the boot window — which is precisely the
most-deterministic, replay-critical window); an over-512-byte frame **drops
silently, uncounted** (`tracing.rs:73`); steady-state backpressure **never
drops but spins the kernel forever** (no timeout on `used.idx` — the wedge
signature). None of these was *chosen* as the channel's contract. Replay needs
"lossless or detectably-divergent"; budgets/IFC add frame volume;
`ContextSwitch` already dominates bandwidth. Surfaces: recording overhead
perturbing the system it records (the observer effect the snemu doc worries
about), or a lost frame silently holing a replay log. Hedges now: count the
oversized drop (near one line); write down the intended contract per frame
category; consider a second virtqueue (replay/lossless vs observability/lossy)
when axis 1 lands. *Promoted by axes 1, 3, 5.*

### 3. Ambient authority is quietly re-accumulating
17 of 30 syscalls need no capability. Individually reasonable; collectively:
**`Spawn`/`SpawnImage` are ambient** (any process can spawn any registry
program *or arbitrary ELF bytes* and delegate from its own table),
**`MapAnon`** grants memory unmetered (undermines axis 5 before it starts),
**`ClockNow`/`ConsoleRead`** are the direct blockers for axis 2's purity claim,
and **`EndpointCreate`/`NotifyCreate`** manufacture kernel objects without
quota. The pillar holds for *reaching existing objects*; it does not hold for
*creating load or new authority*. Surfaces: the purity bit is unimplementable;
budget enforcement has a hole the width of `MapAnon`; any least-authority shell
demo is over-claiming while spawn is free. Hedge: no rush of code — adopt the
policy that new syscalls default cap-mediated, and let Q1's manifest be the
vehicle for the ambient diet (spawn-authority, memory, clock as slots).
*Promoted by axes 2 and 5.*

### 4. Attenuation is documented as an invariant but only partially enforced
"You can only ever attenuate, never amplify" (capability-system-design.md) —
but `MintBadged` lets the MINT-holder set child rights freely (by design —
owner grants to own object), delegation is copy-semantics with no narrowing
step, client re-delegation + the GRANT gate are deferred, and init knowingly
over-holds RECV on endpoints it delegates. The *observability* tree (cap-id
spine) is excellent; the *enforcement* tree is thinner than the docs read.
Surfaces: axis 4/5 designs that assume kernel-enforced monotonic narrowing;
any security claim in a post. Hedge: a short "what the kernel actually
enforces" table in the cap doc now; monotonic narrowing lands with client
re-delegation as already planned.

### 5. Reply caps are terminal — every intermediary is a full relay
The ABI audit's sharpest finding: `Object::Reply { caller }` is affine,
TaskId-bound, and has **no transfer path** — a proxy that receives a `Call`
cannot hand the client's reply cap to the real server, and the server's
`CopyToCaller` reaches only the *proxy's* memory, so bulk payloads double-copy
through every interposer. Surfaces: axis 7's transparent proxy; equally, plain
service layering (FS calling a block server on behalf of a client) pays the
same tax with no proxy involved. This is the one place a *kernel* change is
clearly implied. **⚖ Design fork**: (a) delegable reply caps (transfer-on-call,
E-style), (b) a `CallThrough` that re-targets the reply cap at forward time,
or (c) accept relay semantics and optimize the double-copy later. My lean: (b)
— it keeps reply caps affine and adds one explicit, snitchable forwarding
event, which is also exactly the hook interposition telemetry wants.

### 6. Boot-time single-writer assumptions are unrecorded invariants
From the cross-hart audit: `HEAP_TOP` does load-then-store (not CAS — safe only
while heap growth is hart-0-only); the secondary-boot handoff statics are
non-atomic `static mut` ordered by the SBI `hart_start` MMIO barrier;
`shootdown_va` is a single slot flagged hazardous under multi-initiator
contention. All sound today at `MAX_HARTS = 2`; all silent traps at hart 3 or
concurrent heap growth. Hedge: debug asserts encoding each assumption + entries
in the debt register; the replay work should treat them as *recorded
invariants* (record-mode asserts they held).

### 7. Rights namespaces are about to multiply
Kernel rights mask + server-interpreted badge-packed object rights is already
two namespaces (documented). IFC clearances and budget constraints would make
four ad-hoc ones. The deferred typed-capability generalization (kernel-carried,
server-interpreted object rights) should be re-evaluated *before* axes 4/5
rather than after — it's the difference between labels/budgets being fields on
one mechanism vs three parallel bolt-ons.

What I'd *not* flag: Sv39-only, MAX_HARTS=2, QEMU-virt hardcoding, the parked
DTB — all known, scoped, and documented trade-offs with clear surfacing points.

---

## Q4. Sequencing — the dependency-ordered spine

The two most-constraining pieces of design are confirmed as **the manifest
(Q1)** — it shapes the ABI everything else touches — and **replay (axis 1)** —
it dictates the channel contract, wire durability, and lock-site discipline.
Both should be *designed* before anything else is *built*. Ordered spine
(order, not calendar; snemu M2/M3 proceed in parallel throughout):

1. **Wire hardening** (Q2's three hedges + counted oversized drop). Days;
   protects every artifact produced afterward; zero design risk.
2. **Nondeterminism census** — the audit here already covers the cross-hart
   third (enumerable: `kernel::sync` mutexes + `ipi_pending` / `shootdown_ack`
   / `SECONDARY_READY` + device inputs `used.idx`/UART-RX). Remaining: rdtime
   sites, timer-arrival points, entropy. Pure audit → doc.
3. **Manifest v2 design doc, then implementation** (typed slots + BootInfo +
   `bootstrap.get`). Unblocks redesign #2, axis 2, axis 6, the shell's grant
   step, and the Stitch `uses` bridge — the highest-fan-out build item.
4. **Differential oracle #1** (heartbeat-cadence claim vs snemu instret) the
   moment snemu M2 lands — cheapest possible axis-3 payoff, and it hardens
   snemu itself.
5. **Single-hart record/replay** of boot-to-heartbeat (needs 1 + 2 + snemu M2).
   The capture container from Q2 is its file format.
6. **Purity bit / det-spawn** (needs 3; masks the ambient nondet syscalls,
   refusals snitch).
7. **Budgets phase 1 — attribution only** (needs nothing structural; keying
   `(task, active_budget)` + one object type; enforcement deferred to a later
   phase per Q6a).
8. Then IFC-bit, checkpoint-v0, and the proxy become independent leaves —
   choose by appetite; none blocks another.

The axis-7 reply-forwarding decision (Q3 #5) can be made at design level
anytime; its implementation has no dependents until a proxy or layered service
exists.

---

## Q5. Stitch: should `uses` become a real effect system?

The language doc already commits to `uses` and asks the right question — *how
much is compile-time vs reified in the VM?* Answer, in three layers:

**⟶ Layer 1 — compile-time: row discipline, then erased.** The host compiler
checks `uses` transitively up the call graph (a function calling `Net.get`
must carry `uses Net`); effect aliases keep rows readable. Static-only,
zero-cost, catches "forgot to declare." Koka-style, monomorphic v1.

**⟶ Layer 2 — runtime: capabilities are unforgeable *values*.** The decisive
design move: **no ambient natives.** Every effectful native takes its
capability as an (implicitly threaded) argument — Scala `using`/`given`
semantics, already in the lineage list. A cap value can only be *received*,
never constructed. This makes the `uses` row not documentation but plumbing:
the row *is* the declaration of which implicit values thread through. Without
reification, interposition inside the language is impossible; without the
static row, threading is manual and forgettable.

**Layer 3 — the kernel boundary stays the hard floor** (already the doc's
honest position: a VM bug voids language-level authority; the process edge
holds).

Two consequences make this more than hygiene:

- **Handlers are membranes.** The algebraic-effects machinery already planned
  (for `use <-`, iteration, concurrency) is *also* the interposition mechanism:
  `with fs = readonly(fs) { … }` installs a handler that attenuates every FS
  effect in its extent. This is axis 7 at language level — the two-layer
  authority story completed at the effect layer. The editor demo becomes one
  concept at two enforcement strengths: normal mode's handler set simply lacks
  the write effect (soft, language); the `~>`-spawned process variant lacks the
  write *cap* (hard, kernel). Same shape, same telemetry.
- **Purity is one concept at three scopes.** Empty-of-nondeterministic-effects
  row ⇒ memoizable function (VM-level, axis 2's logic applied in-language);
  the manifest's `uses` row *is* `main`'s effect row (already true by
  construction) ⇒ the process purity bit falls out of the same vocabulary; and
  `with scheduler(Deterministic.seed(1))` — already in the concurrency sketch —
  is determinism-as-handler. One vocabulary should span all three: Stitch
  effect names ↔ manifest slot names ↔ kernel object kinds. That mapping table
  belongs in the Q1 design doc.

**⚖ Decision:** whether affine cap *values* (can't be duplicated in-language)
are worth the type-system cost in v1, or copyable values with kernel-side
revocation as the backstop suffice. Recommend copyable-v1: affinity buys little
while the kernel edge exists, and it's the single biggest complexity cliff in
effect-system land.

---

## Q6. Semantics reviews

### Q6a. Budget donation over rendezvous Call/Reply

The mechanism template already exists in-tree: **trace context is a
kernel-populated slot in every IPC message, swapped at the rendezvous**
(ipc-design.md). Budgets are the same slot pattern with enforcement:
`active_budget` is per-task kernel state; `Call` donates the caller's active
budget to the server for the duration of the rendezvous; `Reply` returns it;
onward `Call`s re-donate the same budget (transitivity by construction — the
chain unwinds as replies unwind). Trace context and budget context are the
same idea — one observed, one enforced. That symmetry is the design.

Semantics to pin (the parts seL4's MCS took years over):

- **Charging**: the tick-accounting machinery (`cpu_time_ticks`, entry-tick)
  re-keys by `(task, active_budget)`; `MapAnon` charges the active budget's
  frame quota. Spans gain `budget_id` alongside `task_id`.
- **Exhaustion is a boundary event, never a mid-flight preemption.** An
  exhausted budget fails the *next* boundary operation — syscall, `Call`,
  timer-tick check — with `RefusalReason::Quota` (which already exists),
  snitched. Code between boundaries runs to the boundary, so userspace locks
  release via normal paths and the mid-critical-section hazard never arises.
  The cost: a compute-only loop overruns until the next preemption tick —
  bounded by the tick, acceptable, and *observable* (overrun is a metric).
- **The house budget**: a server's receive-loop housekeeping runs on its own
  budget; the swap to the client's donated budget happens at `Receive`-return
  and back at `Reply`. Kernel-automatic, like the span-context swap.
- **Blocked costs nothing**: a task parked in `Receive`/`Wait` accrues no
  charge (entry-tick machinery already handles run/park edges).

**⟶ Phase 1 is attribution-only** — attribute and emit, enforce nothing. It
derisks every accounting question with zero failure-mode risk, is pure
observability (on-thesis), and Grafana's per-principal-cost-inside-a-shared-
server panel is the demo *before* any enforcement exists. Enforcement is a
policy layer over proven accounting. snemu makes both testable exactly
("refusal at instret N").

### Q6b. Sync-order replay sufficiency (the DRF audit results)

The audit's conclusion is the strong one: **cross-hart communication is
enumerable.** All shared mutable state is (a) behind `kernel::sync` (scheduler
runqueues incl. cross-hart spawn, console RX, virtio TX, frame allocator, IPC
tables, intern/pre-init), (b) exactly three genuine Release/Acquire pairs
(`ipi_pending`, `shootdown_ack`, `SECONDARY_READY`), (c) device inputs
(virtio `used.idx`, UART RX bytes), or (d) genuinely per-hart. **No unintended
production data race was found.** So the recording set is:

1. **Lock-acquisition order per `kernel::sync` Mutex** — and the wrappers were
   built as the chokepoint with hook points reserved for preempt/IRQ;
   recording is a third hook, landing in one file. Hot-path mitigation: only
   cross-hart *handoffs* need an edge (per-lock logical clocks, the Kendo
   trick); same-hart consecutive acquisitions are ordered by program order.
2. **The three atomic pairs** — all rare (IPIs, shootdowns, boot handoff);
   trivially frame-able.
3. **Device inputs** — `used.idx` advance observations and RX bytes, recorded
   as external inputs with arrival points.
4. **Relaxed counters need no edges** (commutative; they never drive control
   flow — heap watermark decisions derive from allocation state, which is
   schedule-determined). Their *values* surface in heartbeat metrics, so:
   **⟶ treat metric values as checked outputs with tolerance, not replay
   inputs** — a metric divergence is a finding, not a replay failure.
   `NEXT_TASK_ID`/`NEXT_CAP_ID` values already appear in
   `ThreadRegister`/`CapEvent` frames, so replay *checks* them for free.
5. **The latent assumptions become recorded invariants**: record-mode asserts
   heap growth stays hart-0, secondary handoff happens once, single-slot
   shootdown never sees a second initiator. A violated assumption fails the
   recording loudly instead of corrupting the replay silently.

Divergence detection: replay re-emits the frame stream; byte-diff against the
recording (control-flow frames exact, metric values with slack). Any
divergence is a bug in one of exactly three places — kernel DRF discipline,
the recorder, or snemu — all worth finding (the differential-testing
property). The open engineering risk is **recording bandwidth on the wedge-
prone channel** (Q3 #2): lock-order events are the potentially hot category;
contended-only recording plus the channel-contract work is the mitigation to
design.

---

## Q7. Falsifying "legibility is the missing half"

Preregister the criteria; publish hits and misses alike (a "we preregistered
and missed" devlog is itself a strong artifact). Each falsifier maps to
machinery that exists or is already planned:

1. **Overhead ceiling.** If always-on recording + telemetry costs more than a
   preregistered bound (propose: ≤5% instruction overhead, measured exactly by
   snemu M4's nested overhead-factor methodology), then "everything observable"
   is a demo posture, not a design. The measurement spine makes this a number,
   not a vibe.
2. **Attribution coverage.** Axis 3's dark-matter metric: if the fraction of
   retired instructions attributable to a task/span can't be driven above a
   preregistered floor (propose: 95%), the kernel's self-narration is
   decorative. Publish the panel either way.
3. **Debugging practice, self-audited.** The thesis predicts *you* stop
   debugging via UART `println!`. Habit: every bug post-mortem in the devlog
   records whether the trace/metrics or the printf found it. If after N bugs
   the split favors printf, observability-first failed its primary user. (The
   TX_STAGING hunt is the existing baseline: the classifier — a telemetry
   consumer — was the hero.)
4. **Replay fidelity rate.** Axis 1's claim fails if real captures don't
   replay: preregister that TX_STAGING-class flakes reproduce from capture
   with ≥ near-certainty, and track divergence rate as a first-class metric.
   Persistent unexplained divergence = the wire-as-replay-log claim collapses.
5. **Label creep, n=1 diary.** Run the one-bit IFC for weeks of real editor +
   shell use. If you find yourself blanket-declassifying to get work done
   *despite* being able to see every taint path, then legibility did **not**
   fix IFC's usability failure — the strongest possible falsification of the
   research pitch, and publishable in either direction.
6. **Third-party legibility.** Show a refused-flow trace to someone who
   doesn't know the system; can they answer "why was this refused?"
   unprompted? The pitch claims the trace is the explanation. n=1 friend
   suffices for a devlog data point.
7. **The snitch-on-the-snitch check** (already designed in
   capability-system-design.md): does the host-reconstructed cap tree match
   kernel enforcement state? Axis 3 generalizes it; a standing mismatch that
   goes unnoticed for weeks falsifies "observability keeps the system honest"
   from the inside.

---

## Cross-references

- [cross-cutting-axes-brainstorm.md](cross-cutting-axes-brainstorm.md) — the
  seven axes these questions interrogate.
- Audit findings folded in above: cross-hart shared-state audit (Q6b, Q3 #6),
  syscall/cap ABI map (Q1, Q3 #3–#5), wire-format durability audit (Q2, Q3
  #1–#2). Key file anchors: `hitch/src/lib.rs:167` (Manifest),
  `abi/src/lib.rs:28` (Syscall enum), `protocol/src/lib.rs:99` (Frame),
  `kernel/src/smp/percpu.rs:102` (IPI mailbox), `kernel/src/obs/tracing.rs:73`
  (silent oversized drop), `collector/src/state.rs:207` (ignored version).
