# Plan: stim v1 — a minimal modal editor as a Stitch program

**Status**: **RESUMED (2026-07-08)** — the A+B blocker is cleared: [Stitch core
redesign](stitch-core-redesign.md) Phase A (spans) is done and Phase B's fuel,
depth-guard, **and self-tail trampoline (B4)** are all in (`self_tail_recursive_*`
tests green). stim now builds on the rebuilt foundation as intended. The
**stim-vs-bytecode-VM** ordering is still decided at the redesign's post-Phase-D
decision point (informed by the Phase-B leak finding). B4's trampoline may let the
driver loop (Step 4.1) be Stitch, not a native.

**Group 1 COMPLETE (2026-07-08, 5/5)** — `Str.slice` + the `List` module
(`at`/`set`/`insert`/`removeAt`); all total (out-of-range → `None`/unchanged),
mutation-clean, 534 green. **Next: Group 2 — the editor FSM in `.st`** (Step 2.1
`initialState`), the thesis proper. Open item to confirm at 2.1: where `stim.st`
lives (embedded in shell build vs. seeded into ramfs).

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

#### Step 2.1: `initialState(text)` splits text into a line buffer at row/col 0
**Acceptance**: `"a\nb"` → lines `["a","b"]`, row 0, col 0, mode Normal; `""` → one
empty line. **RED**: a program asserting the constructed state's fields.

#### Step 2.2: Normal-mode `j`/`k` move row, clamped, with col re-clamp
**Acceptance**: `j` at last line is a no-op; `k` at row 0 is a no-op; moving onto a
shorter line clamps col to its length. **RED**: boundary + clamp cases.

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

- **Where the `stim.st` program lives** (embedded in the shell build vs. seeded
  into the ramfs) — follow the shell's program-loading convention; confirm at 2.1.
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
