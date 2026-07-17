# The kernel snitches its own death — a panic-safe telemetry frame

**Status: increments 1–6 SHIPPED + verified. 1–5: gate 10/10; oracle conditions
on the panic frame. Increment 6 (dynamic message) SHIPPED — the frame now carries
`"kernel panic: <PanicInfo>"` (reason + location), formatted no-alloc via
`panic_log::MsgWriter` (char-boundary truncation, 11/11 mutants caught); the itest
now asserts the real reason reaches the wire, gate 10/10 @ ~500 ms.**
Motivated by the snemu differential-oracle work
([notes/snemu-guard-page-fail-is-timing-not-mmu.md](../../notes/snemu-guard-page-fail-is-timing-not-mmu.md)):
a kernel panic is currently invisible on the structured telemetry channel — it
goes out the emergency UART only. For an OS whose first-class concern is
observability, the single most important event (the kernel dying) is the one thing
that never emits a `Frame`. This plan fixes that, best-effort and panic-safe.

## Why (three payoffs)

1. **Observability-first, honestly.** The collector, Tempo, Grafana, and the
   integration harness all watch the virtio-console frame stream. A panic should
   land there — a red span / a `kernel.panics_total` tick — not just scroll past on
   a UART nobody's tailing.
2. **Robust differential oracle.** `snemu-diff`'s benign-name filter currently
   forgives `kernel.heartbeat`-only-in-snemu *unconditionally* (documented
   limitation), because there's no crash frame to prove snemu actually *reached*
   the crash vs. failed-to-halt and over-heartbeated. A panic frame lets the filter
   condition the benign pass on "snemu's stream contains the panic" — closing the
   one real follow-up from the oracle work.
3. **Structural panic testing.** The itest harness (and the future snemu-backed
   suite) can then assert a workload panicked by the *frame*, not by absence of
   progress — which today is indistinguishable from a hang.

## The hard constraint: a panic can fire from anywhere

The current handler (`kernel/src/panic.rs`) bypasses the console/UART mutexes and
writes to a *fresh* emergency UART on purpose: a panic may fire from inside the
allocator, inside the virtio TX lock, or inside the intern table. So the telemetry
emit **must not**:

- **allocate** — no `format!`, no heap (would re-enter a panicking allocator);
- **intern** — no `StringId` registration (takes the intern lock, may allocate);
- **block** — no plain `lock()` on the virtio queue (the panicking code may already
  hold it → deadlock).

The existing `PANICKING` recursion guard already covers "the emit itself panics."

## Why it's cheaper than it looks

- **No wire-format change.** Reuse `Frame::Log { msg, task_id, t, hart_id }`. `Log`
  inlines its message as a `&str` — it does **not** use the intern table, so no
  `StringId`, no registration. A panic frame is just a `Log`. `OwnedFrame` already
  handles it; nothing in `protocol`/`stream` changes.
- **Everything the frame needs is panic-safe to read:** `timestamp()` is a `rdtime`
  CSR read; `current_hartid()` reads `tp`; `current_task_id()` is an atomic load;
  `postcard::to_slice` encodes into a caller-provided buffer (no alloc);
  `mmu::va_to_pa` is a pure page-table read on a kernel static. None allocate or
  block.
- **Best-effort by construction:** `try_lock` the virtio console; if it's held (the
  panicking code was mid-send, on this or the other hart), **skip** — the UART
  already has the human-readable version. A dying kernel emitting *sometimes* is
  strictly better than *never*, and never-deadlock is the invariant.

## v1 scope

Emit a **fixed** `"kernel panic"` marker Log (no dynamic message). Safe, simple,
and enough for the oracle + itest to key on. A bounded stack-formatted message
carrying the panic location/reason (via a `core::fmt::Write` into a `[u8; N]`, no
heap — same trick the handler already uses for the UART) is increment 6, kept
separate so the risky path lands minimal first.

## TDD discipline (per project rules)

Each increment is **RED first, in its own edit** (failing test), then minimum
GREEN, then MUTATE (`cargo xtask mutants` on the touched core) and kill survivors,
then assess refactor. Host-testable logic lands in `kernel-core` / `protocol` and
runs under `cargo test`; kernel-side `try_lock`/MMIO/panic wiring is covered by the
QEMU itest. Do not batch test + impl into one edit.

---

## Increment 1 — `kernel::sync::Mutex::try_lock` (kernel-side seam)

The wrapper in `kernel/src/smp/sync.rs` currently exposes `lock()`. Add
`try_lock() -> Option<MutexGuard<'_, T>>` forwarding to `spin::Mutex::try_lock`,
with the same no-op preempt/IRQ hooks as `lock()`. This is the non-blocking seam
the panic path needs; keeping it in `kernel::sync` respects the `disallowed_types`
lint that blocks raw `spin::Mutex` outside that file.

- No host test (thin forwarding over `spin`); covered transitively by increment 4.
- Verify: `cargo xtask clippy` clean (the wrapper is the sanctioned type).

## Increment 2 — panic-safe encode into a static buffer (host-tested)

The one genuinely host-testable piece: build the panic `Log` frame and encode it
into a fixed buffer, no alloc.

- New `tracing::encode_panic_log(buf: &mut [u8], msg: &str, task_id, hart_id, t)
  -> Option<usize>` (or a `kernel-core`/`protocol` helper if it can be pure) that
  calls `postcard::to_slice(&Frame::Log { msg, task_id, t, hart_id }, buf)` and
  returns the encoded length, `None` on overflow.
