# snemu itest — the discovered snapshot tree

**Status:** increments 1–2 **shipped** (state-hash dedup deferred); 3–5 unbuilt.
Behind `cargo xtask snemu-itest --share-snapshots` (off by default — the A/B
baseline). On the full 111-scenario suite the collapse is verdict-identical to the
fork-per-scenario path (the oracle), 99/111 scenarios collapse onto shared forward
runs, and total guest instret drops **1479M → 275M (−81%)** (serial wall-clock
25.2s → 19.9s). Increment 2 adds interactive **fork-node** sharing: the 7 `stitch-fs`
and 3 `stitch-repl` scenarios each share one materialised pre-injection node. Pure
core + tap in `xtask/src/itest/snapshot_tree.rs` + `harness.rs`; orchestration in
`snemu_audit.rs`.

**Correctness beyond the oracle — the self-healing fallback.** The collapse depth is
a *hint* (the prior run's instret); a too-short shared stream would silently fail a
positive scenario. Rather than trust the hint, a collapsed **failure** falls back to a
live run for the authoritative verdict, records the true instret (curing the depth
next run), and logs it. So collapse is a pure optimisation that **can never produce a
false verdict** — proven by poisoning *every* depth to 500 K instret: 96 scenarios
fell back to live and the suite still passed 111/111 identical to baseline, then the
next run self-healed to full collapse. This is a different guard than the design's
state-hash (which stays a follow-up for its determinism-regression value).
**Prereq context:** [snemu progress], `xtask/src/itest/snemu_audit.rs` (the current
boot-once/fork-per-scenario harness), the packing/right-sizing work in snemu post 08.

## thesis

The itest suite re-runs the same guest execution many times over. Most of that
repetition is discoverable — not declared — because snemu's guest is a pure function
of its initial state and the harness's input, and nothing else. So we can *derive*
the maximal-sharing structure of the whole suite from the executions themselves,
share each common prefix once, and schedule the rest. The structure is a **tree**
(execution states extend but never merge), the sharing is **content-addressed and
self-verifying** (equal states hash equal, unequal states are a determinism bug), and
the payoff is less total work for the same fidelity.

We build the **discovered** version (find the forks by running) and skip the
**declared** version (annotate the forks by hand). Determinism is what makes discovery
both possible and checkable, and it's the property the whole emulator's value rests
on — so the optimizer and a determinism regression test become the same artifact.

## the redundancy we're paying for

Today: boot each distinct workload once to the `entering heartbeat` checkpoint,
snapshot it (`Machine: Clone`), and fork that snapshot per scenario. Each scenario's
`View` then steps its own machine forward, watching for telemetry frames and
occasionally feeding console input.

Two observations make the waste visible:

1. **Observe-only scenarios of a workload run the guest identically.** Of ~111
   scenarios, only ~11 feed input; the rest just watch. Two watchers forked from the
   same snapshot step the *same deterministic guest* forward and see the *same frame
   stream* — they differ only in what they assert. We currently re-execute that
   identical forward run once per watcher.
2. **Interactive scenarios share a prefix up to their first input.** A scenario that
   sends a keystroke diverges from its siblings exactly at that keystroke — and not
   before.

Both are the same phenomenon: scenarios fork only where their *input* differs.

## the key insight

> The guest is a pure function of `(initial state, harness input)`. Therefore two
> scenarios' executions coincide exactly up to the first point their harness input
> differs.

This holds **because snemu is a closed deterministic system**: `rdtime` returns the
instret clock (not wall-clock), RAM is zero-initialised (not garbage), multi-hart
interleaving is fixed round-robin (not race-dependent), and there is no entropy
source. The only thing the guest can't predict is what the harness injects.

That turns fork discovery from "diff two million-instruction traces" into "prefix-match
the input events." A scenario's *branch key* is the sequence of `(instret, bytes)`
console injections it performs. Two branch keys share a prefix ⇒ the two scenarios
share guest execution up to their first divergent injection. Observe-only scenarios
have the empty key ⇒ they share the entire forward run.

### the fork structure is a tree, not a DAG

An execution state is the result of one specific history; two histories have no
canonical join. You can splice a new branch off an existing state (fork), but you can
never weld two states into one (merge) — the shared global kernel state (frame
allocator, heap, scheduler, intern table) collides pervasively, so a memory-level
merge is undefined. See the "merge would need declared resolution or disjointness,
neither of which raw kernel memory gives you" discussion in the post-08 notes.

This is not a limitation to route around — it is *why the scheduling is tractable*.
Tree/forest precedence-constrained scheduling has near-optimal algorithms (T.C. Hu's
level algorithm, 1961, for the unit-time case); general DAG precedence scheduling is
NP-hard. Keeping it a tree is load-bearing.

## the model

A single rooted tree per suite run:

```
                 (cold boot, hart images loaded)
                          │  ~200M instret, once
              ┌───────────┴───────────┐
        workload=demo             workload=init(None)     … one child per distinct workload
        (boot→checkpoint)         (boot→checkpoint)
          │                          │
   ┌──────┼──────┐            ┌──────┴──────────────┐
 watch  watch  send 'x'@5M   watch  watch   send '\n'@8M   … scenarios fork where input diverges
 (leaf) (leaf)   │           (leaf) (leaf)     │
              [deeper]                       [deeper]
```

- **Root** = cold boot / loaded image (shared by everything).
- **Internal nodes** = execution states at fork points: each distinct workload's
  boot checkpoint, then each point where a subset of scenarios injects a divergent
  input. A node is *materialised* by running its parent forward to the fork point.
- **Leaves** = scenarios. A leaf's assertion runs against the frame stream produced
  along its root-to-leaf path.
- **Edges** = execution segments; edge cost = the segment's instret (the estimate the
  scheduler uses).

