# snemu milestone 1 — console out (UART only)

Stand up a RV64IMC interpreter (`snemu`, the SnitchOS emulator) that loads the kernel ELF, executes the
boot path, and prints the kernel's first unformatted "Hello" to host
stdout via an emulated ns16550a UART. No paging — the run ends at the
`mmu::enable` cliff, by design. This milestone validates the entire
decode / execute / CSR / trap / MMIO core *in isolation*; it is the
right engineering checkpoint even though it is a thin functional slice.

Design + scope: [docs/snemu-design.md](../docs/snemu-design.md).

## Why this step

The decoder is the bulk of the *mechanical* work in the whole emulator,
and console-out exercises all of it (fetch, decode incl. compressed,
integer/mul/div execution, CSR read/write, trap entry, an MMIO store)
without yet requiring the conceptually hard subsystems (Sv39, CLINT,
virtio, SMP). Getting the `Cpu` + `Memory` library surface right here is
load-bearing: every later milestone and every test goes through it.

We stop at the MMU cliff deliberately. The kernel does almost nothing
before paging: `entry.S` → setup → unformatted "Hello" → `mmu::enable` →
trampoline to a higher-half VA. Seeing "Hello" proves the core loop;
the very next instruction stream is milestone 2's problem.

## Decisions locked in

| decision | choice |
|---|---|
| Crate | new `snemu/` workspace member, host-only (`std`) |
| Core API | `Cpu` over a `Memory` trait/struct; `cpu.step() -> Result<Step, Trap>` |
| ISA this milestone | RV64 **IMC** + Zicsr + Zifencei (no F/D, no A beyond what boot hits) |
| Compressed (C) | mandatory — expand 16-bit forms to canonical 32-bit in decode |
| Memory | flat `Vec<u8>` at `0x8000_0000`; address-decode `match` (RAM vs UART) |
| Devices | ns16550a only (`0x1000_0000`); THR write → host stdout |
| Clock | instruction count; no CLINT yet |
| Counters | **cheap counters from day one** — `instret` + wall-clock, low-perturbation. The "measurement mode" seed; measurement is never retrofitted (see M4 / design doc *Exploration notes → Measurement*) |
| Harts | one; per-hart shape in place but trivial scheduler |
| ELF | hand-rolled ELF64 program-header load (PT_LOAD → paddr) |
| Unknown opcode/CSR | dump `pc` + raw word + decoded guess, halt (the meta-loop) |
| Correctness layers | raw-instruction unit tests → riscv-tests → kernel boot to "Hello" |
| Kernel target | a `minimal-boot` itest-workloads profile that prints + halts pre-MMU, as a stable target decoupled from the full kernel (see step 7) |
| Out of scope | Sv39, CLINT, virtio, SMP, U-mode, F/D, A (full set), interrupts |

## Steps (TDD; each leaves the suite green)

Each step is RED (failing test first, in its own edit) → GREEN (minimum
code) → assess. Per house rule, write the failing test *first* and on
its own; don't batch even when the pattern feels obvious.

### Step 1 — crate skeleton + `Memory`
- `snemu/` workspace member; `Memory { ram: Vec<u8>, base }` with
  `read_u8/16/32/64` + `write_*`, little-endian, bounds-checked
  (out-of-range → a typed `BusError`, not a panic).
- Tests: round-trip each width; endianness; OOB returns error.
- No CPU yet.

### Step 2 — `Cpu` state + the raw-instruction test harness
- `Cpu { x: [u64; 32], pc: u64, csr: …, instret: u64, mem }`. `x[0]`
  hard-wired zero. `step()` bumps `instret` — the first (free) counter
  of the measurement spine.
- A test helper that loads hand-encoded words at `0x8000_0000`, sets
  registers, runs `step()` N times, asserts register/memory state. This
  helper is the bedrock loop — invest in its ergonomics.
- First instruction TDD'd through it: `addi` (covers immediate decode,
  `x0` semantics, pc advance). RED: encode `addi x1, x0, 42`, expect
  `x[1] == 42` — fails (no decoder). GREEN: minimal decode + execute.

### Step 3 — RV64I integer core
- Drive in, one failing test each, the integer base: `lui`, `auipc`,
  `op-imm` (`addi/slti/xori/ori/andi/slli/srli/srai`, +`.w` forms),
  `op` (`add/sub/sll/slt/.../and`, +`.w`), branches (`beq…bgeu`),
  `jal`/`jalr`, loads/stores (`lb…lwu`, `sb…sd`).
