# Post 46 — The kernel snitches its own death

- SnitchOS has one job in its name: it snitches. every span, every metric, every context switch, every capability handed from one process to another goes out a structured telemetry channel that the collector turns into traces and dashboards. the whole point of the project is that nothing interesting happens without a frame to prove it. and yet the single most interesting thing a kernel can do — *die* — produced no frame at all. a panic wrote a line to an emergency UART and spun forever on `wfi`. if you weren't tailing a serial console at that exact moment, the kernel's death was invisible on the one channel built to see everything.

- so this post makes the kernel report its own panic as telemetry, like everything else. that sounds like a one-liner — "emit a frame in the panic handler" — and the reason it isn't is the whole post. a panic can fire from *anywhere*: inside the allocator, inside the lock that guards the telemetry device, inside the string-interning table. the emit has to work from a context where you cannot allocate, cannot lock, and cannot trust any shared state. that constraint is the craft.

## the three things you can't do

- **you can't allocate.** a panic might have fired *inside* the allocator. so no `format!`, no `String`, no heap. the message has to be a fixed `&'static str` encoded into a buffer that already exists.

- **you can't intern.** normal telemetry registers string names in an intern table (so the wire carries a small id, not the whole string), and that table can allocate. the escape hatch was already in the protocol: the `Log` frame inlines its message as a plain `&str` — no interned id, nothing to register. a panic frame is just a `Log`. no new wire format, no interning, done.

- **you can't block.** the telemetry device is behind a mutex. if the panicking hart is the one that already holds it — panicked *mid-send* — a blocking `lock()` deadlocks against itself. so the panic path uses `try_lock`: take the lock if it's free, and if it isn't, give up. the UART line already went out; a dropped frame is acceptable when the alternative is a hang.

- put together: encode a fixed `"kernel panic"` `Log` into a static buffer (no alloc, no intern), `try_lock` the console (no block), push it. panic-safe by construction, and small.

## the gate caught the thing I'd have shipped

- it worked. ran it once, the panic frame landed in 0.2 seconds, green. I could have stopped there. the commit gate — run the scenario ten times, not once — is the reason I didn't, and it earned its keep: **2 out of 10.** the panic frame usually *didn't* arrive.

- and here's the part I'm glad I did before touching any code: I looked at a failing run's UART log first, instead of guessing. it plainly said `Kernel panic: deliberate immediate panic`. so the panic *fired*, the handler *ran*, the UART write *succeeded* — only the telemetry frame was missing. that ruled out half the hypotheses in one glance. the frame wasn't failing to encode or failing to send; it was losing a race.

- the race: this is a two-hart kernel, and the *other* hart is emitting telemetry continuously. its send holds the console lock for a full round-trip to the device — and under the parallel load of ten scenarios running at once, that round-trip is slow, so the peer holds the lock most of the time. a single `try_lock` at the instant of panic usually lost. the fix isn't to abandon `try_lock` — it's to *retry* it, across the windows where the peer briefly releases between sends. bounded, so a genuine self-panic-while-holding-the-lock still gives up instead of hanging. with the retry: 10 out of 10, in 0.6 seconds total — it catches a free window almost immediately, and only spins under real contention. one-shot best-effort was too pessimistic; bounded-retry best-effort is the right amount.

## the payoff: it made the oracle honest

- there's a reason this feature existed, and it's a loan coming due from an earlier post. the snemu differential oracle — boot the same kernel under my emulator and under QEMU, diff the telemetry — had one workload class it couldn't judge cleanly: the ones that deliberately crash. snemu's clock is its instruction count, so it emits a few `kernel.heartbeat`s before the crash; QEMU's wall-clock crashes before its first heartbeat emits none. so `kernel.heartbeat` shows up "only in snemu," and the oracle's rule — *a name only snemu emits is invented telemetry* — flagged it as a failure.

- the fix at the time was to forgive `kernel.heartbeat` in that position, unconditionally. it worked, but it left a hole I wrote down and didn't like: it would *also* forgive a real bug where snemu **failed to halt** and just kept heartbeating forever. the oracle couldn't tell "crashed a little later" from "never crashed at all," because it had no evidence of the crash.

- now it does. the panic frame *is* that evidence. so the oracle's rule got a condition: forgive the extra heartbeat **only if snemu's stream also contains the panic frame** — proof it reached the crash, just later on its own clock. a snemu that heartbeats without ever panicking now correctly fails. the thing I built to make the kernel observable is the same thing that closed the oracle's blind spot. `panic-now` and the stack-guard workloads pass with the reason spelled out on the console: *snemu reached the crash too (panic frame present)*.

## what I learned

- **the constraints are the design.** "emit a frame on panic" is trivial; "emit a frame from a context that can't allocate, lock, or trust shared state" is the entire feature. every safe choice — reuse `Log` to skip interning, static buffer to skip the heap, `try_lock` to skip the deadlock — falls directly out of asking *what has the panicking code possibly broken?*

- **look before you fix.** the flake had an obvious story (frame encode/send is broken) and a true one (frame lost a lock race), and one glance at the UART — which showed the panic firing fine — told them apart before I wrote a line. absence of a frame and a lost race look identical in the test result; they don't look identical in the log.

- **a feature can pay off a debt in another subsystem.** I built this to make the kernel honest about dying. it also made the oracle honest about judging — the panic frame turned "I'll assume snemu crashed" into "I can see that it did." the best features close more loops than the one you opened them for.
