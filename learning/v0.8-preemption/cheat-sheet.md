# v0.8 Preemptive + Priority Scheduler — Cheat Sheet

## The one-bit gate
- **`sstatus.SPP`** = Supervisor Previous Privilege. Hardware stamps it on every trap.
  - `SPP == 0` → trap came from **User** mode → safe to preempt (userspace holds no kernel lock).
  - `SPP == 1` → trap came from **Supervisor** (kernel) → **never** preempt (might be mid-critical-section).
- Read from `frame.sstatus` (snapshot), NOT the live CSR — by the time Rust runs you're already in S-mode.
- It is a **privilege-mode** bit, not an address/pointer check.

## Two save layers (the register paradox)
| Layer | Who | Saves | Where | When |
|---|---|---|---|---|
| 1 | `trap_entry` (trap.S, asm) | **all 32 GPRs + sepc + sstatus** (full `TrapFrame`) | task's **kernel stack** | every trap |
| 2 | `switch` (asm) | **14 callee-saved + sp** (`TaskContext`) | per-task `TaskContext` | every reschedule |
- `switch` only needs 14 because it's called like a normal Rust fn — caller-saved already spilled by the compiler; full user state already parked by Layer 1.
- Resume path: `switch` `ret`s back *inside the trap handler*, unwinds to `trap_entry`, which restores the full `TrapFrame` and `sret`s to the exact user PC.

## Preemptive vs cooperative
- Cooperative `yield_now` → `reschedule(Yield)` from a known-safe point.
- Preemptive → timer IRQ at arbitrary PC; full state saved by Layer 1; `reschedule(Preempt)` reuses the *same* `switch`.
- Single call site: `handle_timer` → `maybe_preempt(SPP==0)`. Gate: from_user AND quantum expired AND a ready task is effective-priority ≥ current.

## Priority + aging (anti-starvation)
- `Priority { Low=0, Normal=1, High=2 }`, set at spawn, immutable.
- `aged_priority(base, waited, step) = min(base + waited/step, High)`.
- `pick_next` = max by `(aged_priority, Reverse(enqueued_tick))` → highest effective prio, ties to **longest wait**.
- The `Reverse(enqueued_tick)` tie-break is what converts "Low caught up to High in priority" into "Low actually gets the CPU."
- `AGING_STEP_TICKS = 10_000_000` (1s): a Low task ties High after ~2s and wins on tie-break. Without aging → **starvation**.
- `QUANTUM_TICKS = 2_000_000` (0.2s).

## Why no `preempt_count` yet
- The `SPP == User` gate IS the lock-safety guarantee: userspace never holds a kernel lock, so preempting it can never freeze a lock holder.
- Preemption = an involuntary `yield_now` injected at a random instruction; the gate ensures it's only injected where the existing rule ("never hold a `kernel::sync::Mutex` across a switch") is already satisfied.

## NON-gap: "can a task dodge preemption by spamming syscalls?" → NO
- Hardware clears `sstatus.SIE` on every trap; **SnitchOS never re-enables it during trap handling**.
  So a whole syscall runs with interrupts masked.
- A timer coming due mid-syscall doesn't fire — it sets pending `sip.STIP` and waits. At `sret`,
  `SIE` restores from `SPIE`, we're in U-mode, and the still-asserted timer fires *immediately*
  → `SPP==0` → normal preemption. No escape.
- So `handle_timer` sees `SPP==1` **only** for kernel threads (`task_a`, `idle`, `heartbeat`) —
  which are cooperative-by-design and intentionally not preempted.
- `need_resched` would be dead code **today**. It re-opens ONLY if a future version re-enables
  interrupts inside long syscalls. See `plans/v0.8c-need-resched-on-syscall-return.md`.
- Lesson: preemption safety is governed by **two** bits, not one — `SPP` (where we came from)
  AND `SIE`/`SPIE` (whether an interrupt could even be delivered there).

## Telemetry added
- `SwitchReason::Preempt` (appended to enum); `ThreadRegister { priority }`; `snitchos.sched.preemptions_total`; collector tags spans with `thread.priority`.
