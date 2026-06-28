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
| **JIT / dynamic translation** | Not for the early milestones (the startup win is already large), but explicitly a *later measured arc* — Tier 1/2 (data-not-code) translation, gated on measurement. Tier 3 native codegen is the big-win horizon: a separate native backend that runs on host (`MAP_JIT`) **and nests** (via the `ExecMemory` capability, bounded to the outermost layer) — gated, not host-only. See *Exploration notes → JIT* and *→ Off-host JIT*. |
| **Emulate full RV64 spec** | Unnecessary. We control the toolchain; the envelope is `riscv64gc`-user (finite) + our kernel's system slice. Whole extension families stay off the table. |

## Milestones

Detailed plan for milestone 1: [plans/snemu-milestone-1-console-out.md](../plans/snemu-milestone-1-console-out.md) (M2/M3 sketched there); measurement spine: [plans/snemu-milestone-4-measurement.md](../plans/snemu-milestone-4-measurement.md). Later milestones promote to their own plans as they come up.

**Guiding principle — measure first, then tune what you measured.** An observability-first project building an emulator makes the emulator observe *itself* first, then optimizes against its own telemetry — the same way the kernel tunes its heap watermark against heap metrics. The measurement spine (M4) is the load-bearing artifact; every JIT tier after it is an *episode measured against it*. Cheap counters (instret, wall-clock) are baked in from M1 so measurement is never retrofitted. The whole arc is also a devlog series — one post per milestone.

1. **Console out (UART only)** — RV64IMC core + CSRs + traps + ns16550a, no paging, one hart. + cheap counters from day one. Acceptance: the kernel's first unformatted "Hello" appears on host stdout, validated against riscv-tests for the core.
2. **Reaches heartbeat** — + Sv39 + CLINT + virtio-console. Acceptance: passes `boot-reaches-heartbeat` and `heartbeat-cadence` against the real kernel, with `Frame`s delivered in-process. First end-to-end wall-clock vs QEMU.
3. **Functional parity + concurrency** — + second hart, the controllable interleaving scheduler, U-mode + syscalls, A extension. Runs the itest suite end-to-end, *no JIT*. This is the "working end-to-end" line. Acceptance: runs the userspace/`init` scenarios; the interleaving scheduler reproduces a known cross-hart interleaving bug deterministically from a seed.
4. **Measurement spine** — harden self-telemetry into the two modes (measurement / observability), build the benchmark harness + workload taxonomy, establish the QEMU baseline + Grafana dashboard, and stand up the **nested overhead-factor methodology**. Everything after this is measured against it. See *Exploration notes → Measurement* and the M4 plan.
5. **JIT Tier 1 — decode cache.** Pre-decode + cache by PC; data, not code. Measured delta across the taxonomy. Works on host *and* nested (no exec memory needed).
6. **JIT Tier 2 — block formation + dispatch elimination + chaining.** Threaded/closure translation, software block chaining; still data, not code. Measured delta.

*Horizon (not in this arc):* **JIT Tier 3 — native codegen.** The big-win backend (~10–50×). A separate native-codegen backend coexisting with the interpreter — W^X exec pages via `MAP_JIT` on host, and **nests** via a new SnitchOS `ExecMemory` capability (bounded to the outermost JITing layer; inner guests ride SMC handling). Not host-only, not precluded — gated on handling the scheduler/instrumentation tensions and a measured compute-bound need that M5/M6 + the interleaving scheduler don't already solve. See *Exploration notes → JIT* and *→ Off-host JIT*.

---

# Exploration notes

*Captured from design discussion. These are the "why" and the roads-not-yet-taken behind the milestone arc — kept so the reasoning survives even though the early milestones don't touch most of it.*

## QEMU: what it actually does, and why startup costs a second

QEMU has two engines. **KVM/HVF** (hardware virtualization) runs guest instructions natively and only works when guest arch == host arch — **unavailable** for riscv64-on-arm64 (Apple Silicon). So for SnitchOS-on-Mac, QEMU runs **TCG** (Tiny Code Generator): a *dynamic binary translator* that JITs blocks of guest RISC-V into host arm64, caches and chains them. So QEMU here is already **pure software emulation** — same category as snemu, just JIT'd where snemu interprets. snemu competes with **TCG, not KVM.**

The ~1 s QEMU startup is almost entirely **fixed host-side setup, not guest execution**:
- process + dynamic-linker startup (huge binary, dozens of dylibs; on macOS, code-signing / library-validation of the binary + every dylib is a real launch tax);
- machine construction (instantiate every `virt` device, wire IRQ topology, **generate the DTB at runtime**, set up address spaces);
- firmware (default `virt` runs **OpenSBI** first, then jumps to the kernel);
- cold-JIT warm-up (early boot is all cold paths → TCG pays translation cost with no cache hits);
- possible socket-wait (a `server` chardev without `nowait` *blocks until the harness connects* — a handshake mis-attributed to "boot").

