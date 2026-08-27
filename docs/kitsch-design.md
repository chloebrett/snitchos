# Design: `kitsch` — the desktop

**Status**: Design note (unbuilt). Supersedes the display half of the
[physics desktop](physics-desktop-design.md), which is parked — the physics
mapping was an exploration, and the ideas of it that survive (authority as
geometry, backpressure you can see) arrive here without a solver. Hard
prerequisite: [framebuffer milestone 0](framebuffer-design.md), which shipped.
Hard prerequisite that has *not* shipped: memory-object capabilities, §3.

Implementation increments: [../plans/kitsch-v1.md](../plans/kitsch-v1.md).

## 1. The thesis

A desktop is the place where a user's authority is spent. Every other system
draws that surface first and reasons about authority afterwards, which is why
screen recording, input injection, and "which app drew this dialog?" are all
retrofits that don't quite work.

kitsch inverts it. **A window is a process; a surface is a capability; drawing a
wire between two windows is granting a cap; and the compositor is not privileged
code — it is simply the process holding a write cap on the scanout and read caps
on everything else.** Everything the desktop can do falls out of that sentence,
and — importantly — everything it *cannot* do falls out of it too.

Three consequences worth stating up front, because they're the reasons to build
it rather than a tiling WM that happens to run here:

- **An agent can hold authority over one window.** Not the screen, not the
  keyboard — one window, attenuated, revocable, and every action it takes is
  already a frame on the wire. Multiple agents can therefore drive different
  windows concurrently without seeing each other's.
- **Liveness is externally observed.** kitsch never asks a client whether it is
  responding; it reads the kernel's frames. A client cannot lie about being
  healthy, and "hung" is distinguishable from "slow" and from "blocked on the FS
  server" — by name.
- **Every window has a back side.** Flip it and you see that process's metrics,
  capabilities, blocker and provenance — composed by kitsch from the frame
  stream, with no cooperation from the app, which therefore cannot decline to
  have one or lie on it.

## 2. The device-server skeleton

kitsch is the second instance of a shape this OS keeps building: a server owning
one exclusive output, mediating many contributors. `glitch` is the first; the
network stack ([vf2-gmac-design.md](vf2-gmac-design.md)) will be the third. The
skeleton is stated here because it is reusable, and because the axes below are
what stop it becoming "everything is a multiplexer", which is true and useless.

**Common to every instance:**

- one server holds the sole master-output cap
- contributors hold attenuated, badged, revocable caps — content only, never
  configuration
- taps are read caps; inserts are read+write caps
- a server-owned parameter set the contributor cannot override
- the server nests inside itself (a group bus is a mixer; a nested kitsch is a
  window that is a desktop)
- revocation works on a live holding ([cap-revocation-design.md](cap-revocation-design.md))

**Divergent, and the axes that decide it:**

| Axis | UART | Network | Console (stream) | Audio (`glitch`) | **kitsch** |
|---|---|---|---|---|---|
| How contributions combine | interleave | interleave, by flow | interleave | superpose (sum) | **superpose (by region)** |
| Output is state or stream | stream, sparse | stream, sparse | stream | stream, dense | **state** |
| Can the sink refuse? | yes — backpressure | yes — backpressure | yes | **no — deadline** | **no — deadline** |
| What a contributor owns | nothing; arbitration | a flow | nothing | the same samples | **a region** |

Three things follow that are easy to get wrong:

- **Interleaving and superposition are different problems.** Server-applied
  effects the contributor cannot defeat are meaningful under superposition and
  mostly wrong under interleaving — rewriting someone's bytes in transit is a
  proxy, not a mux. Only audio and display want them.
- **State-vs-stream decides whether damage tracking exists.** A framebuffer has a
  current value, so diffing what changed is coherent; audio has no current value,
  only a next sample, so it has no analogue of damage at all. Anything on the
  *state* side gets damage and idempotent present; anything on the *stream* side
  gets sequencing and backpressure.
- **Deadline-vs-backpressure decides the failure discipline.** Where the sink can
  refuse, a slow contributor blocks and correctness holds. Where the sink has a
  deadline, a slow contributor is *substituted for* — silence, or the previous
  frame. This, not the payload type, is why `glitch` and kitsch must not share a
  mixing engine: the audio callback may not allocate or lock, and holding kitsch
  to that would be a serious over-constraint.

