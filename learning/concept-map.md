# SnitchOS — Concept Map & Self-Rating

This is the backbone of your learning track. It lists every major conceptual
area in the kernel, broken into sub-topics, each grounded in **real files** in
this repo. You rate yourself 0–5 per sub-topic; we fill the ratings in via
quiz, then build a lesson plan that attacks the weak spots.

## Rating scale

| Score | Meaning |
|---|---|
| **0** | Never heard of it / couldn't define it. |
| **1** | Can define the term, but couldn't explain the mechanics. |
| **2** | Understand the concept, fuzzy on the details. |
| **3** | Solid conceptual grasp — could explain it to someone else. |
| **4** | Could implement it from scratch with a reference open. |
| **5** | Could implement it from scratch *and* teach it. |

Ratings start blank (`—`). The "Quiz" column tracks whether we've probed it yet.

---

## 1. RISC-V architecture & the privilege model
*Where the CPU hands control to us and what state it gives us.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| Privilege levels (M / S / U), why we run in S-mode | `main.rs` kmain docs | 2 | ☑ |
| OpenSBI handoff contract (hartid, dtb ptr, MMU off, IRQs off) | `kmain` signature | 2 | ☑ |
| Harts vs cores; what `_hart_id` means | `percpu.rs` | 3 | ☑ |
| CSRs: what they are, how you read/write them | `asm!("csrr/csrw")` everywhere | — | ☐ |
| Key CSRs: `satp`, `stvec`, `sepc`, `scause`, `sstatus`, `stimecmp`, `time` | `mmu.rs`, `trap.rs` | — | ☐ |

## 2. Boot & linking
*From `_start` to `kmain`, and why the addresses are what they are.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| The boot stub: set `sp`, zero `.bss`, call `kmain` | `entry.S` | 3 | ☑ |
| Why `.bss` must be zeroed before Rust runs | `entry.S` | 1 | ☑ |
| Linker script: ORIGIN, sections, `__stack_top`/`__bss_*` symbols | `kernel/linker.ld` | — | ☐ |
| Higher-half linking (kernel at `0xffffffff80200000+`) | CLAUDE.md memory layout | 3 | ☑ |
| Code models: `medlow` vs `medany`, PC-relative addressing | `.cargo/config.toml` | — | ☐ |
| Why no formatted `println!` before `mmu::enable` | `kmain` comments | — | ☐ |

## 3. Virtual memory & the MMU (Sv39)
*The single richest area. Three address spaces, one page table.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| What a page table *is*; VA → PA translation | `kernel-core/src/mmu.rs` | 3 | ☑ |
| Sv39 specifics: 39-bit VA, 3 levels, VPN/offset split | `kernel-core/src/mmu.rs` | 3 | ☑ |
| PTE encoding: PPN + permission bits (V/R/W/X/G/U/A/D) | `leaf_pte`, `PtePerms` | — | ☐ |
| The multi-level walk (and huge-page leaves) | `core_mmu::map` | 2 | ☑ |
| `satp`: turning translation on; the mode field | `mmu::enable` | — | ☐ |
| `sfence.vma`: why & when you flush the TLB | `kernel::mmu::map` | — | ☐ |
| The four address spaces: identity / higher-half / linear / heap | CLAUDE.md "Memory layout" | — | ☐ |
| The trampoline: jumping PC + sp to higher-half | `kmain` asm block | 2 | ☑ |
| `va_to_pa` / `pa_to_kernel_va`: which lens, when | `mmu.rs` | — | ☐ |
| Tearing down the identity map (`unmap_identity`) | `mmu.rs` | — | ☐ |
| **Toy:** `toy-pagetable` (planned) | `learning/` | — | ☐ |

## 4. Physical memory: the frame allocator
*Hands out 4 KiB physical frames.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| Bitmap allocator: 1 bit per frame, free=1 convention | `kernel-core/src/frame.rs` | 2 | ☑ |
| First-free via `trailing_zeros` (O(words)) | `Bitmap::alloc` | 2 | ☑ |
| Maintaining `frames_free` for O(1) empty-check | `set_bit_tracked` | — | ☐ |
| Contiguous allocation & fragmentation | `alloc_contiguous` | — | ☐ |
| Reserving the kernel image (the `va_to_pa(&sym)` trick) | CLAUDE.md gotchas | — | ☐ |
| **Toy:** `toy-allocator` (free-list + bitmap) | `learning/toy-allocator/` | — | ☐ |

## 5. The kernel heap
*`Box`/`Vec` for the kernel.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| `#[global_allocator]` & the `GlobalAlloc` trait | `kernel/src/heap.rs` | — | ☐ |
| Free-list allocation (linked_list_allocator) | `vendor/linked_list_allocator` | — | ☐ |
| Growing on demand: the watermark policy | `kernel_core::heap::watermark_grow_decision` | 1 | ☑ |
| Heap VA window vs scattered backing frames | CLAUDE.md memory layout | 2 | ☑ |
| Re-entrancy: never emit telemetry inside `alloc` | CLAUDE.md gotchas | — | ☐ |
| **Toy:** alloc free-list (shared with `toy-allocator`) | `learning/toy-allocator/` | — | ☐ |

