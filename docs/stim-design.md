# stim — the editor where the axes meet a human hand

**Status: design / exploration (captured 2026-07-04). Pre-implementation.**
Records the design conversation for **stim** (stitch + vim), the v0.14 text
editor. Read [roadmap-and-milestones.md](roadmap-and-milestones.md) §v0.14 for the
milestone framing; [shell-surface-and-tui-design.md](shell-surface-and-tui-design.md)
for the "the shell is a Stitch program, Rust is the platform" thesis this
inherits; and — critically — [cross-cutting-axes-brainstorm.md](cross-cutting-axes-brainstorm.md)
plus [design-explorations-seven-questions.md](design-explorations-seven-questions.md),
which reframe half of stim's "twists" as *the editor being the interactive
demonstrator of a cross-cutting axis*.

---

## The one-line identity

**stim is not "vim for SnitchOS."** It's **the first interactive app where the
OS's two convictions — authority is explicit and delegated, everything is
observed — meet a human hand**, and its degenerate case happens to be a modal
text editor. A twist only counts if it falls out of one of those two convictions
(or the forming third, *everything is typed data*). Everything below is filtered
by that test.

The v0.14 milestone's one job (per the roadmap): prove the loop **console input →
app buffer → FS**, capability-confined and fully traced. The post angle: "editing
a file on a capability OS — and watching the bytes flow."

---

## What already exists (the substrate audit)

| Capability | Status | Where |
|---|---|---|
| Read a file | ✅ done | `Platform::fs_read` → FS-over-IPC `Lookup`+`Read` (`stitch/src/platform.rs`) |
| **Write** a file | ✅ at FS layer, ❌ not in Stitch | `fs_proto` has `Request::Write{offset,src}` + `Create`; FS server handles them (`user/fs/src/lib.rs`). `Platform` has **no `fs_write`** — read-only trait today. |
| Raw byte input | ✅ at syscall layer | `console_read(&mut buf)` returns raw bytes (`user/runtime/src/lib.rs`) |
| **Keypress** input | ❌ | `LineEditor`/`Platform::read_line` is line-discipline, ASCII-only — no arrows, mid-line edit, ctrl-chords (`stitch/src/line_edit.rs`) |
| Static rich output | ✅ Tier-0 | `stitch/src/render.rs` — tables/trees/colored, pure ANSI |
| **Structured read view** | ✅ done | `render.rs`: shape-dispatched `Value → text` (see "The projection seam") |
| Cursor / redraw / alt-screen | ❌ | Tier-1/2 don't exist; render.rs prints once |

So the substrate splits cleanly. The FS **write path exists but is not surfaced
to Stitch**; **raw input exists as bytes but the `Platform` seam only yields
finished lines**; the **structured read view is already built**. Those three
facts scope v0.14.

---

## The projection seam — render is schema-free, edit is schema-driven

The decisive architectural fact, learned from reading `render.rs`: **rendering a
Stitch `Value` needs no schema.** `render_with(value, style)` shape-dispatches on
the *runtime value* — a homogeneous record-list becomes a box table, a nested
record or sum variant an indented tree, a flat product a key/value table, a scalar
its `display`. It works because Stitch `Data` values are **self-describing**
(named fields, `variant` vs `type_name`). Clean model/style split (`Table` model +
`TableStyle` trait), pure, no_std, unicode-width-correct.

So the seam is **two seams**, and the `TypeSchema` lives on only one of them:

- **Render** = `Value → text` — schema-free, shape-dispatched. **Shipped** (`render.rs`).
- **Edit** = where a Hitch `TypeSchema` earns its keep: decoding raw file/xattr
  bytes *into* a `Value`, validating a mutation, and knowing the *edit-space*
  (which fields/variants are legal to add). Schema is for the **write/validate**
  side, never the read side.

**Text is the schema-less projection.** A file with no `user.iface` schema is a
`Value` of lines (`List<Str>` or `Str`) and stim is a modal text editor. A file
that carries a schema is the *same buffer* as a typed `Value` with a structural
view. One editor, two projections; **text is the fallback, not a separate
program.** This also completes the immutable-buffer twist: the buffer *is* a
`Value`, `render.rs` already renders it, undo is its version history, and a
structured edit produces a new `Value`.

### The one real gap: an *addressable* projection

`render.rs` is `Value → String` and throws away the screen↔value mapping. An
editor needs the inverse to be **addressable**: `Value → View`, where each
rendered node carries a **path back into the value** (`items[3].tag`), so a cursor
position resolves to "you are editing this field" and an edit produces a new
`Value`. render.rs produces flat strings with no back-pointers.