**What is shared, then, is the skeleton and the vocabulary — not the engine.**
The two protocols should use the same verbs (`Attach`, `Commit`, `Tap`,
`Revoke`), the same rights names and the same lifecycle states even where the
payloads are unrelated. If they do, a shared crate can be lifted when the third
instance proves the shape. If they diverge on naming for no reason, it never
happens. Extraction waits for the third instance; this is the repo's own
precedent (the `kernel-core` split happened after the grab-bag existed).

## 3. Memory objects — the kernel prerequisite

Today `MapAnon` is the entire memory story: a process can get fresh anonymous
pages and nothing else. There is no way for two processes to see the same frames,
which means a "read-only cap on someone's surface" could only ever be a promise
kitsch keeps, not a thing the MMU enforces.

So kitsch requires a new object:

```
Object::Memory { frames, .. }     rights: READ | WRITE
```

with map/unmap syscalls that install it into the caller's address space at the
rights the held cap carries. Then a read-only surface tap **is** a read-only
mapping, and none of the guarantees in this document depend on kitsch's good
behaviour.

This is the natural completion of the object set, not a special case for the
desktop: the cap system today names endpoints, notifications, replies and sinks —
but not *memory*, which is the one authority every process obviously has. Other
consumers are already waiting: `glitch` v2's ring, zero-copy FS reads, and pixel
surfaces later (§5).

Two hard parts, both real:

- **Revocation of a mapped object** means walking another address space's page
  tables, unmapping, and shooting down the TLB across harts — not clearing a
  table slot. `mmu::remap` and `mmu::shootdown` exist, so the pieces are there,
  but this is where the bugs will live, and an unrevokable read cap on a surface
  is a permanent screen recorder.
- **Shared memory breaks input enumerability**, which matters for replay (§12).
  A surface written by its client and read by kitsch is an *output* and safe; an
  insert makes it an input channel that a replay log doesn't capture.

## 4. Surfaces, taps, effects

**A surface** is a memory object plus the geometry and attributes kitsch keeps.
The split of rights is the crux:

| Right | Held by | Meaning |
|---|---|---|
| `DRAW` | the client | write content into the surface's pages |
| `CONFIGURE` | **kitsch only** | position, size, visibility, stacking, effects |
| `READ` | taps | read the surface's content |

The client's cap is *attenuated* — kitsch mints it and keeps `CONFIGURE`, the
same pattern the FS server uses for badged file endpoints
([filesystem-design.md](filesystem-design.md)). If the rights bit were simply
`WRITE`, a client could move itself and the property is lost by accident.

**Effects** (greyscale, dim, tint) are a fixed, kitsch-owned enum. The client
cannot defeat them because it never holds a cap on the scanout — it writes its
own buffer, kitsch reads, transforms, and writes the framebuffer.

> **The cost, stated as chosen rather than discovered: kitsch always copies.**
> Zero-copy direct scanout from a client buffer is foreclosed *forever*, because a
> surface scanned out directly is a surface that cannot be greyscaled. This is a
> permanent performance ceiling accepted in exchange for the guarantee.

**Taps** come in two modes, and both are needed:

- **Push** — wake me on every commit. The scanout, a recorder.
- **Pull** — I will ask when I want one. An agent, a thumbnail, a test.

A pull tap costs nothing until read. This generalises into the rendering rule
(§5) and answers "what about hidden windows": **rendering is driven by taps, not
by visibility.** A window on another workspace still renders if an agent holds a
tap on it; the user's eyes are just the tap attached to the scanout.

**Commit.** Shared pages plus a client that writes whenever it likes equals
tearing, and the cap model cannot help — the client legitimately holds `DRAW`. So
there is a protocol beat: double-buffer with an explicit flip, or a `Commit`
naming a coherent damage rect. This is `glitch`'s ring with a different payload,
and the two should be settled together
([../plans/glitch-v2-async-ring.md](../plans/glitch-v2-async-ring.md)).

**What kitsch is, restated:** the process holding `WRITE` on the scanout and
`READ` on every surface. Not privileged code — a cap set. Which is why a second
compositor is possible and a nested one is trivial: the inner kitsch's "scanout"
is an outer surface, and nothing in the model changes.

### Uses for read caps

Not a curiosity — the mechanism has more customers than the thing it was invented
for, and each is a feature every other OS retrofitted badly:

