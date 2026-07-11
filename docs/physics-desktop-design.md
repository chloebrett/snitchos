# Design: the physics desktop

**Status**: Vision / design note (unbuilt). A candidate showcase feature that falls
out of the OS's existing convictions rather than a new mechanism. Depends on the
[framebuffer milestone](framebuffer-design.md) as a hard prerequisite — a screen has
to exist before objects can live on it.

## The thesis

A window manager where, underneath it all, is a 2D physics engine. Windows are rigid
bodies. Files are objects you can pick up, stack, and *yeet* across the screen.
Messages between processes are little masses that fly out of one window and thud into
another. The money shot: **watch an IPC `Send` as a physical object crossing the
address-space boundary and landing in the receiver's queue** — and see that same event
as a `Send` frame in Tempo.

The reason this is a SnitchOS artifact and not just a WM that happens to run here:
BumpTop and Compiz did physics-as-garnish. This OS already has the two ingredients that
make physics *mean* something — **everything is a telemetry frame** and **everything is
cap-mediated** — so the physics can be a *truthful projection of real system state*,
not decoration. The engine is the missing third thing: a lens.

**If the projection is faithful, the desktop is an instrument. If it's decorative,
it's a toy that could run anywhere.** That distinction is the whole design.

## The mapping (physical quantity → real system state)

The table is the idea. Everything else is architecture.

| Physical quantity | What it *is* on SnitchOS |
|---|---|
| **Mass** of a body | Memory footprint — a file's size, a process's RSS. Heavy things resist being flung. |
| **Velocity / momentum** | Throughput — a busy endpoint radiates fast-moving bodies. |
| **Friction / drag** | Backpressure — when a queue fills, incoming bodies *decelerate and pile up* against it. |
| **Gravity well** | An attractor — a process pulls its own messages toward it; the scheduler's current task pulls harder. |
| **Collision / absorption** | An IPC delivery — a body reaches an endpoint and is absorbed (or bounces if refused). |
| **Invisible walls** | The capability graph — you can't fling into a window you hold no cap for. |
| **A body going limp / falling** | Process exit — the well vanishes, the window is reaped and swept off-screen. |
| **Global gravity ramping up** | Memory pressure — the whole world gets heavier as the heap nears its ceiling. |
| **The world expanding** | Heap growth — a watermark grow literally enlarges the space. |

None of these need new telemetry. They're re-renderings of frames the kernel
*already emits* — `Send`, `CapEvent`, heartbeat metrics, `Exit`, `heap.grow_total`.
The physics engine is a **subscriber to the frame stream**, not a new source of truth.

## Yeeting is an act of authority

This is the part that's deep on *this* OS specifically. A fling isn't cosmetic — it's a
syscall, and the physics obeys the same rules the kernel does:

- **You cannot fling into a window you lack a cap for.** The throw arcs over and
  *bounces off an invisible wall*. Authority becomes collision geometry — you can
  literally *see* a process lacking least-authority. This is the same principle as the
  framebuffer's cap-bounded scanout, now at the level of gestures instead of pixels.
- **The gesture encodes intent.** A hard throw = *move* (transfer the cap); a gentle
  lob = *copy* (mint a badged `SEND`). Momentum maps to semantics.
- **Every fling emits the frame it already would.** A yeet that lands *is* a `Send` /
  `CapEvent::Transferred`. The physics and the audit trail are the same event — you're
  not instrumenting the WM, the WM's actions *are* the instrumentation.
- **Stacking = composing authority.** A pile of files is a bundle; handing the pile
  over is a batched delegation — exactly the `Spawn` `[u32;N]` handle-array semantics,
  made physical.

## The recursive-delight move: the physics is itself observable

The engine emits spans too. So a body's *physical journey* — "spawned at endpoint A,
bounced off window B, absorbed by process C" — shows up **in Tempo as a trace.** You
get a trace view of a bounce. The compositor snitches on itself, and the desktop
becomes yet another thing you can watch on the wire. That is the SnitchOS thesis
applied to the SnitchOS thesis.

## It wants to be deterministic — and the discipline already exists

A physics desktop is a fixed-timestep simulation. The kernel just landed
[lockstep-preserving native memops](../plans/snemu-lockstep-native-ops.md), and snemu
is a deterministic replayable emulator. Same discipline, one layer up.

