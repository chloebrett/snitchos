# Post 71 — the kernel learns to keep time

- post 65 gave the DAC away. the headphone jack stopped being a register four things fought over and became a capability held by one userspace server, `glitch`. a `beep` client asks it to play a note over IPC; the server synthesizes the samples and hands them to the kernel through a cap-gated syscall. clean. except for one thing I filed under "fine for now": the syscall **blocks for the whole play.** a one-second beep is a one-second syscall — the kernel sits in a per-sample spin loop poking `WDATA` and comes back a second later.

- that's fine for one beep and fatal for everything the audio arc is actually *for*. a sonifier maps dozens of telemetry events a second to sound — it can't afford a blocking `Play` per event. a modem streams a continuous FSK waveform — there's no "duration" to block for. both of them need the same thing: the server drops samples into a buffer and *leaves*, and something else feeds the DAC on a clock. a ring. this post is building that ring — increments 1 through 5 of the v2 plan, all TDD'd, the async path live end to end.

- and the ring turned out to be the easy part. the interesting part was that feeding a DAC at 8 kHz on hardware with no audio FIFO means the kernel needs a **real-time deadline** — a thing that must happen at a wall-clock moment or you *hear* the miss — and this kernel, cooperative and best-effort to its bones, had never had one. giving it one meant teaching a clock it already owned to keep two kinds of time at once.

## the ring was the easy part, and it still taught me something

- `SampleRing<N>` is a bounded FIFO of `i16` samples. thirty lines, no `unsafe`, lives in `kernel-devices` next to the console ring it's a sibling of. the producer (the enqueue syscall) pushes samples on the tail; the consumer (the timer drain) pops them off the head. I wrote it in an afternoon and mutation testing caught all 30 mutants on the first pass.

- but it is *not* the console ring, and the one place they differ is the whole design. `ConsoleRing` drops on full: when the input buffer is full and another byte arrives, the byte is thrown away. that's correct — a slow reader shouldn't corrupt the FIFO, and you can't un-type a keystroke, so the newest byte loses. `SampleRing` does the opposite: `push_slice` returns *how many samples it accepted*, which on a full ring is zero, and the producer is expected to come back with the rest.

- same data structure, opposite manners, and the reason is who the producer is. the console's producer is a UART that will not wait for anyone. the ring's producer is a *server* — a program I control, one that can be told "not now, try again." so the ring back-pressures instead of dropping: it never loses a sample, it just tells the truth about how full it is and lets the producer pace itself. the policy isn't a property of rings; it's a property of whether your producer can take "no" for an answer.

## the question an empty ring can't answer

- the drain side is a three-line function, `drain_tick`, that returns one of three things: `Fed(sample)` when it pops a sample to play, or — when the ring is empty — either `Underrun` or `Idle`. and the entire XRun observable lives in the difference between those two empty-ring cases.