| Holder | What it is |
|---|---|
| kitsch | how compositing works at all |
| Screenshot / recorder | no ambient authority, no meaningless consent dialog — **and the grant is a `CapEvent` on the wire, so you can see who is watching your screen** |
| Window switcher, thumbnails | a previewer must see windows; it holds taps, visibly, rather than being privileged |
| Magnifier, accessibility | read, never write |
| Remote desktop | a process holding a tap on the scanout. That's the whole feature |
| itest | a scenario holds a tap and asserts the grid (§13) |

A **read+write tap is an insert** — a process in the signal path. Remote assist,
pair-programming, an overlay debugger, an IME (§6), or an agent driving a window.

## 5. Three projections

An app's UI has one model and several renderings, produced **only for projections
somebody holds a tap on**. Nobody holding a cell tap means cells are never
computed; an agent attaching means the app starts emitting text, and because
attaching is a `CapEvent`, you can watch a projection begin because someone asked
for it. Emitting three layers is therefore 1× the work plus whatever is actually
being read.

| Surface | What a tap sees | Who wants it |
|---|---|---|
| **Typed** | accepted operations, as a [hitch](typed-processes-and-the-data-model-design.md) schema | agents, scripting, testing, the patch view |
| **Cells** | glyphs and attributes | agents, a11y, search, snapshot tests |
| **Pixels** | samples | images, plots, games, video |

These are **not fidelity tiers**. A CPU chart's honest cell projection is not
ASCII art — it is `[chart: cpu 60s, 12pts, last 87%]`, which for an agent, a
screen reader and a test is *better* than the pixels. Pixels give fidelity, cells
give legibility, the typed layer gives operability.

This is the same move as one `Frame` stream becoming OTLP traces and Prometheus
metrics, and one diagram model becoming several targets
([diagrams-design.md](diagrams-design.md)). One model, many projections.

**Ground truth.** Only the scanout is what the user actually saw. The cell and
typed layers are the app's *claims about itself*; a cooperative app cannot make
them disagree (they come from one tree in shared library code), a hostile one can.
So an agent working from the typed layer trusts the app, and an agent working from
the scanout trusts nobody. Both are legitimate; the difference must be explicit in
the protocol rather than discovered.

**A related hazard:** effects are applied after the client draws, so a tap on a
*surface* sees pre-effect content while the user sees post-effect. If kitsch dims
a window or overlays a warning, an agent tapping the surface never sees it and
acts on a different reality. Agents should prefer the scanout tap, and the
difference should be named in the protocol. The inverse is worth building: **a
window showing what the agent sees**, which is both a debugging and a trust
surface.

### Why cells first, and what forces pixels

The original argument for cells (no shared memory, so 1.2 MB/frame cannot cross
IPC) dissolves once §3 lands. The surviving argument is better: **pixels are
opaque and cells are legible.** An agent tapping pixels must do vision; an agent
tapping cells reads text. Same for accessibility, for search-across-the-screen,
for text selection spanning windows, and for the test suite. This is precisely why
every mainstream OS bolts an accessibility tree onto the side of its pixel
pipeline, and why those trees are perpetually wrong.

Cells genuinely cost: images, plots, anti-aliased and proportional text, multiple
font sizes, sub-cell positioning, animation, video. That is the difference between
a desktop and a very good tiling terminal, and the risk is that cells become
permanent by accident.

**So the trigger is named: the first app that needs real pixels forces
`Surface::Pixels`.** Concretely — a Minecraft- or Factorio-class game in Rust.
The variant is in the protocol from day one so that day is additive, not a
rewrite. Effects apply to both and mean different things: a colour transform on
pixels, an attribute rewrite on cells.

### Why this is not a TUI

A TUI is one process writing a byte stream to one terminal with control **in
band** as escape sequences — the conflation behind essentially every terminal
security bug, and the reason a terminal cannot say who drew what. A cell surface
is out-of-band and structured: many processes, each with its own mapped buffer,
cap-mediated, composited by a server that knows the grid's structure and each
cell's author. It looks like a TUI and is architecturally its opposite.

A bonus falls out: because the client protocol is cells rather than pixels, **the
same app runs unchanged on a framebuffer session, a serial session and a remote
session.** Not available if surfaces are pixels.

## 6. Input authority

