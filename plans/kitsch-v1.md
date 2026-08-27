# `kitsch` v1 — the desktop

**Status (2026-08-27)**: 📋 **NOT STARTED — design agreed, zero code.** The design is
[../docs/kitsch-design.md](../docs/kitsch-design.md); this file is the increment
ladder. Increment 0 is a decision and four numbers, not code.

**Scope of v1**: a keyboard-driven tiling desktop on real framebuffer pixels, with
cell-mode surfaces as capabilities, telemetry-derived window furniture, and three
apps. Explicitly **not** in v1: pointer input, pixel surfaces, effects, the widget
framework, the patch view, replay. Each is deferred for a reason recorded in the
design's non-goals.

**Gate**: `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`,
as always. Every increment lands green.

## The shape

Three crates, following the `fs-proto`/`fs-core`/`user/fs` and
`glitch-proto`/`glitch-core` precedent:

| Crate | What | Tested |
|---|---|---|
| `kitsch-core` | tiling tree, layout, focus, damage merging, grid composition. Pure, no I/O. | host, `insta` |
| `kitsch-proto` | surface/tap/input wire types | host |
| `user/kitsch` | the server: holds the display cap, composites, rasterizes, presents, routes input | itest |

Most of the logic is in `kitsch-core` and lands before a single pixel is drawn.

## Increments

### 0 — Four numbers and a decision

No code beyond throwaway benchmarks. The design has three questions whose answers
change what gets built, and this repo's record on estimating them unaided is bad in
both directions.

1. **Full-screen blit cost** under snemu and on QEMU, at the candidate resolution.
   The always-copy trade is permanent (design §4); the frame budget follows from
   this number, not from an assumption of 60 Hz.
2. **`view`-per-message cost** — build a tree of N nodes in Stitch, diff it, count
   guest instructions under snemu. Settles whether the framework tier is viable
   before its shape depends on the answer.
3. **Font budget** — 8×16, ASCII + box-drawing, embedded. Confirm the byte cost
   against the itest kernel image's shared budget, which has broken once already.
4. **Resolution**, given QEMU, VF2 and snemu-wasm differ. Sets font size and (1).

**Done when**: four numbers are written into this file and the resolution is chosen.

#### Findings so far (2026-08-27)

**(4) Resolution: already decided — 1024×768 XRGB8888**, no row padding, exactly
3 MiB / 768 frames, mapped at `FB_VA_BASE` (root PTE slot 258). Shipped in
`kernel/src/device/ramfb.rs`. At 8×16 that is a **128×48 cell grid**, which is a
good desktop. No decision needed; inherited.

**(1) Full-screen blit: 8.65M guest instructions.** Measured by disassembling the
emitted riscv code rather than by timing — a claim about what a compiler emits is
empirical. `Framebuffer::fill_rect`'s inner loop at opt-3 is **11 instructions per
pixel**:

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

786,432 px × 11 = **8.65M instructions per full-screen clear**. For scale, the
profiler's entire 100M-instruction post-boot window has `sched::prepare_switch`, its
busiest kernel function, at 18.2M — so **one full clear costs about half of what the
whole scheduler does across 100M instructions**, and 60 Hz of full clears would be
~519M instr/s.

**This is a naive-code problem, not a physics problem.** `copy_from_slice` on a
4-byte slice did not lower to a word store, and neither bounds check was hoisted out
of the loop. A `u32` store with the checks hoisted is ~2–3 instructions per pixel —
a **4–5× win**, taking a full clear to ~2M. Two consequences:

- Optimising `fill_rect` is a prerequisite, and it is a `kernel-devices` change with
  existing host tests — cheap, and it belongs before increment 4.
- **Damage tracking is not an optimisation here, it is the design.** Full-screen
  presents are not affordable at any interesting rate even after the 4–5×; kitsch
  must present dirty cells.

**Instrument gaps found while measuring** — both worth fixing, neither blocking:

- `cargo xtask snemu profile` boots without `-device ramfb`, so `present()` is a
  silent no-op and the framebuffer never appears in a profile. Needs a way to enable
  ramfb on the profile path before any display work can be profiled at all.
- **Unexplained**: `--record-instret` over the two `framebuffer-*` scenarios differs
  by only 6,692 instructions (20,660,918 vs 20,667,610), when the presenting one
  should be ≥8.65M higher for even a single present. Either presents are not
  happening in the recorded window, or per-scenario instret measures a post-fork
  delta that excludes them (snapshot sharing). **Do not build on either scenario's
  instret until this is resolved** — a wrong instrument agrees rather than errors.

