# Stitch 7 — it runs on the metal

- for six posts Stitch has been a thing that runs on my Mac. you type `cargo run`, the tree-walker evaluates an AST, you get a number. the whole point of the language — the reason it's called Stitch and lives in this repo at all — was that it would one day run on **SnitchOS**, the little RISC-V kernel this project is really about, and consume \*that*'s* capabilities and telemetry as language primitives. "the platform provides the effects." but the platform was always macOS, and the effects were always a `Vec` in a host process. this is the post where that stops being true.
- by the end, `1 + 2 => 3` printed not by my laptop but by a **userspace process on SnitchOS**, the bytes going out a real UART, the interpreter running in `no_std` on emulated RISC-V. the two side projects — the language and the OS — stopped being two. and most of the work wasn't making it _run_. it was making it _usable_, which turned out to be a different project wearing the same clothes.

## the port was the easy part

- the design always said the *on-target_runtime would be the bytecode VM. but the VM doesn't exist yet, and the tree-walker does — ~7,000 lines of fairly portable Rust. so the move (call it path 3) was: port the **tree-walker** to `no_std` and run *that\_ on the metal now, as a stepping stone, instead of waiting for the VM. it leaks (more on that later); it runs months sooner.
- and the port was startlingly small. the interpreter used `HashMap` — but `HashMap` isn't in `no_std`, and its default hasher needs entropy the target doesn't have. the fix wasn't `hashbrown` + a seeded hasher; it was **`BTreeMap`**, which lives in `alloc`, needs no hasher, and is plenty for tables of a few dozen string keys. zero new dependencies. wrap the crate in `#![cfg_attr(not(test), no_std)]` (so `cargo test` still gets `std` and the snapshot tests work), add a tiny prelude re-exporting the `alloc` essentials `std` gives you for free, leave `main.rs` as a separate `std` binary for the host CLI — and the library builds for `riscv64gc-unknown-none-elf`. a mechanical afternoon, not the rewrite I'd braced for.
- the OS had to grow one thing to host it. userspace could already _read_ the console (a polled UART syscall) but could only **write to telemetry** — a program's output went to the trace, not the terminal. a shell that prints into Grafana isn't a shell. so: a `ConsoleWrite` syscall, the mirror of the read, sharing the one UART the kernel logs to. small. and then a userspace crate that links the `no_std` interpreter, reads a line, evaluates it, writes the result — a **Stitch REPL, as a SnitchOS process**. it booted. `Stitch on SnitchOS — the tree-walker runs on the metal.` and a `stitch>` prompt blinking in QEMU.

## then it had to be usable

