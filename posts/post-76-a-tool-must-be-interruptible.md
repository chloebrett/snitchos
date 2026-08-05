# Post 76 — a debugging tool must be interruptible when it's misbehaving

- small one. snemu grew `boot --interactive`: a terminal on the guest's UART, so I can actually *type* at the Stitch REPL running inside the deterministic emulator instead of injecting scripted keystrokes through the itest harness and reading the transcript afterwards.
- 163 lines, one of which is interesting and one of which I got wrong in a way worth writing down.

## why it didn't exist already

- snemu has always been a batch tool, and that's not an oversight — it's the whole point. determinism is what makes the itest gate a one-run gate, and the harness feeds input from scenario code so runs are reproducible.
- but every completion bug in posts 72 and 74 was reproduced by *predicting* what to type, injecting it, and reading a transcript. that loop is fine for a regression test and terrible for exploring. the thing I kept wanting was to press Tab twice and watch.

## three things between a keystroke and the guest, and only one is logic

1. **raw mode.** without it the host terminal's line discipline eats exactly the keys an interactive session is for. Tab gets swallowed or filename-completed by the *host* shell's habits, and nothing reaches the guest until Enter. testing a tab-completer through a line discipline that owns Tab is a special kind of pointless.
2. **streaming output.** batch snemu prints `uart_output()` once, at the end. unusable to type against. the loop flushes the new tail as it appears.
3. **non-blocking reads**, so the step loop doesn't stall waiting for a key.

- only (2) contains a decision — `unshown(output, shown)`, "what hasn't been printed yet" — so it's the only part with tests. it's total by construction: a `shown` past the end yields nothing rather than panicking. not defensive padding; `uart_output()` is a growing buffer owned by the machine, and a caller that resets or replays one would otherwise turn a cosmetic bookkeeping slip into a crash mid-session.
- the raw-mode guard restores the terminal on drop **including on panic**, which is why it's a guard and not a pair of calls. a snemu that left the terminal raw would leave the shell with no echo and no line editing, and the fix — `stty sane`, typed blind — is not obvious to someone who has just watched a crash scroll past.

## the bit I got wrong

- first cut: turn `ISIG` off, so Ctrl-C belongs to the guest. the reasoning was sound in isolation — the Stitch REPL's `:stim` editor exits on Ctrl-C, so the guest wants that key.
- the trade was wrong. it made **the escape hatch depend on the input path working**. and the first time input didn't work, the session could only be ended by killing it from another terminal.
- so: `ISIG` stays on, Ctrl-C kills snemu the way it kills everything else, and **Ctrl-]** — telnet's escape — is a *second* way out rather than the only one. passing Ctrl-C through to the guest can come back later as an opt-in flag, when something actually needs it.
- the general form, which I want to keep: **a debugging tool has to remain interruptible while it is misbehaving — and that is precisely when its own key handling cannot be trusted.** any escape hatch routed through the subsystem under test isn't an escape hatch. same family as post 74's harness swallowing the emulator's halt reason: the diagnostic path must not share fate with the thing being diagnosed.

## addendum — the same rule, with the polarity flipped

- a week later, on the board, the other half of this bit me. `screen` on the VF2's serial console showed nothing. not the kernel, not U-Boot, not the SPL — silence from a board that was fine.
- the cause was that I'd opened `/dev/tty.usbserial-0001` instead of `/dev/cu.*`. on macOS the `tty.*` node is the call-in device: `open()` blocks until carrier detect, and a USB-TTL adapter never asserts it. so the terminal sat in a blocked open, forever, with no error — *and* held the port, so every subsequent attempt on the correct node failed too.
- above, the failure was a tool I couldn't escape from. here it's a tool that **never started and had no way to say so**. and on a board that's much worse than it sounds, because the serial console isn't *a* diagnostic — it's the *only* one. the boot has no other channel until telemetry comes up. so "silence" has four causes that are byte-identical at the terminal: wrong node, port held by something else, wiring, or a board that genuinely never booted. **the diagnostic tool is a member of its own suspect list.**
- which is the rule from this post stated more sharply than I stated it. it isn't only that the escape hatch mustn't route through the subsystem under test — it's that a diagnostic which can fail *silently* and produce its subject's own failure signature has stopped being a diagnostic at all. it can only add candidates.
- the practical form is a bisect that costs nothing: reset the board with the terminal attached, and **the SPL banner is the tool clearing itself.** anything before `booti` is firmware we don't build, so seeing it proves cable, node, and port in one observation, and moves the fault into our code. I'd been about to debug the kernel.
- recorded in [notes/uboot.md](../notes/uboot.md), and it retired a real defect in an unbuilt design: [the board-agent bridge](../docs/board-agent-bridge-design.md) specified opening `tty.usbserial-*`. that bridge is meant to run *unattended* against hardware nobody is watching — it would have hung on open, silently, and reported it as a board that never booted. the design now says `cu.*`, and says the bridge must report "cannot open the port" as a distinct outcome from "the board said nothing."

## and the reason it's binary-only

- `interactive.rs` lives in the snemu **binary**, not the lib. the lib compiles to wasm — for the in-browser snemu the docs site will embed — where there is no tty and no argv.
- that boundary has now paid for itself twice. it's the same line that keeps `libc` out of the emulator core, and it means "runs in a browser tab" stays a property of the library rather than a thing I have to keep re-establishing.
