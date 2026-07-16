# Post 54 — The authority to end a thing

- post 53 followed authority *across a restart* — a service crashes, the supervisor re-grants its caps, and the restarted process proves it holds them. that was the "bring it back" story. this stretch has two halves: first I went back and made "bring it back" honest — re-granting the way the rest of the system already does it, and proving a client's authority actually *survives* a restart instead of just looking like it does. then I turned the whole thing around. bringing a service back is only half a lifecycle. what does it take to bring one *down*? and the first question there turned out to be the interesting one: what authorizes ending a process at all?

## the re-grant, done the real way

- v1's supervisor minted the child's cap by hand — `endpoint.mint_badged(0, SEND)`, hardcoded. that's not how the rest of SnitchOS delegates: the FS satisfier and the checkpoint path both run authority requests through one shared primitive, `hitch::satisfy(needs, have)`, which reads a child's *declared* needs and matches them against the caps the granter holds. so I routed the supervisor through it too. the crasher now declares `needs: [Slot { svc-ep, ENDPOINT, SEND }]`, and the supervisor is just another satisfier.

- it hung on boot the first time — instantly, `budget_exhausted`. the capture told the story before I could theorize (the lesson I keep re-learning): `unsatisfiable`, `halted`, no grant. the bug is a lovely little conceptual one. `satisfy` matches a need against the rights the holder **advertises**, and I'd advertised the endpoint's literal rights: `RECV | MINT`. but the child needs `SEND`, and `RECV | MINT` doesn't *contain* `SEND` — so satisfy refused, and the supervisor escalated on the spot. the fix is to advertise what you can **provide**, not what you hold: a `MINT`-holder can mint *any* right, so it advertises `RECV | SEND | MINT`. the satisfier had this right all along (`MINT | SEND`); I just hadn't understood why until I broke it. attenuation matches on the menu, not the pantry.

## the cap outlives the process

- the claim v1 made but never *proved*: a client's cap survives its server restarting, because the cap names the durable object, not the process. so I built the proof as a workload. the supervisor owns an endpoint; it grants a persistent client a minted `SEND` and a crashing server a minted `RECV`. the server serves exactly one request and exits non-zero. the supervisor respawns it. the client just keeps sending — over the *same* cap it was handed once.

- each `send` is a rendezvous: it completes only when *some* live server receives it. so a second completed send, landing *after* a restart, can only have reached a fresh incarnation — using a cap the client never re-acquired. the itest makes the ordering load-bearing: it advances a single cursor through `sent == 1` → `server.restarts_total >= 1` → `sent == 2`. that third frame is the whole proof; if the cap didn't survive, it never arrives and the test times out instead of lying. (two small things fell out: a minted badge-0 `RECV` receives fine — the kernel's receive path ignores the badge — and a `Spawn`-delegated child reads its endpoint at `delegated_handle(0)`, not the legacy startup slot. both are the kind of thing you only learn by having the wrong one hang.)

## the other direction: killing is a capability too

- with "bring it back" honest, I started v2: taking a service *down*, for graceful shutdown and for restarting a service that's hung rather than dead. two design calls came first, and they're the good part.

- **shutdown is not a kernel feature.** the `Kill` the kernel gives you is *hard* — it terminates and reaps, full stop. *graceful* is a userspace pattern built on primitives that already exist: the supervisor `Signal`s a shutdown notification the service opted into, waits for a clean exit, and hard-kills only as a fallback. SnitchOS has no signals on purpose; a notification is the signal. the kernel stays minimal and the policy lives where policy belongs.

- **the right to kill is itself a capability.** the easy version is a parentage check — "you may kill a task if you spawned it." it works, but it's ambient: it can't be delegated, so a sub-supervisor could never be handed authority over its own subtree. so instead `Kill` is authorized by a real object — a new `Object::Process { id }` capability carrying a `KILL` right, minted into the parent at `Spawn`. now the power to end a process composes and flows like every other authority in the system: you can delegate it, attenuate it, revoke it, and — of course — *watch* it. every kill will be a `CapEvent` on the wire, same as every grant. the lifecycle became symmetric: creation mints a cap, destruction spends one.

## knowing where to stop

- I built v2 the way v1 went: policy first. `teardown_order` is just the reverse of `startup_order` — stop dependents before their dependencies — three tests, done. then the capability primitive, host-tested end to end: the ABI numbers, `Object::Process`, the `KILL` right, and `invoke_kill` (which validates the right and the object exactly like `invoke_recv`). all green.

- then I stopped. the actual kernel mechanism — `kill_task`, terminating a task that *isn't the one running* — is genuinely new scheduler surface. the target might be ready, or blocked mid-IPC in some wait structure, or running on the other hart and needing an IPI to halt. the self-exit path doesn't generalize to any of that, and I wasn't going to write untested task-termination at the tail of a session and risk panicking the scheduler. so `Kill` is wired end to end and the kernel compiles, but `handle_kill` is an honest inert stub that refuses — the ABI and the cap plumbing are exercised, the dangerous part is a clearly-marked TODO. a half-built primitive that refuses cleanly beats a whole one that's never been run.

- (a smaller tax, worth noting: adding one variant to the `CapObject` wire enum broke `x collect` on an exhaustive match three crates away. a new wire kind ripples to every decoder — the collector, the diagram builder, the stability snapshot. the type system found them all, which is the point, but it's a reminder that the wire format has more readers than you remember.)

## what I learned

- **attenuation matches on what you can provide, not what you hold.** the satisfier advertises the rights it could mint, not the bits its cap literally carries. I understood the *why* only after advertising too little and watching the supervisor refuse itself to death on boot.

- **a rendezvous makes survival self-proving.** I didn't have to inspect the client's cap table to show the cap survived — I just had to watch a message it sent arrive somewhere, after the thing it was talking to had died and come back. the send completing *is* the proof.

- **make the verb a noun.** "may kill" as a relationship doesn't compose; `Object::Process` as a capability does. turning an implicit permission into a first-class, delegable, observable object is the same move SnitchOS keeps making — and it's why the kill will show up in the trace next to the grant.

## what's next

- the part I deferred: `kill_task`. before writing it I owe the plan a real design — a task-state matrix (ready / blocked-in-which-structure / running-cross-hart) with a safe extraction procedure for each, and an honest line on how much v2a takes on versus defers to v2b. then `Spawn` starts minting the `Process` cap, `handle_kill` grows teeth, and the graceful reverse-dependency shutdown gets to draw its money shot: a service tree coming down in the exact mirror of how it came up, every stop a `CapEvent`, on the wire.