The output side is only half a window manager. Focus is routing, and input is
authority, so the two are designed together — if input lands later it lands
asymmetric.

One exclusive source, demultiplexed to many consumers, with kitsch deciding who
receives. The taps invert exactly:

- **A read tap on the input stream is a keylogger.** The mirror of screen
  recording, with the same win: it cannot be ambient, and every grant is visible.
- **An insert on the input stream is an input method.** An IME consumes raw keys,
  holds state across several, and emits committed text plus a preedit string —
  which also needs a surface of its own for the candidate list, so it composes out
  of two existing mechanisms. On every mainstream OS an IME sees *every keystroke
  in every application*, which has been a recurring security embarrassment; here
  it is a cap on one window. Macro expanders, accessibility remappers and agents
  are the same shape.
- **Synthesising input is a right**, not a global permission.

### Provenance is kernel-stamped

Every input event carries an origin field set **where the interrupt is taken**,
not writable from userspace:

```
Origin::Hardware | Origin::Synthesised(pid)
```

kitsch cannot forge it, because kitsch does not mint the event. This is small —
a few bits — and load-bearing for §8 and §10.

### The compositor-injection hole, and the trusted path

kitsch routes input, so kitsch can synthesise keystrokes into any client and
thereby wield that client's authority. **A compositor that can inject input has
every authority any of its clients has.** This is true of every real system and
it is the actual hole behind "who may launch programs".

Two mechanisms close it, and both are cheap here:

1. **Provenance-gated consent.** A holder may require `Origin::Hardware` for the
   keystroke that confirms an authority-spending action. Synthesised input still
   works for agents and IMEs, and is marked, forever, wherever it goes.
2. **A path that does not traverse kitsch.** The launcher holds a `Notification`
   ([notification-design.md](notification-design.md)) the kernel signals directly
   on a reserved chord. Hardware to launcher, kitsch not in the circuit — a UI
   element the compositor provably cannot spoof.

### kitsch must not be able to spawn

If kitsch launches applications it needs `Spawn` plus a pile of caps to hand out,
and becomes the most authoritative process on the system.

Instead: **the launcher is not a service kitsch calls, it is a client kitsch
draws.** kitsch holds no send cap to the launcher's control endpoint and cannot
ask it for anything; it shows the launcher's pixels and routes keys to it. The
launcher holds spawn authority, obtains a surface cap from kitsch, and delegates
it to the child along with whatever else that app should get. Topology is
`init → launcher → apps`, with kitsch *beside* them, not above.

The launcher's own power is bounded by per-app **manifests**
([manifest-design.md](manifest-design.md)) rather than being arbitrary — and a
manifest can be checked against the app's declared `Cmd` type (§11), so an app
asking for authority its interface never uses is visible at launch.

## 7. Provenance, in three kinds

| Kind | Question | Status |
|---|---|---|
| **Authority** | how did you get the right to do this? | **shipped** — the cap-id spine records `parent_cap_id`, so `CapEvent::Transferred` frames reconstruct the derivation tree |
| **Action** | what caused this to happen? | the gap — see below |
| **Output** | who drew this pixel? | the [framebuffer](framebuffer-design.md) note's unbuilt "damage as provenance" |

**Action provenance is the trace spine.** A hardware keypress opens a root span;
everything it causes is a child. An agent's synthesised keypress opens a root span
attributed to the agent. "What did the agent cause" becomes a Tempo query, and
causality-across-processes is exactly what distributed tracing was built for. What
is missing is **trace context propagation across IPC** — a `Call` carrying the
caller's span so the callee's work parents under it, which
[supervision-design.md](supervision-design.md) already lists as unbuilt. Provenance
is causality, causality is a span tree, and this OS already ships span trees.

**Security or observability — the axis that decides the cost.** If a holder may
*refuse* based on cause, the field must be kernel-stamped and unforgeable, or a
compromised intermediate relabels itself. If it is diagnostic, cooperative
propagation is fine. Do both, tiered: the small kernel-stamped `Origin` (§6) is
security-relevant; the rich cooperative trace context is the diagnostic story.
Confusing them is how a system ends up making security decisions on forgeable
metadata.

