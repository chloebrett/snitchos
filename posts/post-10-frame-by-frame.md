# Post 10 — Frame by frame

- v0.4 step 3: the kernel can now allocate physical memory. there's a frame allocator behind `frame::alloc()` that hands out 4 KiB pages, a linear map that makes every allocated frame addressable, and a Grafana panel that shows the kernel running out of memory in real time. ~600 lines of code, two unexpected things, one real perf bug.

## what we had vs what we need

- end of step 2: the kernel runs at higher-half PC, has paging enabled, has a static dual-mapped boot table. but it can only touch the memory it's already statically reserved at link time. that's the kernel image (~70 KiB) plus the four boot page tables (~16 KiB).
- step 3 unlocks the rest. physical RAM that isn't kernel image is just sitting there, unmapped, unreachable. the frame allocator's job is to know which 4 KiB chunks of it are free and to hand them out on request.
- after step 3: any kernel code can ask for a fresh 4 KiB page and gets back a physical address. step 4 (kernel heap) will sit on top of this. step 6+ (per-process page tables) will need it. drivers that DMA will need contiguous-frames variants. it's the floor under everything.

## the bitmap

- one bit per 4 KiB frame. convention: **bit = 1 means free**, bit = 0 means in-use. so "find first free" becomes `trailing_zeros` on the first non-zero word, which is one CPU instruction.
- size: at 4 GiB max RAM = 1M frames = 16K u64s = **128 KiB** of `.bss`. always allocated, regardless of actual RAM. for 128 MiB of QEMU `virt` we use 32K bits = 4 KiB of the bitmap and waste the other 124 KiB. fine.
- the bitmap type lives in `kernel-core::frame` with **11 host tests** covering the edges: clamping at capacity, releasing across u64 word boundaries, alloc finding frees in higher words, double-free idempotency, capacity ≠ `bits.len() * 64`.
- kernel side: a `static mut FRAME_BITS: [u64; 16384]` in `.bss`, a `spin::Once<spin::Mutex<Allocator>>` that wraps the `Bitmap` plus the `ram_base` for index↔PA conversion. same pattern as `INTERN_TABLE` and `CONSOLE`.

## the linear map

- this is the bit that took the most thought. **what address does the kernel use to access an allocated frame?**
- the frame allocator returns physical addresses. the kernel runs at virtual addresses. with the MMU on and identity-MMIO unmapped (which step 2 did), physical addresses are not directly usable as VAs.
- the dumb approach: every time anyone wants to touch a freshly-allocated frame, install a temporary PTE for it, do the access, tear it down, sfence. that's many ops per frame use. and the PTE installation needs a frame for the page table you're modifying. recursion.
- the standard approach (linux: "direct map"; sel4: "physmap"; arm64: PAGE_OFFSET region): **maintain one permanent mapping that covers all of physical RAM at higher-half VAs**. set up at boot, never changes. any PA `p` is reachable at VA `p + LINEAR_OFFSET`. zero per-frame work.
- on Sv39 this is almost free: a **single 1 GiB Sv39 huge-page leaf** in the root page table covers all 128 MiB (and up to 1 GiB) of QEMU `virt`'s RAM. one PTE. one entry in `BOOT_PT_ROOT[322]`. done.

```rust
let linear_va = LINEAR_OFFSET + 0x80000000;            // 0xffffffd0_80000000
let linear_idx = (linear_va >> 30) & 0x1ff;            // 322
let linear_leaf = leaf_pte(0x80000000, perms);         // 1 GiB leaf
(&mut *(&raw mut BOOT_PT_ROOT)).set_entry(linear_idx, linear_leaf);
```

- `LINEAR_OFFSET = 0xffffffd0_00000000`. picked to satisfy Sv39's canonical-high rule (bits 63:39 must all equal bit 38), to land in a root PTE index distinct from the higher-half kernel image (510) and MMIO (508), and... that's it. it's a constant. two host tests pin its arithmetic.
- the inversion of `va_to_pa` is `pa_to_kernel_va(pa) = pa + LINEAR_OFFSET`. lives in `kernel-core::mmu` next to `va_to_pa` and `KERNEL_OFFSET`.
- with this, `PhysFrame::kernel_va()` is one addition. `frame::alloc_zeroed()` is just `alloc + memset` through the linear-map VA. and **every freshly-allocated frame is immediately writable from kernel code**, no further page-table work needed.

