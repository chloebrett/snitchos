# Post 53 — Following authority across a restart

- the v0.5 post followed a *trace* across a context switch — a span opened on task_a, survived a yield, closed on the far side, and Tempo drew the whole thing as one story. this post follows *authority* across a harder gap: a service crashes, a fresh process takes its place, and the question is whether the new one got its capabilities back. the answer has to be observable, because the failure mode is silent — a restarted service that quietly didn't get its cap leaves a client hanging with no error anywhere. so the real work wasn't the restart. it was building the thing that proves the restart carried the keys.

## supervision is capability ownership, viewed twice

- the reframe that makes this more than a systemd clone: **the supervisor owns the durable objects; services borrow authority from it for the lifetime of one incarnation.** a service is restartable *because* its authority isn't its own — it outlives the process holding it. init already had this shape and I'd called it a wart: init `EndpointCreate`s the FS endpoint and over-holds `RECV` after handing it to the server. under supervision that over-hold is the whole mechanism. init owns the endpoint object; when the server dies, init re-delegates against the new process's table, and every client's minted `SEND` still names the *same object*, so clients never notice. capability ownership and the supervision tree are the same tree.

- so "restart" decomposes into two moves that were already built: reap the dead incarnation (`WaitAny` → exit code), then re-run the delegation against the fresh `CapTable`. nothing new in the kernel. the supervisor is a table-walk plus a policy.

## the policy is pure, and it moved

- the decisions — dependency order, restart-or-stop-or-escalate, exponential backoff, the intensity storm-guard — are a pure function of policy, exit outcome, and history. no MMIO, no CSRs, no syscalls. that's the host-tested tier, same as the scheduler's aging math: 14 tests, mutation-clean (the one survivor was a `<` vs `<=` on the intensity window boundary — killed by a "restart exactly one window old has aged out" test).

- I wrote it in `kernel-core` out of habit and it was wrong. the *consumer* is a userspace engine, and userspace **cannot depend on `kernel-core`**. the deps chart settled it: the shared tier (`fs-core`, `hitch`, `ramfs`) is exactly the crates both the kernel and userspace reach, so the policy became its own `supervision` crate there. a small correction, but the kind the dependency graph makes for you whether you like it or not.

## the storm guard earns its keep

- the engine walks the table, brings services up in `startup_order`, and loops on `WaitAny`. each exit consults the policy: restart after a backoff, stop, or — once a service crash-loops past its intensity budget — **escalate**, which at the root means halt. that last branch isn't polish. a service that crashes and restarts with no ceiling is a busy-loop that floods the telemetry channel and starves everything else. the intensity guard (N restarts inside a window → `Escalate`) is what turns a crash loop into a bounded, reported event instead of a livelock.

- the demo table is a stable `spinner` plus a `crasher` that fails every incarnation. you watch `backoff_ticks` double, `restarts_total` climb, and then `escalated` trip — the crash-loop line rising in Grafana until the guard cuts it. the state gauge (Starting → Running → Backoff → … → Escalated) draws the per-service timeline for free.

## the oracle: make the service snitch on itself

- here's the part that's actually SnitchOS. the kernel's `CapEvent::Transferred` is the supervisor's *claim* — "I re-granted the endpoint to incarnation 3." a claim is not proof. so the restarted service reports on *itself*: it enumerates its own `cap_list` and emits whether it actually holds an `ENDPOINT` cap named `svc-ep` with `SEND`. the supervisor's claim and the holder's independent report, cross-checked. the snitch-on-the-snitch.

- but a single "I hold it" isn't enough — the *first* incarnation holding the cap only proves the initial grant, not the re-grant. the failure I'm hunting is "initial delegation works, restart silently doesn't." so the test leans on a property of the harness I hadn't used deliberately before: `wait_for` advances a cursor. order the assertions and the order becomes the assertion. wait for `holds_endpoint == 1`, *then* wait for a restart, *then* wait for `holds_endpoint == 1` again. that second confirmation is now provably from a post-restart incarnation reading a fresh table. if re-grant were broken, later incarnations would report `0`, the second wait would find nothing, and the test would time out instead of falsely passing. the ordering is the proof.

- and it caught a real thing on the way in: I'd assumed minted caps might not carry the object name, and almost weakened the check to just kind-and-rights. left the `svc-ep` name check in — it passed, which *told me* minted caps do propagate the object name through a delegation. the assertion doubled as a probe.

## the bug the capture told me about, not the logs

- first telemetry run hung. `budget_exhausted`, 20 seconds, where the previous version passed in 0.4. the reflex is to reread the engine logic. instead I read the capture first (the rule I keep having to relearn): metrics present *early*, then eight `SyscallRefused` frames, then nothing — and crucially no `escalated`. the control flow was fine. the *registrations* were being refused.

- the per-process metric table is 16 slots, and registration does **not** dedup by name. I was re-registering `state`/`backoff`/`restarts` on every loop iteration — the ergonomic `register_counter("x").emit(v)` one-liner — and after 16 the table was full, so the *terminal* `escalate` counters got refused and the test never saw the frame it was waiting for. the fix is boring: register once, keep the handle (it's `Copy`), emit through it. the lesson isn't. a "silent no-op on refusal" is a lovely API property right up until the thing that goes silent is the thing your test is waiting for.

## what I'm not pretending I built

- the Tempo money-shot — an umbrella span per service, a child span per incarnation, restart continuity drawn as one tree — isn't here. the kernel span cursor is per-task LIFO: `SpanClose` is validated against the cursor top, so one supervisor task can't hold two services' umbrella spans open across the `WaitAny` loop without closing them out of order. that tree needs spans that take an explicit parent id instead of nesting on a stack. it's a real design, deferred honestly rather than faked with point events.

- and the re-grant is still by hand: `launch` delegates one endpoint directly, not through the general `satisfy(manifest)` path the checkpoint work will share. the proof is real; the plumbing is bespoke. next.

## what I learned

- **a restart is a re-grant.** the interesting half of supervision isn't relaunching a process — the kernel already reaps and re-spawns. it's that authority has to be re-satisfied against a brand-new table, and that step fails *silently*. build the oracle before you trust the mechanism.

- **order can be an assertion.** a cursor that only moves forward turns a sequence of "does this frame exist" checks into "does this frame exist *after* that one." that's how a single `holds_endpoint == 1` stops meaning "it was granted once" and starts meaning "it was re-granted."

- **read the capture before the code.** the hang looked like a logic bug and was a quota bug two layers down. the frames said "refused" plainly; the source said nothing. the transcript is the source of truth for a system whose whole point is that it narrates itself.

## what's next

- v1 is crash-restart, and it's shipped: pure policy, the engine, the telemetry, and the cap-re-grant oracle, all on the wire. v2 needs two new capability-gated syscalls — a supervisor-initiated `Kill` (authorized by a lifecycle cap, so you can only kill what you launched) and a `WaitAny` with a deadline (to catch alive-but-wedged services) — and on top of them, graceful shutdown in reverse dependency order. the honest split: v1 knows how to bring a service back. v2 is where it learns to take one down.
