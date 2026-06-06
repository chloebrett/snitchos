# SnitchOS — Lesson Plan

Generated from your quiz ratings in `concept-map.md`. Ordered by **dependency**,
not just by weakness: your three weakest areas (DMA, scheduling, heap) all rest
on the **virtual/physical address mental model**, so that comes early even
though you scored it OK.

Each lesson has:
- **Goal** — what you'll be able to explain/do afterwards.
- **Why now** — where it sits in the dependency chain.
- **Read** — real files, in order.
- **Do** — the toy exercise that cements it (where one exists).
- **Mastery check** — the question you should be able to answer cold.
- **Fills** — which `concept-map.md` rows this lesson targets.

Legend: 🧩 = has a toy crate to build/finish.

---

## Lesson 0 — Foundation patch-ups *(quick wins, ~30 min)*

**Goal.** Close the three small factual gaps the quiz surfaced so they stop
tripping you later.

**Why now.** These are cheap and everything references them.

**Read.**
1. `kernel/src/entry.S` — re-read the `.bss`-zeroing loop knowing now that
   `.bss` = zero-initialised statics that the binary *doesn't* store, so boot
   code must zero them or your `static`s start as garbage.
2. Top doc-comment of `kmain` in `kernel/src/main.rs` — the OpenSBI S-mode
   handoff (M-mode firmware → drops to S-mode → `a0`=hartid, `a1`=dtb).
3. `kernel/src/sbi.rs` — see `ecall` in action: S-mode asking M-mode for a
   favour. Note the `a7`/`a6` extension/function-id convention.

**Mastery check.** "Walk me from power-on to the first line of `kmain`, naming
the privilege level at each step." And: "If `entry.S` skipped the `.bss` loop,
what specifically breaks?"

**Fills.** 1: privilege levels, OpenSBI handoff. 2: why `.bss` zeroed.

---

## Lesson 1 — The address-space mental model 🧩 *(the keystone)*

**Goal.** Hold all four address spaces in your head at once and know, for any
pointer, *which lens it's in* and *how to convert*: identity, higher-half
(`KERNEL_OFFSET`), linear-map (`LINEAR_OFFSET`), heap window (`HEAP_VA_BASE`),
and raw physical.

**Why now.** DMA, heap growth, per-CPU, and the trampoline are all *corollaries*
of this. Get this rock-solid and three of your 0/1 areas largely fall out.

**Read.**
1. CLAUDE.md → "Memory layout, post v0.4 step 4" (the four-spaces list). This is
   the single highest-leverage paragraph in the repo for you right now.
2. `kernel-core/src/mmu.rs` — `va_to_pa`, `pa_to_kernel_va`, the offset consts,
   `leaf_pte`, and the `map` walker. Pure code, no `unsafe` — readable.
3. `kernel/src/main.rs` — the trampoline asm block, re-read with Q8's answer in
   mind ("move onto the new bridge before demolishing the old one").