## the reserved-region math

- at boot, you start with "all of physical RAM" and subtract what's already in use:
  - **SBI firmware**: `[0x80000000, __kernel_start)` — 2 MiB on QEMU `virt`.
  - **kernel image**: `[__kernel_start, __kernel_end)` — text, rodata, data, bss (which includes `FRAME_BITS` and the four `BOOT_PT_*` tables), stack.
  - **DTB**: one 2 MiB-aligned page containing `dtb_phys` — typically around `0x87e00000`.
- in code: per-frame loop, check each PA against the three reservation ranges, release into the bitmap if it's not in any of them. O(frames × reservations) = ~32K × 3 = trivial.
- **one gotcha**: post-trampoline, `&raw const __kernel_start as usize` gives the *higher-half* VA. for the reservation bounds we need the *physical* address. solution: `va_to_pa(...)` strips `KERNEL_OFFSET`. flagged in the plan, no surprises.

## instrumentation

- five metrics added, all going to the wire and into Grafana:
  - `snitchos.frames.allocated_total` (counter)
  - `snitchos.frames.freed_total` (counter)
  - `snitchos.frames.alloc_failed_total` (counter)
  - `snitchos.frames.in_use` (gauge)
  - `snitchos.frames.free` (gauge)
- same **deferred-emission pattern** as the IRQ duration histogram: counters are `AtomicU64`s that `alloc`/`free` increment outside the bitmap lock; the heartbeat thread reads them. no recursive locking, no allocation-from-allocator-emit-frame-which-reaches-into-virtio path that could deadlock.
- five new Grafana panels (stat + timeseries variants) join the existing dashboard at `y=24` and `y=32`. cumulative counters track each other in steady state because the smoke pattern is "alloc + free per heartbeat." they diverge under the OOM scenario.

## the OOM scenario, and the perf bug it found

- per-heartbeat smoke originally did `alloc_zeroed` + `free`. that's lovely for proving the allocator works but the metrics don't *change* — `in_use` is flat at the reserved count forever.
- the user wanted something more interesting: **a smoke that runs the kernel out of memory over ~30 seconds**, so the Grafana decay curve is visible.
- replaced the alloc+free with a per-heartbeat **leak**: allocate 1024 frames, don't free. with ~32K free frames on `-m 128M`, the pool exhausts in ~32 heartbeats ≈ 30 seconds. exactly the curve we wanted.
- then i wrote an integration scenario asserting `alloc_failed_total > 0` within 45 seconds, plus a follow-up assertion that the kernel keeps producing heartbeats post-OOM.
- **it failed.** alloc_failed_total went positive on schedule, but the post-OOM heartbeat assertion timed out. the kernel was alive — i could see it on the wire — but heartbeats were arriving every 10+ seconds instead of every second.

### the bug

- `Bitmap::alloc` scanned the bits array word-by-word for the first non-zero word, then `trailing_zeros`'d to find the bit. with 16K u64s, that's 16K reads per failed alloc in the worst case.
- per heartbeat under OOM: 1024 alloc attempts × 16K word reads = **16M word reads** per heartbeat just for bitmap scans, all returning `None`. on QEMU TCG that's slow enough to stretch a heartbeat to 10+ seconds.
- count_free() also did a per-word popcount, called once per heartbeat from `stats()`. another 16K reads. not the bottleneck but not great.

### the fix

- maintain a `frames_free: usize` counter on the bitmap itself, updated in `alloc` (decrement on success), `free` (increment on 0→1 transition), and `release_range` (increment on each 0→1 transition).
- `alloc` checks `frames_free == 0` first and returns `None` immediately without scanning.
- `count_free` becomes O(1).
- the **0→1 transition check** on `set_bit_tracked` is what makes the counter robust under idempotent double-`free` and overlapping `release_range`s. one of the existing host tests was specifically `double_free_is_idempotent` — passed before the change, still passes after, now also pins the counter.
- all 11 bitmap tests pass under the new impl. the OOM scenario now completes in **~7 seconds total** instead of timing out.

### bumping the rate

- with the perf fix in, i bumped the leak rate from 1024 → 8192 frames per heartbeat. the pool exhausts in ~4 heartbeats. test budget dropped from 45s to 15s.
- the Grafana demo curve is correspondingly faster — `frames.free` decays in ~4 seconds now instead of ~30. fine; the demo isn't the test.
- the OOM behavior is also gated behind a `oom-leak` cargo feature. default builds do alloc+free (no leak); only the OOM scenario builds with the feature.

