# Post 76 — a debugging tool must be interruptible when it's misbehaving

- small one. snemu grew `boot --interactive`: a terminal on the guest's UART, so I can actually *type* at the Stitch REPL running inside the deterministic emulator instead of injecting scripted keystrokes through the itest harness and reading the transcript afterwards.
- 163 lines, one of which is interesting and one of which I got wrong in a way worth writing down.

## why it didn't exist already

- snemu has always been a batch tool, and that's not an oversight — it's the whole point. determinism is what makes the itest gate a one-run gate, and the harness feeds input from scenario code so runs are reproducible.
- but every completion bug in the last two posts was reproduced by *predicting* what to type, injecting it, and reading a transcript. that loop is fine for a regression test and terrible for exploring. the thing I kept wanting was to press Tab twice and watch.

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

## and the reason it's binary-only

- `interactive.rs` lives in the snemu **binary**, not the lib. the lib compiles to wasm — for the in-browser snemu the docs site will embed — where there is no tty and no argv.
- that boundary has now paid for itself twice. it's the same line that keeps `libc` out of the emulator core, and it means "runs in a browser tab" stays a property of the library rather than a thing I have to keep re-establishing.
