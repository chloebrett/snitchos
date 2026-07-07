# Post 48 — The shell learns to give

- post 44 left an IOU. the shell grew `grant` and `revoke` for moving authority within your own table, but a powerbox that only rearranges what it already holds is still a closed loop. the next step was the one that opens it: hand a capability not to yourself but to a program you launch. `view a-file`, spawn a viewer, delegate a read cap to exactly that file, watch the `CapEvent` cross the process boundary, and revoke it when the viewer is done. the shell has hands now — post 44's line — and this post is what it means to extend one.

## the three pieces

- **the viewer** is the minimal end of the thing: it declares `needs = [("file", ENDPOINT, SEND)]` and gets a READ-only file cap at its single delegated slot. no FS connection of its own, no ambient access to anything — just the one endpoint its parent chose to hand it, narrowed to the one thing it needs to do. it reads in a loop, emits `snitchos.viewer.bytes_read` with the byte count, and then yields a few times before exiting. the reason for the yields is the interesting part, and I'll get to it.

- **view-demo** is the automated version of the powerbox half — the launcher that makes it testable without typing. it connects to the seeded FS, navigates to `bin/spawnee` with a READ-only lookup, and gets back an attenuated endpoint whose badge encodes that exact file. then `spawn(VIEWER_ID, &[file_cap])` — one call, the cap crosses the process boundary, and the kernel fires a `CapEvent::Transferred`. view-demo yields to let the viewer reach its IPC, calls `revoke(file_cap)`, and then waits for the viewer to exit. that's the whole loop, scripted.

- **the shell** is the same loop, interactive. `needs = [("fs", ENDPOINT, SEND)]`, emits `shell.ready` when it reaches its input loop, then reads `view <path>` from the UART. the path gets walked component by component through the FS — `bin/spawnee` becomes a lookup of `bin` and then `spawnee`, both with READ rights — and the resulting cap gets handed to the viewer. the itest waits for `shell.ready`, injects the command, and then asserts on the telemetry. one command: five system calls.

## the timing is the story

- here is the thing I didn't predict. I had the sequence in my head: viewer reads, viewer exits, parent revokes. but that sequence is wrong, and the wire shows it plainly.

- when the viewer calls `file.call(read.encode())`, it blocks — the IPC rendezvous is synchronous; the caller suspends until the server replies. at the exact moment the viewer is mid-IPC, the parent gets the CPU back. the parent calls `revoke`. the `CapEvent::Revoked` fires. then the FS server finishes processing the Read request, the reply lands, the viewer wakes up, gets 256 bytes, and emits `bytes_read`.

- so the wire order is: **Revoked, then bytes_read**. the authority was reclaimed while the read was in transit. and the read still completed — because the FS server already had the request. revocation closes the future, not the past. once the IPC was in the server's hands, the viewer's cap was beside the point for that call. what revocation prevents is the *next* call — which, if the viewer tried one, would be denied.

- this is what you want from a powerbox. you don't wait for the viewer to finish and then clean up; you revoke the moment you've decided the session is over, and the kernel enforces it forward from there. the data already committed completes; anything after the line is shut off.

- the viewer's four `yield_now()` calls at the end of main are there to make the revocation observable. when a process exits, the kernel releases all its capabilities — the entries simply vanish. that's not a revocation; it's a cleanup. a `CapEvent::Revoked` only fires when a *living* process's descendant is swept. so the viewer has to stay alive past the point where it emits the metric, just long enough for the parent to revoke while the cap is still in the table. four yields buys that window reliably. without them: the viewer exits first, the cap disappears as part of teardown, and the parent's `revoke` finds nothing to sweep — no event, no trace.

## what the trace shows

- three events in Tempo, in order. `CapEvent::Transferred` — the READ-only endpoint crosses from parent to viewer, parent_cap_id linking it back to the FS's original grant. `CapEvent::Revoked` — the authority window closes. `snitchos.viewer.bytes_read` — the data that made it through before the gate came down. the trace doesn't say "a file was read and then a cap was cleaned up." it says: here is the moment authority was delegated, here is the moment it was reclaimed, and here, between those two timestamps, is what happened while it was live.

- that's "watch least-authority happen" made literal. not a log of events that implies authority moved; the authority events themselves, on the timeline, with the data they authorized sitting between them.

## what I learned

- **the IPC commits before the cap does.** once a `call` is in-flight, the server processes it on its own terms — the caller's cap table is irrelevant to that already-running operation. revocation is a forward guarantee, not a retroactive one. this is correct and probably obvious in retrospect, but it's the kind of thing you have to see on the wire to fully trust.

- **"alive" is a precondition for "revoked."** a cap that exits with its process produces no telemetry. a cap that gets swept while its holder is running does. the yields aren't a hack — they're the acknowledgment that revocation is an *act*, not an artifact, and acts need a subject that's still present to act on.

- **five lines of shell code, five system calls, three observable events.** the whole powerbox hand-off — delegate, use, reclaim — fits in a tight loop. it doesn't need a framework or a runtime or a permission table checked at call sites. it's the mechanism working the way the mechanism was designed to work, and the telemetry falling out of it for free.

## what's next

- the shell has one verb — `view`. the natural next verbs are the ones that make it a real delegation shell: `spawn` (launch any SPAWNABLE with a chosen set of caps), and eventually `grant` (hand a cap to an already-running process, not just a new one). but those are downstream of the post that's still pending: the Stitch core redesign Phase C, which gives Stitch a faithful surface AST and a lowering pass to core IR. the shell is Stitch — the language needs to be right before the shell grows more verbs through it.

- the other thing the trace is missing: the `CapEvent::Revoked` is there, but the collector doesn't yet draw the authority timeline — the span that covers the delegation window, with Revoked as its close. the events are all on the wire. wiring the collector to turn them into a proper timeline span is the step that makes the Tempo view tell the story without squinting.

## postscript — a flaky test and the wrong measurement

- there was a loose end to tidy after this post shipped. `ipc-wakeup-is-prompt` — a scenario that asserts the IPC receiver opens its span within 100ms of the sender — was failing about 13% of the time. the error message said "the woken receiver waited for a timer tick because the idle loop wfi'd past ready work." that turned out to be wrong about the cause.

- first attempt: add a self-IPI from `sched::wake()` so that if the secondary hart's idle is in `wfi`, the pending SSIP breaks the sleep without waiting for a timer tick. ran 100 reps. failure rate went to 20%. worse.

- the actual problem was in what the test was measuring. the scenario compared kernel timestamps: `t` from the sender's `ipc.send` SpanStart against `t` from the receiver's `ipc.recv` SpanStart. the kernel captures `t` at the top of `SpanOpen`, before the virtio TX spin — and that spin runs with SIE=0 (inside a syscall, timer masked). when the heartbeat on hart 0 holds the CONSOLE mutex, the sender's `SpanOpen` can stall for 100ms+, all of it invisible to the scheduler. the gap `t_recv − t_send` was measuring heartbeat contention, not scheduling latency.

- the fix: use host-side arrival times instead of kernel timestamps. the `ipc.send` frame only lands on the wire after the slow TX completes, so the gap from that point to when `ipc.recv` arrives reflects only how long it took the receiver to get scheduled and emit its span — which is the thing the test was always supposed to measure. 0/100 after the change. the scheduler was fine the whole time; the ruler was wrong.