**Don't rebuild — generalize.** `Table` + `TableStyle` is already the
"compute-the-model-once, render-swappably" seam. stim lifts *display model* →
*addressable view model* and **reuses `as_table`/`as_kv`/`tree` as the layout
logic**. render.rs becomes the layout/style layer *beneath* the addressable view.
So structured mode is materially *less* new work than a from-scratch estimate: the
hard read-side layout (alignment, unicode width, table/tree dispatch) is written
and tested.

### A cheap intermediate milestone this opens

Structured **read-only navigation** is now nearly free: render.rs + a cursor over
its existing layout + Tier-1 redraw ⇒ "open a typed file and *browse* it." That is
a distinct, cheaper step between "text editor" and "full structured editor," and a
candidate for v0.14 (or v0.14.x). Schema-validated *editing* is the genuinely
deferred part.

### Two integration notes

- render.rs renders a `stitch::Value`; a file decoded via Hitch produces a
  `hitch::Value` (its own value type — see Q2). Displaying a decoded typed file
  needs a small **`hitch::Value ↔ stitch::Value` bridge**.
- Multi-line / nested-in-a-cell is explicitly flagged future work
  (`render.rs` test `a_list_column_holding_nested_records_still_tables_and_flattens_them`).
  Editing a nested structure *in place* is exactly where you hit it.

---

## The twists — annotated by enforcement strength

Each twist ships in a **soft** form now (language / editor-local) and **earns its
hard form as its cross-cutting axis lands**. That laddering is the honest framing:
v0.14 ships soft, labeled as such.

| # | Twist | Soft form (v0.14) | Hard form (which axis) |
|---|---|---|---|
| 1 | **Modes are authority levels** | Normal mode runs under an effect-handler set lacking the write effect; the interpreter refuses a write and *snitches* | The `~>`-spawned variant lacks the write **cap** — kernel-enforced. Needs the effect system (Q5) + manifest slots (Q1) |
| 2 | **Cap-confined to its file/subdir** | Handed exactly one file (or dir) cap; no ambient open | Declared as typed **manifest slots** + `bootstrap.get` (Q1); confinement is enforced by what was granted |
| 3 | **Immutable-value buffer; undo = version history** | Buffer is a Stitch `Value`; naive `List`-of-lines; undo is free | Native **rope / RRB-tree** for scale (structural sharing ⇒ O(log n) edit + cheap undo history) |
| 4 | **Every operator is a span** | `dd`/`ciw`/macro each a nested span in the session trace | Structural spans ("removed field `x.y`") once structured mode lands |
| 5 | **Edit stats are real metrics** | keystrokes / bytes-written / time-in-insert → Grafana | — (already the house pattern) |
| 6 | **`:%!cmd` filter *is* `~>`** | Buffer-filter spawns a program, hands it exactly the buffer bytes, touches only that | Reuses the shipped cross-process pipe; nearly free. Vim's ambient footgun → a capability demo |
| 7 | **Enforced read-only** | `:w` without the write cap is a snitched `SyscallRefused`, not a convention | — (kernel already refuses + snitches) |
| 8 | **Structured editing** | Text = schema-less projection; **read view shipped** (render.rs) | Addressable view + schema-validated edits; per-field write caps; can't-construct-invalid |

### Twist #1 has a mechanism now: modes = effect-handler membranes

The seven-questions doc (Q5) uses *the editor* as its worked example: modes are
**algebraic-effect handler membranes**, one mechanism at two enforcement
strengths. `with fs = readonly(fs) { … }` installs a handler attenuating every FS
effect in its extent — normal mode's handler set simply lacks the write effect
(soft, language); the `~>`-spawned variant lacks the write *cap* (hard, kernel).
**Same shape, same telemetry.** This is the principled version of "interpreter by
default, kernel in strict mode": not two ad-hoc options but one membrane at two
strengths. It *depends on* Stitch's `uses` becoming a real effect system (Q5,
"no ambient native caps") — so v0.14 ships the soft form and upgrades when the
effect system lands.

### Twists that are actually *other axes* — don't build them bespoke

- **Time-travel scrub** = **axis 1 (cross-substrate replay)** applied to an edit
  session. stim's local undo-history + operator-spans are the editor's own view;
  full scrub/replay rides the OS's replay axis and lands when that milestone does.
- **Session persistence** = **axis 6 (processes as values / checkpoint)**, which
  literally lists "editor session persistence" as a payoff. Don't build a bespoke
  save-my-session file; ride checkpoint.
- **Taint-aware yank/paste** = **axis 4 (observable information flow)**, verbatim
  tie-in: "text yanked from a secret buffer refuses to paste into a world-readable
  one, kernel-enforced, with the refusal span pointing at the yank." A *seventh*
  candidate twist, downstream of axis 4.
- **Budgeted macros** = **axis 5 (budgets as capabilities)**: a macro/plugin gets
  a budget slice; a runaway macro saturates its ceiling while the editor never
  stutters. Minor for v0.14.

