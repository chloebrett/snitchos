# Post 7 — Teaching the kernel what time it is

- v0.2 made the kernel observable. v0.3 makes it responsive: heartbeat is timer-driven now, not a busy-spin on the cycle counter. CPU sleeps in `wfi` until the timer IRQ wakes it. First real kernel primitive — trap entry/exit — that everything later (scheduler, syscalls, IPC, capability invocation) is going to live on.

## the mental-model unstick

- spent a while bouncing off RISC-V trap semantics before realising I'd been thinking of **MIPS** the whole time. very similar shape (registers, RISC, coprocessor-vs-CSR for system state) but different in the details that matter: no delay slots, three explicit privilege levels, named CSRs instead of "CP0 register 12."
- once the confusion was named, the rest unlocked.
- secondary unstick: **caller-saved vs callee-saved is about *function calls*, not about traps.** the trap handler isn't being "called" in any ABI sense — there's no `call` instruction, the CPU just hardware-jumps. so the handler saves *everything* (the trapped code couldn't save anything for itself), and the convention only re-applies once we're inside the handler making normal Rust function calls.

## what gets saved on trap

- hardware does almost nothing. saves `sepc`, sets `scause`/`stval`, masks interrupts, jumps to `stvec`. **30 GPRs are software's problem.**
- frame layout: 30 × 8-byte GPRs + 2 × 8-byte CSRs (sepc, sstatus) = 256 bytes. allocated 288 for headroom.
- skip `x0` (hardwired zero, never changes).
- the **sp-save trick**: by the time you'd want to save sp, you've already decremented it for the frame. wait until t0 is saved, then `addi t0, sp, 288` to compute the original sp, then store it.

## the entry/exit assembly

- ~40 lines of `sd` (store doubleword) + `ld` to mirror, plus the CSR read/write.
- `.align 4` before the entry label — RISC-V requires the trap vector address to be 4-aligned because the low 2 bits of `stvec` encode the mode (00=direct, 01=vectored).
- the trap handler is just *another section of code*; goes in `.text` like normal.

## scause decoding

- top bit = interrupt vs exception. bottom 63 bits = cause code, meaning depends on the flag.
- decoded into a typed `TrapCause` enum so panic messages read `unhandled trap: Breakpoint (scause=0x3)` instead of raw bit fiddling.
- v0.3 only handles `SupervisorTimerInterrupt` (cause 5). everything else panics, which is the right v0.3 behaviour — surface the unhandled case loudly so we know to handle it next.

## SSTC over SBI

- two ways to program a timer interrupt:
  - **SBI `set_timer`**: `ecall` to M-mode firmware. universal but every arm is an M-mode round trip.
  - **SSTC**: S-mode writes `stimecmp` (CSR 0x14d) directly. one CSR write. no trap to M-mode.
- DTB ISA-extensions list already advertised `sstc`. QEMU virt supports it. chose SSTC.
- minor wrinkle: some Rust assemblers don't yet know the symbolic name `stimecmp` — used the numeric address `csrw 0x14d, ...` as the portable form.

## deferred work, not in the IRQ

- IRQ handler does the minimum: measure rdtime delta, arm next deadline (which also acks the pending bit), set a `TICK_PENDING` atomic flag. **no locks taken.**
- main thread wakes from `wfi`, swap-clears the flag, runs the heartbeat span + metric emits.
- this sidesteps the IRQ↔lock deadlock corner that was already on the scaling-corners punch list. if the IRQ handler had taken the intern-table mutex while main was holding it: hang forever. spin::Mutex isn't reentrant.

## the heartbeat refactor

before:
```rust
let mut next = tracing::timestamp() + timebase_hz;
loop {
    while tracing::timestamp() < next {}     // busy spin
    { span!("kernel.heartbeat"); ... }
    next += timebase_hz;
}
```

after:
```rust
unsafe { trap::init_timer(timebase_hz) };
loop {
    while !trap::TICK_PENDING.swap(false, Relaxed) {
        unsafe { asm!("wfi") };
    }
    { span!("kernel.heartbeat"); ... }
    // next arm happens inside the IRQ handler
}
```

- the kernel went from "100% CPU in QEMU" to ~0% between ticks.

## modern Rust kernel idioms collected along the way

- `#[unsafe(no_mangle)]`, not `#[no_mangle]` — Rust 2024 moved a few attributes behind `#[unsafe(...)]` because they bypass the symbol-name/symbol-placement rules and can cause UB at link time.
- `unsafe extern "C" { fn trap_entry(); }` — extern blocks themselves are unsafe in 2024.
- `unsafe op in unsafe fn` warning — body of `unsafe fn` is *not* implicitly unsafe in 2024; every unsafe op still needs its own `unsafe { ... }` wrapper.
- function pointer → integer needs an intermediate `*const ()` cast. `trap_entry as *const () as usize`. function addresses aren't guaranteed to be representable as plain integers on every platform; the explicit pointer cast says "I know what I'm doing."

## the histogram

- new metric type end-to-end: kernel `register_histogram("snitchos.irq.timer.duration_ticks")`, observations emitted as `Frame::Metric` (same wire format as gauges/counters), collector dispatches by `MetricKind` and accumulates into bucket counts.
- bucket boundaries: exponentialish from 100 ticks to 1M ticks. wide enough to catch both "normal" (hundreds of ticks) and "something's wrong" (millions).
- Prometheus expects cumulative bucket counts at exposition time, but we store non-cumulative and convert at format-time. simpler observation path.
- Grafana panel: `histogram_quantile(0.50/0.95/0.99, rate(..._bucket[1m]))` as a percentile time series.

## numbers from QEMU, and what they actually mean

- p50 ≈ 1250 ticks, p95 ≈ 2470 ticks at 10 MHz timebase = **125–250 µs of real time** for the IRQ handler body.
- but the IRQ body is like 10 instructions! shouldn't be more than a microsecond on real silicon.
- explanation: **QEMU's `time` CSR ticks at wall-clock pace, not at simulated guest-cycle pace.** so `rdtime` delta inside the handler reflects how long the handler took *in actual real seconds*, scaled by the simulated 10 MHz tick rate. on QEMU TCG every guest instruction goes through translate + dispatch; the simulated wall-time elapsed is dominated by emulator overhead, not by guest "cycles."
- on real silicon: ~sub-microsecond. on QEMU: hundreds of microseconds. same code.
- moral: **the units on the wire are honest** (ticks), but their relationship to "instructions executed" depends on the host. file under "things to think about if we ever do real-time scheduling."

## what i learned

- **bit-decode CSRs into typed enums.** `TrapCause` reads better than raw bit math, and the compiler exhaustivity-check pushes you toward handling all known cases.
- **the deferred-work pattern is the IRQ↔lock answer.** also works for any "thing happens in interrupt context that needs to do real work" situation: kick a flag, return; main thread drains.
- **Rust 2024 unsafe-op tightening is a long tail of small papercuts** that adds up to a noticeable mindshift. you're never accidentally inside an unsafe context just because you're in `unsafe fn` anymore.
- **QEMU `time` is wall-clock-flavored.** worth knowing for any timing-sensitive measurement on top of QEMU.
- **once the trap path works, the rest of v0.3 is small.** the trap entry/exit was 80% of the effort; SSTC + enable + heartbeat refactor + histogram fit into the remaining hours.

## v0.3 status

| ✓ | thing |
|---|---|
| ✓ | Trap vector + entry/exit asm (save 30 GPRs + sepc + sstatus) |
| ✓ | Rust dispatcher with typed `scause` decoding |
| ✓ | SSTC timer arm via `csrw 0x14d` |
| ✓ | Interrupt enable (`sie.STIE` + `sstatus.SIE`) |
| ✓ | Timer IRQ handler + `TICK_PENDING` flag |
| ✓ | Heartbeat refactor: busy-spin → wfi |
| ✓ | `Clock` trait + `SstcClock` impl (abstraction shape) |
| ✓ | Histogram metric: kernel emit + collector bucketing + Grafana percentile panel |

## what's next

- **v0.4 (memory)**: higher-half kernel, page tables, physical frame allocator, kernel heap. allocator instrumented — heap pressure visible in Grafana.
- **maybe SMP first.** the `Clock` trait, the scaling-corners doc, and the deferred-work pattern all set things up for the SMP pull we discussed. open question whether to do it before v0.4 or stick to the plan order.
