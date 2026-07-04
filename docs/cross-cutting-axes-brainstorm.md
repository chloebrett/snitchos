# Cross-cutting axes — brainstorm deep-dive

*Status: brainstorm / handoff note, 2026-07-04. Source material for proper design
docs. Context: SnitchOS is driven by two cross-cutting axes of novelty —
**everything is observable** and **everything is capabilities** — with a third
(**everything is typed data**: Stitch/Hitch/typed processes) forming. This note
deep-dives seven candidate additional axes, chosen for demo value, research-grade
novelty, and how hard they collide with existing OS assumptions. Development cost
deliberately deprioritized (learning project).*

*Ranking (roughly): #1–#3 cash in on snemu and are the strongest; #4 is the best
pure-research kernel-only candidate; #5–#7 descend from there. #7 is lowest-novelty
but cheapest and load-bearing for #4/#5.*

---

## 1. Cross-substrate replay — the wire format as replay log

### Thesis

Design the kernel so that its telemetry stream is *sufficient to deterministically
re-execute the run*. Every source of nondeterminism the kernel consumes is emitted
as a frame. Then a run recorded **anywhere** — QEMU, snemu, a real board's UART —
replays in snemu, exactly, scrubbing backwards included. Observability graduates
from *description of what happened* to *causality capture*: the frames become a
sufficient statistic for the execution.

This is distinct from (and complementary to) snemu's own record/replay
(`docs/snemu-design.md` → "record/replay as the itest format"): that records at
the *machine boundary*, inside snemu, and only works for runs snemu itself
executed. QEMU and a real board won't record for you — **only the kernel can**.
The doc's "post-contact reproduction lab" loop (observe on metal → hand-model the
fault → reproduce) becomes automatic: the recording *is* the model.

### Mechanism sketch (grounded)

The work starts with a **nondeterminism census** — audit every point the kernel
consumes an input it didn't compute:

- **Timer interrupts** — the arrival *point* matters. Record `(hartid, instret,
  scause, sepc)` at trap entry; RISC-V gives every hart an `instret` CSR, so the
  guest-visible position of the interrupt is recordable on all three substrates.
  Replay = snemu injects the interrupt when that hart's instret hits the recorded
  value.
- **`rdtime` reads** — every read of the time CSR is an input. Heartbeat reads are
  cheap to record; `yield_now`'s cpu-time accounting reads are hot. Candidates:
  delta-compressed time frames, or a kernel replay mode that derives time from
  instret. The census's job is to find every such site (grep `read_time` /
  timebase users).
- **Console / device RX** — virtio-console input, future keyboard/net. Already
  frame-shaped; just record payload + arrival point.
- **Entropy** — see `docs/randomness-and-entropy.md`; every entropy draw is an
  input frame.
- **Cross-hart interleaving** — the hard one, and where owning the kernel pays.
  Full memory-interleaving recording is intractable, but if all cross-hart
  communication flows through `kernel::sync` (the chokepoint built for exactly
  this kind of hook) plus explicit atomics at known sites, then recording **sync
  order** (per-hart Lamport counters, emitted on cross-hart handoff) suffices —
  the Kendo/CoreDet insight: for data-race-free programs, sync order determines
  the execution. Replay = snemu's interleaving scheduler constrained to the
  recorded sync order. A data race outside the chokepoint breaks replay — which
  makes **replay divergence itself a race detector**.

Wire cost: traps and cross-hart handoffs are rare relative to instructions;
the hot concern is time reads. A dedicated "replay channel" frame category keeps
the observability stream analyzable independently of the replay stream.

### Novelty vs prior art

rr records at the process/syscall boundary, single-threaded schedule. ReVirt and
VMware's record/replay worked at the VM boundary — VMware shipped it and then
*dropped it* because multicore made it intractable at that layer. Antithesis gets
determinism by owning the hypervisor — runs must happen inside their environment.
Kendo/CoreDet do sync-order determinism for user programs. **Nobody has co-designed
a kernel so that its own observability stream is its replay log, portable across
substrates** — the sync-order trick is only available because we own the kernel's
lock chokepoint, and the record-on-real-hardware capability is only available
because recording is a kernel feature, not an emulator feature.

### Interactions

- **Observability**: this *is* the observability axis completed — frames become
  sufficient, not just descriptive.
- **Capabilities**: nondeterminism sources are (or should become) cap-mediated —
  see axis #2; the census and the cap classification are the same list.
- **snemu**: snemu M2/M3 is the replay engine; rewind/watchpoints (already
  designed) apply to *any* recorded run, not just snemu-native ones.
