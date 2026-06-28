# 🦊 snemu — the SnitchOS emulator

*A SnitchOS-native RISC-V emulator: replace QEMU for the functional itest suite with a small RV64GC interpreter written in Rust, running on the macOS host. Telemetry is a first-class concern of the emulator itself — not something decoded off a socket after the fact. Determinism by default; controllable concurrency for race-hunting.*

Status: **proposed** (design only; no code). First milestone planned in detail in [plans/snemu-milestone-1-console-out.md](../plans/snemu-milestone-1-console-out.md).

## Why this step

We run the integration suite by spawning one QEMU per scenario and reading decoded `Frame`s off a virtio-console socket. That works, but three things push toward owning the machine end-to-end:

1. **Startup cost.** ~25 scenarios × ~1 s of QEMU spawn ≈ 25 s of pure overhead per suite run, ×10 under the `--repeat 10` commit gate. A pure-Rust interpreter boots in single-digit milliseconds. This is the difference between the repeat gate being "too slow" and being free.
2. **Telemetry core to the emulator.** Today observability stops at the kernel/hardware boundary — we see what the kernel chooses to emit. An emulator we own can observe what the kernel *can't see about itself*: exact instruction counts, cycle-attributed span timing, every MMIO access, page faults, the full guest memory map. SnitchOS observing SnitchOS from underneath. It can also hand `Frame` bytes straight to the harness in-process, skipping the virtio socket entirely in a "fast mode."
3. **End-to-end I/O ownership.** Owning the device side means we support whatever I/O we like, however we like — and it compounds with (2).

It is also, deliberately, a pedagogical mirror: building the emulator re-walks the kernel's own milestone history *from the other side of the hardware interface*. Every assumption the kernel makes (higher-half addressing, the QEMU `virt` memory map, Sv39, virtio-mmio) becomes something we have to *model*, not just rely on.

## The scope question, settled: complete user ISA, minimal system ISA

We want to run **arbitrary userspace programs**, not just the kernel boot path. The instinct is that this means "implement all of RISC-V." It does not.

Every guest binary — kernel, userspace, the Stitch interpreter (and therefore every `.st` program, which runs *inside* that interpreter) — is emitted by **our** toolchain targeting `riscv64gc`. There is no path by which a guest emits an instruction outside what LLVM emits for that target. So the envelope is exactly:

```
riscv64gc = G + C = IMAFD + Zicsr + Zifencei + C
```

That gives a sharp dividing line:

- **User-level ISA → complete.** Once we accept arbitrary programs we lose the kernel-driven-subset luxury: LLVM will eventually emit every AMO variant, every `fmadd.d`, every float conversion + rounding mode + `fcsr` bit. But "complete RV64GC user-level" is a **finite, enumerable ~200-instruction table** you check off against the spec — not an open-ended set. The unbounded thing about "arbitrary" is the *behaviour*, not the *instruction set*.
- **System / privileged ISA → only what our kernel uses.** We are not Linux-compatible, we are SnitchOS-compatible. We run arbitrary user *programs* under *our* kernel, never arbitrary *operating systems*. So Sv39 only (no Sv48/Sv57), no PMP, only the CSRs the kernel touches, only the trap/interrupt machinery the kernel relies on.

**Entire extension families we skip outright** (not in `gc`; LLVM never emits them): **V** (vector), **B/Zb\*** (bit-manip), **H** (hypervisor), **Zk\*** (crypto).

Consequence for bring-up: the unimplemented-opcode meta-loop (below) still works, but stops converging in a day once userspace is the target — the long tail (a rare AMO, a fused-multiply, a float classify) trickles in program-by-program. This is exactly why **riscv-tests matters more once userspace is the goal**: it front-loads that user-level tail systematically instead of discovering it one crashing program at a time.

## Determinism, and why there is no determinism-vs-races tradeoff

A single-threaded interpreter that steps harts in a scheduler-chosen order makes atomics and ordering **trivially correct and perfectly deterministic** — exactly what a fast, flake-free functional suite wants. Instruction count is the clock; same input, same run, same result.

The obvious worry is that determinism costs us the ability to reproduce the cross-hart races that have eaten real debugging time. It does not — because of a distinction in the *kind* of race:

- **Interleaving races** — two harts touch shared state; the bug appears at certain *orderings* of their operations. Reproduced by **any** concurrent execution, including instruction-granularity interleaving on a single host thread.
- **Reordering races** — need genuine weak-memory *reordering* (RVWMO) to manifest at all.

Our marquee cross-hart bug, the `TX_STAGING` wedge, was a dropped `MutexGuard` — *"not a memory-ordering race"* (see [plans/tx-staging-cross-hart-race.md](../plans/tx-staging-cross-hart-race.md)). That is an **interleaving** race. Nearly all the cross-hart pain we've documented is interleaving, not reordering.