snemu's "boot" is: alloc RAM, parse ELF, set `pc`, go. Small static binary, trivial machine (no DTB generation — the kernel parks DTB parsing), no firmware, no JIT warm-up. **Milliseconds.** This is why the startup win is *structural* (snemu has almost no fixed setup) while any per-instruction loss is *per-instruction* (interpreter vs JIT). Net: snemu is much faster to start, slower to run, **net-faster exactly when startup dominates** — most smokes, few storms. The right end state is **hybrid**: snemu for the fast functional suite, QEMU retained for compute-heavy storms (and as the only true-RVWMO fuzzer). Determinism shifts the math further: a seeded snemu run replaces `--repeat 10`, so the comparison is "1 snemu run vs 10 QEMU runs," not 1-vs-1.

## JIT: the tier ladder, the win, and the conflicts

A JIT only helps **execution-bound** scenarios; smokes are startup-bound where snemu already wins. And the storms' purpose is race-finding, which the deterministic interleaving scheduler addresses better than raw speed. So: **measure whether a compute-bound problem even survives the interleaving scheduler before building any JIT.**

The ladder (effort ↑, win ↑, portability/compatibility ↓):

- **Tier 1 — decode cache.** Decode each instruction once into a struct, cache by PC, `match` on it. Kills re-decode cost. ~**2–4×**. Pure data; no exec memory; portable; no_std; deterministic; instrumentation-transparent; **nests**. A day or two.
- **Tier 2 — block cache + software chaining (threaded/closure).** Group into basic blocks, cache decoded blocks by entry PC, link exits directly (software block chaining); optionally a `Vec` of handler fn-pointers. ~**3–6×**. Still data, not code; still portable; still **nests**. ~a week. The sweet spot for a planned milestone.
- **Tier 3 — native codegen.** Emit host machine code per block. ~**10–50×** (TCG territory). Where all the complexity *and* the conflicts live.

RISC-V makes Tier 3 *less* crazy than typical: fixed-width regular encoding (trivial decode), RISC→RISC is nearly a transliteration, and the **register-pinning gift** (arm64's 31 GPRs ≈ RV64's 31 real regs → pin hot guest regs to host regs). The genuinely hard parts aren't arithmetic — they're the **soft-MMU** (every load/store needs translation + checks; calling back to Rust per op kills the win, so fast JITs *inline* a TLB lookup — this is where much of TCG's complexity actually is), **exits out of generated code** (traps/interrupts/scheduler), and **macOS W^X** (`MAP_JIT` + `pthread_jit_write_protect_np`, JIT entitlement).

**Tier 3 interacts with all four snemu pillars — two genuine tensions, two merely gated:**
1. **Nesting — gated, not precluded.** Native codegen needs W^X exec pages, which SnitchOS userspace doesn't expose *today* — a missing ABI, not a fundamental barrier. The `ExecMemory` capability (see *→ Off-host JIT*) unlocks nested native codegen, and the tower insight bounds the requirement to the **outermost JITing layer** (inner guests ride snemu's SMC handling for free). So Tier 3 **does** nest — on host via `MAP_JIT`, nested via a capability we'd want for the OS anyway. This is the opposite of a blocker: it's a *reason to build the capability*. Don't undersell this.
2. **Interleaving scheduler — genuine tension.** Block chaining *avoids* returning to the dispatcher, but the race-finder *wants* fine-grained yield points. Forces preemption only at block boundaries (or explicit emitted checks); instruction-granularity interleaving gets expensive. (This is exactly why TCG has a special deterministic `icount` mode.)
3. **Instrumentation — genuine tension.** Per-instruction telemetry is the whole pitch; a JIT has no natural per-instruction hook, so you must *emit* instrumentation into the code, which is more work *and* erodes the win. Goal and win partially cancel.
4. **Determinism — fine.** Survives (same input → same code → same run), *if* (2) is handled.

