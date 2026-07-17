# The SnitchOS shell — surface, grammar, and TUI design

**Status:** **Design / exploration (captured 2026-06-27).** Pre-implementation.
Records a design discussion about *what the shell should be* — its surface
language and its terminal UX — as distinct from the *mechanism* of spawning with
delegated capabilities, which lives in
[plans/legacy/spawn-shell-and-console.md](../plans/legacy/spawn-shell-and-console.md). Read that
for `Spawn`/cap-delegation; read this for "what does the terminal look and feel
like, and what's the grammar."

**What's already shipped underneath this:** `ConsoleRead` + `ConsoleWrite`
(syscalls 14/19 — bytes in and out of the UART terminal), `Spawn`/`Exit`/`Wait`
(process lifecycle with cap delegation), the FS over IPC, and — the unlock that
makes a Stitch-program shell feasible — the **Stitch interpreter running on the
metal** as a userspace process (see `project_stitch_on_target` memory / post 7).

---

## The reframe: a shell encodes the OS's worldview

A shell isn't a neutral box — it bakes in what its OS thinks is *primary*. Unix's
shell is built on Unix's worldview: everything is a **file**, you move **text**
between **processes**, and authority is **ambient** (you can touch anything your
uid can). So its vocabulary is file-munging: `ls`, `cat`, `grep`, `rm`. Copy those
nouns and you inherit that worldview by accident.

SnitchOS's worldview differs in exactly two ways, and they should *be* the shell:

- **Authority is explicit and delegated** — no ambient reach; you *hold*
  capabilities and *grant* them.
- **Everything is observed** — the system narrates itself as spans/metrics/events;
  the trace is real.

So the SnitchOS shell shouldn't be a *file-munger*. It should be a **powerbox you
can see through**: the primary act is *granting authority*, and the trace is both
the output and the history. (Prior art for the powerbox lineage: Plash, CapDesk,
Genode, Fuchsia, Capsicum.)

### The reframed reflexes

| Unix reflex | what it assumes | SnitchOS reframe |
|---|---|---|
| `ls` | a global filesystem you can see | **`hold`** — what authority *you* have. You see your *world* (the caps you hold + what they name), not a global tree you don't. |
| `cat foo` | ambient read of any file | **`view foo`** — run a viewer, *handing it* a read cap to `foo`, and the shell *shows you the grant*. |
| `ps` / `top` | processes as opaque PIDs | **`watch`** — the live trace. Processes here are spans; you watch what each was allowed to do and did. |

(Prompt candidate: `∴` — "therefore" — distinctive and quiet.)

---

## Two surface sketches

### Sketch 1 — the grant shell (authority is the grammar)

You can't run anything without it being visible what it may touch. The shell
annotates every run with the delegation and a "touched only X" verdict.

```
∴ hold
  notes        read write
  sensors/     read
∴ view notes
  ├ grant read(notes) → view#7          ← a CapEvent on the wire
  │ buy milk
  │ fix sched bug
  └ view#7 exited 0 · touched only notes ✓
∴ give edit notes          # run edit, granting read+write(notes)
  ├ grant read,write(notes) → edit#8
  ...
∴ who can touch notes?     # query the delegation graph — unaskable in Unix
  you (rw) · edit#8 (rw, exited)
```

### Sketch 2 — the trace shell (the session is a trace)

The prompt is *alive* with system state; most "commands" are observations; the
scrollback is a span tree you can fold and replay, not a list of strings.

```
[heap 41% · 2 spans live] ∴ watch sensors
  sensor.hot.avg  31 → 32 → 30 …        (live, until you stop)
[heap 41%] ∴ since boot, who wrote notes?
  edit#8  wrote 14 bytes @ t=4.2s  (held read,write)
```

**The genuinely-SnitchOS move, in both:** your shell session is itself a span,
every command nests under it, and authority you confer is a `CapEvent` you can
see — so "history" is a trace, not a list of command strings.

### The forks (to decide before building)

1. **Primary verb** — mostly *granting* (a powerbox) or *observing* (a trace
   REPL)? Or one thing: grant, then watch the consequence.
2. **Does authority live in the grammar?** i.e. can you *never* run something
   without an explicit authority clause (least-authority enforced by syntax, not
   convention)?
3. **Is the prompt alive** (held caps / live spans / heap) or quiet?
4. **What replaces the filesystem mental model** — do you browse a tree at all, or
   only ever "what do I hold and what does it name"?

**Lean:** the soul is **"grant, then watch what it did with what you gave it"** —
sketch 1's explicit-grant grammar with sketch 2's trace as the feedback. A shell
that's *only* possible on this OS.

---

## The TUI — a terminal is already a 2D display

The unlock: **a terminal is already a 2D, color, addressable display — you talk to
it in escape codes.** ratatui, htop, vim — none use a framebuffer or GPU. They
write bytes like `ESC[2;5H` (move cursor), `ESC[32m` (green), `ESC[2K` (clear
line) to a terminal emulator that interprets them. We have *exactly* that channel:
`ConsoleRead` for bytes in, `ConsoleWrite` for bytes out. **The moment those
exist (they do), "dumb console I/O" *is* a TUI surface.** The "huge engineering
effort" people imagine is the graphics stack — and we don't need one.

**Host-testable, which fits the TDD culture.** A little library that takes a
screen model and emits escape-sequence bytes is *pure* — snapshot-test the byte
output, no QEMU. A useful subset (cursor move, color, clear-line, box/tree/table/
sparkline) is a few hundred lines of testable Rust, grown incrementally.

### What the liked ideas become, rendered — all just escape codes

