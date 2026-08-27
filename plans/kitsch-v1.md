# `kitsch` v1 — the desktop

**Status (2026-08-27)**: 🚧 **INCREMENT 0 IN PROGRESS.** Two of four numbers in
hand, and the mode change they prompted has **landed and is gate-green** (132/132
itests plain and `--scramble`). No kitsch code yet. Design:
[../docs/kitsch-design.md](../docs/kitsch-design.md).

**Scope of v1**: a keyboard-driven tiling desktop on real framebuffer pixels, with
cell-mode surfaces, telemetry-derived window furniture, and three apps. Explicitly
**not** in v1: pointer input, pixel surfaces, memory-object capabilities, effects,
the widget framework, the patch view, replay. Each is deferred for a reason
recorded below or in the design's non-goals.

**Gate**: `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
Every increment lands green.

## Three decisions that shape the ladder

**1. kitsch is a Stitch program that calls natives.** Not Rust. The interpreter is
mature enough on target, and where it isn't, kitsch is the thing that lifts it. The
leak risk is accepted deliberately: a long-running Stitch process's heap growth
shows up in per-process `snitchos.heap.*` on the wire — and on kitsch's own
back-of-window view — so a leak here reports itself rather than being discovered.

**The native boundary is at compose-rasterize-present**, and that choice is what
makes it viable. Cost class by granularity:

| boundary | interpreter→native transitions per full frame | verdict |
|---|---|---|
| per glyph | 7,200 | dead on arrival |
| per span | tens | viable |
| **per present** | **1** | **Stitch cost is O(events), independent of resolution** |

So **Stitch owns the scene** — window list, rects, focus, z-order, effects,
accumulated damage, all of which change on events at tens per second — and hands it
across once per frame. **Native owns the 7,200 cell-picks and ~921,600 pixel
writes.** Stitch never touches a cell.

**2. The shell is the first GUI application, before the WM.** With one client there
is no input routing, no layout, and no surface sharing — so the first screenshot
needs only a font and a grid. And the shell is already a *working program*
(`workload=shell`: reads keys via `console_read`, runs `view <path>`, delegates a
READ-only file cap to the viewer and revokes it after), so this is a proven program
gaining a new output, not a new app.

> **The trap to avoid**: do not let the shell *become* kitsch. If it renders
> straight to the framebuffer with no surface in between, v1 is a full-screen text
> console and the surface protocol becomes a rewrite rather than an addition. The
> shell is a **client with a surface from the first commit** — sole client, no
> tiling, no focus, but the boundary is real.

**3. Memory-object capabilities are deferred, not a prerequisite.** A full 160×45
cell grid is **28.8 KB**; damage-limited it is a few hundred bytes. That crosses IPC
without strain, so cell surfaces need no new kernel primitive and the hardest kernel
work stops gating the first screenshot.

This corrects an earlier claim in the design that IPC-copied surfaces "degenerate
read caps into policy". For a **scanout** tap that is vacuous — kitsch composites the
scanout, so a scanout-tap holder trusts kitsch inherently and no mapping changes
that. For a **per-surface** tap a mapping does remove kitsch from the middle, but
kitsch is the party granting the tap anyway. The real reasons for memory objects are
**avoiding large copies for pixel surfaces** and completing an object set that names
endpoints and notifications but not memory. Both stand; neither is urgent.

## The shape

| Component | Language | What | Tested |
|---|---|---|---|
| `user/kitsch` | **Stitch** | policy: layout, focus, damage bookkeeping, surface lifecycle, cap grants, input routing | Stitch native tests (`test "…" { expect … }`) |
| `kitsch-render` | Rust, `no_std`+`alloc` | compose the grid, rasterize glyphs, present. Called as Stitch natives, once per frame | host, `insta` grid snapshots |
| `kitsch-proto` | Rust | surface / tap / input wire types | host |

Each layer is tested in its own language at its own altitude: layout and focus as
Stitch tests, the composed grid as Rust `insta` snapshots.

**Damage is a bitmap plus row spans, not a tree.** 7,200 cells is 900 bytes of dirty
bits — 113 `u64` words to mark and scan. Coalescing each row's dirty bits into runs
before rasterizing is the whole optimisation, because the framebuffer is row-major
so a horizontal run is contiguous memory. This is what pixman's banded regions do
and what every real compositor converged on. A quadtree is four orders of magnitude
short of paying for itself and has no query to accelerate.

## Increments

### 0 — Four numbers and a decision

No code beyond throwaway benchmarks. Three questions whose answers change what gets
built, and this repo's record on estimating them unaided is bad in both directions.

1. **Full-screen blit cost.** ✅ measured, below.
2. **Interpreter cost per event** — the `view`-per-message question, now load-bearing
   for kitsch itself and not just for the app framework: how expensive is one Stitch
   event handler, and how many can a frame afford?
3. **Font budget.** ✅ answered, below — 4,096 bytes, one page, CP437 8×16.
4. **Resolution.** ✅ decided and landed, below.

**Status: 3 of 4 done.** Only (2) remains, and it needs a probe harness rather than
a command — see below. It should not be skipped, because kitsch's policy layer
running on the interpreter is a decision that rests on it.

#### Findings so far (2026-08-27)

**(4) Resolution: changed to 1280×720 XRGB8888** (was 1024×768 — an arbitrary pin,
which `docs/framebuffer-design.md` explicitly said to revisit). No row padding,
3,600 KiB, exactly **900 frames**, mapped at `FB_VA_BASE` (root PTE slot 258). At
8×16 that is a **160×45 cell grid**.

The first reason is aesthetic and that is fine: **4:3 looks wrong on every monitor
made this century**, and the desktop is a thing people look at. Three more that
happen to agree: 720p is a standard CEA mode a real HDMI sink will accept, which the
VF2 display driver will need; its bytes divide evenly into frames; and **160 columns
is exactly two 80-column panes**, where 1024's 128 splits into a cramped 64+64.

Rejected, with numbers, in case the blit cost ever forces a smaller mode:

| | pages | cols × rows @8×16 | blit | real mode? |
|---|---|---|---|---|
| 960×540 | 506.25 ✗ | 120 × 33.75 ✗ | 5.70M | no |
| 854×480 (FWVGA) | 400.31 ✗ | 106.75 ✗ × 30 | 4.51M | video, not display |
| **1024×576** | **576 ✓** | **128 × 36 ✓** | 6.49M | no |
| **1280×720** | **900 ✓** | **160 × 45 ✓** | 10.14M | **yes** |

**1024×576 is the fallback** if the blit cost ever forces a smaller mode — clean on
every axis, exactly 16:9, 36% cheaper — and its only flaw (not a standard display
mode) costs nothing under snemu. Nothing in the design depends on the choice.

**Landed**, with a bug fixed on the way: `FRAMES` was `SIZE_BYTES / FRAME_SIZE`,
truncating division, so any mode whose bytes did not fill whole pages would have
under-allocated and left the device DMA-ing past the allocation. Now
`kernel_devices::ramfb::frames_needed`, host-tested on the ragged and exact cases.

**(1) Full-screen blit: 11 instructions per pixel.** Measured by disassembling the
emitted riscv code rather than by timing — a claim about what a compiler emits is
empirical. `Framebuffer::fill_rect`'s inner loop at opt-3:

```
bltu t3, a0     ← bounds check 1 (not elided)
addi a4, a0, 4
bltu a2, a4     ← bounds check 2 (not elided)
add  a0, a0, t5
addi a3, a3, -1
sb   a1, 0(a0)  ← FOUR BYTE STORES, not one sw
sb   a5, 1(a0)
sb   t6, 2(a0)
sb   t4, 3(a0)
mv   a0, a4
bnez a3
```

Measured at 1024×768 (786,432 px = **8.65M instructions**); at the new 720p mode
921,600 px = **10.14M**. For scale, the profiler's entire 100M-instruction post-boot
window has `sched::prepare_switch`, its busiest kernel function, at 18.2M — so **one
full clear costs about half of what the whole scheduler does across 100M
instructions**, and 60 Hz of full clears would be ~608M instr/s.

**This is a naive-code problem, not a physics problem.** `copy_from_slice` on a
4-byte slice did not lower to a word store, and neither bounds check was hoisted. A
`u32` store with hoisted checks is ~2–3 instructions per pixel — a **4–5× win**,
which puts 720p *below where 1024×768 sits today*. Two consequences:

- Optimising `fill_rect` is increment 1: a `kernel-devices` change with existing
  host tests.
- **Damage tracking is not an optimisation, it is the design.** Full-screen presents
  are unaffordable at any interesting rate even after the 4–5×.

**(3) Font: 4,096 bytes — exactly one page.** An 8×16 glyph is 16 bytes (one byte
per row). A **256-entry CP437 table at 8×16 is 256 × 16 = 4,096 bytes**, and CP437
already contains everything a cell desktop needs: ASCII, single- **and** double-line
box drawing, block elements, shading, arrows. Public domain, and the classic PC
text-mode font, so it looks right by construction.

The itest kernel image's shared budget is a non-issue at this size — the concern was
4.5 MB of model weights, not 4 KB. A trimmed ASCII-only table (95 glyphs, 1,520
bytes) is possible but pointless: it would save 2.5 KB and cost the box-drawing the
window furniture needs. **Take the whole page.**

**(2) Interpreter cost per event: NOT MEASURED — needs a purpose-built probe.**
Neither existing instrument can see it (below). The harness it needs: a scenario that
boots `workload=stitch-repl`, uses `View::send_input` to inject a probe line, and
reads `stitch.eval.duration_ticks` (which the REPL already emits as a histogram) off
the wire — subtracting a trivial line's cost to separate per-operation cost from the
REPL's per-line parse + prelude re-registration. Since Stitch has no loop keywords,
the probe iterates by fold or recursion. **This number now gates a design decision
(kitsch's policy layer runs on the interpreter), so it should not be skipped.**

#### Two instrument gaps, same family

Both found while measuring, both worth fixing, and together they say something:
**snemu's measurement spine sees the kernel and not much else.**

- **`snemu profile` cannot see userspace.** `--workload stitch-drivel --steps 60M`
  returns the *same top-12 kernel functions* as a default `init` boot — no
  `[userspace]` bucket at all. It profiles the post-boot heartbeat steady state
  rather than the workload's work.
- **`snemu profile` boots without `-device ramfb`**, so `present()` is a silent
  no-op and the framebuffer can never appear in a profile.
- **`--record-instret` does not measure what its doc comment claims.** The two
  `framebuffer-*` scenarios differ by **6,692** instructions (20,660,918 vs
  20,667,610) when a mandatory **≥8.65M** separates them. That is not a maybe:
  `heartbeat` calls `ramfb::present()` *before* `counter::drain_all()`, so the
  `frames_presented_total ≥ 1` the scenario waits on cannot be observed until a full
  clear has run. Two independent measurements of the same quantity disagree by three
  orders of magnitude, and the disassembly is the trustworthy one.

  **This matters beyond kitsch**: `--check-instret` is documented as *the
  deterministic-perf gate*. If per-scenario instret is not full guest work, that gate
  is weaker than believed. Worth its own investigation —
  [snemu-milestone-4-measurement.md](snemu-milestone-4-measurement.md) owns this
  spine. **Until then, do not build on per-scenario instret.**

### 1 — Make `fill_rect` not terrible

One `u32` store instead of four `sb`, bounds checks hoisted out of the loop. Pure
`kernel-devices` change with existing host tests covering the behaviour, so the
tests come first and the disassembly is the acceptance check.

**Done when**: the inner loop is ≤3 instructions per pixel, verified by
disassembly, and the existing `fill_rect` tests are unchanged and green.

### 2 — `kitsch-render`: font, compose, rasterize

Pure logic, no syscalls. 8×16 bitmap font embedded. Compose a scene into a cell grid;
rasterize damaged spans into pixels. The grid is the thing tests assert on, as text,
via `insta`:

```
┌─ shell ────────────┬─ files ───────────┐
│ $ ls               │ > docs/           │
│ docs  kernel  user │   kernel/         │
│ $ █                │   user/           │
└────────────────────┴───────────────────┘
```

Colour and attributes go in a parallel letter-keyed grid (`f` focused border, `d`
dim, `r` reverse) so they are asserted without becoming unreadable.

Damage property-tests directly: every dirty cell appears in exactly one span, no
span contains a clean cell, spans never cross a row.

**Done when**: `cargo nextest run -p kitsch-render` covers composition, damage
coalescing and rasterization, and a known scene rasterizes to a byte-identical PPM.

### 3 — The display capability

`Object::Display` plus a cap-guarded present syscall. **The kernel does the copy for
v1** — no framebuffer-mapping machinery, since there is nothing to be fast for yet
and the cap story is identical either way.

**Done when**: a process holding the display cap can present; one lacking it is
refused, and the refusal snitches.

### 4 — First pixels

A Rust workload composes a static scene through `kitsch-render` and presents it. No
Stitch, no clients, no protocol. Proves font + compose + rasterize + display cap end
to end, and gives the PPM-dump comparison something real to look at.

**Done when**: `workload=kitsch-static` produces a PPM showing a framed, glyph-filled
grid, byte-identical across runs.

### 5 — The Stitch native bridge

The natives kitsch needs: present, IPC send/recv, `console_read`, cap operations.
This is the known "natives need syscall backing" gap, and kitsch is a better forcing
function for it than anything else on the list.

Also the first **long-running** Stitch process — every Stitch program so far runs and
exits — so this is where per-process heap telemetry starts earning its keep.

**Done when**: a Stitch program holding the display cap presents a scene built in
Stitch, and its heap footprint is flat across 1,000 presents.

### 6 — kitsch, with the shell as its only client

kitsch in Stitch: holds the scene, receives cell commits over IPC, calls
compose-rasterize-present once per frame. The shell becomes a **client with a
surface** — sole client, full-screen, no tiling, no focus — emitting cells instead of
writing bytes to the UART.

**The first useful screenshot: a working shell you can type at.**

**Done when**: `workload=kitsch` boots to a shell on the framebuffer, `view <path>`
still works, and its existing cap-delegation itest still passes unchanged.

### 7 — Layout and focus

Master-stack layout in Stitch, as a pure function of `(windows, params)`. Property
tests: for any window count, no overlap, no gaps, exact coverage. Split-tree is
additive later behind the same seam.

A second client appears here, which is what makes layout and focus mean anything.

**Done when**: two clients tile, focus moves between them, and the layout property
tests pass as Stitch tests.

### 8 — Input routing

`console_read` → kitsch → the focused client only. Plus the kernel-stamped
`Origin::{Hardware, Synthesised(pid)}` field, set where the interrupt is taken and
not writable from userspace — small, and load-bearing for design §6, §7 and §10.

Push/pull tap modes land here too: pull taps are what agents and thumbnails want, and
cost nothing until read.

**Done when**: keystrokes reach only the focused window, and an itest asserts an
event's `Origin` is `Hardware`.

### 9 — The scanout tap, and the test harness it buys

A read cap on the scanout. The itest holds one and asserts the composed grid — so the
tap mechanism is exercised by the suite that verifies it, and every later increment
gets a cheap, readable assertion.

**Done when**: an itest scenario asserts a full grid snapshot from inside the guest.

### 10 — Three apps

- **shell** — already there from increment 6.
- **file browser** — holds an FS endpoint cap; the app that makes the titlebar's
  capability display mean something.
- **tetris** — holds *one* capability, its own surface, and nothing else. The
  least-authority demo: a nearly-empty titlebar beside the file browser's.

**Done when**: all three run concurrently with different cap sets, and the grid
snapshot shows the desktop.

### 11 — The back of the window

Flip any window to see that process's metrics, capability set, liveness and recent
refusals — composed by kitsch from the frame stream, with no cooperation from the
app. It cannot decline to have one, cannot lie on it, and need not know it exists.

No new mechanism; highest value-per-line in the plan.

**Done when**: the flip works on all three apps, including tetris, which was never
modified.

### 12 — Failure states

Liveness from the kernel's frames, never self-reported: deadline missed and by how
much, spinning, **blocked on a `Call` to a named process**, refused a syscall, OOM.
Plus **tombstones** — a dead window keeps its last frame, greyed, with the exit
reason on the border.

Tombstones are the same mechanism as animation and hung-window rendering: kitsch
compositing the last committed content without consulting the client.

**Done when**: a workload that wedges one client, kills another and refuses a syscall
in a third produces three visibly different windows in one snapshot.

### 13 — kitsch is restartable

Clients are separate processes holding their own surfaces, so a compositor crash need
not kill the session: the supervisor restarts kitsch and clients re-commit. Cheap to
design in now, very expensive to retrofit — and it is what makes the interpreter risk
tolerable, since the failure mode becomes a flicker rather than a lost session.

**Done when**: an itest kills kitsch mid-session and the desktop comes back with the
same clients.

### 14 — The launcher, and the topology

`init → launcher → apps`, with kitsch *beside* them. The launcher holds spawn
authority; kitsch holds no send cap to it and therefore cannot ask it for anything.
kitsch draws the launcher; it does not call it.

**Done when**: apps launch from inside the desktop, and an itest proves kitsch holds
no `Spawn` authority and no send cap to the launcher.

## Deferred, with reasons

| Deferred | Why | Trigger |
|---|---|---|
| Memory-object capabilities | cells fit through IPC; not on the critical path | pixel surfaces, or the first large zero-copy consumer |
| Pointer input | tiling is drivable by keyboard | drag-to-grant (design §10) |
| `Surface::Pixels` | the variant is in the protocol from day one | a Minecraft/Factorio-class game |
| Effects | fixed enum, nothing needs them yet | the first accessibility or focus-dimming need |
| The widget framework | multi-year; must be designed against real apps | after increment 10 gives it three |
| Patch view + wiring | needs pointer, and the keyboard version needs the trusted path | after pointer |
| Trusted path + provenance-gated consent | two customers (launcher confirm, wiring) | the launcher's first authority-spending dialog |
| Trace context across IPC | action provenance; supervision wants it too | when "what did the agent cause" is asked in anger |
| OS-level replay | a consequence of capabilities, not a feature | after taps and inserts stabilise |
| Split-tree layout | additive behind the existing seam | when master-stack is what gets complained about |
| React/DOM consumer | ordering, not prohibition — design §14 | after a real desktop renders on real pixels |

## Acceptance criteria for v1

1. Three apps, three different cap sets, one tiled screen, on real framebuffer
   pixels under snemu **and** QEMU.
2. kitsch's policy is a Stitch program; only compose-rasterize-present is native,
   at one call per frame.
3. kitsch holds `WRITE` on the scanout and `READ` on surfaces — **no `Spawn`, no FS
   cap, no send cap to the launcher**, proven by an itest.
4. The itest suite asserts composed cell grids via a scanout tap, plain **and**
   `--scramble`.
5. Any window can be flipped to its telemetry back side without that app knowing.
6. Wedged, killed and refused clients are visibly distinguishable from healthy ones.
7. Input events carry a kernel-stamped `Origin`.
8. Killing kitsch does not end the session.

## Risks

- **A long-running Stitch process is a new regime.** Everything so far runs and
  exits. Accepted deliberately: per-process heap telemetry makes a leak self-reporting,
  and increment 13 makes the failure mode a flicker. But it is the largest unknown here.
- **The interpreter is in the display's TCB.** If Stitch wedges, the screen wedges.
  Increment 13 is the mitigation and should not slip.
- **The always-copy composite** is inherent to compositing, not a tax — but increment
  0's blit number still sets the frame budget, and increment 1 must land before it is
  comfortable.
- **Cells becoming permanent by accident.** The named trigger is the game; the
  protocol variant is already there if it appears.
