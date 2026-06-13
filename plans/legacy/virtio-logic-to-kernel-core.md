# Plan: Extract virtio-console logic into kernel-core

**Branch**: main (project rule: all work on main; user commits)
**Status**: Active

## Goal

Move the pure parts of the virtio-console driver — data layout, feature/queue
decisions, virtqueue ring arithmetic, and the handshake state machine — into
host-testable `kernel-core`, leaving only volatile MMIO, `va_to_pa`, the
`fence(Release)`, the static queues, and the `send` mutex/TX_STAGING discipline
in `kernel/`.

## Why / what this does NOT buy us

Host tests will cover **logic**: the dead error paths (`NoVersion1`,
`FeaturesRejected`, `QueueTooSmall`) that QEMU never exercises, the ring index
wrapping, and the handshake ordering — driven against a `FakeVirtioDevice`.

A fake device tests *what we believe the device does*, NOT what QEMU/silicon
does. It **cannot** validate the two genuinely hard properties of this driver:
1. the `fence(Release)` between ring-fill and the notify write, and
2. the cross-hart `TX_STAGING` + `CONSOLE` mutex discipline in `send`
   (the bug fixed in plans/tx-staging-cross-hart-race.md).

Those stay in `kernel/` and remain covered ONLY by the QEMU integration tests.
This extraction is additive to those tests, never a replacement.

## Constraints (the kernel/host boundary)

Stays in `kernel/` (irreducibly hardware):
- `read_reg`/`write_reg`/`write_reg64` (volatile MMIO) — become the kernel's
  `impl MmioTransport`.
- `va_to_pa` translation of queue/buffer addresses (devices have no MMU).
- `core::sync::atomic::fence(Release)` before the notify.
- `static TX_QUEUE`/`RX_QUEUE` in `.bss` (need stable physical addresses).
- `find_console_base` (DTB walk + probe).
- `send`: the `CONSOLE` mutex held across stage+transmit, `TX_STAGING` copy.

Moves to `kernel-core::virtio`:
- `#[repr(C)]` structs + spec constants (pure layout/data).
- `negotiate_features`, queue-size check.
- ring submission/completion arithmetic.
- `handshake` + `setup_queue` logic, driven over an `MmioTransport` trait.

## Acceptance Criteria

- [ ] `cargo test -p kernel-core` covers feature negotiation (accept + the
      `NoVersion1` reject path).
- [ ] Host tests cover `QueueTooSmall` (max < QSIZE).
- [ ] Host tests cover ring enqueue wrapping at the QSIZE boundary and u16 idx wrap.
- [ ] Host tests drive the full handshake against a `FakeVirtioDevice`, including
      the `FEATURES_OK`-cleared rejection path and write ordering.
- [ ] `kernel/src/virtio_console.rs` delegates decisions/arithmetic/handshake to
      kernel-core; volatile MMIO, fence, statics, and `send` discipline unchanged.
- [ ] `cargo xtask itest --repeat 10` stays green (cross-hart `send` path intact).
- [ ] `CONSOLE` guards the staging buffer itself (`Mutex<TxStaging>`), so the
      `let x = *lock();` early-release footgun no longer compiles.
- [ ] A `#[cfg(loom)]` harness in kernel-core deterministically: (i) passes the
      correct stage-and-emit primitive across all interleavings, and (ii) FAILS a
      buggy twin that mirrors the original `Mutex<usize>` + outside-the-lock
      buffer shape — with a meta-assertion pinning both outcomes.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. One step at a time;
present acceptance criteria and wait for approval before writing any code.

### Tier 1 — structs + decisions

#### Step 1: Move `#[repr(C)]` virtqueue structs + spec constants to `kernel-core::virtio`

**Acceptance criteria**: `VirtqDesc`/`VirtqAvail`/`VirtqUsedElem`/`VirtqUsed`/`Virtqueue`
and the spec constants live in `kernel_core::virtio`; `kernel/` re-imports them; a
host test pins their `size_of`/`align_of` (DMA layout is a wire contract).
**RED**: Test asserting `size_of`/`align_of`/field offsets of the repr(C) structs
match the virtio-mmio layout the device expects.
**GREEN**: Move structs + constants; re-export from kernel; fix imports.
**MUTATE / KILL MUTANTS**: Layout test only; assess survivors.
**REFACTOR**: None expected.
**Done when**: kernel builds (riscv), kernel-core tests pass, layout pinned.

#### Step 2: `negotiate_features(device_features: u64) -> Result<u64, FeatureError>`

**Acceptance criteria**: returns `F_VERSION_1` when the device advertises it;
returns the `NoVersion1` error otherwise.
**RED**: Two tests — accept (bit 32 set) and reject (bit 32 clear).
**GREEN**: Pure function in kernel-core; `init_handshake` calls it.
**MUTATE / KILL MUTANTS**: Verify the mask/compare mutants die.
**REFACTOR**: Fold `InitError` mapping at the kernel boundary if cleaner.
**Done when**: both paths host-tested; kernel delegates.

