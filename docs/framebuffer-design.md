# Design: a framebuffer that snitches

**Status**: Vision / design note (unbuilt). Infrastructure milestone. Surfaced as
the load-bearing prerequisite for the [physics desktop](physics-desktop-design.md),
but it stands on its own — a screen is something many future SnitchOS features will
want, and on this OS a screen is not a black box.

## The thesis

Every other OS treats the framebuffer as a dumb slab of bytes a compositor scribbles
into. Nobody knows who drew what, nobody can enforce where a process is allowed to
draw, and the whole present loop is invisible. That is exactly the kind of
un-principled primitive SnitchOS exists to re-derive.

The claim of this note: **a display server is a capability-mediated, fully-observable
service like everything else here.** Output authority is the mirror of the input
authority we already have — you can't `Send` to an endpoint you lack a cap for; you
shouldn't be able to *draw a pixel* outside a rectangle you lack a cap for. If that
holds, we get least-authority enforced at the granularity of a single scanout
rectangle, on the wire, in Tempo. I don't know of another system that does that.

## The device choice forks the browser dream

There are two realistic framebuffer devices on QEMU `virt`, and the choice trades off
along the axis we care most about: **[snemu](../plans/snemu-lockstep-native-ops.md)
parity → SnitchOS-in-a-browser-tab.**

| | **ramfb** (via fw_cfg) | **virtio-gpu** |
|---|---|---|
| Setup | Write one config struct to fw_cfg → get a linear framebuffer | Full virtqueue lifecycle: `GET_DISPLAY_INFO` → `RESOURCE_CREATE_2D` → `ATTACH_BACKING` → `SET_SCANOUT` → `TRANSFER_TO_HOST_2D` → `RESOURCE_FLUSH` |
| Code cost | Tiny (no virtqueues, no resource table) | Real — but it's the **same virtio muscle** we already have from virtio-console |
| Fidelity | A QEMU-ism; not real hardware | Faithful; the path real hardware/compositors use |
| **snemu cost** | **~50 lines**: model as "a guest-RAM region + a pixel format"; each present, copy → `ImageData` → `putImageData` on a `<canvas>` | Model the whole control-queue protocol + resource table |

The counterintuitive call: **ramfb wins the MVP**, precisely because it's the device
snemu can blit to a canvas cheaply. SnitchOS-in-a-browser-tab is a stated dream, and
determinism/replay is the whole discipline — ramfb is the device that reaches an
audience. virtio-gpu is the v2 "grown-up" path once we want real-hardware fidelity,
and it's not a new device *class* to learn: it's the virtio-console driver's
virtqueue/descriptor dance (plus the `va_to_pa` and TX-staging lessons) applied to a
new device.

> **Action item for the snemu track**: a ramfb device model is now on snemu's critical
> path. It's the cheapest possible display device to emulate and it's what lets the
> physics desktop render into a browser canvas. See [snemu progress](../plans/snemu-lockstep-native-ops.md).

## The framebuffer is just a big, persistent DMA buffer

Nothing about the memory model is new. A 1024×768×4 framebuffer is 3 MiB of
**contiguous physical frames**, mapped into the linear map (`LINEAR_OFFSET`) for CPU
draws and handed to the device as a **physical address**. That is exactly the DMA
discipline already documented for the four virtio-console sites: `mmu::va_to_pa` at
the device boundary, the linear-map lens for CPU access, the staging-buffer gotcha for
anything whose VA isn't in the `KERNEL_OFFSET` range. The framebuffer is that
discipline at 3 MiB and persistent instead of a few KiB and transient — not a new
memory story, the existing one scaled up.

## What makes it *snitch* (the actual point)

Three things, none of which any other display server does:

**1. Damage rectangles as provenance.** Damage tracking is normally a perf trick —
only re-blit what changed. Here it's an **information-flow record**: every frame emits
"region (x,y,w,h) was dirtied by process P." Who drew which pixels is on the wire.
Reconstruct any frame's authorship from the frame stream.

