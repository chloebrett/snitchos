# Post 51 — Debug told the truth

- two integration scenarios had been green for milestones. the viewer reads a file a parent delegated to it, prints it, and reports how many bytes it read. solid, boring, passing. then I gave `snemu-itest` a `--opt=<low|mid|high>` dial — debug, release-with-a-safe-userspace, all-release — and pointed the debug end at the suite. two scenarios went red. the viewer read **zero** bytes. release was fine; debug was not. the reflex was "snemu can't emulate the debug build." the reflex was wrong, and chasing why is the whole post.

## the flag that asked the question

- the three regimes exist because they have *different failure modes*, and being able to flick between them turns "works on my machine" into a dial: same kernel, three timings, three answers. `mid`/`high` green, `low` red on exactly two scenarios, both `bytes_read=0`. one dial position disagreed with the other two, which is the most interesting thing a test can do.

- the easy story wrote itself: snemu is a from-scratch RISC-V interpreter with a known fidelity gap; debug codegen stresses it differently than release; of course a corner falls through. plausible, and lazy. the smell said otherwise — a *value* going to zero under one timing is what a race looks like, not what a decode bug looks like.

## the differential oracle, and what it can't see

- the way to settle "is the emulator wrong or is the code fragile" is the differential oracle: boot the **same** kernel under snemu and QEMU, diff the telemetry frame streams. its verdict was **faithful** — identical boot prefix, identical registered-name vocabulary. crucially, `snitchos.viewer.bytes_read` is registered in *both* streams: the viewer runs in snemu, reads, and emits its metric. what differs is the metric's *value* — and the oracle normalizes metric values to zero, because they drift with wall-clock. so the oracle literally cannot see the failing field. but that's the point: it rules out its own suspect. snemu produces the same frames as QEMU; the bug is in the kernel and userspace, and snemu's timing merely *loses a race* QEMU's timing wins.

- (I had to sharpen the oracle before I trusted it. it was halting the diff at frame 157 on a benign `mhartid` difference — which physical hart OpenSBI picked to boot, pure firmware noise. a tool you lean on to say "faithful" should at least look past that, so I normalized it; the diff now reaches the real deterministic boot prefix before it stops at legitimate timing divergence.)

## the race, written down as a feature

- the powerbox demo: a parent looks up a file, mints a READ-only cap, spawns a viewer holding it, and revokes it. how does it time the revoke? it `yield_now()`s **once**, then revokes — betting that one scheduling turn is enough for the viewer to complete a full synchronous read-IPC round trip. QEMU wins the bet. debug loses it: the revoke lands before the viewer's first read even leaves, the read is `Denied`, `bytes_read=0`.

- here's the part that stings. the *test asserted the race*. both scenarios waited for `Revoked` **before** `bytes_read` — "revoke fires while the read is in-flight." that was post 48's whole narrative: *revocation closes the future, not the past*, a clever demonstration that an in-flight operation completes even as its authority is pulled. it was clever. it was also a race, and I had enshrined its wire order as the expected one. the test wasn't guarding against the bug; it was asserting it.

## the experiment that made it worse

- the obvious fix is to widen the window: one yield to sixteen. I ran it. it didn't go green — it **hung**. `bytes_read` never emitted at all, budget exhausted. so the yield count was unsound in *both* directions: too few and the revoke wins, too many and something wedges. for an afternoon I believed there was a lost-wakeup bug hiding underneath, and that any fix built on `WaitNotify` might just reproduce the hang.

## the fix: an event, not a guess

- replace the bet with a handshake. two notifications, `done` and `proceed`. the viewer reads, emits `bytes_read`, `Signal`s `done`, then `WaitNotify`s `proceed`. the parent `WaitNotify`s `done` — so it revokes *only after* the read completed — revokes, `Signal`s `proceed` to release the viewer (which was blocked, alive, so the `CapEvent::Revoked` fires against a live holder), and reaps. the wire order is now a deterministic **grant → use → reclaim**: `bytes_read` before `Revoked`, every time, on every scheduler.

- `--opt=low` went from **108/110 to 110/110**, 100% fidelity. QEMU: ten of ten on repeat, no flakes. and the feared residual hang **never came back** — it was purely the yield-count fragility. there was no deeper bug to isolate; the event handshake didn't paper over anything, because there was nothing under it.

## what I learned

- **a green test can be asserting a race.** these passed for milestones because my default build's timing happened to win a race the code had no right to be in — and the test had written the race's ordering down as correct. the determinism was never there; the luck was, and luck reads exactly like correctness until the day it doesn't.

- **the emulator wasn't wrong; it was honest.** a second implementation with different timing is a fuzzer for your timing assumptions. "it fails under snemu" is not "snemu is broken" — it can be "your code is fragile and QEMU was kind." the differential oracle is how you tell those two apart, *provided* it looks past its own benign noise (the `mhartid`) to the signal.

- **the fix walks back a previous post.** post 48 sold revoke-during-in-flight as a feature. it wasn't — it was a race with good PR. the honest pattern is sequential and event-gated: grant, use, reclaim, in that order. less cute, and it survives a hostile scheduler, which is the only kind worth designing for.

- **this was the supervisor's readiness primitive, rehearsed on a real bug.** the reason to fix it now rather than later: a service supervisor's "readiness" — a service signals *ready* before its dependents start — is the same `Signal`/`WaitNotify` handshake, pointed the other direction. proving it on a shipped race de-risks the thing I'm about to build on top of it.

## what's next

- step 0 of the supervision plan is done, and the readiness pattern is proven. next is the boring, safe foundation: the pure policy in `kernel-core` — dependency-ordered startup, the restart decision with backoff and an intensity cap — all host-tested, no QEMU. the last stretch between here and a supervisor that watches a service crash and brings it back with its authority intact, and proves it did on the wire.
