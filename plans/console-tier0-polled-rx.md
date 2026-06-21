# Console input — Tier 0 scaffold: polled UART RX

**Status:** **Plan only (2026-06-21).** No code yet. The cheap, no-interrupt
console-input on-ramp so the shell can be driven by hand before the Tier-1
userspace virtio driver exists. See `plans/spawn-shell-and-console.md` §E.

**Goal:** a byte typed in the terminal flows host → UART → kernel → userspace, and
a tiny demo program echoes it back. Zero new interrupt infrastructure (no PLIC).

## Verified current state

- `kernel/src/device/uart.rs`: **TX-only.** `putchar` spins on `LSR` bit 5 (THRE),
  writes `THR` (`base+0`). `LSR` at `base+5`. No init/FIFO config (relies on
  OpenSBI). **RX = read `RBR` (`base+0`, read side) when `LSR` bit 0 (Data Ready) is set.**
- `handle_timer` (`kernel/src/trap/mod.rs`): fires every `TIMER_INTERVAL_TICKS`,
  runs on **every hart**, takes **no locks** (deferred-emission discipline). The
  natural place to poll RX. _(Confirm the interval value to do the FIFO-overflow
  math — it's sub-second, driving both preemption and the heartbeat.)_
- Syscalls max out at `CopyToCaller = 14`. **`ConsoleRead = 15`** (append-only;
  `Spawn` then becomes 16).
- ⚠️ **The kernel currently does not build** (the in-progress `UserLayout`
  refactor). Steps 2–6 below can't be _tested_ until that lands — but **Step 1 is
  host-testable in `kernel-core` and is unblocked right now.**

## Design

The wrinkle (§E13): with no interrupt, who drains the UART and when? Answer: a
**periodic poll in `handle_timer`** drains the hardware RX into a **kernel byte
ring**; `ConsoleRead` drains the ring into the caller. This decouples "bytes
arrive" from "userspace reads."

```
type a key → UART RX FIFO (≤16B hw) → [handle_timer drains, hart 0] → kernel RX ring
                                                                         │
                                  ConsoleRead syscall ── copy_to_user ──┘ → userspace
```

- **Producer:** `handle_timer`, gated on `current_hartid() == 0` (keep it
  single-producer). `while LSR & DR { push(RBR) }`. Lock-free (atomic head/tail),
  drop-on-full (bounded; overflow-drop is fine for a scaffold). Tiny — fits the
  "no locks in the timer handler" rule.
- **Consumer:** `ConsoleRead(a0=ptr, a1=len) → a0=count`. Drains
  `min(len, available)` bytes from the ring, `copy_to_user` into the caller's
  buffer (the _current_ process — a plain copy-to-user, not the cross-AS
  `CopyToCaller`). Returns `0` if empty.
- **Cadence/overflow:** the hw FIFO (≤16B) + the timer-poll period bound tolerable
  typing speed. At a sub-second poll, human typing (~5–10 cps) is comfortably
  drained. Optionally enable the 16-byte RX FIFO via `FCR` for headroom (today the
  driver relies on OpenSBI's config).

### Blocking vs non-blocking (decision)

- **v1 (non-blocking):** `ConsoleRead` returns immediately (`count` or `0`). The
  demo program loops with a `yield` between empty reads. Simplest; good enough to
  prove the path. ✅ start here.
- **v1.1 (blocking, fast-follow):** on empty, `block_current()`; the timer drain
  `wake`s a parked reader when it pushes the first byte. **Reuses the existing
  v0.9 `sched::block_current`/`wake`** — no new notification primitive needed for
  this narrow case. Avoids the shell busy-spinning. Add once the loop works.

## TDD steps (RED first; each leaves a working state)

1. **RX ring — `kernel-core` ✅ DONE (2026-06-21).** `ConsoleRing<const N>` in
   `kernel-core/src/console.rs`: push/pop/len/is_empty/is_full, `% N` wraparound,
   drop-on-full, explicit `len` (no full-vs-empty ambiguity). 6 host tests green;
   `cargo xtask mutants --file kernel-core/src/console.rs` = 25/25 caught.
   `push`/`pop` implemented by Chloe.
2. **UART RX in `device/uart.rs` ✅ DONE (2026-06-21).** `Uart16550::read_byte()
   -> Option<u8>` — reads `RBR` iff `LSR & DR` (bit 0). Written by Chloe;
   clippy-clean. (FIFO `FCR` init not needed — relying on OpenSBI's config.)
3. **Timer-drain producer ✅ DONE (2026-06-21).** `CONSOLE_RX: Mutex<ConsoleRing<256>>`
   + `drain_rx()` + `read_into()` (consumer half, for Step 4) in
   `kernel/src/device/console.rs`; `handle_timer` calls `drain_rx()` gated on
   `current_hartid() == 0`, before `maybe_preempt`. **Deadlock avoided:** the
   drain does NOT lock the println `UART` mutex (would deadlock vs a task holding
   it mid-`print!` with `SIE==1`); it uses a separate RX-only `Uart16550` handle.
   The `CONSOLE_RX` lock IS safe in the timer handler (leaf lock, drain+ConsoleRead
   only, both `SIE==0`, no alloc/emit). Verified: `boot-reaches-heartbeat` green
   with the drain in the hot path.
4. **`ConsoleRead` syscall.** `abi::Syscall::ConsoleRead = 15` + a trap handler
   that drains ring→`copy_to_user`→returns count.
5. **Userspace binding.** `console_read(&mut [u8]) -> usize` in `user/runtime`
   (mirror `debug_write`).
6. **Demo + itest.** `workload=console-echo`: a program looping
   `n = console_read(buf); if n>0 { debug_write(&buf[..n]) }`. Scenario types bytes
   and asserts they come back as a `Log` frame.

## The test-infra wrinkle (§E flagged — real work)

The itest harness today **reads** the telemetry virtio-console socket and dumps
UART to a `.log`; it has **no way to _write_ to the guest UART**. Testing RX needs
the harness to **inject bytes into the guest's UART chardev** — i.e. wire QEMU
`-serial` to a socket/pty the harness can write, then a `Harness` method to send
input. This is a prerequisite for the Step-6 scenario (and reusable for any future
interactive test). Scope it as its own sub-task before Step 6.

## Out of scope (Tier 0)

- Line discipline (echo, backspace, line assembly) — **userspace**, lives in the
  shell / a console lib, not this scaffold. The scaffold delivers raw bytes.
- Interrupt-driven RX, virtio console, cap-mediation — that's Tier 1 (its own
  milestone): PLIC + notification + MMIO caps + DMA.

## Dependency note

Independent of Spawn/shell — a self-contained scaffold demoable on its own
(`console-echo`). But its **itest** (Steps 2–6) needs the kernel to build, so the
`UserLayout` refactor must land first. **Step 1 (the ring) needs nothing** and can
start immediately.
