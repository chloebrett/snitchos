# TX_STAGING cross-hart race — found and fixed

The residual cross-hart wedge that `plans/residual-race-investigation.md`
chased for several sessions (and concluded was "uncorrelated jitter,
Bug B is gone") is **real, specific, and now fixed**. It is a dropped
lock guard in `virtio_console::send`.

## Symptom

`sched-span-survives-yield` (and, at lower rates, other fully-loaded
scenarios) wedge at ~2% per run: the kernel stops, QEMU's virtio-console
client disconnects, the harness sees a fast-exit. No panic, no UART
diagnostic — the classic silent cross-hart wedge.

## How it was found — the classifier earned its keep

The old tooling could only say "5% flaky, QEMU disconnected." The
failure-signature classifier + per-failure `FailureCapture` sidecars
(Workstream A, `plans/itest-flake-reduction.md`) turned that into a
diagnosis:

1. **Bucketing.** A 500-iteration run of `sched-span-survives-yield`
   produced `12/500` failures: **10 `wedge`, 2 `budget_exhausted`**. The
   classifier separated 10 genuine kernel wedges (socket disconnects,
   which host slowness cannot cause) from 2 timing timeouts. The stress
   run was fully parallel, so the timing-bucket-inflation caveat applied
   — and the data showed it *didn't* dominate: 83% were real wedges.

2. **Clustering the wedge captures.** The 10 `.capture.json` sidecars
   were not scattered. 8 of 10 disconnected at **frame ~142–145, during
   hart-1 task registration** (`hart_1_main` ThreadRegister + the
   `snitchos.task.hart_1_*` metric registrations). The frame payloads
   were visibly corrupted on the wire:

   ```
   ThreadRegister { id=8 name="hart_\u{8}\u{8}\u{b}har" }
   StringRegister { StringId(63) = "snitchos.tas\u{6}?$snitchos.task.hart_1_" }
   MetricRegister { "?" kind=Counter }
   ```

   Two frames' bytes smashed into one buffer — not a decode bug, but
   garbage the kernel *emitted*. That fingerprint (interleaved string
   bytes, concentrated at the one window where both harts emit
   concurrently) pointed straight at a shared emission buffer.

The 2 `budget_exhausted` captures were a different shape entirely
(`frames_seen` ~2500, both harts long-active, died mid-`ContextSwitch`
at large `t`) — confirming the bucket split was meaningful.

## Root cause

`virtio_console::send` staged frame bytes through a single static
`TX_STAGING: [u8; 256]`, meant to be serialised by the `CONSOLE` mutex.
The lock was acquired like this:

```rust
let base = *handle.lock();   // BUG
```

`handle.lock()` returns a `MutexGuard<usize>` **temporary**. `*guard`
copies the `usize` base out (it's `Copy`); `base` does not borrow the
guard, so no temporary-lifetime-extension applies, and the guard is
**dropped at the `;`** — releasing the lock immediately. The subsequent
`copy_nonoverlapping` into `TX_STAGING` and the `transmit` (which drives
the shared virtqueue descriptor ring) both ran **unlocked**. The SAFETY
comment claimed the lock was held "for the duration of the lock +
transmit" — the exact invariant the code failed to keep.

Two harts in `send()` simultaneously therefore:
- both `copy_nonoverlapping` into the same `TX_STAGING` → interleaved
  bytes → corrupted frame text (the wire fingerprint above), and
- both mutate the virtqueue avail/descriptor ring → a clobbered
  descriptor → QEMU reads a malformed buffer → device wedge / disconnect.

One cause, both symptoms (garbled text *and* the wedge).

### Why it clustered at hart-1 registration

Outside bring-up, hart 1 sits in `wfi` and does not emit. The only
sustained window of *concurrent cross-hart emission* is hart 1's startup
burst of `ThreadRegister` + `StringRegister` + `MetricRegister` while
hart 0 is mid-heartbeat. That is exactly where the captures cluster, and
exactly why the suite is ~98% fine.

### Why every storm scenario missed it

`plans/residual-race-investigation.md`'s five storms each isolated a
single subsystem in *steady state* with a single emitter. `deflake-
virtio-storm` had hart 0 emit while hart 1 did pure atomics — never two
concurrent emitters. None reproduced "both harts in `send()` at once
during bring-up." This is the boot-time race (H6) that doc listed but
could not pin.

## The Rust mechanism (why the guard dropped)

A temporary lives until the end of its enclosing statement (the `;`) and
is then dropped. `MutexGuard::drop` releases the lock.

```rust
let base = *handle.lock();   // guard is a temporary, dropped at `;` → UNLOCKED after this line
let base = &*handle.lock();  // base borrows the guard → lifetime-extended → locked for base's scope
let guard = handle.lock();   // guard is named → lives to end of block → locked across stage+transmit
let base = *guard;           // copy the usize out; guard still owns the lock
```

Temporary lifetime extension only applies when the `let` binds a
*borrow* of the temporary. `*handle.lock()` is a deref-and-copy, not a
borrow, so extension does not apply.

## The fix

Bind the guard to a named local so it lives across the whole
stage+transmit (with an explicit `drop(guard)` after `transmit` to
document the release point):

```rust
let guard = handle.lock();
let base = *guard;
// ... stage into TX_STAGING, transmit ...
drop(guard);
```

One line of substance. `kernel/src/virtio_console.rs::send`.

## Verification

Kernel races aren't host-unit-testable; the integration suite is the
test. The classifier makes the before/after measurable instead of
eyeballed:

- **Before** (commit prior to fix): `sched-span-survives-yield
  --repeat 500` → 10 `wedge` / 500.
- **After** (prediction): `wedge` → ~0; any residual is the
  `budget_exhausted` timing tail (≈2/500), which is Workstream B/C
  territory, not this bug.

Run `cargo xtask itest sched-span-survives-yield --repeat 500
--update-baseline` and read the `Failure signatures` breakdown. A clean
wedge count confirms the fix.

## Audit: other dropped-guard sites

Swept every `.lock()` call site in `kernel/`, `kernel-core/`, and the
host crates. The footgun (`let x = *MUTEX.lock();` — extract a value,
then touch *other* shared state after the temporary guard drops) appears
**nowhere else**. The other shapes are all safe:

- **Bound guards** (`let g = X.lock();`, `let mut sched = SCHEDULER.lock();`)
  — held to end of scope.
- **Statement-scoped** (`HEAP.inner.lock().allocate_first_fit(..)`,
  `INTERN_TABLE.lock().register_or_lookup(..)`, `alloc.lock().alloc()`,
  `SCHEDULER.lock().tasks.push(..)`) — the guard temporary is held for
  the whole statement, the critical-section work happens inside it, and
  only an owned result escapes. Safe because nothing *else* shared is
  touched after the statement. This is the exact distinction `send()`
  violated: it extracted `base`, then wrote `TX_STAGING` and drove the
  virtqueue *after* the statement.
- **Borrow form** (`write!(&mut *uart.lock(), …)` in `console.rs`) — the
  `&mut *guard` borrow keeps the temporary alive for the macro call.
- **Host (`collector`)** — `let state = state.lock().unwrap();`, bound.

One footgun, one fix.
