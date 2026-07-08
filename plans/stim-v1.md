# Plan: stim v1 — a minimal modal editor as a Stitch program

**Status**: **RESUMED (2026-07-08)** — the A+B blocker is cleared: [Stitch core
redesign](stitch-core-redesign.md) Phase A (spans) is done and Phase B's fuel,
depth-guard, **and self-tail trampoline (B4)** are all in (`self_tail_recursive_*`
tests green). stim now builds on the rebuilt foundation as intended. The
**stim-vs-bytecode-VM** ordering is still decided at the redesign's post-Phase-D
decision point (informed by the Phase-B leak finding). B4's trampoline may let the
driver loop (Step 4.1) be Stitch, not a native.

**Group 1 COMPLETE (2026-07-08, 5/5)** — `Str.slice` + the `List` module
(`at`/`set`/`insert`/`removeAt`); all total, mutation-clean.

**Group 2 UNDERWAY** — the editor FSM in `fs-image/stim/stim.st` (own ramfs folder,
user decision). Done: **2.1 `initialState`** (`Mode`/`Editor` types + line split);
**2.2 `j`/`k`** (`moveUp`/`moveDown`, clamped, col re-clamp). All via the
`stitch::testing` harness. Next: **2.3 `i`/`Esc` mode switch**.

**Enabling fix (2026-07-08): built-in `use` now resolves in the REPL/single-program
path.** Found while answering "can I launch stim from the shell?": `build_env_in`
(the `eval_program`/REPL path, incl. `:load`'s `load_source`) silently dropped
`Item::Use` (`register_items` `_ => {}`), so the FSM's `use Str  use List` faulted
"unbound variable `Str`" — built-in linking lived *only* in the multi-module
`eval_modules` path. Fixed by linking whole-module built-in `use`s in `build_env_in`
(filter to `names: None` + known built-in → `link_imports` takes its infallible
arm). Now `:load /stim/stim.st` + calling `initialState`/`moveDown` works on the
metal (after a rebuild reseeds the ramfs). Regression tests:
`stitch/tests/builtin_module_use.rs` (incl. a `Repl`-API test driving the real
`:load` path). Selection imports (`use Str.{upper}`) in the single path stay a
no-op — still multi-module-path-only, a documented follow-up.

**Lands on**: `main`, incrementally (project convention: no feature branches;
the user commits each known-good increment). The five groups below are the
natural commit/review milestones.

## Goal

Open a delegated file, edit it modally (`j`/`k` + insert mode), `:w` to save —
written as a **Stitch program** over Rust primitive/effect natives, invoked from
the Stitch shell, cap-confined and traced. Demonstrable by one boot itest.

## Architecture (decided)

- **Stitch owns the logic.** The editor is a `.st` program: an immutable state
  value `{lines, row, col, mode}`, a pure `step(state, key) -> Step{state, effect}`
  transition (using `..spread` functional update), and `renderFrame(state) -> Str`.
  This is the thesis ("the editor *is* a Stitch program") and the transition is
  FP-shaped — Stitch's home turf.
- **Rust natives own the primitives + effects.** Two kinds: (a) *pure primitives*
  the FSM needs that Stitch lacks (string/list index slicing) — mutation-tested
  with `cargo-mutants`; (b) *host effects* — raw key read, console write, fs write
  (with truncate). All small and host-tested.
- **The driver is the runtime, not the logic.** A thin `stim` entry runs the
  unbounded read→step→effect loop and performs each effect. Kept out of Stitch to
  sidestep unbounded tail-recursion in the tree-walk interpreter — the loop is
  "the platform," the `step`/`render`/state are "the program" (same split as
  kernel↔userspace). *Open sub-decision at Step 4.1:* native trampoline (default,
  safe) vs. Stitch loop needing interpreter TCO (thesis-max follow-up).
- **The Stitch FSM's rigor** comes from behavior tests now, and from
  [stitch-mutation-testing-design.md](../docs/stitch-mutation-testing-design.md)
  later (a decoupled sibling — do not gate this plan on it).

## Scope (v1 = bones, no bells)

**In:** modes Normal + Insert; keys `j`/`k` (line up/down), `i`/`Esc` (mode
switch), printables + `Backspace` + `Enter` in insert; `:w` (save); full
clear-and-redraw each keystroke; whole-buffer save; cap-confinement + enforced
read-only; session + per-`:w` spans. Engine carries a full `(row, col)` cursor
(so `h`/`l` are unbound keys, not missing engine features).

**Out (fast-follow / other axes):** `:q` (Ctrl-C ends v1 — yes, it's an
un-exitable vim on purpose), `h`/`l`, `x`/`dd`/`o`, arrow keys (no ESC-sequence
parsing — every v1 key is a single byte), diffed redraw, structured editing, `~>`
filter, scrub/replay, checkpoint, taint-yank, metrics, the Stitch-loop/TCO variant.

## Acceptance Criteria

- [ ] Booted from the Stitch shell, `stim <file>` opens a delegated file and draws
      it full-screen with a cursor.
- [ ] `j`/`k` move the cursor between lines (clamped to buffer + line length).
- [ ] `i` enters insert; typing, `Backspace`, and `Enter` edit the buffer; `Esc`
      returns to normal.
- [ ] `:w` writes the whole buffer back through the write cap; a **shorter** buffer
      truncates correctly (no stale trailing bytes).
- [ ] Handed only a read cap, `:w` is a snitched `SyscallRefused`, not a silent
      no-op.
- [ ] The session is a span; each `:w` is a nested span.
- [ ] An itest boots init→shell→stim, feeds scripted keys, `:w`s, and asserts the
      file's new bytes (via re-read) and the spans on the wire.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR (see the `tdd` +
`mutation-testing` skills). Write the failing test FIRST, in its own edit, then the
impl (project rule). Rust natives are `cargo-mutants`-tested; the Stitch FSM is
tested through the interpreter harness (`run_program_on` + `insta` snapshots).