**(2) `view`-per-message and (3) font budget**: not yet measured.

### 1 — Memory objects in the kernel

`Object::Memory { frames, .. }` with `READ`/`WRITE` rights, an ambient `MemCreate`
(matching the existing ambient `MapAnon` / `NotifyCreate` / `EndpointCreate`
pattern), and map/unmap syscalls installing at the rights the held cap carries.

The one genuinely new kernel primitive in this plan, and the completion of an
object set that names endpoints, notifications, replies and sinks but not memory.
Other consumers are already waiting ([glitch-v2-async-ring.md](glitch-v2-async-ring.md),
zero-copy FS reads).

Host-testable: the page-planning and rights logic goes in `kernel-mem`/`kernel-proc`
behind the existing `PtMem` mock; only the MMIO-adjacent glue lives in `kernel/`.

**Done when**: two processes map one object, one read-only, and an itest proves the
read-only mapping actually faults on write.

### 2 — Revocation of a mapped object

Revoking a memory cap means walking another address space's page tables, unmapping,
and shooting down across harts — not clearing a table slot. `mmu::remap` and
`mmu::shootdown` exist; this wires them to the transitive `Revoke` already shipped.

Its own increment because it is where the bugs will live, and because an
unrevokable read cap on a surface is a permanent screen recorder.

**Done when**: an itest revokes a read cap mid-run and the holder faults on next
access; `--scramble` passes.

### 3 — `kitsch-core`: layout, focus, damage, composition

Pure logic, no framebuffer, no syscalls. Master-stack layout only (split-tree is
additive later behind the same `fn layout(&Tree, Rect) -> Vec<(WindowId, Rect)>`
seam).

Composition produces the **cell grid** — a 2-D array of `(glyph, fg, bg, attrs)` —
which is the thing the tests assert on, as text, via `insta`. Colour/attributes go
in a parallel letter-keyed grid.

Master-stack takes property tests directly: for any window count, no overlap, no
gaps, exact coverage of the screen rect.

**Done when**: `cargo nextest run -p kitsch-core` covers layout, focus movement,
damage merging and composition, with snapshot tests that read like source.

### 4 — The rasterizer

Cell grid → pixels. 8×16 bitmap font, embedded. Glyph blitting, clipping, the
cell→pixel mapping — each with its own small pixel tests. The ramfb PPM dump stays
visual proof, not the assertion.

**Done when**: a known grid rasterizes to a byte-identical PPM across runs.

### 5 — The display capability

`Object::Display` plus a cap-guarded present syscall. **The kernel does the copy for
v1** — no framebuffer-mapping machinery, since there is nothing to be fast for yet
and the cap story is identical either way. Mapping the framebuffer into kitsch is a
later optimisation, not a design change.

**Done when**: a process holding the display cap can present a buffer; one lacking
it is refused, and the refusal snitches.

### 6 — `kitsch` presents an empty desktop

The server: holds the display cap, composites `kitsch-core`'s grid, rasterizes,
presents. **Zero clients.** It draws the tiled frame and a status bar populated from
telemetry it already receives — `snitchos.task.<name>.cpu_time_ticks` and friends.

The first screenshot, with no client protocol in existence yet.

**Done when**: booting `workload=kitsch` produces a PPM showing a framed desktop
with live per-task numbers in the bar.

### 7 — Surfaces and the first client

`kitsch-proto`. A client attaches, receives a cell surface backed by a memory object
(`DRAW` only — kitsch keeps `CONFIGURE`), writes glyphs, and commits.

The commit beat is the tearing fix, and it should be **`glitch` v2's ring with a
different payload** — settle the two together rather than inventing a second
protocol ([glitch-v2-async-ring.md](glitch-v2-async-ring.md)).

Verbs are deliberately isomorphic with `glitch`'s: `Attach`, `Commit`, `Tap`,
`Revoke`. Same rights names, same lifecycle. This is what makes a shared crate
liftable when the network stack becomes the third instance.

**Done when**: two static clients render side by side under the tiling layout.

### 8 — The scanout tap, and the test harness it buys

A read cap on the scanout. The itest holds one and asserts the composed grid — so
the tap mechanism is exercised by the suite that verifies it, and every later
increment gets a cheap, readable assertion.

**Done when**: an itest scenario asserts a full grid snapshot from inside the guest.

### 9 — Input

`ConsoleRead` → kitsch → the focused client. Focus movement, window switching. And
the kernel-stamped `Origin::{Hardware, Synthesised(pid)}` field, set where the
interrupt is taken and not writable from userspace — small, and load-bearing for
everything in design §6, §7 and §10.

