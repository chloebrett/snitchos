# Post 62 — The constraints were already the design

- this session started with a small question — "how much work to drive the VisionFive 2's headphone jack?" — and ended somewhere I did not book passage to: a design for a microkernel that could **modem its own telemetry over sound to another instance**, and the first few vertebrae of the server that makes it possible. the beep itself ([[project_vf2_audio]], post 61) took an afternoon. the *rest* of the session was the interesting part, and it's the kind of thing I want written down before I forget how the thought went.

- because the thought kept doing the same startling thing. every time I pushed the audio idea one step further, the step I needed turned out to be **a shape the kernel already had.** the constraints didn't fight the design. they *were* the design, waiting.

## audio was never the feature

- the first reframe, and the one everything hangs on: audio in an observability kernel isn't a thing to *finish*. it's a **lens**. it's three different things at once, and only one of them is "make a tone."

- there's the boring one — build a real audio stack, DMA, mixing, files. do it when something needs it, not for itself.

- there's the on-thesis one: audio as an **observability output.** you already narrate the kernel as structured `Frame`s; audio is just another sink. heartbeat → tick, context-switch → click, OOM → a falling tone. you'd *hear* a boot. the scheduler becomes rhythm. no other OS lets you listen to its internals as a designed experience, and it's a small amount of code on top of what's already there.

- and there's the one that turns audio load-bearing: a **forcing function.** audio is a real-time, deadline-bound workload, so it drags capabilities into existence that a batch workload never would — deadline scheduling, and the observability of a *missed* deadline (an XRun is the perfect thing to snitch on); userspace DSP, which needs lazy FP context switching (a real kernel feature); and a server to arbitrate the one scarce DAC. the workload doesn't just use the kernel. it grows it.

## the modem, and the shape it already had

- then the delightful detour: it sounds like an old-school modem, so — could two instances *talk* over it? and here's where the pattern announced itself, over and over.

- a modem handshake is **data encoded as audio.** my telemetry is data. so the modem isn't cosplay — you FSK-modulate the actual `Frame` stream out the jack, and the audio channel becomes a real second transport, a peer of the UART. because the frames are already **COBS-framed** (from the UART-telemetry work), the byte stream is self-delimiting and resyncable — the annoying part of any modem, *already built.*

- an interactive session over half-duplex audio needs turn-taking — "over, your turn" — which felt like a protocol I'd have to invent. it is not. **half-duplex turn-taking is `Call`/`Reply`.** the kernel's IPC rendezvous already blocks a caller for a reply; the message boundary *is* the "over." the physical channel demands exactly the shape the syscall already has.

- a lossy channel wants best-effort-with-resync for telemetry and reliable-with-retry for data — TCP vs UDP. that's not new machinery either: it's the **transport-policy** abstraction the collector already carries (`Lossless` / `Resync`), plus one more tier. and the loss-tolerance is only *viable* because the unit is a frame — drop one, resync, continue — which a byte pipe can't do.

- two instances talking is remote access, which in a capability OS **cannot** be "log in with your authority." it's cap-delegation over the link, proxy-mediated because a live cap can't teleport — and it's the same mechanism whether you call it "ssh" or a cross-machine pipe. and the cross-machine pipe already has an operator: **`~>`**, which I confidently mis-described as a "fuzzy pipe" and then looked up and found is the *shipped* typed, capability-checked cross-process pipe. I wasn't inventing it. I was rediscovering it with a new transport bolted on.

- five pushes, five times the kernel said "I already know this shape." that's not luck. that's what it feels like when a system's core abstractions are the right ones — the far edges fold back onto the center.

## the moneyshot is the demo, not the dependency

- the capstone, noted for a future-me with more time: `Frame` isn't a telemetry format, it's a **network protocol**, and the transport is pluggable. two acoustically-coupled microkernels gossiping their own observability is just the most charming instance. Frame-over-IR, Frame-over-QR would work too. audio's the one that sounds like 1996.

- and the test strategy is the best part, because it's *deterministic all the way down.* wire two snemu instances' audio buffers together digitally — no speaker, no microphone — and everything runs and is asserted except the analog PHY, which was never interesting to test. inject loss as a *knob* (`--loss 5%`) and the reliability layer becomes TDD-able, which is normally the least-testable part of any protocol. you can test a two-machine acoustic modem with zero hardware and zero sound. real audio becomes the moneyshot; the emulator is the workbench.

## then I built the least glamorous thing first

- and this is the discipline the whole session was really about. the beep works, but its architecture is a lie of convenience: a kernel task pokes the DAC register directly. that doesn't survive a *second* sound source, and it makes "the right to make noise" ambient. so before any of the fun, the first real build is **`glitch`** — a userspace audio server that holds the DAC as a **capability.** (snitch · stitch · glitch. it had to be.)

- same shape, again: a single scarce resource wants a server plus caps, which is *exactly* the FS server. so glitch isn't new architecture, it's the FS pattern pointed at a different device. the beep becomes its first client. everything downstream — sonification, the modem — becomes a client too, instead of four things fighting over one register.

- I built it the way I build now, and got four increments deep, all TDD'd: the DAC as an `Object::AudioSink` cap with its own `AUDIO` right and `authorize_audio` gate; an `AudioWrite` syscall that checks the cap and refuses a non-holder out loud; and the `glitch-proto` `Play` wire type — where I decided the client names the *note* and the server owns the *volume*, because "no volume protocol yet" was a non-goal and the note is all a client should need.

- one lesson fell out for free. adding a new `CapObject::AudioSink` **wire variant** compiled fine in the crate I was editing and broke three exhaustive `match`es in crates I wasn't — the diagram generator, the collector, the kernel's own decoder. none of them fail *your* build; they wait for the full-workspace compile to ambush you. a positional wire enum has a blast radius, and the radius is every exhaustive match that never needed a wildcard.

## what I learned

- **the right abstractions make the edges fold back onto the center.** half-duplex is `Call`/`Reply`. lossy framing is the resync policy. remote access is cap-delegation. `~>` already exists. when pushing a feature to its extreme keeps landing on primitives you already have, that's the system telling you the core is sound — and it's the strongest design signal I know, worth more than any amount of up-front cleverness.

- **audio is a lens, not a feature.** the question was never "how good can the sound be." it was "what does a real-time, framed, capability-mediated, lossy-channel workload force the kernel to become." the tone is almost incidental. the pressure it applies is the point.

- **discipline before delight.** the modem is the fun part; the audio server is the load-bearing part, and it goes first — because it's the one that changes the *shape* of everything after it (client-of-a-server vs. everyone-pokes-the-register). build the boring capability, and the fun becomes clients.

- **look it up before you name it.** I invented a crisp story for what `~>` "should" mean from the shape of the tilde, and it was wrong, and the real thing was better. a clever guess about your own system is still a guess. the repo remembered; I didn't.

- **a wire variant has a blast radius.** append a positional enum case and go hunt every exhaustive match before the workspace ambushes you. `matches!` is immune; a bare `match` is not.