- `auipc` test must assert PC-relative resolves to the *physical* PC —
  the property the kernel's pre-MMU "Hello" depends on.
- Misaligned access: fault (typed trap) for now; implement only if a
  guest hits it.

### Step 4 — M extension
- `mul/mulh/mulhu/mulhsu`, `div/divu/rem/remu`, +`.w` forms.
- Tests include the spec's division edge cases (div-by-zero → all-ones;
  signed overflow `INT_MIN / -1`).

### Step 5 — Zicsr + traps
- CSR file with on-demand registration; `csrrw/csrrs/csrrc` (+imm).
  Unknown CSR → meta-loop dump, not silent zero.
- Trap entry machinery the boot path needs (mtvec/stvec, mepc/sepc,
  mcause/scause, mstatus bits) — implemented to the extent the kernel
  reads/writes them pre-MMU. `fence`/`fence.i` as no-ops (no caches).
- Tests: CSR read/write semantics; a forced illegal-instruction trap
  lands at the trap vector with the right cause/epc.

### Step 6 — C (compressed) extension
- Decode front-end: read 16 bits; low two bits `== 11` → 32-bit, else
  expand the compressed form to its canonical 32-bit equivalent and
  reuse the step-3/4 execution paths.
- Tests: a table of compressed encodings → expected expansion → same
  observable effect as the canonical form. Cover the common ones the
  compiler sprays (`c.addi`, `c.li`, `c.mv`, `c.lw/sw/ld/sd`,
  `c.j/c.jr/c.beqz`, `c.addi16sp/c.addi4spn`).

### Step 7 — ns16550a + ELF load + the `minimal-boot` target
- ns16550a device: minimal register set; **THR write → push byte to a
  host sink** (default stdout; an in-memory sink for tests). LSR reports
  always-ready. Tests: writing bytes to THR yields exactly those bytes
  at the sink.
- Hand-rolled ELF64 loader: parse program headers, copy `PT_LOAD`
  segments to their `p_paddr`, set `pc` to `e_entry`, `a0 = 0` (hartid),
  `a1 = 0` (null DTB — the kernel parks DTB parsing). Test against a
  tiny fixture ELF.
- Kernel side: a `minimal-boot` profile under the `itest-workloads`
  umbrella that writes an unformatted greeting to the UART and halts
  (`wfi` loop) *before* `mmu::enable`. This is the stable acceptance
  target — decoupled from full-kernel churn. **Build this profile in
  the snemu harness and refuse to run a stale binary** (the cfg-gated
  stale-binary footgun bit us before).

### Step 8 — riscv-tests as ground truth
- A test mode that loads the official per-instruction riscv-tests ELFs
  (rv64ui, rv64um, rv64uc) and asserts each test's pass convention
  (the `tohost` / `gp == 1` signal, or our equivalent halt+check).
- Decouples "is my ADD correct" from "does the kernel boot." Bring the
  core **green on riscv-tests before** trusting it against the kernel.

### Step 9 — acceptance: boot to "Hello"
- snemu loads the `minimal-boot` kernel ELF, runs to the halt, and
  the captured UART sink contains the greeting. Wire this as an xtask
  subcommand (`cargo xtask snemu …`) and/or a snemu integration test.
- Expected end state: greeting captured, then the kernel halts (or, if
  pointed at the *full* kernel, the run hits `mmu::enable` and stops at
  the cliff — a clean, recognized "needs milestone 2" outcome, not a
  crash).

## Acceptance criteria

- Core is green on rv64ui + rv64um + rv64uc riscv-tests.
- Raw-instruction unit tests cover every implemented instruction's
  distinguishing behavior (mutation-tested where it adds value).
- The `minimal-boot` kernel ELF boots under snemu and its greeting
  appears at the UART sink, in-process, in milliseconds.
- Pointing snemu at the full kernel reaches `mmu::enable` and halts
  with a clear diagnostic, not an opaque fault.

## Deliberately deferred to later milestones

- Sv39 page-table walk (milestone 2 — the cliff).
- CLINT (`mtime`/`mtimecmp`/`msip`), interrupts, the heartbeat (m2).
- virtio-console device + in-process `Frame` delivery (m2).
- A (full AMO/LR-SC set), F/D + `sstatus.FS` state machine (m3+).
- Second hart + the controllable interleaving scheduler (m3).
- U-mode + syscalls (m3).