- **#3 differential observability**: replay fidelity is testable — replay a run
  and diff the emitted frames against the recording (they must match).

### Demo / post angle

A cross-hart flake fires once in CI under `--repeat`. `.itest-runs/<ts>/capture.json`
already holds the frames. Feed it to snemu → deterministic reproduction → scrub
*backwards* to the first bad write, in a browser tab, synced to the Tempo trace.
Later, the same from a VisionFive board's UART capture: **a bug that happened on
real silicon, replayed and rewound on a laptop.** Given the project's history (the
TX_STAGING wedge hunt, the failure-signature classifier, the capture corpus), the
post writes itself: "we made flakes reproduce by definition."

### First milestone

1. **Nondeterminism census** — enumerate every consumption site; classify:
   already-a-frame / frame-able / hard (cross-hart). Pure audit, no code.
2. **Single-hart replay-sufficiency** for boot-to-heartbeat: record timer-trap
   instret + time reads + console RX; replay in snemu (needs M2); assert the
   replayed frame stream is byte-identical to the recording.
3. Cross-hart sync-order recording via `kernel::sync` hooks (needs snemu M3's
   scheduler).

### Open questions

- instret fidelity on QEMU TCG without `-icount` — is the recorded position
  exact? (May need icount mode for QEMU-sourced recordings; real HW instret is
  architecturally exact; snemu's is exact by construction.)
- Hot-path time reads: record vs derive-from-instret in a replay build.
- Do any legitimate cross-hart communications bypass `kernel::sync` today
  (raw atomics: `CURRENT_TASK`, IPI acks, virtqueue indices)? Each needs a
  recorded happens-before edge or an argument for why it can't diverge replay.

---

## 2. Determinism as a capability

### Thesis

Nondeterminism enters a process only through authority: clock, entropy, console
input, IPC from a nondeterministic peer. In a capability system, that authority is
enumerable per-process — so **purity is a property the kernel can compute from the
cap table**. A process holding no nondeterminism-bearing caps is a deterministic
function of its inputs, *by construction*, and the kernel knows it, enforces it,
and can exploit it.

The framing that sells it: **the cap table is the process's effect signature**.
Haskell puts IO in the type; SnitchOS puts it in the CapTable. This is the
capability axis and the typed-process axis (Hitch, `user.iface` manifests) meeting
the determinism axis.

### Mechanism sketch

- Classify every `Object` and every ambient syscall as det / nondet. Today
  `ClockNow` and `ConsoleRead` are **ambient** — the enemy of this axis. Either
  move them behind caps (fits the redesign-from-scratch direction: typed, named
  startup authorities) or add a spawn flag that masks ambient nondet syscalls
  (refusals snitch, per house style).
- Kernel computes `deterministic: bool` per process from its cap set; carried on
  `ThreadRegister` → Grafana colors the delegation graph by purity.
- **Compositionality**: purity extends to subgraphs — a set of processes with no
  nondet caps and no inbound IPC from outside the set is deterministic *as a
  unit*. Purity becomes a delegation-graph analysis, not just a per-process bit.
  (Subtlety: `WaitAny` / multi-peer receive order is scheduling-visible — a
  nondet input unless the subgraph is single-input-stream. The honest claim needs
  this carve-out.)
- **Payoffs**:
  - **Memoization**: same pure program + same inputs → same outputs. The shell or
    a cache service can content-address process executions — Nix/Bazel
    hermeticity, but as an *OS-verified runtime property* rather than a build-tool
    convention. Kernel-verified cache keys.
  - **Cheap single-process replay**: a pure process's inputs cross a narrow, typed
    cap boundary — record just those and replay it *without* whole-system replay
    (#1). The two axes are the same idea at two scopes.
  - Deterministic services are flake-free test subjects forever.

### Novelty vs prior art

Determinator (Ford, OSDI '10) made determinism the default via a restricted
kernel API — determinism as *regime*. Nix/Bazel do hermeticity at build level by
sandbox convention. Language purity is compile-time. **Purity as a first-class,
kernel-computed, observable, delegation-graph property of ordinary processes** —
and memoization as an OS service backed by it — appears to be unclaimed territory.

### Demo / post angle

`~> sort < big.txt` (syntax illustrative only) runs; run it again — the span comes back annotated
`memoized`, sub-millisecond. Grant the program a clock cap; memoization visibly
turns itself off (the purity bit flips in the graph view). Post: "your process's
type signature is its capability table."

### First milestone

Cap-mediate (or maskable-ambient) `ClockNow`; compute + emit the purity bit;
itest: a det-spawned process's entropy/clock attempt is refused and snitched.
Memoization comes later (needs #6-adjacent input/output hashing at the cap
boundary).

---

## 3. Differential observability — snemu audits the kernel's self-reports

### Thesis

Telemetry is the one part of a system that never gets tested — dashboards are
trusted, not verified. snemu (design doc: "observe what the kernel can't see
about itself") knows ground truth: exact instret per hart, every MMIO, every
memory write. The kernel makes *claims*: `cpu_time_ticks` per task, span
durations, `context_switches_total`, heap gauges. **Diff the claims against the
machine truth, continuously, as a test layer.** Is my telemetry *true*?

### Mechanism sketch

- snemu attributes instructions to tasks two independent ways: watchpoint on the
  `CURRENT_TASK` atomic (machine truth) and the `ContextSwitch` frame stream
  (kernel claim) — cross-check the two, then use the attribution to audit
  `cpu_time_ticks` and per-span durations.
- Oracles, cheapest first: heartbeat interval claim vs instret delta;
  `context_switches_total` vs watchpoint-counted executions of the asm `switch`;
  per-task cpu time; span duration vs instret between the emitting instructions;
  `Dropped` counter vs actual drops.
- The product isn't pass/fail — it's the **gap, measured**: cycles attributed to
  no task (trap entry/exit, mutex spin, the scheduler itself) are the kernel's
  observability *dark matter*. Define **attribution coverage** — % of retired
  instructions attributable to some task/span — as a first-class metric, put it
  on a dashboard, and drive it up with fixes (e.g., attribute trap overhead).
  This is the M4 "measure first, then tune what you measured" ethos applied to
  the telemetry itself.
- Differential-testing property: a disagreement is always a bug *somewhere* —
  kernel accounting, telemetry pipeline, or snemu. All three findings are
  valuable; the oracle can't cry wolf.

### Novelty vs prior art

Distributed tracing has clock-skew correction; seL4 has proofs about info flow —
neither answers "are my metrics accurate?" **"Test your telemetry like you test
your code," with an emulator as ground-truth oracle**, is a genuinely new test
layer, and "instruction-level attribution coverage of an OS" is a metric nobody
has published.

### Interactions

Hardens the observability axis rather than adding a fourth. Needs snemu M2
(heartbeat) for the first oracle, M3 for task attribution. Feeds #1 (replay
fidelity check = diff replayed frames vs recorded) and #5 (exact billing needs
accurate accounting — this is how you know it's accurate).

### Demo / first milestone

Dashboard: kernel-claimed vs machine-true per-task CPU side by side, plus the
dark-matter panel ("7.3% of cycles unattributed; 61% of that is trap
entry/exit"). First oracle after snemu M2: heartbeat cadence claim vs instret
delta, as an itest.

---

## 4. Observable information flow — where data may go

### Thesis

Capabilities govern who may *act*; they say nothing about where data may *flow* —
a process legitimately holding read-secrets and send-network can exfiltrate. IFC
(labels on data, checked at sinks) is the classic answer, and it famously died of
**illegibility**: label creep plus "why can't I write this file?" with no answer.
The research bet: **IFC failed for legibility reasons, not mechanism reasons, and
observability is the missing half.** Every label join and every flow check is a
frame; the taint's path through the system is a *trace you can read*.

### Mechanism sketch

- Start coarse (HiStar-granularity): a label set per **process** (its taint), per
  **FS file** (an xattr — the `user.iface` xattr machinery already exists), per
  **endpoint** (clearance). Read labeled data → taint joins into the process;
  write to a sink → check clearance ≥ taint. Kernel-checked at the IPC/syscall
  boundary.
- **Declassification is a capability** — the right to *remove* a label is a
  delegable, revocable, observable authority. This is the prettiest junction with
  the caps axis: the two systems meet exactly at the declassifier, and every use
  of it is a `CapEvent`.
- Telemetry: `LabelFlow { from, to, label }` frames on every join; refusals
  snitch (`SyscallRefused`, house style). Tempo renders the taint propagation
  graph; Grafana counts flows per label.
- Granularity ladder: process-level (tractable, do this) → per-message → per-byte
  (research cliff, don't). **Stitch option**: the interpreter can do
  language-level fine-grained IFC (Jif-style) *inside* a process while the kernel
  does process-level — a two-layer flow story mirroring the two-layer authority
  story in the actor-model doc.

### Novelty vs prior art

Asbestos, HiStar, Flume built the mechanism (process-level labels) on Unix-shaped
systems; Jif did language level. None could show you a flow. **Observable IFC —
the trace as the answer to "why was this refused"** — has not been built, and
directly attacks the documented usability failure of the whole research line.

### Demo / post angle

`cat /secrets/key | transform | netcat`: fails **at the network write** — the
opposite end of the pipeline from every Unix intuition — and Tempo shows the
hop-by-hop taint acquisition that got it there. Editor tie-in: text yanked from a
secret buffer refuses to paste into a world-readable one, kernel-enforced, with
the refusal span pointing at the yank.

### First milestone

One bit ("secret"): FS-file label + process taint + clearance check at
`ConsoleWrite`. Itest: taint crosses one IPC hop and blocks a write; frames
record the path. Declassifier cap second.

### Open questions

Label creep containment (declassifiers, per-endpoint scoping); interaction with
`Revoke` semantics; covert/timing channels explicitly out of scope (say so in the
doc).

---

## 5. Budgets as capabilities — end-to-end cost attribution

### Thesis

CPU time and memory as **delegable, meterable authority**: a `Budget` is a cap
you hold, spend, split, and — crucially — *attach to an IPC call*, so work a
shared server does on your behalf is billed to you and bounded by you. This is
distributed tracing's "baggage," except **enforced**. It answers the question
Linux structurally cannot: cgroups stop at process boundaries, so cross-process
work-on-behalf-of (who caused the FS server's CPU burn?) is unattributable. Here
the delegation graph *is* the attribution graph.

### Mechanism sketch

- `Budget` object (cap): CPU-tick balance + frame quota. `Spawn` and `Call` can
  attach a slice. The scheduler charges the running task's **active budget** —
  when the FS server processes client A's `Call`, it runs on A's donated slice
  (this is seL4 MCS scheduling-context donation; SnitchOS's rendezvous
  Call/Reply maps onto it cleanly). `MapAnon` charges the frame quota.
- The per-task accounting machinery (`cpu_time_ticks`, entry-tick tracking)
  already exists — the change is keying attribution by `(task, active_budget)`
  and emitting per-budget metrics. Spans already carry `task_id`; add
  `budget_id` and per-principal cost *inside shared servers* falls out of the
  existing pipeline.
- Exhaustion policy: `Call` refuses (snitched) or degrades (priority drop) —
  a policy knob worth a design section. Servers need a *house budget* for
  housekeeping so client exhaustion can't wedge them.
- snemu makes billing **exact and deterministically testable**: "this call fails
  at precisely instret N when the budget hits zero" is an assertion, not a
  timing-flaky hope. (#3 is how you verify the accounting is honest.)

### Novelty vs prior art

seL4 MCS built the donation mechanism, with no observability and no attribution
story. cgroups/resource-controls can't follow work across processes. W3C baggage
propagates but is advisory. **Enforced baggage + observable per-principal cost
inside shared servers + exact deterministic accounting** is a fresh combination.

### Demo / post angle

Two clients hammer one FS server. Grafana shows per-client cost *inside* the
server; client A exhausts its budget → its calls refuse cleanly, B is untouched.
Editor tie-in: plugins get budget slices — a runaway plugin visibly saturates its
ceiling while the editor never stutters.

### First milestone

`Budget` object + charge a task's ticks against it + per-budget metric (no
donation yet). Then donation across `Call`. Priority-inversion interaction
(exhaustion mid-critical-section — the classic MCS problem) is the design doc's
hard section.

---

## 6. Processes as values — checkpoint, persist, resume

### Thesis

A process is data: pages + register context + cap table. Two of those serialize
trivially. The third is the research problem: **what does it mean to serialize
authority?** The proposed answer: **a serialized capability is a petition, not a
bearer token.** Checkpoint captures the process's authority as a typed, named
manifest of *requests* (exactly the redesign-from-scratch #2 shape); restore
**re-runs the delegation decision** — some restoring authority (init, the shell)
satisfies the manifest, all-or-nothing, and every re-grant is an observable
`CapEvent`. Authority never survives serialization as raw power; it survives as
a *description* that must be re-honored.

### Mechanism sketch

- Checkpoint only at **quiescent points** — a process blocked in `Receive` is a
  natural one (no in-flight IPC, no live `Reply` caps). Reply caps are declared
  non-persistable; endpoints serialize by name/badge into the manifest.
- The v0.13 **cap-id spine** already gives every holding a stable identity and
  parentage — the serialized manifest inherits provenance for free, and the
  restore's re-delegation is diffable against the original derivation tree.
- Image = pages + `TaskContext` + manifest → a file in the FS ("programs are
  data in the FS," extended from executables to *running* programs).
- Payoffs: suspend-to-FS; reboot-surviving services; editor session persistence;
  and the flagship — **resume the checkpoint inside a browser-tab snemu**, with
  its telemetry continuing as the same Tempo trace across the substrate hop. (No
  QEMU cooperation needed — this is why resume-from-checkpoint beats live
  migration as the demo shape.)

### Novelty vs prior art

KeyKOS/EROS did orthogonal persistence — whole-system, single-machine, raw
sealed caps; the lineage fits (they were cap systems) but their model can't
cross substrates or survive a hostile restore context. CRIU's file-descriptor
restoration hacks are the standing evidence that *authority* is the unsolved
part of process serialization. **Checkpoint = memory + manifest; restore =
observable re-delegation** is a fresh, publishable-shaped resolution.

### Demo / first milestone

`~> checkpoint spinner > spinner.img`, reboot, restore — it resumes counting
where it left off, same span names (the span/metric name GC work already handles
name lifecycle). Milestone: checkpoint a quiescent, IPC-less process to RAMfs,
restore under a new pid with re-granted bootstrap caps, itest the resumed state.
Browser demo after snemu M3.

### Open questions

Quiescence protocol for processes not conveniently blocked (checkpoint
notification? force-park at next syscall?); endpoint identity across restore
(names + badges vs fresh mint); what the manifest language is (this should be
the same answer as redesign #2 and Hitch — one authority-description language
for spawn, checkpoint, and IFC declassifiers).

---

## 7. Authority-layer interposition — chaos, strace, virtualization as one primitive

### Thesis

In a cap system, any authority can be **transparently replaced by a proxy** —
the holder can't tell. One primitive yields: semantic tracing (strace at the
protocol level, not the syscall level), fault injection, rate limiting,
filtering, and virtualization (a proxy that *reinterprets* the protocol). The
shell's `~>` delegation point is exactly where proxies compose in.

Honest note: this is the lowest-novelty axis — "interposition is trivial under
caps" is KeyKOS/seL4 folklore. It earns its place by being cheap, by being the
**substrate #4 and #5 run on** (declassifiers and userspace quota policies are
proxies), and by one genuinely fresh twist: under snemu's seeded scheduler,
**chaos is deterministic** — an injected-failure run is replayable, so a chaos
finding becomes a fixed-seed regression test, not a war story. Chaos engineering
without the flakiness is not a thing that exists.

### Mechanism sketch

- Ingredients mostly exist: endpoints, badges, `MintBadged`. A proxy holds RECV
  on a fresh endpoint + SEND on the real one and forwards. The likely missing
  kernel piece is **reply-cap forwarding** (can a proxy pass the caller's Reply
  cap through, so the real server replies direct — or must it double-hop?).
  Audit the ABI; this is the one kernel change.
- Then everything is userspace programs: `log-proxy` (semantic FS-op tracing),
  `chaos-proxy(policy, seed)`, `quota-proxy`, `subtree-proxy` (presents a
  subtree as the whole FS — **chroot/containers as an ordinary userspace
  pattern**, no kernel namespace machinery; flips the Linux-namespaces
  assumption that virtualization needs kernel support).
- Composition surface: `~> with-chaos 10% prog`, `~> sandbox-fs ./dir prog`.

### Demo / first milestone

Inject 10% failure into one process's FS cap; watch the supervision tree (the
redesign #6 work) recover, live in Grafana — then replay the same seed. First
milestone: audit/extend reply forwarding, write one generic forwarding proxy,
and an **indistinguishability itest**: a client behaves identically against
proxied and direct endpoints.

---

## Interaction map (why these are axes, not features)

- **#1 ⟷ #2**: the nondeterminism census and the nondet-cap classification are
  the same list at two scopes (system replay vs per-process replay/memoization).
- **#1 ⟷ #3**: replay fidelity is verified by diffing frames; diff divergence in
  cross-hart replay doubles as a race detector.
- **#2 ⟷ #6**: memoization needs input/output capture at the cap boundary —
  the same boundary serialization checkpointing needs; the manifest language is
  shared with redesign #2 and #4's declassifiers.
- **#4 ⟷ #7**: declassifiers and label-scoped endpoints are interposition
  patterns.
- **#5 ⟷ #3**: exact billing is only credible if the accounting is audited.
- **All ⟷ observability**: every axis emits its enforcement as frames — that's
  the house identity, and the recurring research claim is that *legibility is
  the missing half* of each prior research line (IFC's label creep, MCS's opaque
  donation, EROS's sealed persistence, chaos engineering's unreproducibility).

## Suggested doc treatment

Each axis that survives discussion deserves its own design doc in the house
style (thesis → mechanism → prior art → milestones → open questions). #1's
nondeterminism census and #7's reply-forwarding audit are pure-investigation
first steps that can start before any design commitment.
