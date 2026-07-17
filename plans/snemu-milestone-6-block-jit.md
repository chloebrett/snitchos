# snemu M6 — the block JIT (reified-IR, backend A first)

**Status: SHIPPED.** Backend A (the portable block JIT, `snemu/src/block.rs`)
landed and is at parity with the interpreter — which is this plan's whole scope.
Backend B (native AArch64 codegen, `snemu/src/jit.rs`) is the "later, host-only
milestone" flagged below and is **still in progress**: it is oracle-clean
(byte-identical instret to the interpreter) but not yet faster than A, because
the hot scheduler/spin blocks contain loads/stores and fall back to A. The next
lever there is memory-op codegen.

## Where this sits

The meta-loop's speed levers, in order shipped: M5 decode cache (skip re-fetch/
expand), tier-0.5 native memops (collapse `memset`/`memcpy`). The profiler says the
makespan pole is now the **scheduler + spin-wait blocks** — ordinary
integer/branch/load-store instructions in `prepare_switch`, the run-queue, the
heartbeat's busy-yield. Those are exactly what a block JIT speeds and the memop
helper can't touch. M6 builds it.

**The unit of work changes from one instruction to a basic block** (a straight run
ending at a branch/jump/trap). Compile each hot block once, cache it by PC, re-run
the compiled form. Tiering: interpret cold code, compile hot blocks, keep a
`PC → block` map. The tier-0.5 memop dispatch is already that map with two
hand-written entries; M6 generalises "block" from hand-written to compiled.

## The real lever (not dispatch — per-instruction overhead)

M5 found the dominant per-step cost was **not** decode — it was the interrupt
probe: `step` calls `pending_interrupt()` (reading `sip`/`sie`/`sstatus`) **twice
per instruction**, plus the idle check, the decode-cache probe, and a `pc`
read/advance. Over a straight block none of that needs to happen per instruction:

- **Check interrupts once per block**, at block entry, not per instruction. (An
  interrupt is only *taken* at an instruction boundary anyway; within a
  side-effect-free straight run, delivering it at block entry vs mid-block is
  observably identical because nothing external changes mid-block — the clock
  advances but no new interrupt source arrives that the block itself created.
  Precise rule in "Correctness", below.)
- **Advance `pc` once** at block exit (the interpreter writes it every instruction).
- **Keep hot guest registers in host locals** across the block instead of array
  load/store per op.

That amortisation — especially hoisting the interrupt probe out of the inner loop —
is where most of M6's win lives. The codegen *technology* (backend A vs B) matters
less than moving these overheads to block granularity. So even backend A (a
portable IR interpreter) captures the bulk of the gain.

## Two backends, one frontend — and why the IR is the whole design

A JIT is a frontend (guest bytes → IR) and a backend (IR → execution). The frontend
is **identical** for both backends we care about:

```
guest bytes
   │  block discovery (walk from an entry PC to the block-ending instruction)
   │  decode + validate each instruction (reuses the M5 fetch/expand path)
   ▼
BLOCK IR   —   Vec<Op>, a reified data enum: opcode + resolved operands
   │                                  │
   ▼ Backend A (portable, ship now)   ▼ Backend B (native, later, host-only)
 walk the IR:                        lower each Op to a machine-code stencil
 for op in block { op.exec(hart) }   (copy-and-patch), execute the buffer;
 + block-level amortisation          fall back to A per-Op it can't emit
```

Everything above the IR — block discovery, the `PC → block` cache, tiering,
**invalidation** — is written once and shared. **B is just a second backend over
the same IR.** That is only true if the IR is *reified inspectable data*, not opaque
closures (`Box<dyn Fn>`): a closure can't be lowered to native code. So:

> **Design decision (locked): the block IR is a `Vec<Op>` of plain data — a reified
> enum with extracted operands — never closures.** Each `Op` is a *simple,
> stencil-able* operation (one machine-code snippet's worth), so backend B can map
> `Op → stencil` later without reshaping the IR.

This costs backend A almost nothing and is the entire seam that makes B an add-on
rather than a rewrite. **We build A now; the IR is specified for both.**

## Backend A vs B (for the record — B is a *later* milestone)

| | A: reified-IR interpreter | B: copy-and-patch native |
|---|---|---|
| runs in | **host + wasm/browser** | host only |
| `unsafe` | none | yes (execute generated mem) |
| deps | none | small (build-time stencil gen) |
| speed | modest–good (block amortisation) | high (near-native) |
| serves | the **browser** path + the oracle tier | fast **tests + on-host terminal** |

A ships first because it is the only option that runs in the browser (a committed
future target) and it is the correct-by-construction oracle tier. B is a deferred,
host-only accelerator for the compute tail (the `snemu-itest` Stitch tree-walker,
OOM scenarios) — added behind `cfg(not(wasm))` + a runtime flag, **falling back to A
for any Op it doesn't emit**, so it never needs full ISA coverage and stays
low-risk. B is out of scope for this plan beyond keeping the IR ready for it.

## Correctness — the oracle discipline (non-negotiable)

M6 is an optimisation that must be **byte-identical** to the pure interpreter:
same instret, same telemetry, same faults, same timing. Same discipline as the
decode cache / idle-skip / native-ops:

- **On/off flag** (`Machine::set_block_jit`, default OFF). OFF is the oracle; a run
  with it ON must match one with it OFF, proven by `snemu-itest` (110/110 + guest
  instret identical) and a `--verify-jit` A/B taxonomy.
- **Block boundaries respect traps.** A block ends at any instruction that can
  change control flow or trap: branches, jumps, `ecall`/`ebreak`, `sret`, `wfi`,
  `sfence`, CSR writes to `satp`/`sie`/`sstatus`, and **any load/store/AMO** (which
  can page-fault). A faulting op inside the block must trap exactly as the
  interpreter would — so either end blocks at every memory op, or (better) have the
  IR op for a memory access perform the same `translate_or_trap` and bail out of the
  block on a fault, leaving `pc` at the faulting instruction. Start by ending blocks
  at memory ops (simplest, correct); fuse later if the profiler wants it.
- **Interrupt hoist is only valid across a straight run.** Deliver a pending
  interrupt at block *entry*; within the block, the only clock source is the block's
  own retirement, and an armed timer that would fire mid-block is handled by
  **charging the block's instret and re-checking at the next block entry** — the
  same one-boundary-late delivery the interpreter already tolerates (it checks at
  instruction boundaries; a block is a coarser boundary). Bound block length (e.g.
  ≤ 64 ops) so a timer can never be more than a block late — and verify the A/B
  stays identical at that bound. **If any scenario diverges, shorten the bound or
  re-check interrupts per memory-op.** The A/B is the proof.

- **Invalidation rides the guest's TLB contract**, exactly like the decode cache:
  flush the `PC → block` cache on `satp` write and `sfence.vma` (O(1) epoch bump).
  Self-modifying code without an `sfence` would go stale — the kernel doesn't do it,
  and the A/B would catch it.

## Snapshot safety

The block cache **must not** enter the snapshot. It is a pure function of the
immutable kernel binary and rebuildable, so a cloned `Machine` starts with a cold
cache that re-warms — the "drop-and-rebuild" story the decode cache already uses.
Concretely: either exclude the cache from `Clone` (rebuild lazily) or clone it as
plain data (the IR is `Vec<Op>`, trivially `Clone`). The decode cache clones as data
today; match that. No executable memory in backend A means no snapshot hazard.

## Increments (each TDD, oracle-verified, green throughout)

1. **Block IR + a hand-built executor (no frontend yet).** Define `enum Op` (reified,
   stencil-able) and `Block { ops: Vec<Op>, ... }`, and `Block::exec(&mut Hart, &mut
   Bus)` that walks the ops. Unit-test: a hand-authored 3-op block (two ALU + a
   branch) executes to the identical register/pc state as the interpreter running
   the same raw instructions. This nails the IR shape — the seam — before any
   discovery machinery.
2. **Block discovery (frontend).** `compile_block(entry_pc)` walks from `entry_pc`,
   decoding via the existing fetch/expand path, appending `Op`s until a
   block-ending instruction, returning a `Block`. Test: a straight run of N ALU ops
   then a branch produces an N+1-op block ending in the branch; a leading memory op
   ends the block immediately (conservative boundary rule).
3. **`PC → block` cache + tiering + invalidation.** A per-hart block cache (mirror
   `DecodeCache`: direct-mapped or a small map, epoch-flush on `satp`/`sfence`).
   Hotness counter so only re-executed entry PCs compile. `Hart::step` (JIT on):
   if the current PC starts a cached block, run it; else interpret (and bump the
   counter; compile on threshold). Test: flush drops blocks; a hot PC compiles once
   and re-runs.
4. **Block-level amortisation.** Hoist the interrupt/idle check to block entry;
   advance `pc` once at exit; (optionally) cache the touched registers in locals.
   Test: instret + final state identical to per-instruction interpretation across a
   loop; hits > 0.
5. **Flag + full oracle A/B.** `set_block_jit` (default OFF) on `Hart`/`Cpu`/
   `Machine`; `snemu-itest --block-jit` and `snemu-bench --verify-jit`. Gate: full
   `snemu-itest` **byte-identical** guest instret + 110/110 on↔off, and a measured
   MIPS / makespan delta vs the `smp-tlb`/scheduler pole. Ship only when green.

## Measurement

Report against the pole the profiler named, not just aggregate MIPS: the scheduler
(`prepare_switch`, run-queue) and spin-wait blocks. Expect the win to concentrate
there (the interrupt-hoist + pc-amortisation applies to every block, uniformly, like
M5's decode cache). Compare debug and release; snemu is opt-3 in every profile
(root `Cargo.toml` override), so there's no debug/release inversion to wait for.

## Non-goals

- Backend B (native copy-and-patch) — a later, host-only milestone; this plan only
  keeps the IR ready for it.
- Cross-block optimisation / superblocks / trace compilation — single basic blocks
  first.
- Fusing memory ops into blocks — start by ending blocks at memory ops; fuse only if
  measured to matter.
- Relaxed memory / any change to the SC model.
- Register allocation beyond keeping a block's touched registers in locals.