Materialising a node is a run-forward; **forking** it for children is a cheap clone
(cheaper still with copy-on-write, below). So a node, once materialised, unblocks all
its children at once with negligible marginal cost — the benign shape Hu-style
scheduling assumes.

## discovery

Discovery is *free in wall-clock*: the first run of the suite executes every scenario
anyway. That pass records the tree; subsequent runs consume it.

1. **Record branch keys.** As each scenario runs, log its `(instret, bytes)` console
   injections. (The harness already routes all input through `push_console_input`;
   this is a tap on that path.)
2. **Build the tree by common input-prefix.** Group scenarios; split a node wherever
   their injection sequences first differ. Observe-only scenarios collapse to the
   workload node.
3. **Content-address and verify with state hashes.** At each claimed shared fork
   point, hash the machine state for the scenarios that are supposed to coincide
   there. Equal hash ⇒ confirmed shareable (and the two can literally share one
   materialised node). **Unequal hash ⇒ a determinism leak** (or the input model is
   incomplete): the guest diverged with identical input, which is a bug, and
   discovery reports exactly where.
4. **Persist + invalidate.** Cache the tree (a JSON sibling of the packing report).
   Editing a test changes its branch key or a segment's cost; re-discover only the
   affected branch, keep the rest warm. (This is memoisation of execution — the
   self-adjusting-computation angle.)

The zero-input collapse is discovery restricted to the empty key: it needs no tree
and no hashing to be *correct* (identical workload + no input ⇒ identical run by
construction), only to be *verified*. It's the first increment for that reason.

## the determinism invariant (and why it audits itself)

