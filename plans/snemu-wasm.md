# Plan: snemu in a browser tab (milestone 1 — the boot log)

**Branch**: main (project works directly on main; the user commits)
**Status**: Active

Companion to [docs/snemu-wasm-design.md](../docs/snemu-wasm-design.md), which carries
the rationale and the decisions. This plan is the increments.

The premise, verified live rather than assumed: **`cargo build -p snemu --lib --target
wasm32-unknown-unknown` succeeds today, unmodified.** The emulator core needs no
changes. `jit.rs` self-excludes via its inner
`#![cfg(all(target_arch = "aarch64", target_os = "macos"))]`, `cpu.rs` has a paired
`run_block_native -> None` fallback to Backend A, and `libc`/`minifb` are already
scoped off wasm32. The lib has no fs, threads, sockets, entropy, or clock; the clock is
`instret`. So this plan builds a **shim and a page**, not a port.

## Goal

A static page that fetches the release kernel ELF, boots it in snemu compiled to wasm,
streams the UART boot log into the DOM, and renders decoded telemetry `Frame`s as a
live span/metric view — without ever freezing the tab. No canvas, no guest input, no
wall-clock pacing.

## Explicitly out of scope

- **The canvas / ramfb path.** The default boot (`init`) draws nothing and
  `enable_fwcfg_ramfb()` is opt-in, so pixels need a drawing workload wired up too —
  a second project riding along. Milestone 2, once this proves the shim.
- **Guest input.** `push_console_input()` exists and works; wiring keystrokes is
  milestone 3.
- **Wall-clock pacing.** Named in [docs/scaling-down-snitchos.md](../docs/scaling-down-snitchos.md);
  irrelevant to a page that boots, prints, and stops. Milestone 4.
- **Backend B / any JIT in the browser.** wasm gets Backend A by construction.
- **A bundler, React, or `viz/` convergence.** `wasm-pack --target web` emits an ES
  module a `<script type="module">` loads directly. Keep the build step at zero.

## Precedent to mirror

**`xtask/src/itest/harness.rs` is already a second embedder of the lib** — it holds a
`snemu::machine::Machine` and drives `step()` in a loop (`harness.rs:45,60,108`). The
browser host plays the identical role; read it before writing the shim.

For the crate's internal shape, mirror `snemu/src/framebuffer.rs`: `to_minifb_buffer`
is a **pure, host-tested** function and `machine.rs` only wraps it. Every non-trivial
behaviour here goes in a pure function tested by `cargo test` on the host; the
`#[wasm_bindgen]` layer stays a shell too thin to hide a bug. That is what keeps TDD
honest for a wasm target.

`cargo xtask test` picks up a new workspace member **automatically** —
`itest::run_unit_tests` derives its list from `workspace_members()` minus
`NOT_HOST_TESTED`. Nothing to update; do not add it to a list.

## Acceptance criteria

- [ ] `cargo test -p snemu-wasm` runs on the host and covers the drain cursor and
      status encoding.
- [ ] `cargo xtask test` runs `snemu-wasm`'s suite without any list edit.
- [ ] Opening the page boots the real kernel and shows the UART boot log, ending with
      a `kernel.heartbeat`-era log line.
- [ ] The page shows decoded telemetry: at minimum `kernel.boot`, and span/metric
      names resolved through their `StringRegister`s.
- [ ] The tab stays responsive throughout (a button or animation keeps working while
      the guest boots).
- [ ] Two loads of the same page produce byte-identical UART output — determinism
      survives the browser.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code without
a failing test. Steps 0 and 1 are the exceptions worth naming: step 0 is a comment fix
and a dependency move (no behaviour), and step 1's only content is a manifest.

### Step 0: Correct the stale JIT gate comment and scope `clap` off wasm32