#### Step 3: queue-size check `check_queue_size(max: u32, qsize: usize) -> Result<(), QueueError>`

**Acceptance criteria**: `Err(QueueTooSmall)` when `max < qsize`, `Ok` otherwise
(boundary `max == qsize`).
**RED**: Tests for too-small, exact-fit, and larger.
**GREEN**: Pure function; `setup_queue` calls it.
**MUTATE / KILL MUTANTS**: Kill the `<` vs `<=` boundary mutant.
**REFACTOR**: None expected.
**Done when**: boundary host-tested; kernel delegates.

### Tier 2 — ring arithmetic

#### Step 4: avail-ring enqueue index math

**Acceptance criteria**: a pure function computes the ring slot
(`idx % qsize`) and the next `avail.idx` (`wrapping_add(1)`), correct across the
QSIZE wrap and the u16 idx wrap.
**RED**: Tests at idx 0, idx = QSIZE-1 (wraps ring), idx = u16::MAX (wraps idx).
**GREEN**: Pure function over the in-memory ring representation.
**MUTATE / KILL MUTANTS**: Kill `%`-removal and `wrapping_add` mutants.
**REFACTOR**: Consider enabling >1 descriptor in flight later (out of scope now).
**Done when**: wrap cases host-tested; `transmit` uses it for slot/idx math.

#### Step 5: completion detection

**Acceptance criteria**: a pure predicate decides "device drained our buffer"
from `used.idx` before/after (handles u16 wrap).
**RED**: advanced, not-advanced, and wrap-boundary tests.
**GREEN**: Pure predicate; `transmit`'s spin loop uses it.
**MUTATE / KILL MUTANTS**: Kill the equality/inequality mutants.
**REFACTOR**: None expected.
**Done when**: predicate host-tested; spin loop delegates the decision.

### Tier 3 — transport trait + handshake state machine + fake device

#### Step 6: `MmioTransport` trait + `FakeVirtioDevice` test double

**Acceptance criteria**: a `trait MmioTransport { fn read_reg(off)->u32;
fn write_reg(off,u32); }` in kernel-core; a `FakeVirtioDevice` in kernel-core
tests models QEMU status semantics (records writes, clears `FEATURES_OK` on a
feature set it rejects, advances `used.idx` on notify).
**RED**: Test that the fake clears `FEATURES_OK` when driver writes an
unsupported feature set, and reflects a supported one.
**GREEN**: Trait + fake.
**MUTATE / KILL MUTANTS**: On the fake's own logic (it's test infra) — light.
**REFACTOR**: None.
**Done when**: fake's contract host-tested; nothing in kernel yet.

#### Step 7: `handshake(transport) -> Result<(), HandshakeError>` over the trait

**Acceptance criteria**: drives RESET → ACKNOWLEDGE → DRIVER → negotiate →
FEATURES_OK (verify stuck) → DRIVER_OK in order; returns the rejection error and
sets FAILED when the fake clears FEATURES_OK; the success path leaves
DRIVER_OK set.
**RED**: success-ordering test + FEATURES_OK-cleared rejection test (assert the
recorded write sequence).
**GREEN**: Port the status sequence from `init_handshake` into kernel-core over
`MmioTransport`; queue setup still done by the kernel for now.
**MUTATE / KILL MUTANTS**: Kill mutants that drop/reorder a status write.
**REFACTOR**: Collapse duplicated FAILED-on-error handling.
**Done when**: ordering + reject host-tested against the fake.

#### Step 8: `setup_queue` logic over the trait

**Acceptance criteria**: given queue PAs, writes SEL/NUM/desc/avail/used/READY in
the right registers; surfaces `QueueTooSmall` from the device's NUM_MAX.
**RED**: test asserting the recorded register writes (addresses + READY) and the
too-small path against the fake.
**GREEN**: Port `setup_queue` register logic into kernel-core; kernel passes the
`va_to_pa`-translated PAs in.
**MUTATE / KILL MUTANTS**: Kill wrong-register / missing-READY mutants.
**REFACTOR**: None expected.
**Done when**: register sequence + too-small host-tested.

#### Step 9: Wire kernel to the kernel-core driver

**Acceptance criteria**: `kernel/src/virtio_console.rs` implements
`MmioTransport` over volatile MMIO and calls the kernel-core `handshake` /
`setup_queue`; `init`/`transmit` keep `va_to_pa`, the `fence(Release)`, the
statics, and `send` keeps the `CONSOLE` mutex + `TX_STAGING` copy exactly as today.
**RED**: no new host test (integration territory) — guarded by QEMU.
**GREEN**: Replace the in-kernel handshake/setup bodies with kernel-core calls.
**MUTATE / KILL MUTANTS**: n/a (covered by kernel-core + itest).
**REFACTOR**: Remove now-dead in-kernel logic.
**Done when**: `cargo xtask itest --repeat 10` green; `xtask clippy` clean.