---

### Group 1 — Editor primitives (Rust natives; `cargo-mutants`-tested)

The string/list index ops the FSM needs, which Stitch lacks today. Each is one
step. (First confirm whether a string concat operator `++`/`+` already exists in
`ops.rs`; add `Str.concat` only if missing.)

#### Step 1.1: `Str.slice(s, start, end)` — char-indexed substring, bounds-clamped
**Acceptance**: `Str.slice("hello",1,3)=="el"`; out-of-range indices clamp (no
panic); `start>=end` → `""`. Counts Unicode scalars (chars), matching `Str.length`.
**RED**: `run_str` tests for the interior, both-ends-clamped, and empty cases.
**GREEN**: a `Str.slice` native via `chars()` indexing.
**MUTATE/KILL/REFACTOR**: per skill. **Done when**: criteria met, report reviewed.

#### Step 1.2: `List.at(xs, i) -> Maybe<T>` — ✅ DONE (2026-07-08)
**Acceptance**: in-range → `Some(elem)`; out-of-range → `None`.
**RED**: in-range and out-of-range tests. **GREEN**: index into the `Rc<[Value]>`.
Landed as the `List` builtin module (`BUILTIN_MODULE_SPECS`) + `listAt` native;
negative and past-end indices both `None` (total, never panics). Mutation-clean
(the one generated mutant is unviable — `Value: !Default`).

#### Step 1.3: `List.set(xs, i, v)` — functional, returns a new list — ✅ DONE (2026-07-08)
**Acceptance**: returns a list equal to `xs` with index `i` replaced; out-of-range
→ unchanged (or error — decide in RED); original `xs` unchanged.
**RED**: replace-middle + out-of-range + originality tests.
DECISION: out-of-range (negative or `>= len`) → **unchanged** (total, mirroring
`List.at`). `listSet` native + `List.set` mapping. Mutation-clean (`>=`→`<` and
`==`→`!=` both caught; whole-fn mutant unviable). 532 green. Originality test binds
the `[xs, ys]` pair — a bare trailing `[…]` maximal-munches onto the prior call as
an index (`stitch_maximal_munch_call_paren` applies to `[` too).