**Output provenance kills a bug class.** If every damage rect carries its author,
the screen is partitioned by who drew it — so a window that looks exactly like the
launcher but was drawn by something else is *mechanically detectable*, and
combined with the trusted path there can be a "what am I actually looking at"
gesture whose answer kitsch cannot fake. Phishing and clickjacking exist because
no display server knows who drew what.

**Consent conditioned on cause** — allowed if hardware-caused, confirm if
agent-caused — is policy applied by the holder *on top of* the cap check, never
instead of it. The cap decides what is possible; provenance decides what is
prudent. Keeping that crisp matters, because "it holds the cap but we'll allow it
based on a metadata field" is how a capability system quietly becomes an ACL
system.

**The clipboard is the sharpest instance** and shares this spine: a paste is the
canonical provenance event ([clipboard-design.md](clipboard-design.md)).

## 8. The back of the window

Because per-process state is already telemetry, and telemetry goes to the frame
stream rather than to the app's window, **kitsch can render any window's
instrumentation with no cooperation from that app.** It is not a feature apps opt
into; it is a view composed from data kitsch can already see.

So: every window has a back side. Flip it and get that process's metrics, its
capability set, its liveness and blocker, its provenance and its recent refusals.
Flip the whole desktop and get the system. **An app cannot decline to have a back
side, cannot lie on it, and need not know it exists.**

This is the SnitchOS thesis as a single gesture, and it delivers what the physics
desktop was reaching for at a fraction of the cost and with far more legibility.
First-class, not a nice-to-have.

## 9. Failure states

Every other desktop conflates these. kitsch can distinguish them because it reads
the kernel's frames rather than asking the client.

| State | Elsewhere | Here |
|---|---|---|
| Alive, idle | normal | normal — correct |
| Alive, committing late | *identical to normal* | deadline missed, **by how much** |
| Alive, spinning | beachball | running, not blocked, burning CPU |
| Blocked on IPC | beachball | **blocked on a `Call` to a named process** |
| Exited normally | vanishes | exit status |
| Killed by supervisor | vanishes | killed, by whom, why |
| Refused a syscall | *nothing at all* | `SyscallRefused` — it lacks a cap |
| OOM | vanishes or freezes | which allocation failed |

Two are worth building deliberately:

- **"Waiting on `fs`" instead of a beachball.** The kernel knows which endpoint a
  task is parked on. And once windows name their blockers, **a dependency cycle is
  drawable — you can render a deadlock as it happens.**
- **Tombstones.** A dead window does not vanish, because vanishing destroys the
  evidence. It leaves its last frame, greyed, with the exit reason on the border,
  until dismissed; if a supervisor restarted it, the replacement shows its
  lineage ([supervision-design.md](supervision-design.md)).

The property underneath both: **liveness is externally observed, never
self-reported.**

## 10. Wiring is granting

The edges already exist — endpoints are caps, every `Send` is a frame. kitsch
merely isn't drawing them. But the gesture goes further than visualisation.

**Drawing a wire between two windows mints and delegates a capability.** It is
checked twice:

- **Type compatibility** — both ports are hitch schemas, so an incompatible
  connection is refused *at draw time, with a reason*. Dropping a file on the
  audio mixer fails as a schema mismatch.
- **Authority** — you must hold what you are granting, and attenuation only.

Rights are *orthogonal to types*, which is why the gesture has two beats. Dragging
a file onto an editor type-checks as "an endpoint speaking `fs-proto`"; whether it
is `READ` or `READ|WRITE` is the cap's rights bits, and deserves its own moment.
That menu is **bounded above by what you hold** (an option that would fail is
never shown) and **defaulted to what the receiver declared it needs**, with
anything beyond flagged as *more than it asked for*. **Least authority becomes the
default of the gesture rather than a discipline you have to remember**, and
over-granting becomes a thing you are told about.

Direction disambiguates by who is initiating, and therefore by who must consent:

| Gesture | Meaning | Consent |
|---|---|---|
| Drag a resource **onto** a window | granting | the gesture *is* the consent — one motion, defaults to least authority |
| Drag **from** a window's port onto a resource | requesting | the user confirms; it is their authority being spent |

An agent can therefore *request* but never *grant*: granting requires
`Origin::Hardware` (§6). This is the second feature to need the trusted path,
which is decent evidence it is load-bearing rather than speculative.

**Ports are derived, not hand-drawn.** The `Msg` variants that accept endpoints
are input ports; the `Cmd` variants that need them are output ports (§11). An app
cannot advertise a port it cannot use.

