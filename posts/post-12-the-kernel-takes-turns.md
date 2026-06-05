# Post 12 — The kernel takes turns

- v0.5: kernel runs four threads — `main`, `idle`, `task_a`, `task_b` — cooperatively scheduled over one CPU. Context switching, per-task observability, and a Tempo trace view that shows scheduler decisions inline. ~1100 lines of code, one debugging dragon (`va_to_pa` mishandling heap stacks), and the moment "the asm worked first try" actually meant something.

## what we had vs what we need

- end of v0.4: one execution context. Boot → `kmain` → heartbeat loop → `wfi`. Every span, alloc, IRQ handler was tangled into a single thread.
- it is not enough for: multiple periodic kernel jobs (one heartbeat plus future driver workers), userspace (which is a thread that also has its own page table and runs in U-mode), or the first hard observability problem in the project — *a span starts on thread A, gets descheduled, A is resumed, the span ends. Meanwhile thread B opened its own spans in between. Whose spans go where in the trace?*
- v0.5 is the kernel restructuring that lets us answer that question. Not userspace yet — that's v0.6. Just kernel threads, sharing the kernel's address space, taking turns on the CPU.

## the SMP-shaped pre-factor

- before threading itself, a small but load-bearing refactor. **Every `spin::Mutex` in the kernel was wrapped behind `kernel::sync::Mutex`**, a single chokepoint type that's a thin no-op wrapper today and the home of preempt-disable + IRQ-disable hooks tomorrow. Six static `Mutex` sites + three `Once` sites migrated in one pass.
- a workspace `disallowed_types` clippy lint blocks raw `spin::Mutex` outside `kernel::sync`. Anyone reaching for the original gets a warning pointing at the wrapper.
- a `kernel::percpu::PerCpu<T>` + `current_hartid()` stub sets up the SMP-shaped access pattern. `MAX_HARTS = 1` and `current_hartid()` returns 0 today. When SMP arrives those two lines change and call sites stay put.
- **none of this ships any new behavior**. The wrappers are no-ops, the PerCpu stub stores into slot 0, the lint doesn't reject anything in `kernel::sync` itself. The value is in *the diff*: every kernel lock acquisition now goes through one function. When preempt-disable lands, it's one file's worth of change, not a survey of every lock site.
- this is the same trick as splitting the page-table-mutation API into P1 (primitive) + P2 (consumer) in v0.4 step 5. Get the surface shape right under the laboratory conditions of "nothing depends on the new behavior yet"; commit; *then* build on it.

## what a "thread" is here

- a kernel-mode execution context. Runs in S-mode, shares the kernel's address space, has its own stack and saved register state. Not "less privileged" — has full kernel privileges. The thread part just means "schedulable unit."
- four threads at boot:
  - **task 0 = `main`** — `kmain` itself, declared retroactively as task 0 via `register_bare_task("main", Running)`. Boot stack from `entry.S` becomes task 0's stack. Heartbeat work runs inside task 0's loop. Pleasing consequence: there's no "scheduler thread" sitting on top of the tasks — the kernel runs out of one of them, always.
  - **`idle`** — `loop { wfi; yield_now(); }`. Owns the `wfi` for the whole kernel. When everyone else has yielded, idle runs and sleeps until the next interrupt.
  - **`task_a`, `task_b`** — demo workers. Each opens a `task_x.tick` span, burns some CPU, yields, closes the span. `task_a` does it *across* a yield as a deliberate stress test (see "the spancursor footgun" below).
- a `Task` struct holds id, name, scheduler state, saved register context (`TaskContext`), stack (`Box<Stack>` — 16 KiB, 16-byte aligned for the RISC-V ABI), per-task `SpanCursor`, and per-task atomics (`cpu_time_ticks`, `runs`, and the pre-registered metric StringIds).
- `Vec<Box<Task>>` in a `static Mutex<Scheduler>`. The `Box<T>` is load-bearing: it guarantees the `Task` lives at a stable heap address even if the `Vec` reallocates. The asm switch hands raw pointers around; if the `Task` moved, those pointers would dangle.

## the asm