**"who can touch notes?"** → a drawn tree, rights colored (read green, write amber,
exited dim):
```
notes  ·  file #3
├─ you      read write
└─ edit#8   read write   (exited)
```

**live metric** → an in-place sparkline that redraws as samples arrive:
```
sensor.hot.avg   32°   ▁▂▃▅▇▆▄▃   ●live
```

**grant / observe** → a split-screen "observatory": left where you *grant*, right
where you *watch*. The layout *is* "grant, then watch what it did with what you
gave it":
```
┌─ snitch ─────────────────────────────┬─ live ──────────────┐
│ ∴ view notes                         │ spans               │
│   ├ grant read(notes) → view#7       │  ▸ shell            │
│   │ buy milk                         │    ▸ view#7 ▱▱▰ 0.3s │
│   └ exited 0 · touched only notes ✓  │ metrics             │
│ ∴ _                                  │  hot.avg 32 ▃▅▇▆     │
│                                      │ grants              │
│                                      │  read(notes) → #7   │
└──────────────────────────────────────┴─────────────────────┘
 heap 41% · you hold: notes(rw) sensors(r)
```

### The TUI spectrum (cheap → rich)

- **Tier 0 — rich static output.** Line shell, but each command prints color +
  Unicode (trees for delegation, colored tables for `hold`, a sparkline that
  prints once). Cheapest; pure formatting; ~immediate once we use `ConsoleWrite`.
- **Tier 1 — live widgets.** In-place updates: a persistent status bar (the
  "alive prompt"), `watch` redrawing a region as data arrives. Needs cursor
  save/restore + a small redraw loop. Moderate.
- **Tier 2 — full-screen observatory.** Alt-screen, panes, focus, diffed redraws
  (only changed cells) — the mock above. Most work (a tiny layout + input-mode
  layer) but still zero graphics. ratatui-grade.

**Recommendation:** Tier 0 immediately (it makes even a plain line shell
unmistakably *ours*), architect the formatting lib so Tier 1/2 are a growth path
not a rewrite.

### Two honest caveats

1. **Channel split.** The TUI lives on the **UART** (`ConsoleWrite` → a real
   terminal emulator that interprets the escapes). The **telemetry** channel
   (virtio → collector → Grafana) stays the postcard stream and does *not* render
   ANSI. So the rich shell is the QEMU-console experience, separate from Grafana —
   the right split (UART = human terminal, virtio = the decoded telemetry stream).
   Worth checking `xtask` routes the UART to a real terminal, not a logged pipe.
2. **Rendering is free; the live *data* sometimes isn't.** Drawing a sparkline is
   cheap. *Feeding* it a system-wide metric needs a read path — and today
   telemetry is **emit-only** (no "read this metric" / "tap the trace" syscall).
   The version that's free *now*: the shell narrates **its own actions** — the
   grants it performs, the children it spawns and `Wait`s on — which it knows
   directly, and which is exactly enough for the "watch authority happen" demo. A
   live pane of the *whole system's* trace is a bigger, later thing →

### The telemetry-tap capability (future)

A system-wide live trace/metrics pane needs the shell to *observe* telemetry it
didn't emit — a **tap**. That's itself a great cap-flavored feature: "you hold a
cap to observe the system." It's a new mechanism (kernel feeds a subscriber, or a
metric-read syscall), deferred. The observatory's right pane **starts as the
shell's own delegation log** and *earns* the system-wide tap later.

---

## The convergence: the shell is a Stitch program

The strongest framing, and the reason the interpreter was ported to the metal:
**Rust is the platform, Stitch is the shell.** The shell is a **Stitch REPL on
SnitchOS**, and Rust provides the primitives as Stitch *natives* — exactly how
`emit`/`span` and `Str.upper` already work:

- terminal: `term.moveTo`, `term.color`, `tree`, `sparkline` → escape bytes (the
  host-testable rendering lib above, exposed as natives)
- caps: `hold()`, `grant(cap, to)`, `attenuate(cap, READ)`
- observe: `watch(metric)`, `spans()`
- console: `readLine()`, `write()`

This is Stitch's whole thesis made literal — *"the platform provides the effects"*
(the Roc line): the OS supplies capabilities/telemetry/console, the language
consumes them via natives (and, later, the `uses` capability rows). The "grant
grammar" sketched above **is** Stitch's capability system; `who can touch notes?`
is a query over the cap graph the language wants to model. The two side projects
become one application.

**Feasibility:** the `no_std` tree-walk interpreter already runs as a userspace
process with a live REPL (post 7). So `grant`/`view`/`watch`/`hold` as Stitch over
native syscall wrappers is now buildable — the remaining work is the natives
(cap/console/telemetry/TUI bindings) and the grammar, not the runtime.

---

## What's settled vs open

- **Settled (leaning):** powerbox identity ("grant, then watch"); `hold`/`view`/
  `watch` reframing of `ls`/`cat`/`ps`; the session-as-a-span; TUI via ANSI escapes
  with a host-testable rendering lib; Tier 0 first, Tier 1/2 as a growth path; the
  shell as a Stitch program with Rust natives.
- **Open:** whether authority is *enforced* by the grammar or only *made visible*;
  exact prompt + verb vocabulary; how far up the TUI spectrum to commit; the
  telemetry-tap mechanism (for system-wide observation).
- **Dependencies:** an `init` to spawn the shell with its session caps
  (plans/legacy/spawn-shell-and-console.md); the in-progress **notification** primitive
  (gateway to interrupt-driven input — which would also make the polled-RX latency
  work moot); the cap-delegation `CapEvent` trace (so grants are visible — "mostly
  free", item 21 in the spawn plan).
