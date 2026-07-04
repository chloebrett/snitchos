# snemu multi-hart

## Why

The real kernel is compiled `MAX_HARTS=2` and kmain **unconditionally** brings up
hart 1 (`sbi::hart_start(1, …)`, then spin-waits `SECONDARY_READY`,
`kernel/src/main.rs:466-473`) — the actual heartbeat loop is at line 610, *after*
the bringup. So snemu cannot fully boot the kernel single-hart, and the
differential oracle (`xtask itest --snemu`) needs the same 2-hart topology QEMU
runs. Multi-hart is the gate on both.

This is also the "eventually the second [memory model]" thread: we start with a
**sequentially-consistent** machine (shared flat memory, instruction-atomic
interleaving) and leave relaxed-memory (store buffers, reordering) as a later
milestone. An SC snemu already cleanly separates "logic bug" (fails under SC too)
from "weak-memory bug" (only fails under QEMU/relaxed).

## Design decisions

- **Interleaving: round-robin, one instruction per running hart per round.**
  Deterministic (no `Math.random`, reproducible — the property the post
  celebrates), dead-simple, and correct for cross-hart spin-waits: every running
  hart advances each round, so a hart spinning on a flag the other sets will see
  it. Quantum-based interleaving is a perf optimization we don't need; finer
  interleaving also surfaces more schedules for future bug-finding.
- **Memory model: sequentially consistent.** One shared `Bus`/`Memory`; an
  instruction is indivisible. `aq`/`rl` stay no-ops (correct under SC). Relaxed
  memory is deferred.
- **`time` is a shared clock.** Architecturally `time` is a common real-time
  counter, not per-hart `instret`. A `Machine`-level `cycle` counter (one tick
  per instruction executed by any hart) is the `rdtime` source; the timer
  compares shared `time` vs each hart's own `stimecmp`. (Today `time = instret`
  on the one hart — fine single-hart, wrong for two.)
- **Cross-hart effects flow through the `Machine`, not hart-to-hart.** A hart's
  `step` can't hold `&mut self` *and* `&mut other_hart`. So `Hart::step` returns
  an effect the `Machine` applies with full access: `SendIpi` (set `sip.SSIP` on
  the target hart), `HartStart` (wake a parked hart), and the SBI return value
  written back into the caller. Per-hart interrupt *delivery* (own `sip.SSIP`,
  own `stimecmp` vs shared `time`) stays inside `Hart::step` — it only needs the
  hart's own CSRs.

## Target shape

```
struct Machine { harts: Vec<Hart>, bus: Bus, time: u64 }
struct Hart { x, pc, csr, privilege, reservation, cur_ilen, hartid, state }
enum HartState { Running, Stopped }         // hart 1 starts Stopped (parked)
enum HartEffect { None, Sbi(SbiRequest), … } // cross-hart work for the Machine
```

`Hart::step(&mut self, bus: &mut Bus, time: u64) -> Result<HartEffect, StepError>`
does interrupt-check → fetch/decode/execute against the shared bus. `Machine::step`
round-robins the running harts, ticks `time`, and applies each returned effect.

`Cpu` is kept as a **single-hart convenience wrapper** (`{ hart: Hart, bus: Bus }`)
so the ~94 existing `cpu.rs` unit tests keep their API (`Cpu::new`, `cpu.step()`,
`cpu.reg()`, `cpu.csr`). The loader and `main` move to `Machine`.

## Increments (each TDD, green throughout)

1. **Extract `Hart` from `Cpu`** ✅ DONE. `Hart` = registers/pc/csr/privilege/
   reservation + `step(&mut Bus)`; `Cpu { hart, bus }` is a thin wrapper keeping
   the whole external API. Behavior-preserving (96 tests green, kernel boots
   identically). No effect-return needed yet — single-hart `send_ipi` still
   targets self.
2. **`Machine` + round-robin scheduler + shared `time`** ✅ DONE. `snemu/src/
   machine.rs`: `Machine { harts, bus, time }`, hart 0 running / secondaries
   `park()`ed, `step()` round-robins running harts and ticks the shared clock.
   `Hart` gained a `cycle` field (the shared-clock snapshot the driver sets each
   step) — `rdtime`/`stimecmp` read it instead of per-hart `instret`; the `Cpu`
   wrapper sets `cycle = instret` to preserve single-hart behavior. Test: a
   2-hart machine advances only hart 0 while hart 1 is `Stopped`.
3. **`hart_start` (SBI HSM).** Service EID `0x48534D` in the `Machine`: wake the
   target hart at `start_addr` (physical, MMU off), `a0 = hartid`, `a1 = opaque`,
   `state = Running`; return `SBI_SUCCESS` (or error if already running). Test:
   after the call, hart 1 runs from the entry with the right registers.
4. **Cross-hart `send_ipi`.** Target the *other* hart's `sip.SSIP` (not just
   self). Test: hart 0's `send_ipi(1<<1, 0)` makes hart 1 take a software
   interrupt.
5. **2-hart DTB + real boot.** Re-dump `virt.dtb` with `-smp 2`; wire `main` to a
   2-hart `Machine`. Run the kernel: it should pass `hart_start`, hart 1 runs
   `_secondary_start`, sets `SECONDARY_READY`, and hart 0 proceeds to the
   heartbeat loop (`entering heartbeat` should finally print).

## Open / external

- The `-smp 2` DTB re-dump needs `qemu-system-riscv64` on PATH
  (`-machine virt,dumpdtb=snemu/virt.dtb -smp 2 -m 128M`). Increment 5.
- LR/SC reservation is already per-hart; under SC a store from either hart to a
  reserved address must break it (the reservation lives on the hart, the store
  goes through the shared bus — the bus/Machine must notify the other hart's
  reservation, or we accept single-hart-only SC breaking as a known gap to start).

## Non-goals (for now)

Relaxed memory, >2 harts, hart hotplug/stop (`sbi_hart_stop`), and external
interrupts (PLIC). Two harts, SC, round-robin.
```