The clean framing: **stim is where the axes become tangible.** Each twist earns
its hard enforcement as its axis lands; the editor is the standing demonstrator.

---

## No ambient input

The raw-keypress seam must **not** be ambient `console_read`. Q3 #3 names
`ConsoleRead` ambient as "the enemy" of the purity axis, and Q5 layer 2 is "no
ambient natives." So stim's input arrives as an **input capability threaded as a
value**, not an ambient call. stim's entire world is then `{input, file(s),
telemetry}` — a legible least-authority demo instead of a quiet reach for ambient
console. (v0.14 may ship the ambient path first for expedience, but the design
target is a cap.)

---

## Sequencing — where stim sits in the dependency graph

The two most-constraining design pieces (Q4) are the **manifest / authority-slot
language (Q1)** and **replay (axis 1)**. stim's *principled* forms of twists #1
and #2 sit **downstream of the manifest (Q1) and the effect system (Q5)**. The
honest call:

- **v0.14 ships the soft/language versions** — text projection, soft (handler-set)
  modes, cap-confinement via **today's positional handles**, operators-as-spans,
  metrics, enforced read-only — and **upgrades each twist as its axis lands.**
- Alternative (rejected as the default): slip stim behind manifest v2 + the effect
  system. Rejected because it makes v0.14 depend on the highest-fan-out build item;
  ship-soft-first keeps v0.14 achievable and honest.

stim should nonetheless be **designed to target manifest slots + `bootstrap.get`**
so it becomes the first real *consumer/demonstrator* of manifest v2 when that
lands, rather than re-inventing cap-plumbing.

---

## Substrate v0.14 must build

Regardless of twists, "stim" forces three pieces that do not exist:

1. **A raw-keypress `Platform` seam** (cap-shaped per "No ambient input") — the
   bytes are already at `console_read`; the trait is line-only today.
2. **`fs_write` on `Platform` + a `writeFile` native** — the FS layer already has
   `Request::Write`/`Create`; it is simply not surfaced to Stitch.
3. **Tier-1 rendering** — cursor positioning, clear-region, a redraw loop, over
   render.rs's existing layout (`render.rs` is Tier-0 print-once).

The vim grammar itself is almost all pure Stitch/computation once these exist.

---

## Engineering calls on record

- **Modes enforcement:** soft = effect-handler set (Q5), free once the effect
  system exists; hard = `~>`-spawned, born without the write cap. One membrane,
  two strengths.
- **Buffer performance:** naive `List`-of-lines for v0.14 (fine at RAMfs scale —
  a keystroke copies a few hundred refcounted `Rc` pointers). Native
  rope / RRB-tree is the scale path and a clean, host-testable pure-Rust lib on
  its own.
- **Projection:** render is schema-free (shipped); edit is schema-driven (new).
  The new component is an **addressable view model** over render.rs's layout.

## Open forks (decide at plan time)

- **Grammar scope for v0.14:** minimal vim core (`h j k l i a x dd o :w :q`,
  normal + insert only) vs. more (visual mode, operators+motions, registers).
  Recommend minimal core, grow.
- **Structured-mode UX:** projectional (edit structure directly, never see raw
  text — pure, novel, known UX friction) vs. text-with-schema-validation (edit
  text, parse + check against the schema — familiar, weaker). The load-bearing
  question for structured mode; not decided here.
- **Kernel-strict authority path:** "born read-only, request write on `i`" needs
  an *ask-parent-for-a-cap* IPC that does not exist yet — a feature (arguably a
  milestone: "watch an editor earn write access mid-session"), not v0.14 scope.
- **Whether structured read-only browsing is pulled into v0.14** (cheap, given
  render.rs) or held to v0.14.x.

## Cross-references

- [roadmap-and-milestones.md](roadmap-and-milestones.md) §v0.14 — the milestone.
- [shell-surface-and-tui-design.md](shell-surface-and-tui-design.md) — the
  Stitch-program / Rust-platform thesis and the Tier-0/1/2 TUI spectrum.
- [cross-cutting-axes-brainstorm.md](cross-cutting-axes-brainstorm.md) — axes 1
  (replay), 4 (IFC), 5 (budgets), 6 (checkpoint) that half the twists demonstrate.
- [design-explorations-seven-questions.md](design-explorations-seven-questions.md)
  — Q1 (manifest slots), Q3 #3 (ambient authority), Q5 (effect handlers as
  membranes — the editor is the worked example).
- [typed-processes-and-the-data-model-design.md](typed-processes-and-the-data-model-design.md)
  — Hitch, `TypeSchema`, `user.iface` xattr: the schema side of structured editing.
- `stitch/src/render.rs` — the shipped, schema-free read projection.