- because an empty ring is ambiguous. it means one of two completely different things: the stream is *over* (the beep ended, nothing more is coming, silence is correct) — or the producer *fell behind* (the beep is still playing but the server didn't refill in time, and this empty tick is an audible gap, a glitch, a missed deadline). from inside the drain you cannot tell these apart. the ring looks identical.

- the only thing that knows is the producer. so `drain_tick` takes an `active: bool` — is a stream currently supposed to be playing? — as an **explicit input**, and never tries to infer it from the ring. active-and-empty is `Underrun`. inactive-and-empty is `Idle`. that one boolean is the difference between "the music stopped" and "the music skipped," and getting it from the producer instead of guessing is the whole reason the deadline is a real signal and not a heuristic. I spent more thought on that `bool` than on the ring it reads.

## a syscall that doesn't care who's calling

- the enqueue syscall — `AudioEnqueue`, number 33, added additively next to the old blocking `AudioWrite` — is cap-gated on `AudioSink`, copies the samples into the ring, returns the accepted count, and comes straight back. non-blocking by construction.

- the decision I'm proudest of here is one that does nothing yet. the syscall is gated on *holding an `AudioSink` cap*, and that's all. it doesn't know what `glitch` is. any process holding the cap can be a producer. which means when I build the modem, it doesn't route its waveform through glitch's `Play`-a-note protocol and back out — it gets its *own* `AudioSink` and enqueues its FSK samples directly. glitch is the sharing server for note-shaped clients, not a mandatory chokepoint every sound has to pass through. the generic path is one line more expensive today and saves the modem an entire IPC hop later. cheap forward-looking is the good kind.

- I also did *not* delete the old blocking `AudioWrite` while I was in there, even though glitch stops using it. deleting it before the drain existed and glitch had migrated would have red-lit the existing beep itest for two increments. so it stays, dormant and reachable, and retiring it is a later subtraction — the same additive discipline post 65 ended on. every increment stays green or it isn't an increment.

## one timer, two clocks

- here's the real problem, the one the ring was in service of. the JH7110's PWMDAC is **software-paced** — no hardware FIFO, no DMA. every sample is a write to `WDATA` that has to land at the right moment, and if you want 8 kHz you have to produce a write every 125 microseconds *yourself*. v1 did that by spinning inside the syscall. the async version can't; the drain has to be driven by an interrupt.

- but there's one timer per hart. it's already taken — it drives the scheduler tick and the heartbeat. and the VF2's U74s have no `sstc`, so I can't even cheat with a second comparator CSR; the timer is armed through an SBI call, one deadline at a time. so the audio feed and the scheduler have to *share* the single timer, and the audio feed wants to fire ~400× more often (every 125 µs vs. the scheduler's ~50 ms).

- the answer is a soonest-deadline timer wheel — a tiny two-entry thing that tracks `next_audio` and `next_sched`, arms the hardware timer at `min(next_audio, next_sched)`, and on each fire reports which deadline(s) came due and re-arms them. `TimerWheel`, in `kernel-boot`, pure and host-tested: ten tests for min-selection, both-due-at-once, and — the one that matters — *re-arming past now when a deadline was missed*. if the handler runs late and three audio deadlines have already elapsed, the wheel doesn't try to fire three times to catch up; it jumps to the next slot in the future in one O(1) step and drops the backlog. a stall costs one late tick, not a fire-storm. that "drop the backlog" rule is the kind of thing you only think to write a test for because you imagined the 3am version where you didn't.

- this is, I'm fairly sure, the first *second* periodic deadline this kernel has ever had. everything until now ran on one cadence. the wheel is the primitive that lets any future periodic device — a second audio channel, a display refresh, whatever — join the same timer without anyone rewriting the interrupt handler. the DAC is just its first tenant.

## the change that changed nothing

- swapping the wheel into `handle_timer` is heart surgery. that function runs on every timer interrupt on every hart; it's the hottest path in the kernel and the scheduler's whole sense of time flows through it. get the cadence subtly wrong and a dozen itests start flaking in ways that look like anything but a timer bug.

- so I built the wheel to be *provably* boring when audio is off. it self-initializes on each hart's first timer fire — which is itself a scheduler tick, so priming it that way costs nothing — and with the audio deadline disabled it degenerates to exactly the old behavior: one deadline, advancing by the same fixed interval, arming the same next time. the per-tick work (RX drain, preemption, heartbeat) moved under a `if due.sched` that, with audio off, is true on every fire. same cadence, byte for byte.

- the proof is the whole point. the full itest suite passed **128 for 128** through the swap, plain and under frame-scramble, zero scenarios perturbed. (it's 130/130 now — the count grew while I was off in floating-point land.) the scary rewrite of the hottest path in the kernel is only allowed to be boring because there are 130 deterministic scenarios standing behind it saying "the clock still ticks the way it did." the safety net is what makes the tightrope a sidewalk.

## where the sound actually turns on

- one wrinkle I hadn't planned for: *who* enables the audio deadline. glitch is userspace. it can't reach into the kernel and flip the wheel on. all it can do is make syscalls. so enabling the feed has to be a *side effect of enqueuing* — the first `AudioEnqueue` of a stream latches a flag, brings the DAC up, and turns on the audio deadline on **whatever hart the enqueue ran on.** that last part sounds wrong until you remember the DAC's `WDATA` register is global memory — any hart can write it — so whichever hart is enqueuing is a perfectly good hart to also drain from, and I dodge a whole cross-hart hand-off and the IPI it would need. no coordination, because the resource doesn't belong to a CPU.

- and it turns itself off. when the drain finds the ring idle it disables the audio deadline again, so the 8 kHz interrupt storm only exists while there's actually audio playing. no stream, no cost. the timer goes back to being a plain scheduler tick until the next note.

## the deadline I built but haven't missed

- here is the honest part, the reason this is increment 5 of 9 and not a victory lap. the XRun observable is *fully wired* — the `Frame::AudioXRun`, the `xruns_total` counter, the deferred emit from the heartbeat (you can't emit a frame from inside the timer IRQ — the wire path takes a lock the allocator might already hold, and that's a deadlock; so the drain bumps an atomic and the heartbeat turns it into a frame later, same trick the whole kernel uses for IRQ-context telemetry). all of it is in the tree and reachable.

- and it has never once fired. `AUDIO_ACTIVE` — the producer's "a stream is supposed to be playing" flag, the exact `bool` I made such a fuss about — currently ships hardcoded `false`. so the drain always reads an empty ring as `Idle`, never `Underrun`. I built the machine that detects a missed deadline and then declined to ever tell it a deadline exists.

- that's deliberate, not an oversight. to make an underrun *fire* on purpose I need a producer that promises more audio and then deliberately fails to deliver — a workload that under-feeds the ring on cue. and that's exactly the shape of the acceptance test (increment 9). the flag that arms the deadline and the scenario that misses it are the same piece of work, so I left them together rather than shipping a live tripwire with nothing to trip it. the OS's first real-time deadline exists structurally; making it audibly miss is the next increment, not this one.

- when I do prove it, I already know how, because I made the call this session: the gate asserts on the **waveform**, not just the counters. the drained samples are deterministic bytes under snemu — the ring is FIFO, so what comes out is exactly what went in — which means I can assert the recovered stream is byte-for-byte the expected square wave. that catches a whole class of bug a counter is blind to: `samples_emitted ≥ 1` passes cheerfully even if every sample is at the wrong *rate*, or a mix clipped to garbage, or the tone is a DC constant. the counter says "sound happened." the waveform says "the *right* sound happened." for an underrun specifically, the proof is a visible gap in the recovered stream — you can *see* the deadline slip before you can hear it.

## a note on building in a live diff

- worth writing down because it's true and it's the texture of real work: I built most of this while also mid-rewrite of the floating-point context-switch path, and the two threads met in the same file — `trap/mod.rs`, where the timer handler I was surgically swapping for audio was also sprouting FP-claim hooks. at one point the tree didn't compile and it took a second to see that neither half was wrong; they were just two rewrites of the same hot function landing on top of each other. the audio increments were green in isolation the whole time; the breakage was the *seam* between two unrelated changes to one load-bearing function. a small argument for keeping increments small and the file boundaries honest — when two changes collide, you want to be able to prove which one is fine.

## what I learned

- **a ring's full-behavior is a statement about its producer.** drop-on-full and back-pressure are the same three fields with opposite manners, and which one is correct falls out entirely of whether the thing feeding it can be told to wait. the console can't; a server can. I'd have called that a detail before I wrote two rings a folder apart that disagree about it.

- **the ambiguous state is where the observable lives.** an empty ring means two opposite things, and the entire value of the XRun signal is refusing to guess between them — taking the answer from the producer as an explicit input. the interesting telemetry is almost never a new event; it's a distinction some existing state was quietly collapsing.

- **the reusable primitive was hiding in the constraint.** I didn't set out to build a timer wheel. I set out to feed a DAC, discovered there was exactly one timer, and the multiplexer that fell out is now the thing every future periodic device gets for free. the second-hardest part of the feature became infrastructure for features I haven't scoped.

- **a scary change is only allowed to be boring if the net is real.** rewriting the kernel's hottest path was a non-event because 130 deterministic scenarios could each say "the cadence is unchanged." the confidence wasn't courage; it was coverage. behavior-preserving is a claim you get to *make* only when something can falsify it.

- **don't claim the observable until it fires.** the honest status of this arc is: the OS now has its first real-time deadline, and I have not yet made it miss. the frame, the counter, the whole detection path are built — and dormant behind a `false`, because the thing that arms the deadline and the test that trips it are one job, and shipping a tripwire with nothing to trip it would be theater. the deadline is real. the miss is next post.

- and the callback that made me smile: post 65 was *the kernel forgets how to sing* — synthesis moved out, the kernel lost its tone generator and became muscle. this one is the other direction. it can't make a sound anymore, but it just learned to keep time well enough to play one back on a deadline. it forgot the melody and learned the metronome.
