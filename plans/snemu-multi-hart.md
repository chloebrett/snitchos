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
3. **`hart_start` (SBI HSM)** ✅ DONE. Introduced the effect mechanism: an S-mode
   `ecall` stashes a `SbiRequest` in `pending_sbi`; `step` drains it into a
   `HartEffect::Sbi`; the driver (`Machine`/`Cpu`) runs `service_sbi(&mut [Hart],
   caller, req)` with full access to all harts and writes `a0`/`a1` back.
   `hart_start` wakes the target hart at `start_addr` (`Hart::start`), errors on
   unknown/already-running id.
4. **Cross-hart `send_ipi`** ✅ DONE (fell out of increment 3). `service_sbi`'s
   `send_ipi` iterates every hart and raises `sip.SSIP` on those the mask selects
   (hart `i` = mhartid `i`). Test: hart 0's IPI raises only hart 1's SSIP.
5. **2-hart DTB + real boot** ✅ DONE. Re-dumped `virt.dtb` at `-smp 2`; added
   `loader::load_machine` (shares ELF parsing via `load_memory`); `main` boots a
   2-hart `Machine`. **The kernel boots on two harts:** hart 1 comes up with its
   own page table (distinct `satp`), `SECONDARY_READY` releases hart 0, it clears
   the SMP smoke, reaches the heartbeat loop (`entering heartbeat` prints), and
   emits 414+ ongoing telemetry frames. Next meta-loop stop was a compressed
   `c.subw` (unrelated to SMP), since implemented.

## Open / external

- The `-smp 2` DTB re-dump needs `qemu-system-riscv64` on PATH
  (`-machine virt,dumpdtb=snemu/virt.dtb -smp 2 -m 128M`). Increment 5.
- LR/SC reservation is already per-hart; under SC a store from either hart to a
  reserved address must break it (the reservation lives on the hart, the store
  goes through the shared bus — the bus/Machine must notify the other hart's
  reservation, or we accept single-hart-only SC breaking as a known gap to start).

## Follow-on: host-thread fan-out of the audit (post-5 "what's next")

Distinct axis from guest harts above: parallelise the **`snemu-itest` audit
across host threads**, one scenario per worker. Post 5 sold this — scenarios are
independent (each owns its own `Machine`, no shared mutable state), so it's
embarrassingly parallel; the win is deterministic *and* fast.

- **`parallel_map(items, jobs, f)`** in `xtask/src/itest/snemu_audit.rs`: an
  order-preserving work-queue (`AtomicUsize` cursor + slotted `Mutex<Vec<Option>>`)
  over `thread::scope`. Results land in **selection order regardless of worker
  count** — the property the report's determinism rests on. `jobs <= 1` runs
  serial. Unit-tested for order+value equivalence across job counts.
- **`cargo xtask snemu-itest --jobs/-j N`**, default `available_parallelism()`
  (10 on the M1 Max), `1` forces serial. snemu is a pure interpreter
  (**CPU-bound**), so useful `jobs` tops out at the physical core count; RAM is
  not the ceiling (128 MiB/machine × jobs).
- **Measured:** full 108-scenario audit **355s serial → 66.2s at jobs=10**
  (~5.4×), fidelity unchanged at **106/108** (the two revoke-race holdouts, and
  *only* those — parallelism moved no outcome). Beats QEMU's ten-run flake gate
  (~570s) by ~8.6×, and it's a proof you run once, not dice you roll ten times.
- **Floor is the compute tail, not core count.** Wall-clock bottoms out at the
  single heaviest scenario: a handful carry 3B-step budgets (`frame-allocator-oom`,
  `heap-oom`, `stitch-fs-loads-and-runs`, `workload-cooperative-baseline`,
  `spawn-reclaims-*`) at ~30–56s each. They now overlap instead of stacking, but
  no parallelism beats the slowest one alone — so 66s, not 355/10 ≈ 35s. The
  lever for the next tier is post-04's JIT, which attacks that tail directly.

## Non-goals (for now)

Relaxed memory, >2 harts, hart hotplug/stop (`sbi_hart_stop`), and external
interrupts (PLIC). Two harts, SC, round-robin.
```
