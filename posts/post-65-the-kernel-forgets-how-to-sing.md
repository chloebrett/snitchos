# Post 65 — the kernel forgets how to sing

- post 62 was a design session that got out of hand — it started at "how hard is the headphone jack" and ended at a plan for two microkernels modeming their telemetry to each other over sound. the discipline I promised myself at the end of it was: build the boring load-bearing thing first. not the modem, not the sonifier. the **audio server** — the piece that makes the DAC a capability instead of a register four things fight over. `glitch`.

- this post is that build. eight increments, all TDD'd, and the thing works: `workload=glitch-beep` boots a userspace server that holds the DAC as a cap, a `beep` client asks it to play 440 Hz over IPC, the server synthesizes the tone and streams it through a cap-gated syscall, the kernel drives the register. the beep from post 61 is now a *client*.

- and then, once it was green, I deleted the old beep — and something quietly satisfying happened, which is really what I want to write down.

## the shape held, again

- post 62's whole thesis was that pushing the audio idea kept landing on primitives the kernel already had. the build was more of the same, less romantic: `glitch` is the **filesystem server with a different device behind it.** the FS server holds a `RECV|MINT` endpoint and serves file requests; `glitch` holds an endpoint and serves `Play` requests. same `run_ipc` launch, same bootstrap-caps-then-delegated-caps startup ABI, same `serve()` loop shape, same `Call`/`Reply` rendezvous. I didn't design an audio server. I pointed the FS server at the DAC.

- which meant most increments were mechanical. the interesting ones were where the audio workload was *different* from a file server, and each difference taught me something.

## the split I didn't plan

- the server needs pure DSP — turn a frequency and a duration into samples. I'd already carved that out into a dependency-free `synth` crate (`Tone`, `Gain`, a square wave) so that both the kernel and userspace could use it without userspace depending on a kernel crate. good. the server's *policy* — "a `Play` becomes this many samples at this fixed volume" — I wanted to host-test, because it has real logic (sample counts, amplitude, rejecting a supra-Nyquist frequency).

- but the server crate depends on the userspace runtime, which is riscv-only — it can't compile for the host at all. so the pure policy *can't be host-tested inside the server crate.* the same wall the FS server hit: that's exactly why there's an `fs-core` (host-tested logic) separate from `user/fs` (the riscv-only serve glue). the boundary made me split `glitch-core` out from `user/glitch` whether I'd planned to or not. a constraint producing the right structure — the design's favorite trick, showing up in the plumbing this time.

## least authority is a thing you have to actually do

- when I wired the server's boot grant, I copied the FS server's rights: `RECV | MINT`. it compiled, it would've worked. then I looked at what `serve()` actually does — it receives, it replies, it never mints anything — and took the `MINT` back off. the server gets `RECV` and nothing more.

- this is the small embarrassing lesson. the capability system doesn't grant least authority *for* you. it makes over-granting one word long and easy to copy from the crate next door. what it gives you is that the over-grant is **visible** — the grant is a `CapEvent` on the wire, so "why does the audio server hold MINT" is a question the telemetry would eventually ask out loud. the system doesn't stop you being sloppy. it snitches on you. which, for this kernel, is the entire point.

## the bug, and who caught it

- the itest failed the first time. and the failure is the best advertisement for the whole project I've built.

- I dumped the frame stream — `snemu boot --workload glitch-beep --frames` — and read the story the kernel told about itself. every capability was granted correctly: the server's `AudioSink` cap, rights `128`, right where it should be. the endpoint. the `beep.request` span, then the `glitch.play` span nested under it *across the process boundary*. the reply cap transferring back. the client called, the server received, the server tried to play — and then:

```
SyscallRefused { syscall: 32, reason: BadUserRange, task_id: glitch_server }
```

- the cap check **passed.** the IPC **worked.** the whole capability edifice — the thing that's genuinely hard, the thing post 62 was so pleased about — was flawless. the bug was the most boring plumbing imaginable: I sent 256 samples per syscall, samples are two bytes, that's 512 bytes, and the kernel's per-copy limit is 256 bytes. off by a factor of two in the least glamorous constant in the codebase. capped it at 128 samples, added a `const _: () = assert!(…)` so it can never drift past the copy limit again, done.

- but look at how I found it. I didn't attach a debugger or add print statements. the observability kernel *narrated its own failure* as structured frames, and the refusal frame said exactly which syscall, which task, and why. an OS whose first-class concern is watching itself is an OS that hands you the bug report before you think to ask. that's not a slogan anymore; it's just how I debug now.

## then the kernel forgot how to sing

- glitch-beep green, I retired the old in-kernel beep — the boot task that poked `WDATA` directly. deleted the task, deleted its workload, deleted its itest. mechanical.

- except one line of the diff is the whole point of the arc. removing that task removed the last thing in the kernel that used `synth`. so the kernel's dependency on the synthesis crate **vanished.** the arrow reversed. before, the kernel knew how to make a sound — it owned a tone generator. now it doesn't. it knows how to *move a register* when a userspace program with the right capability hands it bytes, and nothing more. synthesis is a userspace concern; the kernel is just the muscle with the key to the DAC.

- that dependency dropping is the proof the boundary was real and not just tidy words. you can *say* "the kernel shouldn't synthesize audio" all day. the compiler agreeing to drop the `synth` line from `kernel/Cargo.toml` is the boundary being load-bearing. the beep didn't move. the beep's *authorship* moved out of the kernel entirely, and the build recorded it.

## what I learned

- **discipline before delight pays out later, quietly.** building the server first felt like a detour from the fun stuff. the payoff isn't visible until now: the sonifier and the modem, when I build them, are *clients of glitch*. they don't touch the register. I didn't build one audio feature; I built the thing that makes every future audio feature a well-behaved tenant instead of a squatter.

- **the hard part worked; the boring part broke.** I spent the design energy on capabilities and IPC and they were correct on the first real run. the bug was a byte-count. worth remembering the next time I'm nervous about the sophisticated part and cavalier about the plumbing — it's usually the other way round.

- **retirement is the real done.** the additive approach — ship glitch *beside* the working beep, retire the beep only once its replacement is green — made the finish a clean subtraction. and the subtraction is where you find out if the abstraction held: a dependency you can delete is a boundary you actually drew.

- **least authority is a practice, not a property.** the caps don't hand it to you. you have to look at what the code does and take back what it doesn't need — and the telemetry is what makes the sloppy version visible enough to fix.

- and the small joy: I can *watch* the DAC being a capability now. the `AudioSink` grant, the `glitch.play` span crossing into the server, the samples counting up — all on the wire, all decodable, all true. the thesis of the whole OS, playing a 440 Hz tone, and telling me about it the whole way down.