For interleaving races a **single-threaded interpreter with a controllable, seedable scheduler** is *strictly better* than real threads:

- **Deterministic** — same seed → same interleaving → same result. No flakiness.
- **Reproduces races** — interleaving at instruction (or memory-access) granularity exposes far more orderings than QEMU's coarse thread scheduling.
- **Actively hunts them** — a PCT-style randomized scheduler (or systematic exploration over preemption points) *finds* the bad interleaving instead of running `--repeat 10` and praying. The "confirm flakiness statistically, ~1/r runs" problem disappears: a Heisenbug becomes a regression test with a fixed seed.

So "support multithreading" is more precisely **support controllable concurrency**: build the per-hart stepping abstraction from the start so concurrency is *expressible*, but realize it as a controllable interleaving scheduler, not host threads. Determinism and race-finding at once.

Real OS-thread-per-hart with shared memory only buys genuine **RVWMO reordering** bugs. Doing that *faithfully* means explicitly modeling the memory model (relying on incidental Apple-Silicon weak ordering is unfaithful and irreproducible) — research-grade, nondeterministic, aimed at a bug class we've barely hit. Keep it as a someday-maybe "RVWMO fuzz mode," never a goal.

### Relationship to loom

Two concurrency explorers, by scope — complementary, not merged:

| tool | scope | regime | model |
|---|---|---|---|
| **loom** | kernel-core host tests | exhaustive-on-tiny-units | C11 over `loom::sync::atomic` |
| **emulator scheduler** | whole-kernel guest interleavings | sampling-on-huge-system | RVWMO-ish over a flat byte array |

