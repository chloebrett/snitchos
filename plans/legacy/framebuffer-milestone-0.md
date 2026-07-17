# Plan: Framebuffer Milestone 0 — a screen that snitches (first pixel)

**Branch**: main (project works directly on main; the user commits)
**Status**: Active

Companion to [docs/framebuffer-design.md](../../docs/framebuffer-design.md). This plan
implements the *first coherent slice* of Milestone 0: **a ramfb framebuffer brought up
over fw_cfg, cleared to a color, with a present counter on the wire.** The moving rect,
input, damage/scanout caps, and the snemu ramfb model are explicit follow-ups (see
"Out of scope").

## Goal

Boot the kernel with a live ramfb framebuffer: allocate contiguous physical memory,
map it into a kernel VA window, hand its physical address to QEMU via fw_cfg, clear it
to a color, and emit `snitchos.display.frames_presented_total` on the telemetry wire —
proven by an itest.

## Why this slice first

It exercises every load-bearing piece exactly once — fw_cfg (greenfield), contiguous
frame allocation, a new VA window, the DMA-at-the-device-boundary discipline, and
`display.*` telemetry — with a **cleared screen** as the visible proof-of-life. No
physics, no input, no compositor. If this lands, the rest of Milestone 0 is
incremental.

## Testing doctrine (per project layering)

The kernel binary has no `#[test]`s. So:

- **All pure logic lives in `kernel-core` and gets full RED→GREEN→MUTATE** — fw_cfg
  directory parsing, big-endian ramfb/DMA serialization, the transport-driving
  sequence (mock transport, mirroring `kernel_core::virtio::handshake`), and pixel
  ops over a `&mut [u8]`. **These steps (2–5) are the real TDD surface**, and the
  highest-value tests are the **big-endian serialization** ones (fw_cfg is big-endian;
  RISC-V is little-endian — that boundary is where bugs hide).
- **Kernel-side MMIO / mapping / boot wiring** (steps 1, 6, 7) can't be host-tested;
  they're covered transitively by the QEMU **itest** in step 7, asserting on the
  `display.*` metric. No pixel readback exists in the harness (confirmed) — telemetry
  is the assertion surface.

## Acceptance Criteria

- [x] `frame::alloc_contiguous(n)` returns `n` physically-contiguous frames or `None`,
      and decrements the maintained free-count by exactly `n`.
- [x] fw_cfg file-directory parsing finds `etc/ramfb` by name and returns its select
      key + size; returns `None` for absent names.
- [x] The `RamfbCfg` (28-byte) and fw_cfg `DmaAccess` (16-byte) blobs serialize to the
      exact big-endian bytes QEMU expects.
- [x] Booting with `-device ramfb` clears the QEMU display to a solid color.
- [x] `snitchos.display.frames_presented_total ≥ 1` appears on the wire within 10 s,
      and the kernel keeps heartbeating after.
- [x] Booting **without** `-device ramfb` snitches a refusal (`etc/ramfb` absent) and
      the kernel keeps heartbeating — no panic, no hang.

All six proven live: `framebuffer-presents` and `framebuffer-absent-degrades-gracefully`
itests, both green (see step 7).

## Steps

Every step follows RED→GREEN→MUTATE→KILL→REFACTOR. Pure steps mutation-test in
`kernel-core`; wiring steps are covered by the step-7 itest.

### Step 1: Surface contiguous physical frame allocation

**Acceptance criteria**: `frame::alloc_contiguous(n)` yields `n` contiguous frames or
`None` under fragmentation, and `stats().frames_free` drops by exactly `n`.
**RED** (kernel-core, `mem/frame.rs`): strengthen `Bitmap` tests — assert
`alloc_contiguous(n)` decrements `frames_free` by `n`, that the returned run is truly
contiguous, and that it returns `None` when no run of `n` exists *even though* `≥ n`
frames are free (fragmentation). (`Bitmap::alloc_contiguous` exists at
`kernel-core/src/mem/frame.rs:98`; verify/close the free-count gap.)
**GREEN**: add the kernel wrapper `frame::alloc_contiguous(n) -> Option<PhysFrame>` in
`kernel/src/mem/frame.rs` over `bitmap.alloc_contiguous`, returning the run's base.
**MUTATE / KILL**: kernel-core Bitmap only.
**Done when**: criteria met; wrapper compiles; free-count invariant proven.

### Step 2: fw_cfg file-directory parsing (pure)

**Acceptance criteria**: given a fw_cfg directory blob, `find_file(dir, "etc/ramfb")`
returns `Some { select_key, size }` with the correct big-endian-decoded key; absent
name → `None`; the entry `count` header is respected.
**RED** (`kernel-core/src/fwcfg.rs`, new): host tests over a synthetic directory blob
(big-endian `u32` count + fixed-size entries: BE `u32` size, BE `u16` select, 2 pad,
56-byte NUL-padded name).
**GREEN**: `pub fn find_file(dir: &[u8], name: &str) -> Option<FwCfgFile>`.
**MUTATE / KILL**: full — this is pure.
**Done when**: criteria met, mutants killed.

### Step 3: ramfb + DMA descriptor serialization (pure, big-endian)