#### Step 1.4: `List.insert(xs, i, v)` — functional insert-before-index — ✅ DONE (2026-07-08)
**Acceptance**: inserts at `i` (0..=len); `len` appends; original unchanged.
**RED**: insert-front/middle/end tests.
`listInsert` native + `List.insert` mapping. Valid range `0..=len` (inclusive so
`i==len` appends); out-of-range (`> len` or negative) → unchanged (total). Mutation-
clean (all three `>`-boundary mutants — `==`/`<`/`>=` — caught by the append +
past-end tests; whole-fn mutant unviable). 533 green.

#### Step 1.5: `List.removeAt(xs, i)` — functional remove — ✅ DONE (2026-07-08)
**Acceptance**: removes index `i`; out-of-range → unchanged (or error — decide in
RED); original unchanged.
**RED**: remove-middle + out-of-range tests.
DECISION: out-of-range (`>= len` or negative) → **unchanged** (total, matching the
family). `listRemoveAt` native + `List.removeAt` mapping. Mutation-clean (`>=`→`<`
and filter `!=`→`==` both caught; whole-fn mutant unviable). 534 green.

**Group 1 COMPLETE (5/5).** The `List` builtin module = `at`/`set`/`insert`/
`removeAt`, all sharing one contract: total, never-panic, out-of-range is a value
(`None`/unchanged), original untouched. `Str.slice` from 1.1. All the primitives the
FSM needs now exist. **Group 2 (the editor FSM in `.st`) is unblocked.**

*PR boundary: "Stitch editor primitives" — the slice/index natives, useful beyond
stim.*

---

### Group 2 — The editor FSM in Stitch (`.st`; interpreter-tested)

Pure logic, no I/O. Tested by running the `.st` through the interpreter and
asserting on the returned state / rendered string. State is
`{lines: List<Str>, row: Int, col: Int, mode}` with `..spread` updates.

#### Step 2.1: `initialState(text)` splits text into a line buffer at row/col 0 — ✅ DONE (2026-07-08)
**Acceptance**: `"a\nb"` → lines `["a","b"]`, row 0, col 0, mode Normal; `""` → one
empty line. **RED**: a program asserting the constructed state's fields.
DONE: `sum Mode = Normal | Insert`, `prod Editor(lines, row, col, mode)`, and
`initialState = Editor(lines: Str.split(text, "\n"), row: 0, col: 0, mode: Normal)`
in **`fs-image/stim/stim.st`** (its own ramfs folder → `/stim/stim.st`).
`Str.split` gives `[""]` for `""` (one empty line, never zero). Snapshot-tested in
`stitch/tests/stim_fsm.rs` via `stitch::testing`.

**Harness (new infra, built this step):** `stitch::testing` — the `pub`, feature-
gated (`testing`) promotion of the old `pub(crate)` `test_support` helpers, plus a
new `run_source(defs, body)` that runs a `.st` program's function through the
module path (so `use Str`/`use List` resolve). External consumers: `tests/stim_fsm.rs`
now, the Stitch mutation tester later. Wired into the `xtask test` gate as
`("stitch", &["--features", "testing"])` (stitch was previously **not** gated at
all — its 535 tests only ran via a direct `cargo test -p stitch`).

#### Step 2.2: Normal-mode `j`/`k` move row, clamped, with col re-clamp — ✅ DONE (2026-07-08)
**Acceptance**: `j` at last line is a no-op; `k` at row 0 is a no-op; moving onto a
shorter line clamps col to its length. **RED**: boundary + clamp cases.
DONE: `moveUp`/`moveDown` (+ `lineAt`/`clampedCol` helpers) in `stim.st`, each a
`match {}` clamp returning the state unchanged at the boundary and `Editor(..state,
row, col: clampedCol(...))` otherwise. Uses prelude `count`/`unwrapOr`; clamps with
subjectless `match` (prelude `min`/`max` are *list* folds, not scalar `min(a,b)`).
5 FSM tests green (row move both ways, both no-op boundaries, col re-clamp both
branches). No cargo-mutants (Stitch code — behavior-tested; the Stitch mutation
tester covers the FSM later).

#### Step 2.3: `i` enters Insert, `Esc` returns to Normal
**Acceptance**: mode transitions both ways; buffer/cursor unchanged by the switch.
**RED**: round-trip mode test.

