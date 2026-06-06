# learning/

A self-contained track for understanding the SnitchOS kernel. Three moving parts:

1. **`concept-map.md`** — every conceptual area, broken into sub-topics, each
   tied to a real file in the repo. You rate yourself 0–5 per sub-topic.
2. **`lesson-plan.md`** *(generated after the quiz)* — an ordered curriculum
   that attacks your weak spots, using real code from the kernel.
3. **`toy-*/` crates** — standalone, host-runnable Rust crates that isolate one
   kernel concept (the allocator, page-table walk, scheduler…) so you can poke
   at it without the rest of the OS in the way. Critical pieces are left as
   **exercises** you implement to make failing tests pass (TDD).

## How the toy crates work

Each toy crate is a normal `cargo` project (host target, `std` allowed — these
are *learning aids*, not kernel code). They ship with:

- A working scaffold + data structures.
- A full test suite that **fails** because the core functions are `todo!()`.
- An `EXERCISES.md` describing each exercise, the underlying concept, and the
  exact lines in the real kernel it maps to.

Workflow:

```bash
cd learning
cargo test -p toy-allocator      # watch it fail at the todo!()s
# ...implement the exercise in src/lib.rs...
cargo test -p toy-allocator      # green = you understand the algorithm
```

This is its own cargo workspace (see `learning/Cargo.toml`), deliberately
**excluded** from the root kernel workspace, so it never touches the
`no_std` / `riscv64` build.

## Current toys

| Crate | Concept | Maps to | Status |
|---|---|---|---|
| `toy-allocator` | free-list / bitmap allocation, splitting, coalescing | `kernel-core/src/frame.rs`, `vendor/linked_list_allocator` | ✅ scaffolded |
| `toy-pagetable` | Sv39 multi-level VA→PA walk | `kernel-core/src/mmu.rs` | ⏳ planned |
| `toy-scheduler` | round-robin runqueue + context-switch model | `kernel-core/src/sched.rs`, `kernel/src/sched.S` | ⏳ planned |
| `toy-virtqueue` | descriptor ring / DMA addressing | `kernel/src/virtio_console.rs` | ⏳ planned |

We'll build the planned toys as the lessons reach them.