Push/pull tap modes land here too: pull taps are what agents and thumbnails want,
and they cost nothing until read.

**Done when**: keystrokes reach the focused window only, focus moves, and an itest
asserts an event's `Origin` is `Hardware`.

### 10 — Three apps

- **shell** — the existing shell surface, as a cell client.
- **file browser** — holds an FS endpoint cap; the app that makes the titlebar's
  capability display mean something.
- **tetris** — holds *one* capability, its own surface, and nothing else. The
  least-authority demo: a nearly-empty titlebar beside the file browser's.

Rust, on the paint library. Hand-rolled layout — their layouts are fixed and small.
These three become the concrete customers the widget framework is later designed
*against* rather than *for*.

**Done when**: all three run concurrently, each with its own cap set, and the itest
grid snapshot shows the desktop.

### 11 — The back of the window

Flip any window to see that process's metrics, capability set, liveness and recent
refusals — composed by kitsch from the frame stream, with no cooperation from the
app. It cannot decline to have one, cannot lie on it, and need not know it exists.

No new mechanism; it is a view over data kitsch already receives. Highest
value-per-line in the plan.

**Done when**: the flip gesture works on all three apps, including tetris, which was
never modified.

### 12 — Failure states

Liveness derived from the kernel's frames, never self-reported: deadline missed and
by how much, spinning, **blocked on a `Call` to a named process**, refused a
syscall, OOM. Plus **tombstones** — a dead window keeps its last frame, greyed, with
the exit reason on the border, until dismissed.

**Done when**: a workload that wedges one client, kills another and refuses a
syscall in a third produces three visibly different windows in one grid snapshot.

### 13 — The launcher, and the topology

`init → launcher → apps`, with kitsch *beside* them. The launcher holds spawn
authority; kitsch holds no send cap to it and therefore cannot ask it for anything.
kitsch draws the launcher; it does not call it.

**Done when**: apps are launched from inside the desktop, and an itest proves kitsch
holds no `Spawn` authority and no send cap to the launcher.

## Deferred, with reasons

| Deferred | Why | Trigger |
|---|---|---|
| Pointer input | tiling is drivable by keyboard; the model already covers pointers | when drag-to-grant (design §10) is built |
| `Surface::Pixels` | the variant is in the protocol from day one | the first app that needs real pixels — a Minecraft/Factorio-class game |
| Effects | fixed enum, a day's work, nothing needs them yet | the first accessibility or focus-dimming need |
| The widget framework | multi-year; must be designed against real apps | after increment 10 gives it three |
| Patch view + wiring | needs pointer, and the keyboard version needs the trusted path | after pointer |
| Trusted path + provenance-gated consent | needed by the launcher confirm *and* by wiring — two customers | with the launcher's first authority-spending dialog |
| Trace context across IPC | action provenance (design §7); [../docs/supervision-design.md](../docs/supervision-design.md) also wants it | when "what did the agent cause" is asked in anger |
| OS-level replay | a consequence of capabilities, not a feature (design §12) | after taps and inserts stabilise |
| Split-tree layout | additive behind the existing seam | when master-stack is the thing complained about |
| React/DOM consumer | ordering, not prohibition — see design §14 | after a real desktop renders on real pixels |

## Acceptance criteria for v1

1. Three apps, three different cap sets, one tiled screen, on real framebuffer
   pixels under snemu **and** QEMU.
2. Every surface is a memory object; a read-only tap is enforced by the MMU, and
   revoking it faults the holder.
3. kitsch holds `WRITE` on the scanout and `READ` on surfaces — **no `Spawn`, no FS
   cap, no send cap to the launcher**, proven by an itest.
4. The itest suite asserts composed cell grids via a scanout tap, and passes plain
   **and** `--scramble`.
5. Any window can be flipped to its telemetry back side without that app knowing.
6. Wedged, killed and refused clients are visibly distinguishable from healthy ones.
7. Input events carry a kernel-stamped `Origin`.

## Risks

- **Memory-object revocation** (increment 2) is the hardest single piece and the
  most likely source of subtle bugs. It is early on purpose.
- **Stitch's runtime maturity moves onto this plan's critical path** if the app
  story becomes Stitch-first — the per-run env/closure leak, the unclaimed ~20×
  release build, natives needing syscall backing. v1 sidesteps it by writing the
  three apps in Rust, but the framework tier cannot.
- **The always-copy ceiling** (design §4) is permanent. Increment 0's first number
  decides whether it is comfortable or tight.
- **Cells becoming permanent by accident.** The named trigger is the game; if no
  such app appears, cells were right, and if one does, the protocol variant is
  already there.