- it booted, and the first eval *hung\_. printed the banner, called the interpreter, and never came back. the crash signature was a hang, no fault, nothing — which is itself a tell. the userspace stack was **16 KiB**, and a recursive-descent parser plus a recursive tree-walk evaluator chewing through the whole prelude is *deeply\_ recursive. it overflowed the stack, and with no guard page there's no clean fault — it silently scribbled past the bottom and wandered off. bumped the stack to 512 KiB and it evaluated instantly. the whole bug was a number in a linker script, and the whole _diagnosis_ was "a hang with no fault means corruption, and a recursive interpreter on a tiny stack is the obvious suspect."
- then it worked but felt \*slow\_, and here's the thing I keep relearning: **you can't tell by feel what's slow.** there were two completely separate lags with different causes, and they felt like one.
- the **typing** lag was the OS. console input is polled — a timer drains the UART receive buffer, and that timer fires at **1 Hz** (it's the heartbeat clock; the input drain was just piggybacking on it). so a keystroke could sit for up to a _second_ before the kernel even noticed it. nothing to do with the language. the fix was to decouple them: fire the timer fast (every 50 ms) for input and preemption, but only run the heartbeat every 20th tick — so the heartbeat keeps its 1 Hz cadence (the telemetry tests still pass) while input latency drops 20×. as a bonus, preemption got sharper too: a CPU hog now gets descheduled near its actual quantum instead of up to a second late.
- the **per-command** lag was the language, and it was self-inflicted. the REPL re-parsed and re-registered the _entire prelude_ on every single line — rebuilding the whole world to evaluate `3 * 4`. the fix is the obvious one once you see it: build the environment **once**, reuse it for every expression, and only rebuild when a new definition arrives. one struct, a cached env, an invalidation flag.

## so measure it

- "the fix is obvious" is a hypothesis, not a result. I wanted the number, and the text trace only prints raw frames — reading a span's duration out of it means hand-correlating timestamps. so I added the primitive the project was missing anyway: a **`ClockNow` syscall** (read the monotonic tick counter; the stubbed `Instant::now()` rides on it). now the REPL times each eval and prints it. booting alone runs three self-tests:

```
[buildenv]   1393970 ticks (~139 ms)   1 + 2  => 3
[  cached]     21660 ticks (~2 ms)     3 * 4  => 12
[pipeline]    322040 ticks (~32 ms)    1.. |> map($ * $) |> take(5) |> toList  => [1, 4, 9, 16, 25]
```

- there it is. building the env — registering the whole prelude — is **~139 ms**. a cached eval is **~2 ms**. *sixty-five times\_ cheaper. before the fix, *every\_ command paid the 139 ms; now only the first does. the cache wasn't a guess, it was a 65× line you can read straight off the terminal. (and all of this is under QEMU's software emulation, ~10–50× slower than real silicon — so the interpreter itself is faster than these numbers; the emulator is most of the cost.)

## the soul — making it work is not making it usable

- the port — the thing I thought was the milestone — took an afternoon and was never in doubt. everything \*after\_ "it boots" is where the real work was: a stack-size overflow, two intertwined latency sources with unrelated causes, a measurement primitive built just to confirm a fix. "it runs" and "you'd want to use it" are separated by a surprising amount of unglamorous engineering, and almost none of it is the part you'd put in the demo.
- and the throughline underneath: **"the platform provides the effects" became literal.** Stitch's whole thesis was that the OS supplies the capabilities and the language consumes them. for six posts that was an aspiration with a `Vec` standing in for the platform. now the platform is real — `ConsoleWrite` is the terminal, `ClockNow` is the clock, the spans go to the same wire as the kernel's own. the language and the OS aren't two projects that reference each other in design docs anymore. one runs inside the other.

## what i learned

- **a silent hang is a corruption tell.** no fault, no panic, just a stop — that's not "slow," that's something scribbling where it shouldn't. on a kernel with no stack guard pages, "the recursive thing on the small stack" is the first suspect, and it was a one-line fix once named.
- **you cannot feel which thing is slow.** the typing lag and the eval lag were different by two orders of magnitude in cause (a 1 Hz timer vs a redundant prelude rebuild) and indistinguishable by hand. measuring isn't the last step after optimizing — it's the first step, and I had to build the clock to take it.
- **the stepping stone is allowed to be ugly.** the tree-walker has no garbage collector, so it leaks a little every line — fine for a session, fatal for a server. the honest move was to ship the leaky thing that runs _now_ and write the caveat down, not to wait for the VM that fixes it properly. it's running on the metal a season early, and the leak is a sentence in a doc instead of a reason it doesn't exist.

## what's next

- the **shell**. now that a Stitch program runs on SnitchOS and the OS just grew an async **notification** primitive (the gateway to interrupt-driven input and to processes waking each other), the capability shell finally has its foundation: a Stitch REPL where `grant` and `watch` and `hold` are the language over real syscalls, and every delegation of authority is a span you can see.
- and underneath, eventually, the **bytecode VM with a garbage collector** — the thing that turns "leaks a little every line" into "doesn't," and turns these lazy cells into objects a collector walks. the tree-walker bought a season; the VM is the thing it was buying time for.
- but it runs on the metal now. you type at a prompt on an operating system I wrote, in a language I wrote, and it answers in two milliseconds. the two halves met.