**Cranelift** is the pragmatic Rust backend for Tier 3 (it's what Wasmtime uses; designed for "many front ends, one backend"). But: heavy **std-only** dep (can't run in no_std SnitchOS userspace → useless for nesting / on-target), and **coarse control** over codegen (bad for inserting snemu's per-instruction instrumentation). Hand-rolling arm64 emission is more work but more control + no dep + teachable (fits the ethos). Verdict for snemu: **Tiers 1–2 are the near-term planned milestones** (cheap, keep snemu one coherent, nesting, instrumentable, deterministic engine). Tier 3 is a *separate native-codegen backend* that **coexists** with the interpreter — host via `MAP_JIT`, nested via `ExecMemory` — not a host-only dead-end; it's the big-win horizon (~10–50×) and the on-target-codegen capability is genuinely SnitchOS-shaped. It's gated on two things: handling tension (2)/(3) above, and a *measured* compute-bound need that Tiers 1–2 + the scheduler don't already solve (if a few storms remain compute-bound, "keep QEMU for just those" is the cheaper answer). Gated and ambitious — not precluded, and not host-only.

## Nesting: snemu inside SnitchOS

If `snemu-core` is **no_std + alloc** with host I/O behind a `Platform`-style trait, snemu can run as a **SnitchOS userspace program** and boot a *guest* SnitchOS. Precedent in-repo: **Stitch's interpreter is already no_std+alloc and builds for the riscv64 target** — snemu follows the identical pattern, and the recent `Platform` trait (`write`/`read_line`) is the same seam.

snemu's demands on a host are astonishingly thin — and that thinness is *why* it nests:
- **allocator** (guest RAM is a `Vec<u8>`);
- **a byte source for the guest ELF** (host: file; SnitchOS: RAMfs read over IPC);
- **a byte sink for guest console** (host: stdout; SnitchOS: `ConsoleWrite`);
- **no host threads** (single-threaded interpreter + scheduler);
- **no host clock** (instruction-count clock → the nested guest is deterministic *regardless of how chaotically the outer SnitchOS preempts snemu*);
- **no host FPU** if F/D is soft-floated (pure integer bit-twiddling) — then it nests even on an FP-less SnitchOS; choose host-`f64` and the layer below needs userspace FP (the `sstatus.FS` story).

**The split this forces** mirrors `kernel`/`kernel-core` (and is worth doing even if we never nest, as design pressure for the `Platform` seam):
- `snemu-core` — no_std+alloc: `Cpu`, `Memory`, decode, execute, devices over `Platform`. The machine.
- host shell — std: xtask glue, riscv-tests harness, real file/socket I/O.
- SnitchOS-userspace shell — links the `user/` runtime + talc, implements `Platform` over syscalls.

**The turtle stack** (L0 real HW/QEMU → L1 SnitchOS kernel → L2 snemu process → L3 guest SnitchOS → L4 guest userspace). The **fixed point**: if snemu faithfully emulates QEMU `virt`, the L3 kernel ELF is *byte-identical* to the L1 one — SnitchOS booting an identical copy of itself.

**The payoff is nested observability** (the on-brand part): snemu's own telemetry (guest instret, every MMIO, page faults) flows out through SnitchOS's telemetry channel as spans/metrics, while the guest's *own* `Frame`s (out its virtio-console) are captured by snemu's device and can be re-emitted as nested spans — a **trace-within-a-trace**: SnitchOS observing a guest SnitchOS observing itself.

**Caveats:** RAM shrinks geometrically per level (shrink the guest, bump the outer `-m`; 1–2 levels fine, deep towers hit a RAM wall); needs M2+ to boot a *real* kernel; **speed compounds multiplicatively**, so nesting is a pedagogical/demo artifact, *not* an itest path — the practical wins (fast itests, telemetry) are all snemu-on-host.

## Off-host JIT: data-not-code works free; native needs a capability

"Can a nested snemu JIT off-host?" splits by whether the artifact is **data** or **executable memory**:
- **Tiers 1–2 work off-host today, at any nesting depth, with zero kernel support** — the artifact is a `Vec`/internal bytecode (data); the handlers are already-compiled Rust. The no_std/no-exec-memory limit only ever blocked *native-code* JITs.
- **Tier 3 native codegen off-host needs a new kernel capability.** The MMU primitive exists (`mmu::map` can set the X bit in Sv39 PTEs); what's missing is an *ABI* (userspace `MapAnon` hands out RW only). Two shapes: **(a)** a W^X-toggleable exec-memory mapping, or **(b)** kernel-mediated code submission (hand the kernel bytes, it maps them executable).

**This is the maximally-SnitchOS feature.** The right to make memory executable is exactly the kind of authority that should be a **capability** — an `ExecMemory` object, explicitly granted, revocable, with every code-emission an observable `CapEvent`/span: literally *watch a JIT compile as a trace* (span per emitted block, metric for code-cache bytes, the W^X flip as an audited event). Neither QEMU nor Cranelift hands you that; it exists only because we own the kernel. (Relates to the explicit-authority-shell idea.)

**The tower insight — depth doesn't multiply the requirement:** only the **outermost real-execution layer's** native JIT needs *real* exec memory (granted once by its immediate host). A guest *inside* snemu that JITs needs nothing new — it writes code into *guest RAM* and sets the X bit in *guest* page tables; that's the guest doing **self-modifying code**, which snemu just has to *emulate correctly* (detect writes to pages it has cached translations for, invalidate — classic SMC handling, what QEMU does by write-protecting translated pages). So inner JITs ride snemu's SMC handling for free; real executable memory is needed exactly once, at the top.

## Syscalls (snemu's, two senses)

1. **Nested host I/O → SnitchOS syscalls.** When snemu runs as a SnitchOS process, the `Platform` trait routes its host needs onto syscalls: guest-ELF bytes ← RAMfs (IPC), guest console ← `ConsoleWrite`, guest RAM ← `MapAnon`, time ← none needed (instruction-count clock). snemu's host surface is a *subset* of what Stitch already needed, plus a file read.
2. **Guest syscalls (M3).** snemu must emulate the trap path the kernel implements: `ecall` from U-mode, `sstatus` SPP/SPIE transitions, the cap-mediated + ambient syscall dispatch — so guest userspace (`init`, FS server, clients) runs. This is "model the privilege machinery," not "implement a syscall ABI" — snemu runs the *kernel's* dispatch, it doesn't reimplement it.
3. **New kernel syscall for off-host Tier 3:** the `ExecMemory` capability above. Not needed for Tiers 1–2.

## Measurement: the spine the JIT arc stands on

Determinism is what makes the JIT numbers *honest*: same workload + seed → **identical guest execution, identical instruction count** across every tier; only wall-clock varies. True apples-to-apples deltas — something QEMU can't give (nondeterministic, no fixed instret).

**Two modes** (the observer effect is real — rich per-instruction telemetry perturbs what it measures):
- **measurement mode** — cheap counters only (instret, wall-clock, cache stats); low perturbation; source of the speedup numbers.
- **observability mode** — full per-instruction frames / MMIO traces / page-fault spans; for debugging + "watch a guest execute" demos; accepts the slowdown.

**Metric set** (flows out as `Frame`s → Grafana): guest **MIPS** (instret/wall-clock, the headline); **wall-clock per itest scenario** (ties to the QEMU-startup motivation); **host-work-per-guest-instruction** (the overhead factor each tier attacks); **hot-block concentration** (predicts JIT payoff *before* building it; explains why a workload did/didn't speed up); **block-cache hit rate / dispatch counts**; **startup time** (keep visible so JIT work doesn't regress it); code-cache size + guest RAM.

**Workload taxonomy** (so "various workloads" has texture and the diminishing-returns story is honest): **startup-bound** (boot-to-heartbeat — JIT barely helps, proves it's no panacea); **compute-bound tight loop** (storm / synthetic LCG burner — JIT helps most, the hero number); **memory-bound** (load/store heavy — soft-MMU dominates, shows why TLB inlining is the real Tier-3 lever and why Tiers 1–2 plateau); **trap/MMIO-heavy** (syscall-y — exits cap the win, explains the ceiling).

### Nested overhead measurement (the elegant one)

A process can't easily count its *own* retired host instructions (needs OS perf counters — platform-specific, sampled, nondeterministic, whole-process noise). **Nesting converts this un-self-measurable quantity into ordinary deterministic telemetry:**
- **inner snemu** emulates the test guest, counts guest instructions `G` (its own instret);
- **outer snemu** runs the inner one, counts `H` = every instruction the inner executed = the inner's host-instruction count;
- **overhead factor = H / G**, from two ordinary instret readings — exact, deterministic, platform-independent, no `perf`.

**Per-class breakdown** (what actually drives the JIT): run targeted microbenchmarks in the inner snemu, read the outer's instret **delta** — a loop of `add`s → host-instrs per ALU op (decode+dispatch); `ld`/`sd` → host-instrs per memory op (**the soft-MMU cost**); a trap/MMIO crossing → exit cost. Now you have a precise map of where host instructions go, telling each JIT tier what to attack and *proving* it did (decode-cache craters ALU-op cost, chaining craters dispatch, TLB-inlining craters memory-op cost).

**Algorithmic vs wall-clock — keep both.** The nested factor measures *instruction count* (pure algorithmic overhead, microarch-noise-free — exactly what a JIT removes); host wall-clock MIPS measures real-silicon speed. Their **disagreement is itself a finding**: a tier that drops H/G but barely moves wall-clock traded instructions for cache misses / mispredicts — a sophisticated, honest post almost no hobby emulator can write.

**Cost is a non-issue:** a slow interpreter under a slow interpreter is brutally slow, but you're measuring *counts, not time* — counts are exact no matter how slow. Run small bracketed microbenchmarks (a few million guest instructions, a few seconds), perfect numbers.

**Plumbing it needs:** (1) a **measurement-marker channel** so the inner can bracket its measured region (magic MMIO write / recognizable nop pattern the outer watches for) → `H` excludes inner startup/IO; (2) **inner runs in measurement mode** so its own telemetry doesn't inflate `H`. The nested setup is the killer app for the two-mode split.

Framing: *snemu measures snemu using nothing but snemu* — the observability emulator self-hosting its own benchmark.
