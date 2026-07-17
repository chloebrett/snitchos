# Plan: stim v1 — a minimal modal editor as a Stitch program

**Status**: **RESUMED (2026-07-08)** — the A+B blocker is cleared: [Stitch core
redesign](legacy/stitch-core-redesign.md) Phase A (spans) is done and Phase B's fuel,
depth-guard, **and self-tail trampoline (B4)** are all in (`self_tail_recursive_*`
tests green). stim now builds on the rebuilt foundation as intended. The
**stim-vs-bytecode-VM** ordering is still decided at the redesign's post-Phase-D
decision point (informed by the Phase-B leak finding). B4's trampoline may let the
driver loop (Step 4.1) be Stitch, not a native.

**Group 1 COMPLETE (2026-07-08, 5/5)** — `Str.slice` + the `List` module
(`at`/`set`/`insert`/`removeAt`); all total, mutation-clean.

**Group 2 UNDERWAY** — the editor FSM in `fs-image/stim/stim.st` (own ramfs folder,
user decision). Done: **2.1 `initialState`** (`Mode`/`Editor` types + line split);
**2.2 `j`/`k`** (`moveUp`/`moveDown`, clamped, col re-clamp); **2.3 `i`/`Esc`**
(`enterInsert`/`enterNormal`, round-trip identity); **2.4 `insertChar`** (first
buffer edit — `Str.slice` split + string `+` + `List.set`); **2.5 `backspace`**
(delete / line-join via `List.removeAt`); **2.6 `splitLine`** (Enter — split via
`List.insert`; inverse of backspace-join); **2.7 `save`** (`Effect`/`Step` types +
`serialize` + `Save` effect); **2.8 `renderFrame`** (state → ANSI screen string;
needed the new `\e`/`\r` lexer escapes); **2.9 `step`** (key→`Step` dispatch +
`Command` mode / `:w`).

**★ GROUP 2 COMPLETE (2026-07-09) — the whole pure FSM is a Stitch program.**
`fs-image/stim/stim.st`: types (`Mode`/`Effect`/`Editor`/`Step`), transitions
(`initialState`, `moveUp`/`moveDown`, `enterInsert`/`enterNormal`, `insertChar`,
`backspace`, `splitLine`), `serialize`/`save`, `renderFrame`, and `step`. 21 FSM
tests via `stitch::testing`, incl. two inverse-pair invariants (`Esc∘i`,
`Backspace∘Enter`). No I/O — pure logic. **Next: Group 3** (effect natives +
Platform seam + FS truncate) — the host side that lets a driver actually read keys,
write the console, and save.

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