- RED: a `protocol` (or `kernel-core`) test that encodes a `"kernel panic"` Log
  into a `[0u8; 256]` and decodes it back via the stream decoder to an
  `OwnedFrame::Log` with the same fields. Proves no-alloc encode + roundtrip.
- MUTATE the helper.

## Increment 3 — `virtio_console::try_send_panic(bytes) -> bool`

A non-blocking, static-buffer send that never touches the heap or the intern
table.

- `try_lock` the console mutex; return `false` if held. **Dropped the plan's
  separate `PANIC_TX_STAGING`:** the staging buffer lives *inside* the mutex, so a
  successful `try_lock` proves it (and `TX_QUEUE`) are idle — reuse `staging.buf`.
- **Gotcha found at the gate (2/10 → 10/10):** a single `try_lock` flaked badly.
  A *peer* hart emitting telemetry holds the console lock for a full device
  round-trip (the `transmit` spin), which under the `--repeat` parallel load is
  most of the time — so one-shot `try_lock` lost the race ~80% and dropped the
  panic frame (UART confirmed the panic *did* fire; only the telemetry was lost).
  Fix: **bounded retry** of `try_lock` (`PANIC_SEND_TRY_LOCK_SPINS`), which catches
  the peer's release windows. Still bounded (no blocking `lock()`), so a
  self-panic-while-holding-the-lock gives up instead of self-deadlocking. In
  practice it exits almost immediately (gate ran in 0.6 s), only spinning under
  real contention.
- Kernel-side; covered by increment 4's itest.

## Increment 4 — wire into `panic.rs` + itest

- In `panic()`, after the emergency-UART `writeln!`, best-effort emit: encode the
  fixed `"kernel panic"` Log into a static buffer (increment 2) and
  `try_send_panic` it (increment 3). Guard it behind the existing `PANICKING`
  first-entry branch so a panic-during-panic never re-emits.
- RED: new itest scenario `kernel-panic-emits-frame` (reuse the existing
  `workload=panic-now`): assert a `Log` frame whose message contains
  `"kernel panic"` appears on the wire within a few seconds. Register in
  `xtask/src/itest.rs::SCENARIOS`.
- This is the end-to-end proof; run `cargo xtask itest kernel-panic-emits-frame`,
  then `--repeat 10` per the commit gate.

## Increment 5 — oracle robustness (the payoff for the follow-up)

With the crash now observable, tighten `snemu_diff`: forgive `kernel.heartbeat` in
only-snemu **iff** snemu's stream contains the panic `Log` (proving it reached the
crash, just later), instead of forgiving unconditionally. A `panic-now` /
stack-guard run still PASSes; a hypothetical snemu that *fails to halt* and
over-heartbeats *without* ever emitting the panic now correctly FAILs.

- RED: extend the `invented_names` / `faithful` tests — `kernel.heartbeat` alone is
  benign **only** when a panic `Log` is present; benign name **without** a panic
  frame is still an invention.
- Replaces the documented limitation in the oracle note.

## Increment 6 (follow-up) — a real message, still no heap — SHIPPED

Format the panic `info` (location + reason) into a bounded `[u8; N]` via a
`core::fmt::Write` cursor (the handler already streams `info` to the UART this way
— no allocation), truncating on overflow, and use that as the Log message instead
of the fixed marker.

- `kernel_core::panic_log::MsgWriter` — a `core::fmt::Write` over a caller buffer;
  overflow drops whole chars at a UTF-8 boundary (never splits a code point, never
  writes past the end), `as_str()` valid by construction, `write_str` never errors.
  Host-tested (fits / exact-fill boundary / multi-byte truncation); 11/11 mutants
  caught after the exact-fill test killed the `>`→`>=` survivor.
- `panic.rs::snitch_panic(info)` formats `"kernel panic: {info}"` into a second
  static `PANIC_MSG_BUF` (192 B, < the 256 B frame buf for postcard framing
  headroom), then encodes that `&str`. Keeping the `"kernel panic"` prefix means
  the collector/oracle keying is unchanged while the reason now rides along.
- itest `kernel-panic-emits-frame` strengthened: the `Log` must contain both
  `"kernel panic"` **and** the workload's `"deliberate immediate panic"` — proving
  dynamic content flows, not just the marker. Gate 10/10 @ ~500 ms.

## Deferred / out of scope

- **A dedicated `Frame::Panic` variant** (vs. reusing `Log`). Would let the
  collector treat panics first-class — a Tempo error span, a `snitchos.kernel.
  panics_total` counter. Cheap to append later (postcard is positional/append-only;
  update `OwnedFrame::from_borrowed`). Reusing `Log` for v1 keeps the wire frozen
  and the collector pattern-matches the message; revisit if first-class panic
  handling in Grafana earns its keep.
- **Multi-hart panic fan-out.** v0.1 contract is "any hart panics → whole system
  panics" (aspirational; the handler only idles the current hart). A panic frame
  per hart, or a "system halted" summary, waits on real fault isolation.

## The post angle

"the kernel learns to snitch its own death." The one event an observability-first
kernel couldn't observe was its own panic — and making it observable, *safely*,
from a context where you can't allocate, can't lock, and can't trust any shared
state, is the whole craft of a panic path in one feature.