```
switch:
    sd ra,    0(a0)     # save callee-saved into `from`
    sd sp,    8(a0)
    sd s0,   16(a0)
    ...
    sd s11, 104(a0)

    ld ra,    0(a1)     # load callee-saved from `to`
    ld sp,    8(a1)
    ld s0,   16(a1)
    ...
    ld s11, 104(a1)
    ret
```

- 30 instructions, no fence, no CSR access. To both threads' code, this looks like a normal function call that took a long time.
- caller-saved (`a0`-`a7`, `t0`-`t6`) are untouched — the C ABI lets the compiler treat them as clobbered across any function call, and `switch` IS a call from both threads' perspective.
- callee-saved is 14 registers × 8 bytes = 112 bytes. The `TaskContext` struct is `#[repr(C)]` so the Rust-side layout matches the asm's byte offsets exactly.

### the smoke test that proved it worked

- before any spawn/yield machinery existed, I rigged a smoke: build a marker `TaskContext` whose `ra` points at a function that bumps a static counter, switch into it, marker switches back, kernel resumes.
- two `UnsafeCell<TaskContext>` statics (`SMOKE_MAIN_CTX`, `SMOKE_MARKER_CTX`) and a 16 KiB `Box<Stack>` for the marker. The `UnsafeCell` was necessary because the marker function re-enters the same critical section that called it — a `Mutex` would deadlock with itself.
- one boot, one integration scenario (`sched-context-switch-smoke`), counter == 1. Asm worked first try.
- the "first try" is genuinely earned by the design — the asm offsets match the `TaskContext` struct layout one-to-one, the `Box<Stack>` provides ABI-aligned stack memory, and the smoke isolates the asm contract from the scheduler's complexity. Nothing about the design depended on anything else also being right.

## yield_now

```rust
pub fn yield_now() {
    let t_entry = timestamp();
    let current_id = TaskId(CURRENT_TASK.load(Relaxed));
    let (current_ctx, next_ctx, next_id, next_cursor) = {
        let mut sched = SCHEDULER.lock();
        let Some(next_id) = sched.runqueue.pop_front() else { return; };
        // ... iterate task table once, capture both context pointers
        //     + cursor pointer for the incoming task, bump cpu_time
        //     for the outgoing, bump runs for the incoming ...
        sched.runqueue.push_back(current_id);
        CURRENT_TASK.store(next_id.0, Relaxed);
        CURRENT_SPAN_CURSOR.store(next_cursor, Relaxed);
        (current_ctx, next_ctx, next_id, next_cursor)
        // lock dropped here
    };
    CONTEXT_SWITCHES.fetch_add(1, Relaxed);
    emit_context_switch(current_id, next_id, SwitchReason::Yield);
    CURRENT_TASK_ENTRY_TICK.store(timestamp(), Relaxed);
    unsafe { switch(current_ctx, next_ctx) };
}
```

- the scheduler lock is dropped *before* the asm switch. The raw context pointers stay valid past the drop because `Box<Task>` heap addresses are stable, but anyone else (e.g. another hart in v0.7+) could mutate the runqueue while we're switching. That's fine — single-hart cooperative today.
- one of the four tasks does this; it doesn't matter which. There's no "scheduler thread" sitting on top of them. The scheduler is a *library*: the calling task does the bookkeeping work and the switch itself, all on its own stack.

## the va_to_pa debugging dragon

This is the moment the test "passed" first try, then immediately failed, and the fix turned out to be one line of v0.4 code I'd written months ago that didn't anticipate v0.5.