loom stays exactly where it is. It **cannot** directly drive the emulated guest: wrong abstraction level (guest accesses are plain interpreter loads/stores over a `Vec<u8>`, not loom atomics; RVWMO, not C11) and wrong scale (loom explodes combinatorially; a booting kernel's interleaving space is astronomically larger). But loom is the right *mental template* for the guest scheduler — systematic schedule exploration to surface the bad interleaving — at a different abstraction and a sampling regime. And because the emulator's *own implementation* is single-threaded, there is nothing in the emulator itself for loom to check — a confirmation that the single-threaded-with-controllable-scheduler design is the right call.

## The pre-MMU addressing trick (why console-out works without paging)

The kernel is linked entirely at higher-half VAs (`0xffffffff80200000+`), yet prints "Hello" *before* `mmu::enable`. The asymmetry (already documented in CLAUDE.md / [plans/v0.4-memory-findings.md](../plans/v0.4-memory-findings.md)):

- Under `code-model=medium`, `&str` literals are reached via **PC-relative** `auipc`. At boot the PC is *physical* (`~0x8020_0000`, where the ELF's `PT_LOAD` paddr puts it), so a PC-relative load of a `.rodata` string resolves to a **physical** address — fine with no MMU.
- `fmt::Arguments` (any *formatted* `println!`) embeds **absolute** fn-pointers to formatter functions, linked as higher-half VAs — only valid once paging is on. Hence "no formatted `println!` before `mmu::enable`."

So console-out is reachable with no page-table walker: load ELF at physical addresses → start PC physical → the early boot writes an unformatted "Hello" to the ns16550a THR at `0x1000_0000`.

**But the cliff is right there.** Functionally the kernel does almost nothing before paging: `entry.S` → a little setup → unformatted "Hello" → `mmu::enable` → trampoline jump to a higher-half VA. The instant it writes `satp` + `sfence.vma` and jumps to `0xffffffff…`, a no-MMU emulator falls off a cliff. So console-out is the right **engineering** checkpoint (it validates the entire decode/execute/CSR/trap/MMIO core in isolation, which is most of the *mechanical* work) even though it is a thin **functional** slice.

## Architecture

- **Pure interpreter, instruction-count as the clock.** No JIT, no pipeline model. The simplest stepping loop serves all three goals (fast, deterministic, instrumented). No clock concept needed until CLINT.
- **Flat `Vec<u8>` RAM** at `0x8000_0000`; an address-decode `match` routes RAM vs UART (vs CLINT / virtio / PLIC later). Devices have no MMU — anything the guest hands a device is already a physical address.
- **`Cpu` + `Memory` as a clean library API — the single most important early decision**, because everything tests through it. Raw-instruction unit tests are the bedrock TDD loop (below even riscv-tests): load N words, set registers, `step()`, assert state.

  ```rust
  let mut cpu = Cpu::new(mem);
  mem.write_u32(0x8000_0000, encode_addi(1, 0, 42));
  cpu.step()?;
  assert_eq!(cpu.x[1], 42);
  ```

- **Per-hart abstraction from day one.** A hart is a `Cpu` over shared `Memory`; the run loop steps harts in a scheduler-chosen order. Milestone 1 has one hart and a trivial scheduler, but the *shape* is multi-hart-ready so the interleaving scheduler is additive, never a rewrite.
- **The unimplemented-opcode meta-loop.** Decode dispatches to an "unimplemented" arm that dumps `pc` + raw instruction word and halts. Run the guest, see what it hits, implement that instruction, repeat. Each panic is a failing test; with the kernel boot path you converge fast because it touches a small subset.
- **Three correctness layers, decoupled.** Hand-crafted raw-instruction unit tests (fast, surgical) → **riscv-tests** ELFs as per-instruction ground truth (decouples "is my ADD correct" from "does the kernel boot") → the kernel/userspace itest scenarios as end-to-end acceptance. Bring the core up green on riscv-tests *before* trusting it against the kernel.
- **ELF loading** hand-rolled (~30 lines of ELF64 program-header parse for a static no-PIE kernel) over pulling `goblin`/`object` — fits the pedagogy and the format is trivial for our case.
- **Crate**: a new `snemu/` workspace member (host-only, std). The kernel is unchanged; itest workloads may grow a `minimal-boot` profile (see milestone 1) as a stable early target.

## Decisions locked in

| decision | choice |
|---|---|
| Replaces QEMU for… | the **functional** itest suite (deterministic). QEMU `thread=multi` optionally retained only for true-RVWMO fuzzing, if ever needed. |
| Execution model | pure interpreter, single host thread, instruction-count clock |
| Concurrency | controllable interleaving scheduler (per-hart abstraction from day one); **not** host-thread-per-hart |
| ISA — user level | **complete** RV64GC user instructions (finite ~200-op table) |
| ISA — system level | **minimal** — only what our kernel uses (Sv39 only, no PMP, on-demand CSRs) |
| Skipped extension families | V, B/Zb\*, H, Zk\* |
| Floats (F/D) | deferred past milestone 1; required before arbitrary userspace + Stitch. Real cost is the `sstatus.FS` dirty-bit save/restore state machine — a *kernel* feature co-evolved with the emulator, not just emulator arithmetic |
| Machine target | QEMU `virt` memory map (RAM `0x8000_0000`, ns16550a `0x1000_0000`, CLINT `0x200_0000`, virtio-mmio, PLIC `0xc00_0000`) — matches what the kernel already hardcodes; DTB can be null/minimal (kernel parks DTB parsing) |
| Correctness ground truth | raw-instruction unit tests → riscv-tests → kernel/userspace scenarios |
| Devices, build order | ns16550a → (MMU) → CLINT → virtio-console → SMP → U-mode/syscalls — the kernel's own milestone order, mirrored |
| New crate | `snemu/` (host, std), workspace member |

## Alternatives considered

| approach | verdict |
|---|---|
| **Keep QEMU only** | The status quo. Slow startup dominates the repeat gate; telemetry stops at the hardware boundary; no I/O ownership. The thing we're improving on. |
| **Host-thread-per-hart, shared memory** | Only faithful for RVWMO *reordering* bugs, which we've barely hit; reintroduces nondeterminism; faithful modeling is research-grade. Rejected as the default; kept as a possible far-future fuzz mode. |
| **Drive the guest with loom** | Wrong abstraction (RVWMO over bytes, not C11 over loom atomics) and wrong scale (exhaustive-on-tiny vs a booting kernel). loom stays on kernel-core; its *algorithm* inspires the guest scheduler. |
| **JIT / dynamic translation** | Premature. Interpreter speed is already a huge win over QEMU startup, and a JIT fights determinism + instrumentation. Revisit only if interpreter throughput ever bottlenecks. |
| **Emulate full RV64 spec** | Unnecessary. We control the toolchain; the envelope is `riscv64gc`-user (finite) + our kernel's system slice. Whole extension families stay off the table. |

## Milestones

Detailed plan for milestone 1: [plans/snemu-milestone-1-console-out.md](../plans/snemu-milestone-1-console-out.md). Later milestones are sketched there at low granularity and will be promoted to their own plans as they come up.

1. **Console out (UART only)** — RV64IMC core + CSRs + traps + ns16550a, no paging, one hart. Acceptance: the kernel's first unformatted "Hello" appears on host stdout, validated against riscv-tests for the core.
2. **Reaches heartbeat** — + Sv39 + CLINT + virtio-console. Acceptance: passes `boot-reaches-heartbeat` and `heartbeat-cadence` against the real kernel, with `Frame`s delivered in-process.
3. **Functional parity + concurrency** — + second hart, the controllable interleaving scheduler, U-mode + syscalls. Acceptance: runs the userspace/`init` scenarios; the interleaving scheduler reproduces a known cross-hart interleaving bug deterministically from a seed.
