# Post 11 — Boxes

- v0.4 steps 4 + 5: `Box`, `Vec`, `String`, `BTreeMap` work inside the kernel. the heap starts at 4 MiB, grows on demand under a watermark policy, ceilings at 1 GiB. the more interesting half of the work isn't the heap itself — it's `kernel::mmu::map(va, pa, perms)`, the first runtime page-table mutation in this kernel's life. ~700 lines of code, one forked dependency, one quietly satisfying boot.

## the gap step 3 left

- step 3 (post 10) gave us `frame::alloc()` — hand out 4 KiB physical pages. that's enough to back page tables, fixed-size structures, anything page-shaped.
- it is not enough for `Vec<Span>` holding 37 spans (148-ish bytes), a `String` built character by character, or a `BTreeMap<InternId, &str>` of mostly-small nodes. those want a *sub-page* allocator carving arbitrary `(size, align)` requests out of a bigger region.
- that sub-page allocator is "the heap." `frame::alloc()` is the floor; the heap turns that floor into furniture.

## step 4: the simplest heap that works

- `linked_list_allocator` 0.10. first-fit, single contiguous region, ~300 LOC. wired behind `#[global_allocator]`. fragmentation isn't a real workload concern yet; the simplest thing wins.
- initial region strategy was the embarrassing one. `frame::alloc()` returns the lowest-indexed free frame, and the bitmap right after `frame::init` is one big run of free bits above the kernel image. so calling it N times in a row returns N contiguous PAs. add a `Bitmap::alloc_contiguous(n)` to pin that as a contract instead of relying on the coincidence, hand the contiguous run's linear-map VA to `linked_list_allocator::init`, done.
- 4 MiB heap. fixed. the whole region lives at `pa_to_kernel_va(first_frame_pa)+` inside the existing 1 GiB linear-map leaf — same region the frame allocator's dereferences go through. zero new page-table code. the kernel-heap version of "the dumb approach that actually works."

### the heap smoke

- `Vec::with_capacity(256)` + push + drop, once per heartbeat. proves the heap is live; `bytes_used` flickers up to 256 and back to 0 on the gauge. costs nothing.
- under a `heap-oom` cargo feature, replace the smoke with a per-heartbeat leak via `Vec::try_reserve_exact(4096)` + `core::mem::forget` — the `try_*` form returns `Err` on OOM instead of panicking through `alloc_error_handler`, and the underlying null-return from `GlobalAlloc::alloc` still bumps `alloc_failed_total`. exhausts in ~4 heartbeats with a visible decay curve. mirrors `frame-allocator-oom` exactly.

### the deferred-emission deadlock you must not write

- `GlobalAlloc::alloc` cannot emit a telemetry frame. the virtio TX path takes a `Mutex`. emitting a string on first use registers through the intern table which itself locks. if anything reachable from inside `alloc` tries to allocate again — instant deadlock through the lock chain.
- so the alloc path bumps a `AtomicU64` and the heartbeat thread reads it and emits the metric. same shape as the IRQ duration counter, same shape as frame stats. **anything you'd be tempted to instrument from inside a global-allocator path, write to an atomic and drain elsewhere.** this is the deepest piece of accumulated SnitchOS wisdom and it keeps paying.

## step 5 — the real story

- step 4's heap is fixed at 4 MiB. growing it means either finding *another* contiguous physical run (which gets harder as fragmentation builds) or building the thing every real kernel has: a function that mutates the live page table at runtime. `map(va, pa, perms)`.
- this is the first time this kernel changes its translation after boot. through v0.4 step 2 the page table was constructed once in `mmu::enable` and only ever cleared by `unmap_identity`. now it grows.

### shape of `map`

- input: virtual address, physical address, permission flags. output: a leaf PTE installed in the right level-0 table, intermediate tables allocated on demand, TLB invalidated for the VA.
- the walk: split VA into `(vpn2, vpn1, vpn0, offset)`. at each level, either follow an existing non-leaf PTE down or allocate a fresh 4 KiB zeroed frame, write a non-leaf PTE pointing at it, descend. at level 0, write the leaf PTE. `sfence.vma vaddr` to flush the TLB for that page.
- ~50 LOC of walk logic. 11 host tests covering: empty root, intermediate reuse, `AlreadyMapped` at every level, `OutOfFrames` partway through, perms propagation, A/D bits set, PPN bit layout.

### the kernel-core forbid kept us honest