The payoff: **record the input stream, replay the entire desktop session bit-exact** —
every fling, bounce, stack, and window shove. Debugging a window manager by scrubbing
a deterministic timeline is a genuinely rare capability, and here it falls out of
constraints the project already imposes on itself for free. "Snitch on your own
gestures."

The present fence from the [framebuffer](framebuffer-design.md) is the timestep
boundary: `integrate → resolve collisions → damage → present → repeat`.

## The three depths (where it stops being a WM and starts being the OS)

**1. Backpressure you can feel.** Grafana shows you a queue depth as a number. Here a
full endpoint *physically stops absorbing bodies* — they decelerate, collide, and pile
against it. Congestion becomes tactile. The same signal, but you feel it in the motion
before you'd ever read it on a dashboard.

**2. Least-authority as spatial geometry.** The capability graph isn't a diagram in a
doc — it's the walls and wells of the room you're in. A process with narrow authority
lives in a small box that most things bounce off of. Watching someone *fail* to fling a
file where they lack a cap is watching access control happen, live.

**3. Scheduling as orbital mechanics.** Tasks are bodies; `cpu_time_ticks` accretes as
mass; a context switch is one physics tick. The scheduler you already trace across
context switches ([post 12](../posts/post-12-the-kernel-takes-turns.md)) now has a
physical body per task, and the `ContextSwitch` frames are the forces moving them.

## Architecture sketch

- **The engine is a capped userspace service.** It holds a `Framebuffer` cap (draws via
  the compositor's `Scanout` grants) and subscribes to the frame stream to source
  masses/wells/collisions. It is *not* in the kernel — it's an actor like the FS server.
- **Bodies are backed by kernel objects, not invented.** A body is a *view* of a
  process, file cap, or in-flight message. The engine doesn't own truth; it renders it.
  A body with no backing object is a bug (a leak you can see).
- **Input** comes from virtio-input (or `ConsoleRead` for the MVP) as cap-gated,
  observable events — which is what makes the whole session replayable.
- **Integration**: Verlet or semi-implicit Euler, fixed timestep, simple AABB/circle
  collision. Nothing exotic — the novelty is the *mapping*, not the solver.

## MVP: prove the mapping in ASCII first (optional pre-framebuffer spike)

The framebuffer is the real target, but the *mapping* can be proven in a character-cell
sandbox first, reusing the Stitch renderer aesthetic already built (box-drawing, emoji, unicode-width): bodies are glyphs with
sub-cell float positions, IPC messages are `●` with mass = payload size, windows are
box-drawn rects. It screenshots beautifully, needs no new hardware, and de-risks the
one hard question — *is the telemetry→physics mapping actually legible?* — before any
GPU work. Then the same mapping renders for real on the framebuffer.

Recommended order:

1. **(optional) ASCII spike** — prove `Send`-as-flying-`●` and backpressure-as-pileup
   are legible in a terminal cell grid.
2. **[Framebuffer Milestone 0](framebuffer-design.md)** — a screen that snitches, dumb
   compositor.
3. **Bodies from the frame stream** — processes/files/messages become rigid bodies;
   masses/wells sourced from real telemetry. Read-only: watch the system as physics.
4. **Yeeting** — input-driven flings become `Send`/`CapEvent`, cap-bounded (bounces off
   walls you lack authority for).
5. **Determinism** — record the input stream, replay the session.

## Open questions

- **Legibility.** Is a physics view actually *readable* as system state, or does it
  become noise at real message rates? The ASCII spike answers this cheaply before GPU
  cost is sunk.
- **Rate mismatch.** IPC happens thousands of times a second; physics runs at ~60 Hz.
  Do we sample, batch bodies, or model only "interesting" (user-initiated) messages as
  distinct bodies and aggregate the rest into flow fields?
- **Where the engine draws the authority line.** Does the kernel enforce that a fling
  becomes a `Send` (trusted engine self-reports), or is the engine untrusted and every
  fling a real cap-checked syscall? The honest version is the latter — the engine holds
  no more authority than the caps flung through it.
- **Input semantics.** How does a continuous drag/throw gesture map onto discrete
  cap operations without accidental sends? (A "commit zone" / release threshold.)
- **Interplay with [stim](stim-design.md) and the actor model.** A window
  is a process is an actor with a typed interface — does a body carry the process's
  `user.iface` schema, so *what you can do by flinging into it* is type-checked?