## 6. Traps & interrupts
*How the CPU interrupts us, and how we get back.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| Trap vs interrupt vs exception vs ecall | `kernel-core/src/trap.rs` | — | ☐ |
| `stvec` & the trap entry path (`trap_entry`) | `trap.S` | — | ☐ |
| Saving/restoring the `TrapFrame` (GPRs + sepc + sstatus) | `trap.S` | 3 | ☑ |
| `scause` decoding (interrupt bit + cause code) | `decode_scause` | — | ☐ |
| Timer interrupts via SSTC (`time` / `stimecmp`) | `trap.rs` `SstcClock` | — | ☐ |
| Software interrupts / IPIs (SSIP) | `ipi.rs` | — | ☐ |
| Why you can't emit telemetry from an ISR | `trap.rs` comments | — | ☐ |

## 7. Concurrency & synchronization
*Even single-hart, the ISR races the main thread.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| Memory ordering: Relaxed / Acquire / Release | `trap.rs` ordering note | 3 | ☑ |
| Why same-hart ISR handoff can be `Relaxed` | `trap.rs` block comment | 0 | ☑ |
| Spinlocks & the `Mutex` wrapper chokepoint | `kernel/src/sync.rs` | — | ☐ |
| `Once` / lazy init | `sync.rs` | — | ☐ |
| Per-CPU storage & the `tp` register | `percpu.rs` | 1 | ☑ |
| Deadlock / re-entrancy patterns (deferred drain) | CLAUDE.md gotchas | 1 | ☑ |

## 8. Scheduling & context switching
*Cooperative round-robin; no scheduler thread.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| Cooperative vs preemptive scheduling | `kernel-core/src/sched.rs` | 3 | ☑ |
| The runqueue data structure | `Runqueue` | — | ☐ |
| `TaskContext` & callee-saved registers | `sched.S` | 0 | ☑ |
| The `switch` asm primitive (save → pick → load → ret) | `sched.S` | — | ☐ |
| "task 0 IS kmain" — `register_bare_task` | `sched.rs` | — | ☐ |
| Stable stack addresses (`Box<Task>`, raw ptrs past mutex drop) | CLAUDE.md scheduler | — | ☐ |
| Per-task span cursor surviving a yield | `CURRENT_SPAN_CURSOR` | — | ☐ |
| **Toy:** `toy-scheduler` (planned) | `learning/` | — | ☐ |

## 9. Devices, MMIO & DMA
*Talking to hardware.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| MMIO: device registers as memory addresses | `uart.rs` | — | ☐ |
| NS16550A UART (the human-readable channel) | `uart.rs` | — | ☐ |
| virtio-console & the descriptor ring (virtqueue) | `virtio_console.rs` | — | ☐ |
| DMA: why devices need **physical** addresses | CLAUDE.md gotchas | 0 | ☑ |
| The `TX_STAGING` buffer & the heap-VA hazard | CLAUDE.md scheduler | 0 | ☑ |
| **Toy:** `toy-virtqueue` (planned) | `learning/` | — | ☐ |

## 10. Observability (the project's whole point)
*Spans → frames → wire → collector.*

| Sub-topic | Grounded in | Rating | Quiz |
|---|---|---|---|
| Spans & the span registry | `kernel-core/src/span.rs` | — | ☐ |
| The intern table (string → id) | `kernel-core/src/intern.rs` | 3 | ☑ |
| `Frame` enum + postcard positional encoding | `protocol/src/lib.rs` | 3 | ☑ |
| Pre-init buffering & flush ordering | `kernel-core/src/preinit.rs` | — | ☐ |
| Two channels: UART log vs virtio frames | CLAUDE.md telemetry | 3 | ☑ |
| Collector: frames → OTLP/Prometheus | `collector/` | — | ☐ |

---

## Score summary (fill as we quiz)

*(Avg = mean of rows probed so far; many rows still unrated and get assessed live during the lessons.)*

| Area | Avg (probed) | Priority |
|---|---|---|
| 9. Devices / MMIO / DMA | 0.0 | 🔴 Highest |
| 7. Concurrency | 1.25 | 🔴 High (load-bearing for SMP) |
| 8. Scheduling | 1.5 | 🔴 High |
| 5. Kernel heap | 1.5 | 🟠 High |
| 4. Frame allocator | 2.0 | 🟠 Medium (toy covers it) |
| 1. RISC-V & privilege | 2.3 | 🟡 Medium |
| 2. Boot & linking | 2.3 | 🟡 Medium (`.bss` quick fix) |
| 3. Virtual memory / MMU | 2.5 | 🟠 High (richest area; deepen) |
| 6. Traps & interrupts | 3.0* | 🟡 Medium (mostly unprobed) |
| 10. Observability | 3.0 | 🟢 Low (your strongest) |

**Headline:** your *conceptual/high-level* understanding is good (3s in observability, memory-ordering theory, what-things-are-for). The gaps cluster in **applied mechanics** — DMA, context-switch registers, per-CPU, heap growth — and in **connecting theory to the running system** (you knew memory ordering cold but not the single-CPU ISR race it solves). The lesson plan targets that.

> **Lesson plan** lands in `learning/lesson-plan.md` once ratings exist.