**Out (fast-follow / other axes):** `:q` (genuinely un-exitable in v1 — no `:q`, and
SnitchOS has no signals so Ctrl-C just types `0x03`; the only exit is killing the
process. an un-exitable vim on purpose), `h`/`l`, `x`/`dd`/`o`, arrow keys (no ESC-sequence
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

#### Step 2.3: `i` enters Insert, `Esc` returns to Normal — ✅ DONE (2026-07-08)
**Acceptance**: mode transitions both ways; buffer/cursor unchanged by the switch.
**RED**: round-trip mode test.
DONE: `enterInsert`/`enterNormal` = one-line `Editor(..state, mode: …)` spreads.
Tested by a snapshot of the Insert state (mode flipped, buffer/cursor intact) + a
**round-trip identity** assert (`enterNormal(enterInsert(s)) == s`, full `Editor`
equality) proving nothing but the mode changed. 6 FSM tests green.

#### Step 2.4: Insert a printable char at `(row, col)`, advancing col — ✅ DONE (2026-07-08)
**Acceptance**: `"ac"` + insert `b` at col 1 → `"abc"`, col 2 (uses `Str.slice` +
concat + `List.set`). **RED**: mid-line + end-of-line insert.
DONE: `insertChar(state, ch)` = split the line with `Str.slice(line,0,col)` /
`Str.slice(line,col,len)`, concat `before + ch + after` (string `+` — resolved the
open item; `ops.rs:58`), `List.set` the line back, `col += Str.length(ch)`. Tested
mid/end/start-of-line + a **multi-line** case proving the edit lands on the cursor's
row and leaves siblings intact. 8 FSM tests green. First real workout of the Group-1
primitives composed together.

#### Step 2.5: `Backspace` — delete prev char, or join with the previous line at col 0 — ✅ DONE (2026-07-08)
**Acceptance**: col>0 deletes the char before the cursor (col−1); col==0 & row>0
joins this line onto the end of the previous (cursor lands at the join); col==0 &
row==0 is a no-op. **RED**: mid-line delete, line-join, and top-left no-op.
DONE: `backspace(state)` = three-arm subjectless `match` (col>0 delete via
`Str.slice`; row>0 join via `List.set(prev+cur)` + `List.removeAt(row)`, cursor →
`Str.length(prev)`; else no-op). Join tested on a **3-line** buffer (catches
hardcoded row-1/removeAt indices); no-op is a round-trip identity. 11 FSM tests
green. **All five Group-1 primitives now exercised by the FSM** (`removeAt` = the
line-join).

#### Step 2.6: `Enter` — split the current line at col into two lines — ✅ DONE (2026-07-08)
**Acceptance**: splits `line` into `[0,col)` and `[col,len)`; cursor to next line,
col 0; line count grows by one (uses `Str.slice` + `List.set` + `List.insert`).
**RED**: split-middle, split-at-end, split-at-start.
DONE: `splitLine(state)` = `Str.slice` head/tail, `List.set(row, head)` +
`List.insert(row+1, tail)`, cursor → (row+1, 0). Tested middle/end/start + a
multi-line row-targeting case, plus **`backspace(splitLine(s)) == s`** (the two are
inverses — one round-trip guards off-by-ones in both). 14 FSM tests green.

#### Step 2.7: `:w` produces a `Save` effect carrying the serialized buffer — ✅ DONE (2026-07-09)
**Acceptance**: entering `:w` yields `Step{state, effect: Save(text)}` where `text`
is the lines joined by `\n`; other keys yield `effect: Redraw`/`None`.
**RED**: assert the effect + serialized payload. (Command-line accumulation for
`:` then `w` then Enter, or a direct `:w` recognizer — decide in RED; keep minimal.)
DONE: introduced `sum Effect = Save(Str) | Redraw | Noop` (**`Noop`, not `None`** —
`None` is the prelude `Maybe`'s), `prod Step(state, effect)`, `serialize(state) =
Str.join(lines, "\n")` (`Value::display` renders Str raw, so no quoting), and
`save(state) = Step(state, Save(serialize(state)))` (state unchanged). Tested the
Save payload (match on `st.effect`), serialize round-trip, and state-unchanged.
**DECISION (the "decide in RED" call):** 2.7 delivers the effect vocabulary +
`serialize`/`save`; the `:`-command *keystroke* accumulation (command-line buffer /
mode) defers to **2.9 dispatch** — it needs the dispatch anyway, and 2.7's essence
(the Save payload) stands alone. 16 FSM tests green.

#### Step 2.8: `renderFrame(state)` → an escape-sequence frame string — ✅ DONE (2026-07-09)
**Acceptance**: emits clear+home (`ESC[2J ESC[H`), each buffer line, and a cursor
move (`ESC[row;colH`, 1-based) to `(row, col)`. Snapshot-tested (`insta`).
**RED**: a snapshot of a small buffer's frame. **GREEN**: string assembly in
Stitch (`Str` ops) — no native beyond what Group 1 added.
DONE: `renderFrame(state) = "\e[2J\e[H{body}\e[{row+1};{col+1}H"` where `body =
Str.join(lines, "\r\n")`. Uses string **interpolation** for the 1-based cursor
coords (`+` can't concat Int to Str). Snapshots verify `\e[2J\e[H…\e[1;1H` and a
cursor at `\e[2;4H`.
**Enabling lexer increment:** the lexer had no way to emit ESC (`0x1b`) from a
string literal (only `\n \t \" \\`, else `\X→X`). Added **`\e` (ESC 0x1b)** and
**`\r` (CR)** escapes (`lexer.rs`, RED/GREEN, standard + idiomatic; behaviour change
only for the previously-pointless `\e`/`\r`→`e`/`r`). 541 lib + 18 FSM tests green.

#### Step 2.9: `step(state, key)` — top-level dispatch tying 2.2–2.7 together — ✅ DONE (2026-07-09)
**Acceptance**: dispatches by mode + key to the right sub-transition; unknown keys
are `None`/`Redraw` no-ops. **RED**: a table of (mode, key) → expected effect/state.
DONE: `step(state, key) = match state.mode { Normal/Insert/Command }` → `stepNormal`
/`stepInsert`/`stepCommand`, each returning a `Step{state, effect}`. Handled keys
`redraw`, `:w` → `Save`, unbound → `noop`.
**KEY PROTOCOL (design decision):** the driver maps raw bytes → key *tokens*; the
FSM dispatches on tokens — a printable is its own 1-char text (`"a"`,`":"`), control
keys are symbolic (`"Esc"`/`"Enter"`/`"Backspace"`). `isPrintable(key) =
Str.length(key)==1` distinguishes them. Keeps byte encodings out of the FSM (Group 3
driver honours this).
**`:w` = the sanctioned direct recognizer:** added `Command` to `sum Mode` (no
`Editor` field change → no snapshot churn); `:`→Command, `w`→`save` + back to Normal
(no Enter, no command-line accumulation — deferred). 21 FSM tests (dispatch table +
the full `:` → `w` → Save path).

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

**Breakdown (2026-07-09).** Group 3 is the host side of the effects the pure FSM
names (`Redraw`/`Save`) — it spans four crates. Dependency graph:

```
3.2 truncate (fs-core + ramfs) ─┐
3.3 Truncate IPC op (proto + server) ─┴→ 3.4 Platform::fs_write ─┐
3.1 Platform::read_byte ─────────────────────────────────────────┼→ 3.5 natives
                                                                  │   (writeConsole reuses Platform::write — no new method)
                                                                  └→ Group 4 driver
```

3.1 and 3.2 are independent leaves; 3.3 needs 3.2; 3.4 needs 3.2+3.3.

**★ DECISION (user, 2026-07-09) — `fs_write` addresses a DELEGATED FILE CAP, not a
path.** `fs_write(fileHandle, bytes)` writes through a cap the shell resolved +
delegated into stim (Group 4.2); stim never walks the FS. **Read-only is
kernel-enforced** — `Write`/`Truncate` on a `READ`-only cap → a real
`SyscallRefused` (snitched), realizing 4.2's acceptance and the explicit-authority
thesis. **Knock-ons:** (a) **create-if-absent moves to the shell** (4.2 resolves the
path, creates the file if missing, then delegates its cap); (b) a **new `FsWrite`
authority** (parallel to `FsRead`) gates the `fsWrite` native — add it to the ambient
set in `interp.rs::build_env_in` (`["Telemetry","ConsoleOut","ConsoleIn","FsRead"]`)
and honour it in `uses`-checking; (c) the **driver holds the file handle** (from its
startup cap ABI) and passes it to `fsWrite` on a `Save` effect — the FSM stays
handle-free.

#### Step 3.1: `Platform::read_byte() -> Option<u8>` (raw, single byte) + fake + on-target — ✅ DONE (2026-07-09)
**Touches**: `stitch/src/platform.rs` (trait + `NullPlatform`/`FakePlatform`/
`RuntimePlatform`). **Design**: `Null`→`None`; `Fake`→a scripted byte queue (new
`with_bytes` ctor, `RefCell<VecDeque<u8>>`) drains then `None`; `Runtime`→raw
`console_read` into a small refill buffer, pop one byte — **bypasses `LineEditor`**
(unlike `read_line`), `yield_now`+retry when the console is empty.
**Acceptance**: fake replays scripted bytes then `None`; on-target drains
`console_read` a byte at a time. **RED**: fake-replay test. **Mutants**: the
refill/empty-queue boundary.
DONE: trait **default** `read_byte→None` (so `Null`/`Counting` doubles need no edit),
overridden in `Fake` (`pop_front`) + `Runtime` (blocking refill; never `None` on the
metal — a UART has no EOF, so v1 exits via Ctrl-C). Host-tested (fake replay + the
no-input default via `NullPlatform`); riscv lib compiles clean. **Mutation-clean
(10/10)** — the 2 initially-missed mutants were the untested trait default; the
`NullPlatform` test killed them. **Infra note:** `cargo mutants -p stitch` needs
`--features testing` (else the `stim_fsm`/`builtin_module_use` integration tests
fail to build) — fold into the `xtask mutants` fix.

#### Step 3.2: `fs-core::Filesystem::truncate(ino, len)` + ramfs impl — ✅ DONE (2026-07-10)
**Touches**: `fs-core/src/lib.rs` (trait method), `ramfs/src/lib.rs` (impl).
**Design**: `Body::File` → `Vec::resize(len, 0)` (shrink drops trailing bytes; grow
zero-fills); `IsADir` on a dir (the not-a-file error, mirroring `write`/`read`). The
one real *missing capability* (audit: `write` only grows).
**Acceptance**: shrink drops trailing bytes, grow zero-fills, `read` reflects both.
**RED**: shrink + grow ramfs tests. **Mutants**: the shrink-vs-grow branch, the
zero-fill length.
DONE: **required** trait method (RamFs is the only impl; `fs::serve` is generic so
unaffected). `Vec::resize(len, 0)` gives shrink-drop + grow-zero-fill in one call.
3 ramfs tests (shrink, grow, dir→`IsADir`); 20 ramfs green; fs-core builds;
mutation-clean (1/1 — the no-op mutant dies on the shrink test).

#### Step 3.3: `fs_proto::Op::Truncate = 8` + `Request::Truncate{len}` + fs-server handler — ✅ DONE (2026-07-10)
**Touches**: `fs-proto/src/lib.rs` (**append** `Op::Truncate = 8` — never reorder;
positional wire), `user/fs/src/lib.rs` (`fs::serve` dispatch). **Design**: wire
`kind_to/from_wire`, `Request`/`Response`, `required_right(Truncate) = WRITE`; server
calls `fs.truncate(badge-inode, len)`.
**Acceptance**: a `Truncate` truncates the badged file (WRITE required; **refused +
snitched without it**). **RED**: proto round-trip + server-handler + refusal tests.
**Mutants**: the rights gate, the op-discriminant mapping.
DONE: `Op::Truncate = 8` appended (+`ALL`[9], `from_u8`); `Request::Truncate{len}`
(`[op, len, 0, 0]`); `required_right` folds it into the `WRITE` arm; success reply =
`Response::Count(new_len)`. Server: one dispatch arm — the **generic `check_rights`
gate auto-refuses without WRITE** (→ `Denied`, snitched), so no per-op gate code.
`fs` compiles clean for riscv. Proto tests: dedicated round-trip + rights + folded
into the 3 exhaustive loops (`every_op/request/response`); renamed the stale
`read_and_write_are_the_only_gated_ops`. 37 fs-proto green; **mutation-clean on the
gate + opcode map** (11 caught / 2 unviable / 0 missed). *(Server dispatch isn't
host-testable — `serve` is `-> !` over IPC; covered by the Group-5 boot itest.)*

#### Step 3.4: `Platform::fs_write(fileHandle, bytes)` — Truncate-then-Write through a delegated cap — ✅ DONE (2026-07-10)
**Touches**: `stitch/src/platform.rs`. **Design** (per the decision above): NOT a
path walk. `Fake`→record `(handle, bytes)` + return success (or scripted refusal);
`Runtime`→`Endpoint::from_raw_handle(fileHandle)`, issue `Truncate(len)` then
`Write(0, bytes)` in ≤256-byte chunks (`DATA_CAP`). **Create-if-absent is the
shell's job** (4.2), not here.
**Acceptance**: truncates to the payload length first (no stale trailing bytes),
then writes; fake records the sequence; a `READ`-only cap → refusal. **RED**: fake
records truncate-then-write; refusal path. (End-to-end "shorter save leaves no stale
bytes" is proven by 3.2/3.3 at the fs layer and by the Group-5 re-read.) **Mutants**:
the truncate-before-write ordering, the chunk loop.
DONE: trait default `fs_write→false` (no FS); `Fake` records `(Handle, Vec<u8>)` +
`writes()` getter + `deny_writes()` (models a read-only cap → `false`, records
nothing); `Runtime` does `Truncate(len)` then chunked `Write` through the delegated
`Endpoint`, `false` on any refusal (the read-only kernel-enforcement). Host-tested
(record + refusal + `NullPlatform` default); riscv clippy-clean. Mutation: 8/9 caught,
**1 equivalent** (default `→false` mutated to `false` — unkillable, like the documented
`advance_anchor` one). 547 lib green.

#### Step 3.5: expose `readByte` / `writeConsole` / `fsWrite` as Stitch natives — ✅ DONE (2026-07-10)
**Touches**: `stitch/src/natives.rs` (+ the `FsWrite` authority in `interp.rs`).
**Design**: `readByte() -> Maybe<Str>` (the byte as a 1-char string, or `None`;
gated by `ConsoleIn`); `writeConsole(s)` — **raw write, no trailing newline** (unlike
`print`; needed for escape sequences; gated by `ConsoleOut`, reuses `Platform::write`);
`fsWrite(fileHandle: Int, bytes: Str) -> Bool` → `Platform::fs_write` (gated by the
new `FsWrite`).
**Acceptance**: the three are callable from `.st` and route to the seam; each is
refused without its authority. **RED**: a `.st` program driving each against
`FakePlatform` (+ undeclared-authority refusals). **Mutants**: mostly covered by the
Platform-level tests.
DONE: three flat natives (`readByte`→`char::from(byte)` 1-char string; `writeConsole`
raw via `Platform::write`; `fsWrite`→`Platform::fs_write`, `Int`→`u32` handle) each
`has_authority`-gated; `FsWrite` added to the ambient set (`interp.rs:82`). **Note:**
the concurrent Group-F cleanup migrated the `NATIVES` table to inline `module`/
`export_as` (my `List` natives came along, still namespaced) — new natives are flat
`module: None`. 6 native tests (3 route + 3 refusals), 553 lib green, clippy-clean
host+riscv, mutation-clean (3 caught / 3 unviable / 0 missed).

*PR boundaries:* **(a)** "FS truncate through the stack" (3.2 + 3.3 — one vertical
slice: trait → ramfs → proto → server); **(b)** "stim effect natives + Platform seam"
(3.1 + 3.4 + 3.5 + the `FsWrite` authority).

**★ GROUP 3 COMPLETE (2026-07-10) — the whole host substrate.** `read_byte`,
`truncate` (trait+ramfs+proto+server), `fs_write` (delegated-cap), and the three
natives + `FsWrite` authority. Everything host-tested + riscv-verified + mutation-
clean. **Next: Group 4** — the `stim` driver loop (read byte → `step` → perform
effect), shell invocation (resolve + create-if-absent + delegate the file cap;
read-only refusal), and tracing.

---

### Group 4 — Driver + shell integration

**Design pass (2026-07-10).** Decisions: **#2 native trampoline first** (the Stitch-loop
variant — now unblocked by B4 — is a post-v1 thesis follow-up); **#6 `:stim <path>` is a
REPL command** (parallel to `:load`, not a native).

**★ Core constraint that shapes the whole group:** 4.1's acceptance drives the loop
against `FakePlatform` (a host test), so the **driver logic must be host-buildable and
`Platform`-generic**. Since `apply_values` is `pub(crate)`, that means it lives *in* the
`stitch` crate as a new **`stitch::stim`** module (`pub fn run<…>(…)` over `Rc<dyn
Platform>`), using crate-internal `apply_values` / `build_env` / `Value` — **no public
embedding API to build** (the phase-2 Stitch loop would shrink the Rust side anyway, so
don't over-invest here). The on-target `:stim` command is a thin wrapper that constructs
the real backends and calls the same `run`.

**★ #4 DECIDED (user, 2026-07-10): spawn a least-authority process — but in-process
first** (a phase split like #2). **Phase 1:** `:stim` calls `run` **in-process** (stim
runs inside the REPL, read-only still real via the file cap's rights). **Phase 2 (the
thesis):** `:stim` Spawns a least-authority `stim` process holding only telemetry + span
+ console + the one file cap, whose `main` reads the cap at `delegated_handle(0)` and
calls the same `run`. The shared `run` means Phase 2 only swaps the thin wrapper.

#### Step 4.1: the `stim` driver loop — `stitch::stim::run` (native trampoline) — ✅ DONE (2026-07-10)
**Status**: `stitch/src/stim.rs`. Used `apply_values` (pub(crate)) not `eval_call`
(private). **4 tests**: edit→`:w`→saved bytes + drew frame; no-`:w`→no save; read-only
(`deny_writes`)→refused, records nothing; and CR/DEL bytes→Enter/Backspace tokens
end-to-end (the driver's unique byte-mapping responsibility). 569 lib green, clippy
host+riscv, **mutation-clean (10 caught / 2 unviable / 0 missed)**. *(Was written +
inspection-verified while `interp.rs` was mid-Phase-C-churn; compiled + passed clean
once the tree settled — no fixes needed beyond one `cloned_ref_to_slice_refs` clippy
nit → `core::slice::from_ref`.)*

**Touches**: new `stitch/src/stim.rs`. **Design**:
`run(source, file_content, file_handle, platform)` —
1. `build_env(prelude + parse(source))` **once** (heeds the B5 leak finding — env built
   once, closures applied per key, never `eval_program` per key).
2. look up the `initialState` / `step` / `renderFrame` closures (`env.lookup`);
   `state = apply(initialState, [Str(content)])`; perform one initial Redraw.
3. loop: `platform.read_byte()` → `None` ends the loop (fake EOF / test end; the metal
   blocks, so on-target it runs till Ctrl-C). `Some(b)` → `key = byte_to_key(b)` →
   `stepv = apply(step, [state, Str(key)])` → pull `state`/`effect` fields off the `Step`
   record → perform: **Redraw** `platform.write(apply(renderFrame,[state]))`; **Save(text)**
   `platform.fs_write(file_handle, text.bytes)`; **Noop** nothing.
- `byte_to_key(u8)`: `0x1b`→`"Esc"`, `0x0d|0x0a`→`"Enter"`, `0x7f|0x08`→`"Backspace"`,
  else `char::from(b)` — the raw-byte→token map the FSM's 2.9 protocol assumes (keeps
  encodings out of the FSM). Small Rust helper, host-tested.
- Marshalling helpers (local): `field(&Value,&str)->Option<&Value>` over a Data record's
  fields; effect-variant match on `Value::Data{variant,fields}`.
**Acceptance**: end-to-end against `FakePlatform` — scripted keys + a seeded file, run to
`:w`, assert `fake.writes()` (saved bytes) and `fake.output()` (console frames).
**RED**: a full scripted-session test asserting the saved bytes. *(Source = `include_str!`
of `fs-image/stim/stim.st`, as in `stim_fsm.rs`.)* **Mutants**: `byte_to_key` arms, the
effect dispatch.

#### Step 4.2: `:stim <file>` from the shell — resolve, create-if-absent, in-process run (Phase 1) — ✅ IMPLEMENTED (2026-07-10), behavior via Group-5
**Touches**: `user/hello/src/bin/stitch_repl.rs` (`:stim` handler + `open_file_rw` — the
new **write-capable path resolver**), `fs-proto` (re-export `NodeKind` so callers get it
without a direct `fs-core` dep).
**Design (Phase 1, in-process — per #4's phase split)**: `:stim <path>` →
`open_file_rw(path)` walks every component with `READ|WRITE` (a minted cap's rights are
`parent ∩ requested`, so a `READ` hop would strip WRITE below it), **creates the leaf
File if absent** (the write side Group-3 `fs_write` skips), reads its current content, and
returns `(cap_handle, content)`; then `stitch::stim::run(<include_str! source>, content,
handle, &RuntimePlatform)` runs **in-process** (stim uses the REPL's platform; read-only
still real via the file cap's rights). `read_all` extracted from `read_file` and shared.
Compiles clean for riscv, clippy-clean; **behavior proven by the Group-5 boot itest** (IPC,
not host-testable). **Phase 2 (deferred):** Spawn a least-authority `stim` process
delegating `[file cap]`; `run` is unchanged.
**⚠ Caveats (record for Group 5 / follow-up):** (1) **read_byte vs read_line** share one
`RuntimePlatform`/console — the REPL reads lines, stim reads raw bytes; a mode switch may
leave stale `LineEditor` state (fine for scripted itest; revisit for interactive use). (2)
**in-process is a modal takeover** — on the metal `run` blocks forever (a UART never EOFs),
so `:stim` never returns to the REPL. **stim is genuinely un-exitable in v1: no `:q`, and
no signals** — SnitchOS has no Ctrl-C, so `0x03` just reaches `read_byte` and gets *typed
into the buffer* like any byte. The only exit is killing the process (quit the emulator).
Phase-2 spawn doesn't add an interrupt (still no signals) — it makes stim a *separate*
process so that when it eventually exits (`:q`, fast-follow) the shell survives, and
killing stim doesn't take the shell down. (3) read-only *refusal semantics* already
host-tested at 3.4 (`deny_writes`); this step is the metal path + shell glue.

#### Step 4.3: tracing — a session root span + a span per `:w` — ✅ DONE (2026-07-10)
**Verified**: 5 stim tests green (incl. the span-sequence assertion); `stitch_repl`
builds clean for riscv (4.2+4.3 on-target); mutation-clean (10 caught / 2 unviable /
0 missed — the `span_open`/`span_close` calls are unit statements cargo-mutants can't
mutate, but the span-sequence test pins their order exactly). 574 lib green.

**Touches**: `stitch/src/stim.rs` (the driver emits, since the FSM is pure) + the
`stitch_repl` `:stim` call site.
**Design**: `run` gains a `telemetry: &dyn Telemetry` param; opens `stim.session` for the
whole loop and, in `perform`, wraps each `Save`-effect `fs_write` in a nested `stim.save`
(`span_open`/`span_close` on the `Telemetry` seam). Tests pass a `RecordingTelemetry` and
read `snapshot()`; on-target `:stim` passes a `RuntimeTelemetry` (spans → real wire frames).
**Status**: written — RED span-sequence test asserts `open session, open save, close save,
open save, close save, close session` for `b":w:w"`; `run`/`perform` emit; all 5 call sites
updated. **Compile/test blocked by concurrent Phase C churn** (lib won't build — `ClosureData`
gaining a `source` field mid-refactor, unrelated to stim). Verify once settled:
`cargo test -p stitch --lib stim` (5 tests) + `cargo build -p hello --bin stitch_repl
--target riscv64gc-unknown-none-elf`; then MUTATE.
**Caveat**: on the metal `run` never returns (UART has no EOF), so `span_close(session)`
only fires on the finite fake path — on-target the session span stays open for the
process's life (fine; it *is* the session).
**Acceptance**: a scripted session emits the session span then a nested save span per `:w`.
**RED**: assert the span sequence from a scripted `run` (host, via the recording telemetry —
`run_program_events`-style).

*PR boundary: "stim driver + shell invocation + tracing" (4.1 host-testable + mutation-
tested; 4.2/4.3's on-target glue proven by the Group-5 boot itest).*

**★ GROUP 4 COMPLETE (2026-07-10) — the driver, shell invocation, and tracing.** The pure
FSM and the host substrate are wired together: `stitch::stim::run` drives read→step→
perform (native trampoline), `:stim <file>` resolves/creates + runs in-process, and the
driver emits session + per-`:w` spans. 574 lib green, riscv builds, mutation-clean.
**Next: Group 5** — the boot itest (the demonstrable proof + validates the 4.2 caveats).
**Deferred (post-v1):** #2 Stitch-loop driver; #4 Phase-2 spawn (least-authority stim).

---

### Group 5 — Boot itest (the demonstrable proof)

#### Step 5.1: an `xtask itest` scenario driving stim in QEMU — ✅ DONE (2026-07-10)
**Acceptance**: boot init→shell→`stim <file>`; feed scripted keys over the console;
`:w`; assert (a) the file's new bytes via a re-read, and (b) the session/`:w` spans
on the decoded wire. Registered in `SCENARIOS`; skips cleanly if no QEMU.
**RED**: the scenario asserting saved content + spans. (Integration — no MUTATE.)
DONE: `stim_edits_a_file_and_saves` (`workload=stitch-fs`). Boots the REPL, `:stim
note.txt` (resolver creates it + WRITE cap), waits for the **`stim.session`** span,
then sends `iZQXMARK\x1b:w` and asserts **`ZQXMARK` on the UART** (renderFrame drew
the buffer — `read_byte` doesn't echo, so it's a genuine bytes→FSM→render→console
proof) and the **`stim.save`** span. **Passed on the metal** (`max wait 0.6s`).
**Re-read deferred:** in-process stim is a modal takeover (REPL never returns), so
byte-level re-read waits for Phase-2 spawn; render-marker + save-span is the Phase-1
proof. Waiting for the `:stim` line's span before sending raw keys sidesteps the
read_byte/read_line console-sharing caveat.

*PR boundary: "stim boot itest" — v1 is demonstrable.*

**★★ STIM v1 SHIPPED (2026-07-10) — all five groups done.** Primitives (G1) → the FSM
as a Stitch program (G2) → the host substrate (G3) → driver + `:stim` + tracing (G4) →
the boot itest (G5). stim runs **end-to-end on the metal**: `:stim note.txt` edits and
`:w` saves, with session/save spans on the wire. The thesis is real — the editor *is* a
Stitch program, cap-confined, fully observed.
**Deferred (post-v1, each its own follow-up):** #2 the Stitch-loop driver (unblocked by
B4); #4 Phase-2 spawn (a least-authority stim process → real cap-confinement + Ctrl-C
returns to the shell + byte-level re-read verification); the fast-follow grammar (`:q`,
`h`/`l`, `x`/`dd`/`o`) and the axis twists. **Before commit:** the full unit gate +
`cargo xtask itest --repeat 10` once the concurrent Phase-C `runner.rs`/`source` churn
settles (this run used `--skip-unit-tests` around a transient doctest break).

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
  - **Grammar batch DONE (2026-07-11):** **`:q`** (new `Quit` effect + `quit` helper
    + `q` arm in `stepCommand`; the driver breaks its loop on `Quit` via `is_quit`,
    which also closes the session span cleanly and un-sticks the modal takeover —
    you return to the `stitch>` prompt). **`h`/`l`** (`moveLeft`/`moveRight`,
    clamped within the line), **`x`** (`deleteChar` — under-cursor, no-op at EOL,
    col re-clamps), **`o`** (`openLine` — empty line below + Insert). All in
    `fs-image/stim/stim.st`, routed in `stepNormal`/`stepCommand`; the driver's
    `byte_to_key` already maps them (printables), so no driver change beyond
    `is_quit`. 24 FSM tests + driver `:q`-quits test (mutation-clean 2/2); the metal
    itest still passes. *Remaining:* `dd`, `:q!`/unsaved-guard, arrow keys.
    ⚠ `:q` return-to-prompt works interactively but a *burst* sending bytes in the
    same console chunk as `:q` can lose them to the `read_byte` prefetch (the
    console-sharing caveat) — so a scripted `:q` metal itest is deferred (wants a
    span-end matcher or careful send timing).
- [Stitch mutation testing](../docs/stitch-mutation-testing-design.md) — then run
  it over the editor FSM to give the Stitch logic the same gate the natives get.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