---

## Milestone 2 — reaches heartbeat *(low granularity; promote to its own plan when started)*

Goal: pass `boot-reaches-heartbeat` and `heartbeat-cadence` against the
**real** kernel, with `Frame`s delivered to the harness in-process.

Rough shape:
- **Sv39 MMU** — page-table walk over the flat memory; the three
  address spaces the kernel uses (higher-half kernel VAs, linear-map,
  heap window). `satp` switch + `sfence.vma` semantics. This is the
  cliff from m1; expect it to be the bulk of the milestone. Drive it
  with the kernel's own trampoline + `unmap_identity` flow as the
  acceptance behavior; unit-test the walker against hand-built tables.
- **CLINT** — `mtime` advancing off the instruction-count clock,
  `mtimecmp` → timer interrupt, `msip` → software interrupt. Wire the
  timer into trap delivery so the heartbeat fires.
- **virtio-mmio console (device side)** — enough of the queue /
  descriptor-ring protocol that the kernel's existing driver drains TX.
  Then a **fast path**: hand `Frame` bytes straight to the harness
  in-process (no socket), while keeping a socket-compatible mode if the
  collector wants it.
- **Formatted output now works** (paging is on) — the "no formatted
  `println!` before `mmu::enable`" constraint lifts; good smoke signal.
- Acceptance: the two named scenarios pass against the real kernel via
  snemu, decoded frames matching the QEMU path.

## Milestone 3 — functional parity + concurrency *(low granularity)*

Goal: run the userspace / `init` scenarios; make the interleaving
scheduler reproduce a known cross-hart bug deterministically.

Rough shape:
- **Second hart** + the **controllable interleaving scheduler** (the
  per-hart shape from m1 pays off; round-robin for determinism, a
  PCT-style sampler for hunting). Choose the preemption-point
  granularity (every instruction vs only memory-access/AMO/fence vs
  yield boundaries) — the race-finding-power vs state-space knob.
- **A extension** complete (LR/SC + full AMO set), trivially correct
  under single-threaded interleaving.
- **U-mode + syscalls** — privilege transitions, `ecall` from U,
  sstatus SPP/SPIE, the trap path the kernel already implements.
- **F/D** + the `sstatus.FS` dirty-bit save/restore (co-evolved with a
  kernel FP-context feature) — the gate for Stitch's floats.
- Acceptance: userspace/`init` scenarios pass; the scheduler replays a
  seeded interleaving that reproduces (e.g.) a `TX_STAGING`-class
  ordering bug on demand — Heisenbug → fixed-seed regression test.

## Milestone 4 — measurement spine *(its own plan: [snemu-milestone-4-measurement.md](snemu-milestone-4-measurement.md))*

The load-bearing artifact: snemu observing itself, the QEMU baseline,
the workload taxonomy, and the nested overhead-factor methodology.
Everything after this (the JIT tiers) is measured against it. M3 is the
"working end-to-end, no JIT" line; M4 makes the measurement rigorous.

## Milestone 5 — JIT Tier 1: decode cache *(low granularity)*

Pre-decode each instruction into a struct, cache by guest PC, `match` on
it; skip re-decode on re-execution. ~2–4×. Pure data, no exec memory —
works on host *and* nested. Deterministic + instrumentation-transparent.
Deliverable + post: measured delta across the M4 taxonomy + the nested
per-class overhead profile (ALU-op cost should crater).

## Milestone 6 — JIT Tier 2: block formation + chaining *(low granularity)*

Group into basic blocks, cache decoded blocks by entry PC, link exits
directly (software chaining); optionally a `Vec` of handler fn-pointers
(threaded/closure). ~3–6×. Still data, not code; still nests. Watch the
interaction with the interleaving scheduler (chaining removes the
fine-grained yield points the race-finder wants — preempt at block
boundaries or emit explicit checks). Deliverable + post: measured delta
(dispatch cost should crater).

---

*Horizon (not planned): JIT Tier 3 native codegen — host-only fork
(W^X exec pages), off-host needs the SnitchOS `ExecMemory` capability.
Gated on a measured compute-bound need that Tiers 1–2 + the interleaving
scheduler don't solve. See `docs/snemu-design.md` Exploration notes.*