- first cut threaded `*mut PageTable` raw pointers through the walk. fast, simple, and immediately rejected — `kernel-core/src/lib.rs` has `#![forbid(unsafe_code)]`, and the whole point of kernel-core is "pure logic, host-buildable, host-testable."
- the redesign: the walk takes a `PtMem` trait with `alloc_zeroed_table`, `read_entry`, `write_entry`. it threads physical addresses, never pointers. the kernel-side `KernelPtMem` impl owns the `unsafe { ptr.add(idx).read_volatile() }` and lives in `kernel/`. the host-side mock backs each "frame" with a `PageTable` in a `Vec`, no unsafe, no allocator dependence.
- this is the second time `#![forbid(unsafe_code)]` has bitten a "convenient" design and forced a better one. it's not a sacred invariant — pure-data types that benefit from `unsafe` for perf could relax it case-by-case — but as a default it concentrates unsafe at the hardware boundary and leaves the algorithmic core fully testable on a laptop. earns its keep.

### two phases, on purpose

- **P1**: ship `map` with no caller. `#[expect(dead_code, reason = "...")]` on the kernel wrapper. all 11 host tests pass. integration suite still 7/7. zero behaviour change. the function exists and is reviewable in isolation.
- **P2**: heap migration. switch `heap::init` from "contiguous linear-map run" to "loop installing 1024 leaf PTEs," add `heap::extend`, add a heartbeat watermark trigger. now `map` has a real consumer.
- the payoff: when P2 booted, it worked first try. 11 host tests against a Vec-mock vs. 1024 real PTE installs in QEMU — the contract held end-to-end. that's the moment the kernel-core/kernel split paid for itself.
- the `#[expect(dead_code)]` annotation has a quiet beauty here: it's a self-cleaning TODO. the moment P2's first caller landed, the lint stopped firing, and the compiler emitted a *new* warning about the expectation being unfulfilled. remove the annotation; the compiler is the enforcer. unlike `#[allow(...)]` which would have silently lied forever.

## the heap, again, properly

- after P2: dedicated VA window at `HEAP_VA_BASE = 0xffffffc0_00000000`, root PTE 256. starts at 4 MiB (1024 individual map calls), grows by 1 MiB at a time, ceilings at 1 GiB (one full root slot).
- heap VAs are contiguous *by construction* — we picked a 1 GiB window and own all of it. PA frames are scattered — `frame::alloc()` gives whatever's free. the `map` calls manufacture the contiguity. this is the punchline of paging: **virtual contiguity is free, physical contiguity is expensive**, and the heap can afford to surrender the latter to get the former.

### the watermark policy

- "grow when free < 25% of capacity, by 256 frames, up to a ceiling." three numbers and a comparison.
- moved that decision out of `main.rs` and into `kernel_core::heap::watermark_grow_decision(stats, &cfg) -> Option<usize>`. pure function over numbers. six host tests pin the boundaries — above threshold, below threshold, exact-equality (strict less-than), at the ceiling, clamping when the requested grow would overshoot, zero-capacity guard for "init hasn't run."
- the kernel side now reads "the policy says do nothing, or grow by N frames." the side effect (call frame allocator, call map, call `linked_list_allocator::extend`) stays at the kernel boundary. policy and mechanism cleanly split.

### growth in action

- `heap-oom` had to learn new tricks. the old scenario leaked 1 MiB/heartbeat and exhausted the fixed 4 MiB in ~4 ticks. with P2 the watermark adds 1 MiB/heartbeat, so the old leak rate balances perfectly and OOM never happens.
- bumped the leak to 16 MiB/heartbeat. growth absorbs the first 1 MiB; net pressure is +15 MiB/tick. the ~120 MiB usable RAM exhausts in ~8 heartbeats. assert grow happened (`grow_total > 0`), then OOM happened (`alloc_failed_total > 0`), then the kernel kept heartbeating. all three.

## the LLA fork

- one nagging thing: `linked_list_allocator`'s public API exposes `size()` / `used()` / `free()`. that's *occupancy*. it does not expose `largest_free_block` or the count of holes. those are *fragmentation* signals — and fragmentation is the whole point of caring which allocator you picked.
- so: forked `linked_list_allocator` locally, added a `Heap::free_block_stats() -> (count, largest)` method that walks the existing hole list. ~20 LOC of new code in the fork.
- plumbed both signals end-to-end: `kernel_core::heap::Stats` gained two fields, `heap::stats()` reads them, the heartbeat emits `snitchos.heap.free_blocks` + `snitchos.heap.largest_free_block_bytes`, Grafana panels render both.
- the gap between `largest_free_block_bytes` and `bytes_free` is exactly the fragmentation cost: "you have N bytes free but can't allocate anything bigger than M." that's the story Grafana now tells. previous incarnation: "the heap is X% full." current incarnation: "the heap is X% full and Y% fragmented."