- first time I wired `yield_now` into the heartbeat loop, the test failed with corrupted Hello frames after the first ContextSwitch on the wire. Multiple "Hello" frames with garbage values. The pattern suggested the kernel was rebooting in a loop.
- added a `println!` after the yield to confirm main resumed. It did — `TASK_DEMO_LOOPS=1`. So the yield round-trip itself worked: switch into demo, demo bumps counter, demo yields back, main runs the line after the yield.
- so why did the wire look corrupted? Realized after a while: when demo emitted its `ContextSwitch` frame, the frame bytes were postcard-encoded into a 128-byte stack buffer. demo's stack lives in the v0.4 heap (`Box<Stack>` allocated from `heap::alloc`). So the buffer's VA was in the `HEAP_VA_BASE` range.
- `virtio_console::transmit()` translates the buffer's VA to a PA for the virtio descriptor by calling `mmu::va_to_pa()`. v0.4's `va_to_pa` only handles `KERNEL_OFFSET`-range VAs (kernel-image higher-half) and identity-range VAs:

  ```rust
  pub const fn va_to_pa(va: usize) -> usize {
      if va >= KERNEL_OFFSET { va - KERNEL_OFFSET } else { va }
  }
  ```

  Heap VAs are *less* than `KERNEL_OFFSET` (they're in the `HEAP_VA_BASE` range, which is a different higher-half slot). So `va_to_pa` for a heap VA returns the VA unchanged. The descriptor's "addr" field was a VA, not a PA. The device DMA'd random physical memory.
- fix: `virtio_console::send` now stages bytes through a static `TX_STAGING` buffer (lives in `.bss`, has a kernel-image VA, `va_to_pa` works on it). Copy into staging, transmit from staging. One static, ~30 lines of change.
- the bug had been *latent* the whole time. Every emit before v0.5 happened on the boot stack (kernel-image VA) or in static buffers. v0.5's per-task stacks were the first heap-allocated stacks, and the first time the bug could manifest. **The threading work was what finally exercised the path that exposed a v0.4 bug.**
- v0.4-style fix would be to make `va_to_pa` smarter — walk the page table to recover the PA. The staging buffer is the cheaper fix and avoids `va_to_pa` having to do a runtime walk on a hot path. Documented for v0.7+ when the cost of staging matters.

## the spancursor footgun

- step 2 split the global `SPAN_REGISTRY` into `SpanIds` (global id allocator, stays static) and `SpanCursor` (per-task innermost-span tracker). Each `Task` got a `span_cursor` field. But the kernel-side `tracing` module *kept using a static cursor* — the per-task fields were data without a consumer.
- this works fine as long as tasks open and close spans within a single execution slice. Open span A, do work, close span A, yield. The cursor's stack is balanced at yield time.
- it falls apart if a span survives a yield. Task A opens span X. Yields. Task B opens span Y. With the shared cursor: X is still on top of the cursor when Y opens, so Y's `parent = X`. Wrong. Y is task B's work; nothing about X is its parent.
- so the design's invariant became "discipline: don't yield while holding a span." Brittle. Easy to violate accidentally as the codebase grows. A latent bug that the existing tests couldn't see, because no task held a span across a yield.
- the proper fix landed in step 13.5: `CURRENT_SPAN_CURSOR: AtomicPtr<SpanCursor>` static, updated on every context switch by `yield_now`, read by `tracing::span_start` to pick the right cursor. The `Span` guard remembers which cursor it was opened on (stored as `*const SpanCursor`), so close pops the right stack even if the running task has changed.
- the integration test that pins it (`sched-span-survives-yield`) makes `task_a` deliberately hold its span open across a yield, then asserts the wire shows:
  1. SpanStart `task_a.tick` with `task_id = task_a` and **`parent = SpanId(0)`** — top-level, not parented to task_b's open spans.
  2. ContextSwitch leaving task_a.
  3. ContextSwitch returning to task_a.
  4. SpanEnd matching the original SpanStart's id.
- with shared cursor: (1)'s parent is whatever was on top of the global at open time. The test would fail.
- the `Box<Task>` choice (rather than `Vec<Task>` directly) is what makes this safe — the per-task cursor lives at a stable heap address, so the raw pointer stored in the `Span` guard stays valid even if the `Vec` reallocates.

## the wire format

three additions to `protocol::Frame`:

- **`ThreadRegister { id, name }`** — one per `spawn()`. Lets the collector resolve numeric task ids to human-readable names.
- **`ContextSwitch { from, to, t, reason }`** — emitted per yield. Reason is `Yield` for v0.5 cooperative; `Preempt` / `Blocked` / `Exit` are reserved variants for v0.5.x and v0.6+. Makes scheduler decisions first-class traceable events, not just a counter.
- **`SpanStart` gains `task_id: u32`** — every span attributable to its owning task. Wire-breaking change (postcard encodes by position), but kernel + collector ship as a pair.

collector arms in `OwnedFrame::from_borrowed`, OTLP `Span` proto gets `attributes` (tag 9), and `export` emits `thread.id` and `thread.name` per OTel semantic conventions. Tempo renders them in the trace detail view. **That's the post-angle:** a span starts in task A, gets descheduled, resumes in A, ends — all visible as one continuous span with attributes telling you whose work it represented and `ContextSwitch` events visible in the gap.

## the observability layer

scheduler metrics, all on the wire and in Grafana:

| metric | shape | what it tells you |
|---|---|---|
| `snitchos.sched.context_switches_total` | counter | cumulative yields |
| `snitchos.sched.yield_overhead_ticks` | histogram | time in `yield_now`'s bookkeeping (excludes the asm + time off-CPU) |
| `snitchos.sched.runqueue_depth` | gauge | how many tasks are `Ready` right now |
| `snitchos.sched.tasks_total` | gauge | how many tasks exist |
| `snitchos.task.<name>.cpu_time_ticks` | counter | per-task on-CPU time |
| `snitchos.task.<name>.runs_total` | counter | per-task scheduling count |

per-task metrics needed dynamic names, which our wire-format intern table doesn't naturally accommodate (it stores `&'static str`). The escape valve is `register_counter_owned(String)` — leaks the name into `'static` via `Box::leak`. Restricted to bounded-cardinality paths (one pair per task). Per-task leaks total ~200 bytes forever; fine.

The Grafana panels:

- **Thread timeline (CPU time share)** — stacked area of `rate(snitchos_task_*_cpu_time_ticks[30s])`. Wide bands show CPU shares. With idle and task_a/task_b doing very different amounts of work, idle dominates wallclock.
- **Active threads (idle excluded)** — same shape, drops idle. Now the task_a vs task_b ratio is visually obvious.
- **Context switches per second** — derivative of `context_switches_total`.
- **Yield overhead percentiles** — histogram quantiles. Currently low — single-digit microseconds. When SMP arrives and lock contention shows up, this is where it'll surface.
- **Per-thread scheduling rate**, **Runqueue depth**, **Tasks total**, **Cumulative context switches** — supporting context.

## what i learned

- **prefactoring pays off, again.** The `kernel::sync` chokepoint was 100 lines of wrapper code that shipped no new behavior. It made all of v0.5's scheduler work read against a clean lock API. When v0.5.x adds preempt-disable, all six lock sites are instantly safe. Same shape as v0.4 step 5's P1/P2 split — get the surface right in isolation, then build on it.
- **`Box<T>` for stable addressing is genuinely load-bearing for any system that uses raw pointers.** `Vec<Box<Task>>` lets the Vec reallocate freely while the Task allocations stay put. Raw `*mut TaskContext` and `*const SpanCursor` stay valid past lock drops. Without `Box`, those would invalidate on the first Vec push past capacity and we'd be debugging memory corruption for days.
- **passive in-task schedulers are simpler than separate scheduler threads.** No extra stack, no extra context switches, no extra synchronization. The "scheduler" is the runqueue data structure plus `yield_now()`. The function runs on whoever called it, manipulating shared state under one mutex. The cost is "cooperative only" — but cooperative is fine for v0.5, and the design works the same way when v0.5.x adds preemption (the timer IRQ handler will just call into the same `yield_now`-shaped function from its own context).
- **latent bugs in old code surface when new code exercises new paths.** `va_to_pa` had been wrong for heap VAs since v0.4 step 5 P2 — but nothing emitted from a heap-allocated stack until v0.5 step 7. The threading work was what *finally* exercised the path that exposed it. The fix is local (staging buffer) and documented; the lesson is that the time-to-bug-discovery isn't a function of how broken the code is, but of how many distinct execution shapes have walked through it.
- **the discipline-as-fix is a tell that the design is incomplete.** "Don't hold a span across a yield" is a rule you'd never remember. The proper fix (per-task cursor wiring) was ~50 LOC and an integration test. The discipline saved ~50 LOC for ~10 commits and would have cost ~500 LOC of debugging when someone violated it. Worth doing the proper thing earlier next time.
- **observability of scheduler decisions is the project's most distinctive contribution at this milestone.** Most teaching kernels treat threading as "you can run more than one thing." SnitchOS treats it as "scheduler decisions are first-class traceable events." `ContextSwitch` on the wire with `from`/`to`/`reason` makes that real. Tempo trace view shows the gap between SpanStart and SpanEnd with the context switches that happened inside — every kernel could emit this, almost none do.

## what's not done

- **preemption.** Cooperative only. A misbehaving task hangs the kernel forever; we trust everyone to call `yield_now` periodically. Preempt-disable + timer-driven scheduler tick lands in v0.5.x or v0.6 prep, with the `kernel::sync` chokepoint as the seam.
- **thread exit / reaping.** Tasks have `fn() -> !` entry signatures. They can't return. v0.5.x or v0.6 will need an exit path (yield, mark Exited, reaper thread collecting stacks + table entries).
- **blocking primitives.** No condvars, no mutexes-that-sleep. Spin locks only. Tasks that need to wait either spin or yield in a loop. Real blocking lands when a workload needs it.
- **stack guard pages.** Stacks are flat `Box<[u8; 16384]>`. An overflow corrupts adjacent heap silently. Guard pages need the v0.6 dedicated-stack-VA shape (per-task VA window mapped through the v0.4 step 5 `map()` primitive).
- **per-process address spaces.** Threads share the kernel address space. Userspace processes are v0.6.

## v0.5 status

| ✓ | thing |
|---|---|
| ✓ | `kernel::sync::{Mutex, Once}` chokepoint + `kernel::percpu::PerCpu<T>` stub + clippy lint |
| ✓ | `kernel_core::sched::{TaskId, TaskState, Runqueue}` (host-tested, 7 tests for the round-robin idiom) |
| ✓ | per-task `SpanCursor` (kernel-core data shape + kernel-side `CURRENT_SPAN_CURSOR` swap) |
| ✓ | wire format: `ThreadRegister`, `ContextSwitch{reason}`, `task_id` on `SpanStart` |
| ✓ | kernel-side scheduler: `Task`, `Scheduler`, `static SCHEDULER: Mutex`, task-id allocator |
| ✓ | context-switch asm (`sched.S`) + `TaskContext` (`#[repr(C)]`, byte offsets match asm) |
| ✓ | `spawn(name, entry)` + `register_bare_task("main")` + `yield_now()` |
| ✓ | `idle_entry` (wfi loop) + four threads at boot (main, idle, task_a, task_b) |
| ✓ | scheduler metrics: switches/sec, yield-overhead histogram, runqueue depth, tasks total |
| ✓ | per-task metrics: `task.<name>.cpu_time_ticks` + `runs_total` |
| ✓ | OTLP `thread.id` + `thread.name` attributes; Tempo trace view shows them |
| ✓ | Grafana: thread timeline (CPU time stacked), active threads (idle excluded), switches/sec, yield overhead percentiles, supporting stats |
| ✓ | 6 new integration scenarios: `sched-context-switch-smoke`, `sched-spawn-registers-thread`, `sched-yield-round-trips`, `sched-spans-carry-task-id`, `sched-context-switches-on-wire`, `sched-span-survives-yield` |
| ✓ | latent `va_to_pa` bug fixed: `virtio_console::send` stages bytes through a kernel-image-VA static buffer |

## what's next

- **v0.5.x (preemption + lock discipline).** Per-task preempt-disable counter wired through `kernel::sync::Mutex::lock` (the chokepoint earns its keep). Timer IRQ wired to invoke the scheduler. Real `Preempt`-reason ContextSwitches on the wire.
- **v0.6 (first userspace process).** Per-process page tables via `mmu::map`. U-mode entry. One ambient-authority syscall. Built deliberately wrong so v0.6b capabilities can feel the pain.
- **thread exit, reaping, blocking primitives, stack guard pages** — incremental as needed.
- **the talc A/B comparison** is still queued; the LLA fork that exposed `Heap::free_block_stats` is the precondition.