**Acceptance criteria**: `jit.rs:5`'s comment names the real gate
(`aarch64` + `macos`), not the `cfg(not(wasm))` it claims. `clap` moves to
`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`, matching the precedent
`libc`/`minifb` already set — it's a `main.rs`-only dep and shouldn't be in the browser
dep graph. `cargo xtask clippy` and the wasm lib build both still pass.
**RED**: None — a comment and a manifest scope, no behaviour. Called out rather than
smuggled in.
**GREEN**: The two edits.
**MUTATE**: N/A (no logic).
**REFACTOR**: N/A.
**Done when**: `cargo build -p snemu --lib --target wasm32-unknown-unknown` and
`cargo build -p snemu` both pass; human approves commit.

### Step 1: Add an empty `snemu-wasm` workspace member that host-tests green

**Acceptance criteria**: `snemu-wasm/` exists as a workspace member with
`crate-type = ["cdylib", "rlib"]`, depends on `snemu` + `protocol` (std) +
`wasm-bindgen`, and `cargo xtask test` runs its (trivial) suite **without any list
edit** — proving the metadata-derived pickup. The `rlib` half is what lets the pure
core be host-tested at all.
**RED**: A placeholder test asserting the crate is reachable, failing before the crate
exists.
**GREEN**: The manifest + a `lib.rs`.
**MUTATE**: N/A (no logic yet).
**REFACTOR**: N/A.
**Done when**: `cargo xtask test` shows a `snemu-wasm` suite; human approves commit.

### Step 2: A pure drain cursor over cumulative device output

**Acceptance criteria**: `uart_output()` returns the **whole** buffer every call
(`uart.rs:83` — `out: Vec<u8>` only ever appends), so the shim must track what it has
already handed out. A pure `Cursor` returns only bytes since the last drain, returns
empty when nothing is new, and never loses or repeats a byte across a boot's worth of
calls. Same type serves `virtio_tx_output()`.
**RED**: Tests for: fresh cursor drains everything; second drain of unchanged buffer is
empty; drain after append yields only the appended bytes; the concatenation of all
drains equals the buffer.
**GREEN**: A `Cursor { consumed: usize }` with `drain<'a>(&mut self, buf: &'a [u8]) -> &'a [u8]`.
**MUTATE**: Run the `mutation-testing` skill on `snemu-wasm`.
**KILL MUTANTS**: Address survivors — the off-by-one on `consumed` is the one that
matters.
**REFACTOR**: Assess only if it adds value.
**Done when**: All criteria met, mutation report reviewed, human approves commit.

### Step 3: A pure step-budget outcome type