### tracking the fork diff

- forked dependencies drift silently. the fix: `vendor/patches/linked_list_allocator.patch`, a unified diff against the registry source, checked in alongside the vendored tree. two hunks, both additions, nothing removed from upstream.
- regenerate with `diff -u ~/.cargo/registry/src/.../hole.rs vendor/.../hole.rs >> vendor/patches/linked_list_allocator.patch`. when 0.11 ships: `patch -p1 < ...` + re-verify. the diff is ~20 lines — small enough to audit and small enough to rebase without drama.

### what fork buys vs costs

- buys: real fragmentation observability. precondition for any meaningful allocator A/B comparison — without it, "is `talc` better than `linked_list_allocator`?" is a vibes-based question.
- costs: maintenance. when upstream LLA releases 0.11, rebase the patch. when adding new introspection (size buckets, allocation lifetimes), that's another fork patch.
- the right call when the convention "use upstream as-is" trades observability for nothing-real. SnitchOS's whole identity is observability-first; the fork follows.

## the heap smoke, properly

- the `Vec::with_capacity(256)` heartbeat smoke from step 4 proves the heap is live but puts nothing interesting on the dashboard. `bytes_used` flickers to 256 and back; `free_blocks` sits at 1; `largest_free_block` monotonically shrinks as the heap grows. technically correct, observably boring.
- wanted something that exercises all three new signals simultaneously. three requirements: (1) cumulative heap state that grows across heartbeats, (2) many small separate allocations so `free_blocks` tells a story, (3) a periodic event that demonstrates what fragmentation *is*.
- `FactorTable`: a `BTreeMap<u64, u64>` mapping each integer to its smallest prime factor. each heartbeat, factorize the next 200 integers and insert. every 10 heartbeats, evict composites (entries where `spf ≠ n`). the eviction is the fragmentation event.
- why `BTreeMap`: each node is a small, individually heap-allocated object (~40 bytes in alloc's B-tree). 200 inserts per tick → 200 separate heap objects → `free_blocks` meaningfully above 1. a `Vec` would give one contiguous slab; you'd see `bytes_used` grow but no fragmentation signal worth watching.
- why prime factorization: the eviction has a clean mathematical definition. before eviction: ~200 entries. after: roughly π(n) primes in that range, so ~17–20 for the first cycle. the freed slots leave non-contiguous holes throughout the B-tree's memory — `free_blocks` spikes, `largest_free_block` stays small even while `bytes_free` recovers.
- three dashboard signals: `entries` (sawtooth — climbs 200/tick, drops ~80% on eviction), `primes` (slow monotone ramp — prime density π(n)/n is decreasing, so each cycle contributes fewer new primes than the last), `candidate` (straight ramp — throughput readout). all three tell different parts of the story simultaneously.
- all the logic lives in `kernel_core::heap_smoke::FactorTable` — the first time kernel-core uses `alloc`. 8 host tests cover: empty state, spf correctness for primes and composites, post-eviction entry counts, correctness of `extend` after `evict_composites`, and prime-count monotonicity across eviction cycles. the kernel side (`kernel::heap_smoke`) is just a `Mutex<Option<FactorTable>>` + `step()` / `stats()`.
- clearly marked temporary throughout with `// SMOKE TEST — remove once real kernel workloads drive heap metrics`. the right removal signal: a real kernel workload (first process, first syscall, a scheduler runqueue) that puts persistent state on the heap. until then, the factor table fills the gap honestly.

## what i learned

- **two-phase delivery is underrated.** P1 + P2 cost almost nothing extra to organise but gave us a quiet test boundary at the most dangerous point — the page-table walk. would not have caught a Sv39 PPN-encoding off-by-one in a single-commit "land map and use it" diff. caught it twice in host tests instead.
- **`#![forbid(unsafe_code)]` as a default is a forcing function for good factoring.** when it bites, the bite usually points at the right seam. concentrating unsafe at the hardware boundary makes the algorithmic core testable, and "host-testable" turns out to mean "actually testable" in practice.
- **`#[expect(...)]` is the version of `#[allow(...)]` you should be using.** it's a TODO with the compiler as enforcer. the dead-code suppression on `mmu::map` auto-cleaned itself the moment a caller landed; nobody had to remember to revisit it.
- **policy / mechanism split keeps showing up.** watermark grow extracted into a pure function with six host tests, vs leaving it as four lines inline in `main.rs` with zero coverage. the inline version "worked"; the extracted version proves it works under boundary conditions.
- **the grafana `datasource` field is non-optional.** new panels constructed programmatically without `datasource: {type, uid}` on both the panel and each target cause the query to hang indefinitely — not "no data," not an error, just a spinner that never resolves. easy to miss when building panels in code; easy to diagnose by diffing against a working panel.
- **virtual contiguity is free; physical contiguity is expensive.** every paging-aware kernel surrenders the latter to get the former. P2's heap is the simplest non-trivial example: `map` calls turn scattered PA into contiguous VA, and the `linked_list_allocator` above is none the wiser.
- **forking a dependency for the right reason is fine.** observability isn't a vague "it'd be nice"; it's load-bearing for this project. the LLA fork is a few dozen lines of patch and unblocks a thread that was otherwise stalled on "we can't see what we'd need to see."

## what's not done

- **TLB shootdown for SMP.** the `sfence.vma vaddr` in `mmu::map` only invalidates the current hart's TLB. v0.4 is single-hart so it doesn't matter; v0.7+ will need IPIs to other harts before changing PTEs they might cache.
- **two-phase commit on `map` partial failure.** mid table allocated, leaf table allocation failed → the mid is leaked. bounded (next map into the same gigapage reuses it) and tested (`map_returns_out_of_frames_partway_through_walk`), but documented as a known leak.
- **huge pages for the heap.** currently 4 KiB leaves. one leaf table covers 2 MiB of heap; the table overhead is 0.2% which is fine, but TLB pressure under heap-sweeping workloads is real. could promote to 2 MiB huge-page leaves when frames happen to be PA-aligned-and-contiguous. not in scope; flagging.
- **the `talc` A/B.** instrumentation precondition: met. status: unblocked, not started.

## v0.4 step 4 + 5 status

| ✓ | thing |
|---|---|
| ✓ | `#[global_allocator]` over `linked_list_allocator` 0.10 (forked) |
| ✓ | `Box` / `Vec` / `String` / `BTreeMap` work inside the kernel |
| ✓ | `kernel_core::mmu::map` walk — pure, 11 host tests via `PtMem` mock |
| ✓ | `kernel::mmu::map(va, pa, perms)` wrapper — `KernelPtMem` + `sfence.vma` |
| ✓ | heap at dedicated VA window (`HEAP_VA_BASE = 0xffffffc0_00000000`, root PTE 256) |
| ✓ | `heap::extend` grows on demand; ceiling at 1 GiB |
| ✓ | `kernel_core::heap::watermark_grow_decision` extracted policy, 6 host tests |
| ✓ | telemetry: `alloc_total`, `dealloc_total`, `alloc_failed_total`, `bytes_capacity`, `bytes_used`, `bytes_free`, `grow_total`, `grow_failed_total`, `free_blocks`, `largest_free_block_bytes` |
| ✓ | Grafana panels for all 10 heap metrics |
| ✓ | `linked_list_allocator` forked locally; `Heap::free_block_stats()` exposes hole-list signals |
| ✓ | integration scenarios: `kernel-heap-metrics`, `heap-oom` (asserts grow → OOM → survival) |
| ✓ | `Bitmap::alloc_contiguous` retained as a primitive (7 host tests); unused by heap post-P2 but useful for future DMA |
| ✓ | `linked_list_allocator` forked; diff tracked in `vendor/patches/linked_list_allocator.patch` |
| ✓ | `kernel_core::heap_smoke::FactorTable` — BTreeMap prime-factor table, 8 host tests; exercises `free_blocks` + `largest_free_block_bytes` |
| ✓ | Grafana smoke panels: `entries` (sawtooth), `primes` (monotone), `candidate` (ramp) |
| ✓ | 92 host tests + 8/8 integration scenarios green |

## what's next

- **v0.5 (threading + round-robin scheduler).** the heap unblocks `Box<TaskControlBlock>`, the kernel-stack-per-task pattern, runqueue data structures. `mmu::map` is the v0.6 prerequisite for per-process address spaces, and now it exists. the foundation under both is in.
- **the `talc` A/B.** swap behind `#[global_allocator]`, run the same workloads, compare fragmentation curves. with the LLA fork providing baseline signals, the comparison can finally be empirical rather than vibes.
- **fragmentation-shaped integration scenarios.** the current `heap-oom` is a leak test. a `heap-fragments` scenario — interleaved allocs and frees of mixed sizes — would exercise the new `free_blocks` and `largest_free_block_bytes` signals and give us a fragmentation curve to point at.