**Do. 🧩 Build `toy-pagetable`** (I'll scaffold it next): implement an Sv39
3-level walk that turns a VA into a PA, including a huge-page leaf at an upper
level (the trick behind the 1 GiB linear map). Exercises:
- split a VA into `VPN[2]/VPN[1]/VPN[0]/offset`,
- walk tables level by level,
- detect a leaf PTE early and stop (huge page).

**Mastery check.** "I hand you a heap pointer and a `&static` pointer. For each:
which address space is it in, what does `va_to_pa` do to it, and which one is
safe to hand to a DMA device?"

**Fills.** Sv39 specifics, multi-level walk, PTE encoding, `satp`/`sfence`,
four address spaces, `va_to_pa`/`pa_to_kernel_va`, trampoline (deepen 2→4).

---

## Lesson 2 — Devices, MMIO & DMA 🧩 *(your lowest score)*

**Goal.** Explain why a device can't take a kernel pointer, and trace one
telemetry frame from a Rust struct to bytes the virtio device DMAs out.

**Why now.** Directly depends on Lesson 1's VA/PA distinction.

**Read.**
1. `kernel/src/uart.rs` — the simplest MMIO: a device register *is* a memory
   address; writing a byte there transmits it. No DMA, no rings.
2. `kernel/src/virtio_console.rs` — the descriptor ring (virtqueue), the four
   `va_to_pa` sites, and the `TX_STAGING` copy. Connect each to "the device has
   no MMU, it speaks physical."

**Do. 🧩 Build `toy-virtqueue`**: model a descriptor ring as plain structs;
implement "publish a buffer" (write descriptor, bump avail index) and have a
fake "device" consume it — using *offsets into a byte arena* to stand in for
physical addresses, so the VA/PA distinction is concrete.

**Mastery check.** "Why does staging through `TX_STAGING` fix the heap-buffer
case that a plain `va_to_pa` would corrupt?"

**Fills.** MMIO, NS16550A, virtqueue, DMA-needs-PA, `TX_STAGING` hazard (0→3+).

---

## Lesson 3 — Allocators: frame + heap 🧩 *(toy already built)*

**Goal.** Implement first-fit + coalescing and the bitmap scan from scratch;
then explain how the kernel heap grows on top of the frame allocator.

**Why now.** "Where new heap bytes come from" (Q16) needs Lesson 1 + the frame
allocator under your fingers.

**Do first. 🧩 Finish `learning/toy-allocator/`** — all three exercises
(`cargo test -p toy-allocator`). You've got the scaffold already.

**Read (after the toy is green).**
1. `kernel-core/src/frame.rs` — compare `Bitmap::alloc` to your `bitmap.rs`.
   Note the maintained `frames_free` counter (the O(1) trick from the OOM
   incident).
2. `kernel/src/heap.rs` + `kernel_core::heap::watermark_grow_decision` — the
   grow chain: heartbeat checks watermark → `extend` asks frame allocator for
   frames → `mmu::map`s them into the heap window → hands them to
   `linked_list_allocator`.

**Mastery check.** "An allocation fails. Trace every step from the failed
`alloc` to a successful one after a grow — who decides, who supplies frames, who
maps them?" And: "Why can't the grow happen *inside* `alloc`?"

**Fills.** bitmap/`trailing_zeros`, free-list, splitting/coalescing,
`GlobalAlloc`, watermark policy, heap-VA-vs-frames (raise the 1–2s to 3–4).

---

## Lesson 4 — Traps & the single-CPU race *(connect theory to system)*

**Goal.** Turn your strong memory-ordering *theory* into the *applied* picture:
why an ISR races the main thread on one hart, and the "defer out of the
interrupt" pattern.

**Why now.** Builds on the trap-frame answer you already nailed (Q9).

**Read.**
1. `kernel/src/trap.S` — the save/restore you described in Q9, now line by line.
2. `kernel/src/trap.rs` — `decode_scause`, the SSTC timer path
   (`time`/`stimecmp`), and **the memory-ordering block comment** (your Q15
   theory, applied: `TICK_PENDING` publishes `LAST_IRQ_DURATION`).
3. Re-read CLAUDE.md's "never emit telemetry from an ISR / inside `alloc`" —
   this is the re-entrant-deadlock you couldn't name in Q10.

**Mastery check.** "On one CPU, give me a concrete two-instruction interleaving
where the ISR corrupts main-thread state — and the design rule that prevents
it." And: "Why is `Relaxed` actually correct *here* despite the race?"

**Fills.** scause, timer IRQ, why-not-telemetry-in-ISR, relaxed-ISR handoff,
deadlock/re-entrancy (0–1 → 3).

---

## Lesson 5 — Scheduling & context switch 🧩 *(your 0 in Q12)*

**Goal.** Explain exactly which registers a context switch saves and *why only
those*, and how "task 0 IS `kmain`" works.

**Why now.** Needs the callee-saved/caller-saved ABI idea (Lesson 0/general)
and the stack/`sp` understanding from Lesson 1.

**Read.**
1. `kernel/src/sched.S` — the `switch` primitive. Map each saved register to
   "callee-saved, therefore must survive the call" (the Q12 answer).
2. `kernel-core/src/sched.rs` — the `Runqueue`.
3. `kernel/src/sched.rs` — `register_bare_task` ("main IS kmain"), `Box<Task>`
   for stable addresses, the per-task `SpanCursor`.

**Do. 🧩 Build `toy-scheduler`**: a runqueue + a *simulated* context switch
(model `TaskContext` as a struct, "switch" = swap which context is current) that
round-robins three tasks and proves each runs. No asm — the data-structure
logic is the lesson; the asm is just how it's spelled on real hardware.

**Mastery check.** "Why does `TaskContext` hold only `ra`, `sp`, `s0–s11` and
not all 32 registers?" And: "What makes `Box<Task>` load-bearing here?"

**Fills.** TaskContext/callee-saved, `switch` primitive, main-is-kmain, stable
addresses, per-task span cursor (0 → 3–4).

---

## Lesson 6 — Concurrency primitives & SMP plumbing

**Goal.** Understand the `kernel::sync` chokepoint and why per-CPU data uses
`tp`.

**Read.**
1. `kernel/src/sync.rs` — the `Mutex`/`Once` wrappers and *why they exist as a
   single chokepoint* (preempt-disable / IRQ-disable land here later).
2. `kernel/src/percpu.rs` — `PerCpu<T>` + `current_hartid()` via `tp` (the Q18
   answer: per-hart access with no shared global, no id lookup).

**Mastery check.** "Why route every lock through `kernel::sync` instead of using
`spin::Mutex` directly?" And: "Why `tp` instead of `GLOBAL[hartid]`?"

**Fills.** spinlocks/Mutex wrapper, `Once`, per-CPU/`tp` (1 → 3).

---

## Lesson 7 — Observability, end to end *(capstone; your strongest area)*

**Goal.** Light consolidation: trace one span from kernel to Grafana.

**Read (skim — you already score 3 here).**
1. `kernel-core/src/span.rs`, `intern.rs`, `preinit.rs` — span registry, intern
   table (Q14), pre-init buffering + flush ordering.
2. `protocol/src/lib.rs` — the `Frame` enum + postcard positional encoding (Q19).
3. `collector/` — frames → OTLP/Prometheus.

**Mastery check.** "Follow `span!(\"kernel.boot\")` all the way to a trace in
Tempo — every transformation in between."

**Fills.** span registry, pre-init buffering, collector (confirm the 3s, fill
the unprobed rows).

---

## Toy crates to build, in lesson order

| Lesson | Crate | Status |
|---|---|---|
| 3 | `toy-allocator` | ✅ built — go finish the exercises |
| 1 | `toy-pagetable` | ✅ scaffolded — 3 exercises (split_va, translate, map_4kib) |
| 2 | `toy-virtqueue` | ⏳ scaffold at Lesson 2 |
| 5 | `toy-scheduler` | ⏳ scaffold at Lesson 5 |

## Suggested cadence

Lesson 0 is a warm-up. Then the natural arc is **1 → 3 → 2 → 4 → 5 → 6 → 7**
(do the allocator toy early since it's already built and builds confidence).
Re-rate the relevant `concept-map.md` rows after each lesson so you can watch
the numbers climb.