### Tier 4 — structural lock fix + loom regression for the TX_STAGING bug

Captures the cross-hart bug from plans/tx-staging-cross-hart-race.md (a dropped
`MutexGuard` in `send`: `let base = *handle.lock();` released the lock at the `;`,
leaving two harts racing the shared `TX_STAGING` buffer + virtqueue ring). It was
only ever caught by flaky integration tests (~2% repro). Root cause: `CONSOLE` is
`Mutex<usize>` — it guards a `Copy` base address while the actually-shared mutable
state (`TX_STAGING`, the ring) lives in separate statics *outside* the lock.

Two moves: (a) make the bug structurally unrepresentable by guarding the staging
state itself; (b) capture the bug class as a deterministic host test via `loom`.

Scope honesty: `loom` validates *lock discipline over shared memory* only. It does
NOT cover the `fence(Release)`-vs-device ordering or the MMIO side — those remain
QEMU/hardware territory. This adds a deterministic guard for the lock-lifetime
class; it does not shrink the integration suite's responsibility.

#### Step 10: Restructure `CONSOLE` to guard the staging buffer (`Mutex<TxStaging>`)

**Acceptance criteria**: `CONSOLE` becomes `Once<Mutex<TxStaging>>` where `TxStaging`
owns the staging byte buffer + the device base; `TxStaging` is NOT `Copy`, so
`let x = *handle.lock();` fails to compile. `send` holds the guard across the
whole stage+transmit and delegates the critical section to a kernel-core
stage-and-emit primitive (the kernel supplies the volatile `transmit` as the emit
closure; `va_to_pa` + `fence(Release)` stay in the kernel emit path, unchanged).
**RED**: no new host test here (the compile-time impossibility + behaviour is
covered by Step 11's loom harness and the existing itest). State the
acceptance criteria and get confirmation before editing.
**GREEN**: define `TxStaging`; move the buffer inside the mutex; rewire `send`.
**MUTATE / KILL MUTANTS**: n/a in kernel (covered by Step 11 + itest).
**REFACTOR**: delete the now-impossible-to-misuse comment scaffolding around the
old `let guard = ...; drop(guard)` dance if the structure makes it redundant.
**Done when**: kernel builds (riscv), `cargo xtask itest --repeat 10` green, and
the early-release idiom provably won't compile (note it in the PR description).

#### Step 11: loom harness in kernel-core — correct passes, buggy twin fails

**Acceptance criteria**: a `#[cfg(loom)]` test module exercises the kernel-core
stage-and-emit critical-section primitive from two threads, each staging a
distinct payload, with a fake `emit` that reads the staging buffer back and
records what it saw. Asserts: (i) the **correct** primitive (buffer inside the
lock) — every emit reads back exactly the staging thread's payload across all
interleavings loom explores; (ii) a **buggy twin** (`#[cfg(loom)]`-only; mirrors
the original `Mutex<Base>` + buffer in an outside-the-lock `loom::cell::UnsafeCell`,
guard dropped before staging) — loom finds an interleaving that cross-contaminates,
i.e. the assertion fails / loom reports the violation. A meta-assertion pins BOTH
outcomes so the harness can't silently rot into a no-op (detector liveness). The
twin differs from the correct primitive in exactly one dimension (lock scope /
buffer placement).
**RED**: write the harness against the buggy twin first; confirm loom reports the
violation (the bug reproduced deterministically).
**GREEN**: point the harness at the correct primitive; confirm loom passes.
**MUTATE / KILL MUTANTS**: n/a (loom IS the model checker; the buggy-twin
meta-assertion is the equivalent liveness check).
**REFACTOR**: factor shared harness scaffolding between the two variants so the
only difference is lock scope.
**Done when**: `RUSTFLAGS="--cfg loom" cargo test -p kernel-core <loom test>`
shows correct-passes + buggy-twin-fails deterministically; the meta-assertion
guards both. (`loom` added as a `[target.'cfg(loom)'.dev-dependencies]` /
`cfg(loom)`-gated dev-dependency so normal `cargo test` is unaffected.)

## Pre-PR Quality Gate

Before each commit:
1. Mutation testing — `mutation-testing` skill on the changed kernel-core code.
2. Refactoring assessment.
3. `cargo xtask clippy` clean; `cargo test -p kernel-core` green.
4. For Steps 9 and 10 specifically: `cargo xtask itest --repeat 10` (cross-hart gate).
5. For Step 11: run the loom harness (`--cfg loom`) and confirm correct-passes +
   buggy-twin-fails before relying on it as a regression guard.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
