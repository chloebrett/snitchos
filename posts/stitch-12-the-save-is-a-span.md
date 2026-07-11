# Stitch 12 — the save is a span

- three posts ago the editor sent me back to the front end. it wanted spans and a real evaluator before it would let me write it, and it was right to. posts 10 and 11 built those. this post is the editor coming back — and, true to form, sending me back a dozen more times on the way. just to smaller things.
- the pattern is the whole story, so I'll say it up front: **stim is an auditor of the platform.** every single feature of the editor, when I sat down to write it, pointed at something missing one layer down. not big things this time — a string escape, a filesystem operation, a REPL that couldn't resolve an import. each a quick fix. but the editor found every one of them, because building a real program is the only honest test of whether the platform underneath it is real.
- and the payoff at the end is the most SnitchOS thing that's happened on this project: I never looked at the screen to know it worked. the proof that the editor saved your file is a **span on the wire.**

## the editor is a Stitch program

- the thesis has always been: Rust is the platform, Stitch is where the interesting logic lives. an editor is the cleanest possible test of that, because an editor's core is `step(state, key) -> state` — a pure, immutable transition, which is exactly the shape Stitch is built for.
- so the state is a Stitch record, and every transition is a `..spread` update:

```
backspace(state) = match {
    state.col > 0 => {
        let line = lineAt(state, state.row)
        Editor(..state,
            lines: List.set(state.lines, state.row,
                Str.slice(line, 0, state.col - 1) + Str.slice(line, state.col, Str.length(line))),
            col: state.col - 1)
    }
    state.row > 0 => {              // at column 0: join onto the previous line
        let prev = lineAt(state, state.row - 1)
        Editor(..state,
            lines: List.removeAt(List.set(state.lines, state.row - 1, prev + lineAt(state, state.row)), state.row),
            row: state.row - 1, col: Str.length(prev))
    }
    _ => state                      // top-left: nothing to delete
}
```

- that is the entire backspace key. insert, enter-splits-a-line, `j`/`k` with cursor re-clamping, the `:w` command — all of it is `.st`, all of it pure, none of it knows what a byte or a syscall is. the whole editor is about forty lines of Stitch.
- the fiddly parts — clamping the cursor when you move onto a shorter line, backspace joining a line, enter splitting one — are exactly the bug-prone logic I was nervous about putting where the mutation gate can't reach. so I leaned on invariants instead: `backspace(splitLine(s)) == s`, and `enterNormal(enterInsert(s)) == s`. one round-trip apiece, and an off-by-one in either direction fails it. those two identities caught more than any individual case test would have.
- the primitives the transitions stand on — `Str.slice`, `List.at`/`set`/`insert`/`removeAt` — Stitch didn't have. so they were the first thing to build, five small natives, and I gave them all one deliberate contract: **total.** an out-of-range index is a value, never a panic — `None`, or the list unchanged. that one decision is why the editor can treat a cursor that's run off the end of a line as ordinary data instead of a crash. the primitive's shape and the editor's needs met exactly where I designed them to.

## every feature was a gap

- then the audit. in order of when the editor tripped over them:
- **the renderer needed to emit an escape character, and the lexer couldn't.** to clear the screen and move the cursor you write `\e[2J\e[H` — ANSI escape sequences, and `\e` is the ESC byte, `0x1b`. Stitch string literals knew `\n`, `\t`, `\"`, `\\`, and nothing else; `\e` just became the letter `e`. so there was literally no way to write a terminal control code in the language. a two-line lexer change — `\e` and `\r` — and `renderFrame` became a one-liner with string interpolation for the cursor coordinates. the editor asked the lexer for a character the language had never needed before.
- **the REPL couldn't run a program that imports a module.** I wanted to poke the editor by hand — `:load` the `.st`, call `initialState("a\nb")`, watch it. it faulted: `unbound variable Str`. it turned out the single-program path silently threw away every `use` statement; builtin-module resolution only ever lived in the multi-*module* path. a `:load`ed program that opened with `use Str` would load fine and then fault the instant you called it. nobody had noticed because nobody had `:load`ed a module-using program before. the editor was the first, so the editor found it.
- **the filesystem could not make a file shorter.** `:w` writes the whole buffer back. if you delete a line and save, the new content is shorter than the old — and `ramfs::write` only ever *grew* a file. there was no truncate, at any layer: not in the `fs-core` trait, not as an IPC opcode, not in the server. a shorter save would have left stale bytes hanging off the end. so truncate got built the whole way up the stack — trait method, ramfs `Vec::resize`, a new `Truncate` opcode on the wire, a server handler — and it's WRITE-gated the whole way, so a read-only cap can't call it. the one genuinely missing *capability*, as opposed to the missing conveniences above.
- **every path-walker in the OS was read-only.** to save, the shell has to resolve the file path and hand the editor a cap that can write. I went to reuse the existing file-lookup code and found that every client that walks a `/`-path — every one — requests only `READ`. a cap's rights are `parent ∩ requested`, so a single read-only hop strips write authority from everything below it. saving needed a walker that asks for `READ|WRITE` at every step and creates the file if it's absent. so there's a new one. (while I was in there, an audit of the filesystem turned up a module doc-comment insisting subdirectories were unsupported — years stale; the code had grown a real inode tree underneath it and nobody updated the sign. the editor's questions keep turning up these.)