#### Step 2.4: Insert a printable char at `(row, col)`, advancing col
**Acceptance**: `"ac"` + insert `b` at col 1 → `"abc"`, col 2 (uses `Str.slice` +
concat + `List.set`). **RED**: mid-line + end-of-line insert.

#### Step 2.5: `Backspace` — delete prev char, or join with the previous line at col 0
**Acceptance**: col>0 deletes the char before the cursor (col−1); col==0 & row>0
joins this line onto the end of the previous (cursor lands at the join); col==0 &
row==0 is a no-op. **RED**: mid-line delete, line-join, and top-left no-op.

#### Step 2.6: `Enter` — split the current line at col into two lines
**Acceptance**: splits `line` into `[0,col)` and `[col,len)`; cursor to next line,
col 0; line count grows by one (uses `Str.slice` + `List.set` + `List.insert`).
**RED**: split-middle, split-at-end, split-at-start.

#### Step 2.7: `:w` produces a `Save` effect carrying the serialized buffer
**Acceptance**: entering `:w` yields `Step{state, effect: Save(text)}` where `text`
is the lines joined by `\n`; other keys yield `effect: Redraw`/`None`.
**RED**: assert the effect + serialized payload. (Command-line accumulation for
`:` then `w` then Enter, or a direct `:w` recognizer — decide in RED; keep minimal.)

#### Step 2.8: `renderFrame(state)` → an escape-sequence frame string
**Acceptance**: emits clear+home (`ESC[2J ESC[H`), each buffer line, and a cursor
move (`ESC[row;colH`, 1-based) to `(row, col)`. Snapshot-tested (`insta`).
**RED**: a snapshot of a small buffer's frame. **GREEN**: string assembly in
Stitch (`Str` ops) — no native beyond what Group 1 added.

#### Step 2.9: `step(state, key)` — top-level dispatch tying 2.2–2.7 together
**Acceptance**: dispatches by mode + key to the right sub-transition; unknown keys
are `None`/`Redraw` no-ops. **RED**: a table of (mode, key) → expected effect/state.

*PR boundary: "stim editor FSM (Stitch)" — the whole `.st` program, pure,
interpreter-tested.*

---

### Group 3 — Effect natives + Platform seam (Rust)

The host effects the driver performs, behind the `Platform` seam (host fake +
on-target). Includes the FS truncate slice (a real gap: `ramfs::write` only grows;
`fs-core` has no truncate).

**FS subdir-readiness (audited 2026-07-08).** Reads from subdirs work end-to-end at
any depth — `user/fs/build.rs` recurses `fs-image/`, `RamFs::seeded_with_xattrs`
does `mkdir -p` per segment, ramfs is a real inode tree (`Body::File | Body::Dir`),
and every client walks `/`-paths one `Lookup` at a time. So `fs-image/stim/…` seeds
to `/stim/…` and `:load /stim/stim.st` works with no fs change. (The stale ramfs
module doc that claimed "flat, subdirs Unsupported" was corrected.) The write side
has exactly TWO gaps, **neither subdir-specific** (both hit a root-level `:w` too):
(1) **no truncate** — Steps 3.2/3.3 below (fs-core method + `fs_proto::Op`=8 +
ramfs impl + server dispatch); (2) **no write-capable resolver / `:w`** — every
client walker requests `READ` only; stim itself won't walk (per 4.2 the shell
resolves with `READ|WRITE` and delegates the file cap), so this is a shell change in
Group 4, not new fs mechanism. `Create` already supports `kind=Dir` over IPC, so
even runtime subdir creation needs no protocol work.

#### Step 3.1: `Platform::read_byte()` (raw, single byte) + fake + on-target
**Acceptance**: fake replays scripted bytes then `None`; on-target drains
`console_read` a byte at a time (bypassing `LineEditor`). **RED**: fake-replay test.

#### Step 3.2: `fs-core::Filesystem::truncate(ino, len)` + ramfs impl
**Acceptance**: `truncate` shrinks a file to `len` (drops trailing bytes) and grows
with zero-fill; `read` afterwards reflects it. **RED**: shrink + grow ramfs tests.

#### Step 3.3: `fs_proto::Request::Truncate{len}` + fs-server handler
**Acceptance**: a `Truncate` request truncates the badged file (WRITE required;
refused + snitched without it). **RED**: proto roundtrip + server-handler test.