**2. Cap-bounded scanout → capability-mediated compositing.** A process holds a
`Scanout{rect}` cap and **cannot draw outside its granted rectangle.** A blit past the
edge is a `SyscallRefused` frame and a bumped counter — *not* a corrupted neighbor.
This is the pixel-level version of "a yeeted file bounces off a wall it has no cap
for": input authority and output authority become the same principle at the top and
bottom of the stack.

**3. Present is the heartbeat.** The present loop emits its own telemetry:
`snitchos.display.frames_presented_total`, `present_latency`, dropped-frame counts.
And `RESOURCE_FLUSH` (or the ramfb equivalent) is the **determinism boundary** —
fixed-step draw → present → repeat. Record the input stream, replay the exact screen,
bit-for-bit. The display server has a heartbeat like every other subsystem.

## The compositor is a capped userspace process

The compositor isn't the kernel. It's a userspace service (per the
[actor model](userland-text-streams-and-the-actor-model-design.md)) holding a
`Framebuffer` cap and minting badged `Scanout{rect}` caps to the windows it manages.
Consequences that fall out for free:

- **The WM's own authority is inspectable.** It only touches the framebuffer because
  it holds the cap; revoke it and the screen freezes — observably.
- **A "screen recording" is a real, revocable cap** — a `Scanout`-read grant, audited
  like any other, not an ambient superpower.
- **Multiple compositors / nested displays** are just multiple holders, the same way
  multiple clipboards fell out of caps in the [clipboard design](clipboard-design.md).

## Input is the matching half

Output is only half a display. The pointer/keyboard side is **virtio-input**
(evdev-like) — and, once more, it's the same virtio family, so it's incremental over
what virtio-console already taught us. For the MVP, `ConsoleRead` can stand in for a
crude input stream and virtio-input is deferred. Input events are cap-gated and
observable on the same terms as everything else (an `Input` cap; events as frames),
which is what makes deterministic replay of a whole session possible — the recording
*is* the input frame stream.

## MVP: a deliberately dumb compositor (Milestone 0)

Resist fusing this with the physics engine. Ship the framebuffer as its own win with a
compositor that does nothing clever:

1. **ramfb bring-up.** fw_cfg handshake, linear FB mapped (linear-map for CPU,
   PA to the device), clear to a color, present. First pixel on screen.
2. **Present loop + determinism.** Fixed-step draw→present. Move one rectangle around
   under `ConsoleRead` input. Prove it's tear-free and replayable.
3. **Damage + scanout snitching.** Emit damage-rect provenance frames and
   `display.*` metrics. Enforce a `Scanout{rect}` cap: an out-of-bounds blit refuses
   and snitches. This is the whole thesis, demoable, with **zero physics**.
4. **(snemu)** ramfb model → the same dumb compositor renders in a browser canvas.

Only after Milestone 0 lands does the [physics desktop](physics-desktop-design.md)
have a screen to live on.

## Open questions

- **Double-buffering / tear-free present.** ramfb is single-buffered by nature; do we
  draw to a shadow buffer in the linear map and copy on present, or accept tearing for
  the MVP? (virtio-gpu's `RESOURCE_FLUSH` gives a clean fence; ramfb doesn't.)
- **Resolution / format discovery.** ramfb lets us *set* a mode; virtio-gpu makes us
  *ask*. Pin a fixed mode for the MVP and revisit with virtio-gpu.
- **Where does the shadow buffer live** — kernel heap, a dedicated FB window (a new
  root-PTE slot, like the heap and guard-page windows), or userspace-owned pages the
  compositor maps? The cap story is cleanest if the compositor owns them.
- **Cost of `Scanout` enforcement.** Per-blit rect clipping in the kernel vs a trusted
  compositor that self-clips. Enforced clipping is the honest version; measure it.