**Acceptance criteria**: A pure function turns a bounded stepping run's outcome into a
status the JS side can branch on — `Running`, `Halted`, `Trapped(reason)` — with the
instret retired. The budget is denominated in **guest instret, not host step-calls**;
[snemu-08](../posts/snemu-08-zero-to-a-hundred-in-two-seconds-flat.md) records exactly
this unit confusion costing real debugging time ("sixty million steps scanned two
hundred and forty-five million guest instructions"). Do not repeat it.
**RED**: Tests that a run hitting its budget reports `Running` with the instret spent;
that a `StepError` maps to `Trapped` carrying the reason; that a zero budget retires
nothing.
**GREEN**: The status enum + the mapping function.
**MUTATE**: Run the `mutation-testing` skill.
**KILL MUTANTS**: Address survivors.
**REFACTOR**: Assess.
**Done when**: All criteria met, mutation report reviewed, human approves commit.

### Step 4: Decode telemetry frames to a JS-shaped value

**Acceptance criteria**: Raw `virtio_tx_output()` bytes decode through
`protocol::stream` into `OwnedFrame`s in-process, and a pure function projects them
into a serializable shape (frame kind + resolved string names + ids). A partial frame
at the end of a drain is **held, not dropped** — the drain boundary is arbitrary and
will land mid-frame. Interning is resolved so the page shows `kernel.boot`, not a
`StringId`.
**RED**: Tests that a known frame byte sequence decodes to the expected projection;
that bytes split across two drains still decode once whole; that a `SpanStart`
resolves its name through an earlier `StringRegister`.
**GREEN**: The decode + projection, holding a partial-frame remainder.
**MUTATE**: Run the `mutation-testing` skill.
**KILL MUTANTS**: Address survivors — the partial-frame boundary is the one to prove.
**REFACTOR**: Assess.
**Done when**: All criteria met, mutation report reviewed, human approves commit.

### Step 5: The `#[wasm_bindgen]` shell

**Acceptance criteria**: A `Handle` exposes `new(elf: &[u8], ram_bytes: usize)`,
`step_budget(instret: u64) -> Status`, `drain_uart() -> String`, and
`drain_frames() -> JsValue`, each a direct call into a step-2/3/4 function with no
logic of its own. The DTB rides along via `include_bytes!` as `main.rs:23` already
does. `cargo build -p snemu-wasm --target wasm32-unknown-unknown` passes.
**RED**: The shell is by construction too thin to unit-test; its behaviour is step
2–4's, already covered. Assert the thinness instead: no branching or arithmetic in the
`#[wasm_bindgen]` layer. If a test would be meaningful here, the shell is too fat —
push the logic down.
**GREEN**: The bindings.
**MUTATE**: N/A — no logic to mutate. Say so in the report rather than skipping
silently.
**REFACTOR**: Assess.
**Done when**: The wasm target builds; human approves commit.

### Step 6: The page — boot log and live spans, without freezing the tab

**Acceptance criteria**: A static page (`web/`, no bundler,
`wasm-pack build --target web`) fetches `kernel.elf`, constructs the machine, and runs
a rAF loop calling `step_budget(~2M)` per frame, appending drained UART text to a
`<pre>` and drained frames to a span/metric view. **The tab stays responsive** — a
spinning element or a clickable button proves it. Boot reaches heartbeat. Two loads
produce byte-identical UART output.
**RED**: Manual, and honest about it: this step is DOM glue, and a headless-browser
harness would cost more than this milestone is worth. The Rust behaviour beneath it is
already covered by steps 2–4. Verify by driving the page and observing.
**GREEN**: The page + a way to serve it with the release kernel alongside.
**MUTATE**: N/A (no new Rust logic).
**REFACTOR**: Assess.
**Done when**: All acceptance criteria at the top of this plan are met; human approves
commit.

## Open questions to settle before step 6

- **Where does `kernel.elf` come from for the page?** The release kernel is 1.8 MB and
  is a build artifact, not a repo file. Options: an `xtask` subcommand that stages it
  next to the page, or a documented manual copy. Prefer the former, but it's a real
  decision — `cargo xtask` currently has no "build for the web" verb.
- **Should the page drive `workload=` selection?** `dtb.rs` already patches bootargs in
  a firmware role, so a `<select>` that reboots into `workload=smp` is nearly free and
  is a genuinely good demo. Tempting scope creep; decide explicitly rather than
  drifting into it.
- **Is `snemu` missing from `run_clippy`'s `-p` list deliberate?** It is absent
  (`xtask/src/main.rs:1401`), as are `stitch` and `hitch`. If that's an oversight it's
  a separate fix — but `snemu-wasm` should land in whichever list is correct.

## Pre-PR quality gate

1. Mutation testing — run the `mutation-testing` skill on `snemu-wasm`; add it to
   `MUTANT_CRATES` (`xtask/src/main.rs:1466`), which **is** hardcoded, unlike the test
   list.
2. Refactoring assessment — run the `refactoring` skill.
3. `cargo xtask clippy` and `cargo xtask test` pass.
4. `cargo xtask links` passes — this plan and the design doc both link relatively, and
   a link check is the only thing that catches a broken one.

---
*On completion, `git mv` this file to `plans/legacy/` (per CLAUDE.md, this project
keeps the historical record rather than deleting plans) and re-run
`cargo xtask links` — a moved file breaks links in both directions.*