**Cutting a wire is revocation** — transitive, so kitsch can show what *else* the
cut will break before you commit. Nothing today can answer "what breaks if I take
this permission away".

### The patch view

Wires are **not** persistently drawn — that is spaghetti, and every patcher UI
learns it the hard way. The working gesture is a drag between the actual windows,
during which compatible drop targets light up and incompatible ones stay visibly
inert. Transient affordance rather than permanent clutter — and that highlight is
doing real work, because it shows you the reachable set. It is the physics note's
"authority as collision geometry", arriving without a physics engine.

The **patch view** is then a separate, zoomed-out mode for audit and revocation,
not for work. Precedents worth taking from node-graph editors (`noise_gui`,
Max/MSP, Blender's shader nodes):

- **Live content in every node** — the patch view is the desktop zoomed out with
  real window content and real traffic on the wires, not a schematic of labelled
  boxes. Free here, because a thumbnail is a pull tap.
- **Composites** — collapse a subgraph into one node. Which is simultaneously the
  nested-compositor idea and the manifest, arriving from a third direction.
- **A committable text format** for a saved graph. **A workspace is a patch is a
  manifest**: a set of processes plus the cap graph between them.
- **Annotation** — a *reason* attached to a grant, which is provenance of intent
  rather than of mechanism.
- **The anti-pattern:** `noise_gui`'s implicit type compatibility ("nodes chain by
  function signature") works with one scalar type flowing through. Here connections
  are authority and the schema check is a security boundary, so it must be
  explicit, checked, and with rights orthogonal to types.

Non-window authority (a cap on the clock, on memory, on the telemetry sink) has no
far-end window. Those live on the back of the window (§8), not as edges; the patch
view shows the inter-process subset and should not pretend otherwise.

**Geometry note:** tiles are a rectangle partition, graphs want free placement. So
there are two views — the spatial one you work in, with transient edges for the
focused window only, and the logical patch view you zoom out to.

## 11. The client side: a framework and a library

Two tiers, and the distinction is **inversion of control**, which explains what
each gets rather than merely asserting it.

**Stitch gets a framework** ([language-design.md](language-design.md)), Elm-shaped,
shipped in the stdlib. It owns the loop and calls the app's pure `update`/`view` —
and *because* it owns the loop it can emit all three projections, compute damage
from the tree diff, and enforce that effects happen only through handlers.

The fit is deeper than aesthetics:

- **The `Msg` type is the typed surface.** Not derived from the app — the app's
  own definition of what may be done to it. Serialize it as a hitch schema and
  you have §5's typed projection, §10's ports, and the agent's interface, all from
  one declaration.
- **The Elm triple maps to three hitch types**: `Msg` (accepted messages), `Model`
  (observable state — where the telemetry overlap lands), `Cmd` (effects it
  requests). **`Cmd` and the cap set are two views of one fact** — what it wants to
  do versus what it may do — so they can be checked against each other (§6).
- **A pipe is a degenerate actor**: one input type, one output type, no state. A
  window is the general case. Hitch types the values either way; `~>`
  ([userland-text-streams-and-the-actor-model-design.md](userland-text-streams-and-the-actor-model-design.md))
  is the easy corner.
- **The cost model suits an interpreter.** `view` runs on message, not per frame,
  so an idle window costs nothing and CPU is proportional to *interaction* rather
  than to time. A retained framework in an interpreted language would be a bad
  idea; an event-driven one is a good one.
- **Effect handlers are inserts inside a process; caps are inserts between
  processes.** Same concept at two scales, so a Stitch app is interceptable at
  both — a fake clock via a handler (already how Stitch's native tests use
  doubles) and an agent on its input from outside.

**Rust gets a library** — `kitsch-draw`: rect, text run, line, blit, clip, with
cell and pixel backends. No widgets, no state, no policy. It cannot own or enforce
anything, so a Rust app declares its typed surface explicitly with
`#[derive(Schema)]` rather than getting it free. This is the right tier for the
game, the plot, and tetris.

**One toolkit shipped with the platform is why platforms feel like platforms.**
Mac, NeXT and Windows were coherent because there was one; Linux desktop never has
been because there are five. Putting the framework in Stitch's stdlib is a decision
only available to someone who owns the language.

**Paint has no semantics**, so a raw-paint app emits one projection, not three —
you cannot recover "a button labelled Save" from a rectangle and a text run. That
is correct rather than a gap, and the *absence is visible*: a window offering no
structural projection is one an agent can only drive by vision, readable from the
cap. Universality gets incentivised, not mandated.

> **Scope firewall.** The widget framework is by a wide margin the largest piece
> here, and toolkits are multi-year projects. It is deliberately **deferred**: v1
> ships the paint layer only, and the framework is designed later *against* three
> real apps rather than *for* imagined ones.

## 12. Replay, at two levels

**Framework replay** is cheap and semantic: pure `update` plus a `Msg` log means
replaying messages reproduces state exactly, and you can *jump* to state N. Elm's
time-travel debugging, arriving as a consequence of the shape.

**OS replay** is universal and mechanical, and it is the distinctive one. A
process is replayable if its interactions with the world are **enumerable** — and
on a capability system they are, by construction: *the complete set of things that
can influence a process is its cap set*. A monolithic OS cannot do this cheaply
because ambient authority means the influence set is not enumerable, which is why
`rr` and friends are heroic engineering. Here it is a list already kept.

Record: syscall returns, delivered messages, clock reads, incoming randomness.
Add periodic process snapshots so rewind does not mean replaying from boot — the
shape is familiar from snemu's snapshot tree. **The hazard is shared memory**
(§3): a surface written by its client is an output and safe; an insert makes it an
unlogged input channel.

This upgrades §7's undo: with rewind you do not merely *enumerate* what an agent
did, you can put the process back. Rewinding one process while the world moved on
is inconsistent — but **the provenance tree defines the rewind boundary**, naming
exactly the causally-connected set to roll back together.

So replay is not a feature bolted on; it is a consequence of capabilities.

## 13. Testing

**Assert on the composed cell grid, not on pixels.** The compositor's real output
is a 2-D array of `(glyph, fg, bg, attrs)`; the blit is a pure function of it. The
grid is text, so it snapshots with `insta`, diffs readably, and reviews like source:

```
┌─ shell ────────────┬─ files ───────────┐
│ $ ls               │ > docs/           │
│ docs  kernel  user │   kernel/         │
│ $ █                │   user/           │
└────────────────────┴───────────────────┘
```

Colour and attributes go in a parallel grid keyed by letter (`f` focused border,
`d` dim, `r` reverse) so they are asserted without becoming unreadable.

- `kitsch-core` — layout, focus, damage merging, composition. Pure host tests,
  no framebuffer. Master-stack layout is a pure function of `(windows, params)`,
  so it takes property tests directly: no overlap, no gaps, exact coverage, any
  window count.
- The rasterizer gets its own small pixel tests — glyph blitting, clipping, the
  cell→pixel mapping. The ramfb PPM dump stays visual proof, not the assertion.
- The **itest holds a read cap on the scanout and asserts the grid** — so the tap
  mechanism is exercised by the suite that verifies it.

## 14. Non-goals

Each of these was considered and declined for a stated reason. They are here so
they are not re-litigated.

- **No physics engine.** [physics-desktop-design.md](physics-desktop-design.md)
  is parked. Its good ideas (authority as geometry, visible backpressure) arrive
  as drop-target highlighting and wire activity.
- **No React/DOM renderer in v1 — gated, not forbidden.** Technically easy: the
  typed projection is already a widget tree, so a host-side renderer is a
  *consumer* of an existing tap rather than a new output format apps must produce.
  It doubles as a **semantic remote desktop** — streaming a widget tree instead of
  pixels is bandwidth-trivial and renders natively at the far end, which is what
  X11 was actually for. Two objections, of which only one survives:
  - **The demo-integrity objection is answerable.** The worry was that DOM output
    looks like a web app and everyone will assume it is one
    ([snemu-wasm-design.md](snemu-wasm-design.md),
    [tour-and-user-docs-design.md](tour-and-user-docs-design.md)). But the
    evidentiary burden need not rest on the DOM alone: show it **beside the
    framebuffer canvas, in lockstep from one guest** — which is itself a
    demonstration of §5 rather than a hedge against it — with the hitch schema and
    `Msg` stream visible, `data-drawn-by="pid N"` provenance inspectable in
    devtools, and controls that let a visitor do what no web app survives: revoke a
    cap and watch the window fail, single-step the machine, kill the process and
    watch it tombstone.
  - **The development-gravity objection stands.** The browser path is faster to
    iterate on, better tooled, and needs no emulator — which is precisely how the
    real target rots into an export path. Layout would also be the browser's, so a
    DOM-rendered app is outside the deterministic gate, adding a flaky test tier to
    a suite deliberately built without one.

  Therefore: **ordering, not prohibition.** Admissible once a real desktop renders
  on real pixels, explicitly semantic rather than visually faithful, outside the
  gate, and never as the primary development surface. Costs nothing now; the door
  stays open because the typed projection is in the protocol from day one.

  > **What the ordering preserves: window teleportation.** Speculative, possibly
  > never, recorded so it is not lost. **Popping a window out of the machine and
  > into a browser tab is a grant** — delegate its typed tap to an off-machine
  > consumer and that consumer renders it; revoke to pop it back. No new mechanism:
  > the same cap transfer §10's drag gesture already performs, with a browser as a
  > drop target. The return path is an input insert, which makes a popped-out window
  > **architecturally identical to an agent driving a window** — same two caps, the
  > browser being a human-shaped agent. So the agent work and the pop-out work are
  > one piece of work.
  >
  > Why it is the demo that actually proves §5: press a physical button on a VF2 and
  > watch DOM update, where the page holds *no application code* — it is a dumb
  > consumer of a hitch-encoded tree. Demonstrable by killing the guest process and
  > watching the browser window tombstone (§9). And a tree diff is small enough to
  > be interactive over a serial line where pixels are hopeless, so the transport is
  > already arriving for other reasons ([../plans/uart-telemetry.md](../plans/uart-telemetry.md),
  > [../plans/network-telemetry.md](../plans/network-telemetry.md)).
  >
  > Caveats it inherits rather than introduces: a popped-out view is the app's claim,
  > not ground truth (§5), so it is strictly less trustworthy than a scanout tap; and
  > granting a tap to something off-machine is exfiltration — visible as a
  > `CapEvent`, which means "this window is being rendered off-machine" is
  > *observable*, not a thing that can happen quietly.
- **No pixel surfaces in v1** — the variant is in the protocol; the game triggers
  the implementation (§5).
- **No pointer in v1** — the model covers it, milestone 1 is keyboard-only. Tiling
  is chosen partly because it is drivable with the input that exists.
- **No programmable effects.** A fixed enum; shaders are a different conversation.
- **No split-tree layout in v1** — master-stack first, and both fit
  `fn layout(&Tree, Rect) -> Vec<(WindowId, Rect)>`, so split-tree is additive.
- **No shared mixing engine with `glitch`** — shared vocabulary, not shared code
  (§2). Extraction waits for the third instance.

## 15. Open questions

- **Font budget.** 8×16, ASCII plus box-drawing, a few KB, embedded — fine for v1.
  Emoji is a different mechanism and a real budget question: the itest kernel image
  is a shared budget that has already broken once when 4.5 MB of weights starved
  other programs with `OutOfFrames`.
- **Frame budget, measured not argued.** The always-copy trade (§4) has a number
  attached. snemu-wasm is paced at ~32.5% of a core; a full-screen blit per frame
  wants measuring before the design assumes 60 Hz.
- **`view`-per-message cost.** Re-running `view` on a 2000-row listing at every
  keystroke is fine or terrible depending on interpreter throughput. A 30-line
  benchmark counting instructions under snemu settles it before the framework's
  shape depends on the answer.
- **Stitch's runtime maturity is now on the critical path.** If most apps are
  Stitch, the desktop's performance floor *is* the interpreter's — including the
  per-run env/closure leak, the unclaimed ~20× release build, and natives needing
  syscall backing. Not fatal; needs to be sequenced deliberately rather than
  discovered.
- **Resolution.** ramfb is fixed at config time and QEMU, VF2 and snemu-wasm
  differ. The target sets the font size and the copy cost.
- **Sessions.** A session is a pair of caps — one on an input source, one on an
  output sink. "Who is attached, and through what" becomes a query over the cap
  graph rather than a file someone maintains, and two sessions contending for the
  keyboard is a visible cap conflict. Interacts with
  [accounts-and-login-design.md](accounts-and-login-design.md).
- **Where layout policy lives** — kitsch's, or a policy client's.