**Acceptance criteria**: `RamfbCfg { addr, fourcc, flags, width, height, stride }`
serializes to the exact 28 big-endian bytes; `DmaAccess { control, length, address }`
to the exact 16 big-endian bytes.
**RED** (`kernel-core/src/ramfb.rs` + `fwcfg.rs`): byte-exact assertions for known
inputs (e.g. XRGB8888 fourcc = `0x3432_5258`, 1024×768, stride 4096) — pinning
endianness explicitly.
**GREEN**: `RamfbCfg::to_bytes() -> [u8; 28]`, `DmaAccess::to_bytes() -> [u8; 16]`.
**MUTATE / KILL**: full — the endianness mutants are the point.
**Done when**: criteria met, mutants killed.

### Step 4: fw_cfg DMA write sequence over a transport trait

**Acceptance criteria**: with a mock transport, `write_file(select_key, bytes)` issues
the correct register operations in order (build a `DmaAccess` with
`control = (key << 16) | SELECT | WRITE`, write the descriptor's physical address to
the DMA register big-endian, observe completion) — mirroring
`kernel_core::virtio::handshake`'s host-tested shape.
**RED** (`kernel-core/src/fwcfg.rs`): a `FwCfgTransport` trait + mock recording
register writes; assert the emitted sequence and the descriptor bytes.
**GREEN**: the sequence logic in kernel-core; the volatile MMIO impl in
`kernel/src/device/fwcfg.rs` (base `0x1010_0000 + KERNEL_OFFSET`, reachable in the
existing higher-half MMIO region — no new `MmioRegions` entry).
**MUTATE / KILL**: kernel-core sequence only; MMIO impl via step 7.
**Done when**: criteria met; kernel impl compiles.

### Step 5: framebuffer pixel ops (pure)

**Acceptance criteria**: `Framebuffer::clear(color)` fills every pixel; `fill_rect`
writes only the given rect and respects stride; both are bounds-safe.
**RED** (`kernel-core/src/framebuffer.rs`, new): tests over a `&mut [u8]` backing with
a known width/height/stride — assert exact pixel bytes for `clear` and a `fill_rect`
that doesn't overrun the stride.
**GREEN**: a `Framebuffer<'a>` view (dims + stride + `&mut [u8]`) with `clear` /
`fill_rect`. (Reused later by the physics renderer.)
**MUTATE / KILL**: full — pure.
**Done when**: criteria met, mutants killed.

### Step 6: ramfb bring-up wired into boot

**Acceptance criteria**: `ramfb::init()` allocates 768 contiguous frames, maps them
into the **free root-PTE slot 258 window** (`0xffff_ffc0_8000_0000`, `R|W|G`, one
`map()` per page à la `heap::grow_va_range`), DMA-writes the `RamfbCfg` (with the FB's
`va_to_pa`), and returns a `Framebuffer`. Called in `kmain` **after `heap::init`**.
If `etc/ramfb` is absent, it snitches a refusal and returns `None` — boot continues.
**RED/GREEN**: kernel wiring — no host test. `-device ramfb` added to the xtask QEMU
args (boot + itest); a display backend (`-display cocoa/gtk`) added to `xtask boot`
for manual viewing (itest stays headless).
**Covered by**: step 7 itest.
**Done when**: `cargo xtask boot --device ramfb`-equivalent shows a cleared color
window; absent-ramfb path degrades gracefully.

### Step 7: itest — `framebuffer-presents`

**Acceptance criteria**: with `-device ramfb`, the scenario asserts
`snitchos.display.frames_presented_total ≥ 1` within 10 s, then that a
`kernel.heartbeat` still arrives after (present didn't wedge the kernel).
**RED**: new scenario in `xtask/src/itest/scenarios.rs` using
`is_metric_named("snitchos.display.frames_presented_total")`; register in
`itest.rs::SCENARIOS`.
**GREEN**: add `pub static FRAMES_PRESENTED: DeferredCounter =
DeferredCounter::new("snitchos.display.frames_presented_total")` in
`kernel/src/device/ramfb.rs`; add it to the `COUNTERS` slice
(`kernel/src/obs/counter.rs`); `.inc()` per heartbeat present (clear-to-color each
tick for now). Optionally wrap the present in `span!("display.present")`.
**Covered by**: the itest itself.
**Done when**: `cargo xtask snemu-itest` / `itest framebuffer-presents` passes; the
absent-ramfb graceful path also covered (assert refusal + heartbeat).

## Pre-PR Quality Gate

1. Mutation testing on the kernel-core additions (steps 1–5) — `cargo xtask mutants`.
2. Refactoring assessment.
3. `cargo xtask clippy` (whole workspace, riscv + host) and `cargo xtask test`.
4. `cargo xtask snemu-itest` (the commit gate) green.

## Out of scope (follow-up plans)

- **Moving rect + input** — `ConsoleRead`/virtio-input driven; determinism/replay of the
  input stream. (Design-doc Milestone 0 step 2.)
- **Damage rectangles as provenance + cap-bounded `Scanout{rect}`** — the actual
  "snitch" thesis; needs the compositor-as-capped-actor. (Steps 3.)
- **snemu ramfb device model** — ~50-line guest-RAM-region → `<canvas>` blit; on
  snemu's critical path but a separate track ([snemu progress](snemu-lockstep-native-ops.md)).
- **virtio-gpu** — the grown-up device; deferred until real-hardware fidelity matters.
- **Double-buffering / tear-free present** — ramfb is single-buffered; a shadow buffer
  in the FB window is a later refinement (open question in the design doc).

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
