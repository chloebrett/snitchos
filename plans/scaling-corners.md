# Scaling corners — issues to revisit at SMP / interrupts

Audit done at end of v0.1 (all of: kernel boot, virtio-console, tracing,
boot span tree, heartbeat loop). These are places where the v0.1 code
is correct on a single hart with no interrupts, but won't scale or will
break when SMP / interrupts arrive.

Living document — update or strike through as we address each one.

## Real corners (will hurt)

### 1. Global `CURRENT_SPAN` (correctness bug at SMP)

```rust
// kernel/src/tracing.rs
static CURRENT_SPAN: AtomicU64 = AtomicU64::new(0);
```

Today: one open-span-stack across the system. Fine on one hart.

On SMP: hart 0 opens a span, hart 1 reads `CURRENT_SPAN` and sees hart
0's id, **claims hart 0's span as its parent**. The span tree gets
cross-hart edges that don't reflect any real call relationship.

**Fix when SMP arrives (v0.6):** per-CPU `CURRENT_SPAN`. RISC-V has
the `tp` register reserved for per-hart pointers; wire it to a
`PerCpu<AtomicU64>` abstraction. The design doc already specifies the
shape ("per-CPU-partitioned `u64` counter") for span IDs — extend the
same treatment to CURRENT_SPAN. The `PerCpu<T>` chokepoint is already
in place from the v0.5 pre-SMP-sync prefactor; v0.6 makes it real.
See `plans/v0.6-smp-cooperative.md`.

### 2. Single TX descriptor slot in virtio-console

```rust
// kernel/src/virtio_console.rs
let desc_ptr = &raw mut TX_QUEUE.desc[0];  // always slot 0
```

Correct under SMP because `Mutex<usize>` around the console base
serializes the whole `transmit` call. But every hart's emit goes
through one slot, fully serialized.

**Fix at SMP time:** use the descriptor table as a real ring — multiple
descriptors in flight, free-list of slots, per-CPU emit buffers
draining to the shared TX queue. The design doc has the shape
("per-CPU rings, drained independently").

### 3. Locks and interrupts (deadlock at v0.3)

When interrupts come online, the timer fires mid-kernel-code. If the
interrupt handler emits a span:

```
hart 0 enters span_start("foo")
  → locks INTERN_TABLE
  → ...
  → TIMER INTERRUPT
  → handler calls span!("timer_irq")
  → locks INTERN_TABLE  ← DEADLOCK (spin::Mutex isn't reentrant)
```

**Fix at v0.3 (interrupts milestone):** either disable interrupts
around locks that IRQ handlers might also take, or never emit spans
from interrupt context (have IRQ handlers enqueue deferred events that
the normal kernel context drains).

The "disable interrupts in the critical section" pattern is the
standard kernel idiom. Probably want a `local_irq_save` / `restore`
RAII guard that wraps lock acquisition.

## Correct but doesn't scale

Things that work on SMP but serialize hard:

| place | what happens | severity |
|---|---|---|
| `console::UART` mutex | every println across all harts serializes | fine — kernel println is rare |
| Intern table mutex | first-use registrations serialize | fine — one-shot per name |
| `SPAN_ID_COUNTER` atomic | `fetch_add` contention-free, but cache traffic | doc-recommended fix: per-CPU partition |
| Heartbeat loop assumes 1 heartbeat | needs N independent or one designated hart | trivially refactorable |
| `spin::Mutex` vs sleeping mutex | wastes cycles under contention | wait for scheduler (v0.5+) |
| TLB shootdown is per-PTE, unbatched | `mmu::map/unmap` does one IPI roundtrip per PTE; multi-page workloads pay N × IPI latency | needs `mmu_gather`-style batching: queue up VAs touched, broadcast once at the end. Already burned us once — heap-oom went red the moment shootdown wasn't filtered to remap/unmap only. |
| No ASID tagging on the TLB | every context switch needs a full shootdown to evict the outgoing process's entries | add ASID allocation + tag PTEs + `sfence.vma rs1=va, rs2=asid` to scope shootdowns per address space. Big win once userspace lands and context switches start crossing page tables (v0.7+). Also enables PCID-style "TLB doesn't have to flush on switch." |

## Not corners

These looked suspicious but checked out:

- **`static mut TX_QUEUE` / `RX_QUEUE`.** Only accessed via `transmit`,
  which is only called via `send`, which is gated by `Mutex<usize>`.
  Serialization preserved as long as nobody bypasses `send`.
- **Pre-init buffer.** Single-writer (boot hart only). Other harts
  haven't been brought up yet during pre-init. v0.6 preserves this
  invariant: secondaries come up post-MMU (after `kmain` has already
  drained the pre-init buffer), so the "single-writer by *design*"
  property holds for free.
- **Panic recursion guard `PANICKING: AtomicBool`.** Global, so "any
  hart panics → whole system panics." Correct v0.1 behavior; we don't
  have fault isolation yet.
- **UART hardcoded `0x10000000` in panic handler.** Platform-portability
  issue, not an SMP one. Already documented as a known weakness on
  `console::UART`.

## Lock-acquisition graph (preserve this)

Current direction of all lock-pair acquisitions:

```
INTERN_TABLE   ──→ virtio_console::CONSOLE
PRE_INIT_BUFFER ─→ virtio_console::CONSOLE   (drops lock before emit)
console::UART     (alone)
virtio_console::CONSOLE   (alone)
```

Nothing currently goes `CONSOLE → INTERN` or `CONSOLE → PRE_INIT_BUFFER`.
**Hold this line.** Any future code that takes locks in the opposite
order would deadlock under contention. Document each new lock-pair as
it's added.

## Summary table

| issue | severity at SMP | when to fix |
|---|---|---|
| Global `CURRENT_SPAN` | **breaks span tree correctness** | v0.6 (SMP) |
| Single TX descriptor slot | slow, not incorrect | deferred past v0.6 — correctness fine under multi-hart, perf follow-up |
| Locks vs interrupts | deadlock if not handled | v0.3 (interrupts) — addressed |
| Per-CPU span ID partition | cache traffic only | v0.6 (SMP), folds into `PerCpu` lift |
| spin::Mutex vs sleeping | wastes cycles | when blocking primitives exist (post-v0.8 IPC) |