## what the dashboard shows

| panel | steady state (default features) | under `oom-leak` |
|---|---|---|
| frames in use | reserved count, flat | climbs by 8192/heartbeat to capacity |
| frames free | total − reserved, flat | decays to 0 in ~4 heartbeats |
| allocation failures | 0 forever | flat 0 until heartbeat ~5, then climbs steeply |
| allocation rate (per second) | matched alloc/free at ~1/s | allocs/s spikes to 8192 then drops to 0; frees/s flat 0 |
| cumulative allocated / freed | both tick together | allocated climbs, freed flat — the divergence visualizes the leak |

- you can watch the kernel "die" of memory exhaustion in about 6 seconds in real time. it doesn't actually die — the kernel survives OOM cleanly because `alloc` returns `None` and the smoke just keeps trying. but the curves look the part.

## what i learned

- **the linear map is a wildly cheap unlock.** one 1 GiB Sv39 leaf and you can address all of physical RAM via a fixed offset. linux, sel4, freebsd, every modern kernel does it. the fact that it costs *one PTE* makes it almost embarrassing not to.
- **deferred metric emission is the right call any time you touch a global lock from inside something that might recurse**. same pattern as the IRQ handler — incrementing an atomic from inside the alloc path is cheap; calling `emit_metric` from there would deadlock through the intern table + virtio mutex chain. write the counter, read it from the heartbeat thread, done.
- **the bitmap counter wasn't obvious until OOM made it obvious.** in steady state, alloc is fast because it finds a free bit in the first non-zero word, usually word 0. under OOM, the same code is catastrophic — every failed alloc scans the whole array. obvious in hindsight; not obvious until i tried to make the integration test fast.
- **host-testable pure logic + idempotent invariants pay for themselves.** the `double_free_is_idempotent` test that i wrote weeks ago caught the counter-double-increment regression instantly when i added `frames_free`. five seconds of cargo test, no QEMU. that's the kernel-core carve-out earning its keep again.
- **cargo features per integration scenario** is the right way to enable destructive behaviors. the OOM scenario is the only thing that wants 8 MiB/s of leakage; nothing else does. one `#[cfg(feature = "oom-leak")]` block and a `spawn_with_features(...)` call, and the production build never sees it.

## v0.4 step 3 status

| ✓ | thing |
|---|---|
| ✓ | `kernel_core::frame::Bitmap` + 11 host tests |
| ✓ | linear map: 1 GiB leaf at `BOOT_PT_ROOT[322]`, `LINEAR_OFFSET = 0xffffffd0_00000000` |
| ✓ | `pa_to_kernel_va` next to `va_to_pa` in `kernel-core::mmu` |
| ✓ | `kernel::frame` global allocator: `Mutex<Allocator>`, `alloc` / `alloc_zeroed` / `free` / `stats` |
| ✓ | DTB-driven init reserving SBI / kernel image / DTB |
| ✓ | 5 metrics on the wire, 5 Grafana panels in the provisioned dashboard |
| ✓ | integration scenario `frame-allocator-metrics` asserting allocs flow |
| ✓ | integration scenario `frame-allocator-oom` asserting clean OOM behavior (under `oom-leak` feature) |
| ✓ | `Bitmap::frames_free` counter for O(1) `alloc` short-circuit + `count_free` |

## what's next

- **step 4 (kernel heap)**: build a `GlobalAlloc` impl on top of `frame::alloc()`. probably `linked_list_allocator` for simplicity. once that lands, `Box`, `Vec`, `String`, `BTreeMap` all start working in the kernel. immediate quality-of-life win — no more fixed-size arrays everywhere.
- **step 5 (allocator telemetry)**: already partially done in step 3; the heap will add its own metrics for bytes-used / alloc-rate / fragmentation indicators.
- there are some unused-function warnings (`free`, `PhysFrame::addr`, etc.) hanging around from the public API that no kernel code calls yet. step 4 will use them.
- one parked landmine still worth understanding: **why `dtb.all_nodes()` crashes pre-MMU under higher-half link.** we worked around it by hardcoding the MMIO base. would be nice to know what's actually happening down there one day.