#### Step 3.4: `Platform::fs_write(target, bytes)` — Create-if-absent, Truncate, Write
**Acceptance**: writes the whole payload, truncating to its length first (shorter
payloads leave no stale bytes); fake records it; on-target does the FS-over-IPC
sequence through the write cap. **RED**: fake write + shorter-overwrite test.

#### Step 3.5: expose `readByte` / `writeConsole` / `fsWrite` as Stitch natives
**Acceptance**: the three natives are callable from `.st` and route to the
`Platform` seam. **RED**: a `.st` program driving each against `FakePlatform`.

*PR boundary: "stim effect natives + FS truncate" — the raw-read, fs-write, and
truncate substrate.*

---

### Group 4 — Driver + shell integration

#### Step 4.1: the `stim` driver loop (read byte → `step` → perform effect)
**Acceptance**: end-to-end against `FakePlatform` — scripted keys + a seeded file,
run to a `:w`, assert the final file content and the emitted console frames.
*Sub-decision (record in the step):* native trampoline calling the Stitch
`step`/`render` closures (default) vs. a Stitch loop (needs interpreter TCO —
defer). **RED**: a full scripted-session test asserting saved bytes.

#### Step 4.2: `stim <file>` from the Stitch shell — delegate the file cap; enforce read-only
**Acceptance**: the shell resolves the file, delegates its cap into stim, and shows
the grant (`CapEvent`); with only a read cap, `:w` is a snitched `SyscallRefused`.
**RED**: a shell-level test (read+write → save works; read-only → refusal snitches).

#### Step 4.3: tracing — a session root span + a span per `:w`
**Acceptance**: opening stim starts a session span; each `:w` opens a nested span;
both appear on the wire (via the `Telemetry` seam). **RED**: assert the span
sequence from a scripted session.

*PR boundary: "stim driver + shell invocation + tracing."*

---

### Group 5 — Boot itest (the demonstrable proof)

#### Step 5.1: an `xtask itest` scenario driving stim in QEMU
**Acceptance**: boot init→shell→`stim <file>`; feed scripted keys over the console;
`:w`; assert (a) the file's new bytes via a re-read, and (b) the session/`:w` spans
on the decoded wire. Registered in `SCENARIOS`; skips cleanly if no QEMU.
**RED**: the scenario asserting saved content + spans. (Integration — no MUTATE.)

*PR boundary: "stim boot itest" — v1 is demonstrable.*

---

## Pre-PR Quality Gate (each group)

1. Mutation testing (`mutation-testing` skill) on the Rust natives of that group.
2. Refactoring assessment (`refactoring` skill).
3. `cargo xtask clippy` (whole workspace) + host tests green.
4. Before the itest group and before "commit": `cargo xtask itest --repeat 10`
   (the commit gate — single-run-green has hidden flakes here before).

## Open items (surfaced, not blocking)

- **Where the `stim.st` program lives** — DECIDED (2026-07-08, user): **seeded into
  the ramfs** at `fs-image/stim.st` (recursively seeded; the shell `:load`s it, same
  as `double.st`/`greet.st`). One canonical file: ramfs seed + shell load + the
  host FSM tests' `include_str!` source.
- **Driver: native trampoline vs. Stitch loop + TCO** (Step 4.1) — default to the
  trampoline; the Stitch-loop variant is the thesis-max follow-up.
- **Command-line vs. direct `:w` recognizer** (Step 2.7) — keep minimal for v1.
- **`Str` concat operator** — confirm `++`/`+` exists before adding `Str.concat`.

## Thesis follow-ups (post-v1, each its own plan)

- Port nothing — the FSM is already Stitch. Instead: add the remaining grammar
  (`h`/`l`, `x`/`dd`/`o`, `:q`), then the axis-tie-in twists (modes-as-authority
  via effect handlers, structured editing via `render.rs`, `~>` filter, scrub,
  persistence) as each underlying axis lands.
- [Stitch mutation testing](../docs/stitch-mutation-testing-design.md) — then run
  it over the editor FSM to give the Stitch logic the same gate the natives get.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