## the driver is the platform, the loop is Rust

- so the logic is Stitch and the effects are Rust, and the seam between them is a small native loop I called the driver. it builds the interpreter environment **once** — the whole point of the last two posts' work was to make a long-running Stitch program viable, and rebuilding the environment per keystroke would have thrown that away — then it loops: read a byte, ask the FSM what to do, do it.
- the FSM's transition doesn't return a new state, it returns `Step{state, effect}` — the next state plus an effect for the driver to perform. three effects: `Redraw`, `Save(text)`, `Noop`. the editor decides *what* should happen; the driver is the only thing that touches the console or the disk. `Redraw` renders the state and writes it out. `Save` writes the text through the file cap. `Noop` does nothing.
- and the byte never reaches the FSM as a byte. the driver translates: `0x1b` becomes the token `"Esc"`, carriage return becomes `"Enter"`, delete becomes `"Backspace"`, and a printable byte becomes its own character. the editor dispatches on tokens and stays innocent of encodings — the same split as kernel and userspace, drawn one level up.
- I owe an honest note here. the driver is Rust. the roadmap's prettier ending is that the *loop itself* is a Stitch program — and post 11's trampoline is exactly what unblocks that, because a Stitch loop needs bounded tail recursion. but I shipped the native driver first. the Stitch-loop version is a deferred follow-up, and I'd rather have a working editor and a clear next step than the elegant version and no editor.

## the save is a span

- here's the part I keep grinning about. the way you verify a text editor is normally to look at it. type some keys, glance at the screen, check the file. SnitchOS's whole identity is that you shouldn't have to — the system narrates everything it does as telemetry — and the editor inherited that for free, because it's a program on this OS like any other.
- so the driver opens a `stim.session` span for the whole editing session, and a nested `stim.save` span around each `:w`. those go out the same wire as every other span on the system. and the integration test — which boots the real kernel in QEMU, starts the shell, and drives the editor over the serial console — never takes a screenshot. it asserts on the trace:

```
:stim note.txt        →  wait for the "stim.session" span    (the editor launched)
iZQXMARK<Esc>:w       →  wait for "ZQXMARK" on the UART       (the edit rendered)
                      →  wait for the "stim.save" span        (the save fired)
```

- the middle assertion is my favorite trick. `ZQXMARK` is what I typed in insert mode. raw byte reads don't echo — unlike the line editor, the driver's byte reader is silent — so that string cannot appear on the console as an echo of my keystrokes. the *only* way `ZQXMARK` reaches the terminal is if the FSM inserted it into the buffer and `renderFrame` drew the buffer back out. finding it on the wire proves the entire path ran on the metal: bytes in, `step`, `insertChar`, `renderFrame`, console out. and the `stim.save` span proves `:w` reached the disk. no eyeballs involved.
- `[stim-edits-a-file-and-saves] ok (max wait 0.6s of 30s budget)`. it edits and saves a file on real hardware-emulation, and I know it did because it told me. an editor that you test by reading its own narration is the most on-thesis thing this project has produced.

## what I'm not pretending

- the read-only story is real but the confinement isn't finished. if the cap the shell hands the editor lacks `WRITE`, the save is a genuine kernel refusal — a snitched `SyscallRefused`, not a flag the editor checks. that part is honest. but for now the editor runs *inside* the shell process, sharing its authority, rather than as its own least-authority process holding nothing but its one file cap. spawning it properly confined is the next step and the whole point of the "explicit authority" thesis, so it matters — but the machinery's all there and the driver code doesn't change, only the wrapper.
- in-process also means the editor is a modal takeover: on the metal the read loop never ends (a serial line has no EOF), so `:stim` never returns to the prompt. and there's no `:q` yet — I deferred it with the rest of the grammar. Ctrl-C doesn't save you either: the kernel has no signals, so `0x03` just arrives at the byte reader and gets typed into your buffer like any other character. **the eternal joke about vim is that people can't figure out how to exit it. stim is a vim clone you genuinely cannot exit** — no quit command, no interrupt, the only way out is to kill the emulator. for once it's a spec and not a user error. `:q` and a properly-spawned, exitable editor are on the same fast-follow list.

- I set out, again, to write a text editor, and again it turned into a tour of everything underneath it — the lexer, the module system, the filesystem, the capability walk. it doesn't feel like a detour anymore; it feels like the point. the editor is the load test. and the thing it was load-testing passed: forty lines of pure Stitch, driven by a small Rust loop, editing a file on a microkernel and narrating every move it makes.