**Invariant.** The guest's trajectory is determined solely by `(initial state,
recorded harness input)`. snemu upholds this today. Anything that would add a hidden
input — a hardware RNG, the `seed`/Zkr entropy CSR, a real-time clock, uninitialised
reads used as entropy — must be **seeded from inside the harness, per scenario**, so
it stays a closed, replayable source. An open entropy source breaks the input-prefix
tree.

**Self-auditing.** The state hash at every fork point *is* a determinism check. A
hidden input cannot silently corrupt the tree, because it would make two same-key runs
hash differently, and the tree-builder would refuse to dedup them and flag the
divergence point. So the snapshot optimiser is simultaneously a continuous
determinism regression test — it has value even if the speed win is modest, because
it guards the property the emulator's entire usefulness depends on.

## scheduling

The tree turns the flat LPT packing into **precedence-constrained list scheduling**:
a leaf can't start until its snapshot node is materialised. Use the weighted
generalisation of Hu's level idea — **critical-path (bottom-level) list scheduling**:

- Priority = longest remaining path, in estimated instret, from a task down to a leaf.
- Whenever a worker frees, hand it the highest-priority *ready* task (a node whose
  parent is materialised, or a leaf whose node is ready).

Why this is robust to *estimates* (we only have prior-run instret, not true wall-time):

- **It's online-greedy.** It never commits to a plan; it fills workers as they free,
  self-correcting on actual completion times. Estimates steer priority, not a rigid
  schedule.
- **Graham's bound is estimate-independent.** Any list schedule lands within
  `(2 − 1/m)` of optimal; good priorities pull the typical case to ~10–20% (where the
  current flat packing already sits at 85% utilisation).
- **Our estimate is deterministic.** Prior-run instret is exact and reproducible — a
  strong *relative-order* predictor, unlike sampled wall-time. Its one distortion
  (instret ≠ wall-time under the JIT) is smooth and monotone, not noise.

The one place estimates bite is the **critical path** — the deepest root-to-leaf
chain floors the makespan, so under-estimating it risks starting it too late.
Bottom-level priority is the mitigation: deepest chains launch first even under rough
numbers, and on a tree the critical path is just a walk to a leaf.

## enabling machinery: page-dirty tracking (shared with CoW)

One mechanism unlocks both the fork clone *and* the state hash cheaply:
**page-granularity dirty tracking** over guest RAM.

- **Copy-on-write forks.** A child shares its parent's clean pages and copies only on
  write. Forking a node becomes near-free regardless of RAM size — removing the
  per-scenario deep-clone cost entirely (the 16 MiB right-sizing helped; CoW ends it).
- **Incremental state hash.** Maintain a Merkle hash over pages; a write dirties a
  page, and a hash query re-hashes only dirty pages since the last query and folds
  them in. Hashing a fork point costs O(pages touched since the last), not O(RAM).

Both fall out of the same dirty-set, so build it once. (snemu already tracks a write
*high-water*; this generalises that to a write *set*.)

## increments (each behaviour-preserving, oracle-checked)

1. **Zero-input collapse.** ✅ **Shipped** (`--share-snapshots`). Each workload's
   guest runs forward once to the deepest instret any observe-only scenario needed
   last run (prior-instret depth; budget fallback); every observer replays a prefix
   of that recorded `(instret, frame)` stream truncated to its own budget
   ([`frames_within`]), so a positive *and* a negative-oracle verdict match the live
   path exactly. Classification is the **branch key**: injections (tapped in
   `View::send_input`) *and* a console-read flag (tapped in `wait_for_log`) — a
   non-empty key or a console read runs live. Keys persist to
   `.itest-runs/snemu-branch-keys.json`, stamped with the kernel fingerprint (a
   kernel change discards them). First run discovers (nothing collapses); later runs
   consume. Verified: 111/111 verdicts identical to sharing-off, −81% instret.
   - **Console-read limitation + fix (follow-up).** Today a `wait_for_log` scenario
     can't collapse because the shared stream carries only telemetry frames, not the
     UART byte stream `wait_for_log` scans. The lift: **record the guest's UART
     output alongside the frame stream in the shared forward run** (snemu already
     exposes `uart_output()`), and give the collapsed replay `View` that recorded
     UART so `wait_for_log` matches against it — same truncation discipline (the
     scenario sees UART bytes emitted within its budget). That folds the
     ~console-output watchers into the collapse too; only genuinely *interactive*
     (input-injecting) scenarios would stay live. Deferred — worth it once the input
     tree (increment 2) is in, since it's the same "record-once, replay-truncated"
     shape applied to a second output channel.
2. **Input-prefix tree + state-hash dedup.** ✅ **Fork-node sharing shipped**;
   state-hash dedup deferred. Interactive scenarios can't replay a recorded stream
   (their result frames depend on the input they feed), but they still share the
   deterministic execution boot→first-injection with every sibling on the same
   workload whose first injection lands at the same instret. That shared
   **pre-injection fork node** is materialised once (`advance_machine_to`) and cloned
   per child; the child's body re-runs boot→injection as already-buffered frame waits
   (no stepping) and injects from the shared state. On this suite the tree is shallow
   (scenarios diverge at their *first* injection, so no deeper prefixes), but the
   `stitch-fs` node is shared 7 ways and `stitch-repl` 3 ways. Correctness rests on
   the same A/B oracle + the self-healing fallback (above), not yet the state hash.
   - **State-hash dedup (deferred).** The design's internal check — hash each fork
     point, equal ⇒ confirmed shareable, unequal ⇒ determinism leak — needs a
     `Machine::state_hash()` in snemu (hash guest RAM via the write-set + registers).
     Its value is the *determinism regression test* (catches a hidden entropy source
     or a mis-share **at the fork point**, before it reaches an assertion), which the
     verdict A/B and the fallback don't provide as precisely. Worth building next; the
     fallback covers the *correctness* role in the meantime.
3. **Precedence-aware scheduler.** Wire the tree into the existing list scheduler as
   bottom-level priorities with the materialise-before-run constraint.
4. **CoW forks + incremental hash.** The dirty-set machinery, if increments 1–2 used
   a cruder clone/hash to start.
5. **Visualisation.** (below)

## correctness / oracle

This is an optimisation: the *set of assertions and their verdicts must be identical*
to the current fork-per-scenario harness. Same discipline as the decode cache,
idle-skip, native-ops, and the block JIT — a flag (`--share-snapshots` off by
default) so a run with sharing on is provably identical to one with it off, on the
full 111/111. The state-hash verification is an *additional* internal check beyond the
verdict comparison, so a mis-share can't reach an assertion.

## visualisation

Render the discovered tree (a JSON sibling of the packing report, consumed by the
existing `viz/` renderer):

- **The tree** — cold-boot root, workload nodes, fork nodes, scenario leaves; edge
  length = segment instret, so a fat shared trunk looks fat.
- **Reuse meter** — shared-prefix work (run once) against the counterfactual where
  every leaf re-runs its whole path; a "reused vs total" figure, in the spirit of the
  packing counterfactual but for setup.
- **Shape at a glance** — the shape *is* the ROI. Bushy-and-shallow (everyone shares
  boot, diverges immediately) ⇒ the zero-input collapse is the whole win and deep
  snapshots buy little. Deep-and-sparse ⇒ fat shared setup (the fs-*/ipc-* server
  bring-ups) worth cutting into. You read "is deeper snapshotting worth it?" off the
  picture.
- **Optional Gantt overlay** — which worker materialised which node, when; the
  critical path highlighted. Tree + Gantt is the whole thesis in one image: here's the
  redundancy, here's what we reclaimed, here's the chain that floored the makespan.

## non-goals

- **Merge / DAG.** States can't merge; the structure stays a tree (and that's what
  keeps scheduling tractable).
- **Declared snapshots.** No hand-placed markers. Discovery derives the tree; if we
  ever want markers, discovery *tells us where to put them* rather than us guessing.
- **Open entropy in the guest.** Any future entropy source must be harness-seeded, or
  the whole approach is invalid (and the state hash will say so).
- **Cross-run guest changes without invalidation.** A kernel change alters every
  branch; the cached tree must invalidate on the kernel binary's hash.

## open questions

- **Does depth pay?** The zero-input collapse is clearly worth it (most work,
  no tree). Whether *deeper* snapshots (subdividing interactive families) earn their
  complexity depends on how much expensive setup is shared beyond boot — the viz's
  shape is the first read, and the per-workload instret/footprint data the second.
- **Hash granularity.** Page-Merkle vs a coarser periodic digest; the cost of hashing
  at fork points vs the frequency of fork points.
- **Input-timing sensitivity.** The branch key includes *when* an input is injected.
  Confirm that injection timing is itself a deterministic function of observed frames
  (it should be), so two scenarios feeding "the same input in response to the same
  frame" genuinely coincide — the state hash is the backstop.
- **Interaction with the block JIT / native-ops flags.** The shared execution must be
  produced under a fixed flag set, since those change instret timing (not verdicts).
  The tree is per-flag-configuration, or discovered under the canonical config.
