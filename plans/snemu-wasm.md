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
  milestone 3 — and that is the milestone worth wanting, since
  `snemu boot --interactive --workload stitch-kvetch` already gives a Stitch REPL with
  a trained model answering Tab. Two things land there and are recorded now so they are
  not surprises: xterm.js's `onKey` needs an explicit map from special keys (Enter,
  Backspace, Esc, arrows) to the byte sequences the guest expects, printable characters
  going through unchanged (`~/c/slay/www/main.js` does exactly this); and the guest's
  emoji-width assumption may disagree with xterm.js's, which we cannot fix from the
  host side because the guest already laid the frame out.
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

## Prior art: `~/c/slay` shipped this exact shape

A sibling project — a Slay-the-Spire clone — already runs a Rust TUI in a browser tab
at [slay.chloe.casa](https://slay.chloe.casa): `slay-core` compiles to wasm32, a
`slay-wasm` crate wraps it in `#[wasm_bindgen]`, and **xterm.js** renders the output.
Read `~/c/slay/plans/wasm.md` and `~/c/slay/www/main.js` before step 6. What it
teaches, in descending order of how much it changes this plan:

1. **Use xterm.js, not a `<pre>`.** This is the biggest correction. snemu's
   `uart_output()` is a *terminal* byte stream — the guest emits ANSI colour, cursor
   motion, and (via the Stitch renderer) emoji. A `<pre>` renders escape bytes as
   garbage. Vendored xterm.js is one 283 KB `<script>` tag, no bundler, no CDN, and
   consumes those bytes directly. This *shrinks* step 6: no ANSI handling of our own.

2. **"Compiles for wasm32" is necessary, not sufficient.** slay's TUI built clean and
   then panicked in the tab — `std::time::Instant::now()` is unavailable on
   `wasm32-unknown-unknown` (`git -C ~/c/slay show 2263ea8`). This plan's premise is a
   successful *build*; that is weaker evidence than it reads as. See step 1b.

3. **A thin bindgen shell is a discipline, not a consequence.** slay's own plan said
   "push the logic down"; its shipped `crates/slay-wasm/src/lib.rs::send` nonetheless
   grew branching, pile-rendering, and save-prompt state — logic that is now only
   reachable through a `#[wasm_bindgen]` method. Step 5's "too thin to test" bar is the
   thing that erodes first. Hold it.

4. **Feature-gate the browser-only crate behind a trait seam.** `slay-wasm/src/persist.rs`
   is the pattern worth copying wholesale: a `Storage` trait, a `MemoryStorage` test
   double, a `LocalStorage` impl behind `feature = "browser"` — 12 lines of `web_sys`,
   everything else host-tested. Any browser state this page grows (chosen workload,
   scrollback) goes through that shape.

5. **Emoji width drifts in xterm.js** — it renders `⚔️` as 2 columns where
   `unicode-width` says 1. slay fixed it by emitting an absolute cursor position per
   cell (`wasm_backend.rs:69`). **We cannot use that fix**: the *guest* computed the
   layout, and we only relay its bytes. The Stitch renderer already carries a known
   "emoji width is terminal-dependent" caveat, so this is a real milestone-3 risk and
   is called out there rather than discovered live.

6. **Small page mechanics that are free to copy:** `visibility: hidden` on the terminal
   div until after the first fit (otherwise the tab flashes an unsized terminal —
   `git -C ~/c/slay show e3bca04`); a `measureCell()` probe span to derive cols/rows
   from the real font metrics; and an explicit mobile bail-out ("works best on a
   desktop browser") instead of a broken experience.

## Acceptance criteria

- [ ] `cargo test -p snemu-wasm` runs on the host and covers the drain cursor and
      status encoding.
- [ ] `cargo xtask test` runs `snemu-wasm`'s suite without any list edit.
- [ ] Opening the page boots the real kernel and shows the UART boot log **in an
      xterm.js terminal**, ending with a `kernel.heartbeat`-era log line, with the
      guest's own colour/formatting intact rather than escaped.
- [ ] The page shows decoded telemetry: at minimum `kernel.boot`, and span/metric
      names resolved through their `StringRegister`s.
- [ ] The tab stays responsive throughout (a button or animation keeps working while
      the guest boots).
- [ ] Two loads of the same page produce byte-identical UART output — determinism
      survives the browser.
- [x] **The wasm build agrees with the native one.** For a fixed program and a fixed
      instret budget, the *architectural* result — every register, the UART bytes, the
      retired instret — is identical between a native run and a wasm32 run. This is the
      criterion that catches 32-bit `usize` truncation; self-consistency across two page
      loads does not. **Corrected after measuring**: this originally said
      "`state_hash()` and the UART bytes", which is not achievable — `state_hash()` is
      pointer-width-dependent by construction (step 1b). The hash is pinned host-only.
- [ ] The page displays the fingerprint of the `kernel.elf` it loaded, so a stale
      artifact is visible rather than mysterious.

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

**What it actually cost (done 2026-08-09).** The metadata-derived pickup works exactly
as advertised — no test-list edit — but "joining the workspace is free" was too strong.
Three derived artifacts do not update themselves, and each is gate-enforced:

- `docs/generated/deps.md` drifts the moment a member is added (it is generated from
  `cargo metadata --no-deps`). `cargo xtask diagram deps` regenerates it.
- `deps_layer` (`xtask-itest/src/diagram_cmd.rs`) is an editorial crate→layer map whose
  own doc says "new crates land here (else they render ungrouped)". `snemu-wasm` is
  `tooling`, beside `snemu`.
- `MUTANT_CRATES`' characterisation test
  (`the_derived_plan_matches_the_previously_hardcoded_set`) is a deliberate tripwire and
  fires on any new mutated crate. Enrolled now rather than at the pre-PR gate, because a
  crate whose entire premise is "the logic is host-tested" is the last one that should
  go unmutated.

The tripwire firing is also what *proved* the pickup: it failed with the derived list
containing `snemu-wasm` and the hardcoded list not. Better evidence than a green run.

### Step 1b: Prove the core *runs* under wasm32, not just builds

**Acceptance criteria**: snemu's existing test suite (or a boot-to-heartbeat subset of
it) executes under a wasm32 runtime — `wasm-pack test --node`, or `wasmtime` against a
`wasm32-wasip1` build, whichever is less ceremony — and passes. The point is to convert
this plan's premise from "it compiles" into "it executes", which is the exact gap that
cost `~/c/slay` a runtime panic.

Two specific hazards to look for, both invisible to the 64-bit host suite:

- **32-bit `usize`.** There are ~72 `as usize` casts across `snemu/src/`. Guest
  addresses are `u64` and higher-half kernel VAs (`0xffffffff80200000`) do not fit in
  32 bits. Spot-checking says the careful pattern is already in use — `fetch_cache.rs:91`
  masks *before* casting and keeps `tag: u64` (`fetch_cache.rs:56`), so aliasing PCs
  4 GiB apart still compare distinct — but the class is unaudited. A truncation here
  does not crash; it silently executes the wrong instruction.
- **128 MiB of guest RAM as a `Vec<u8>`** on a 32-bit target, plus whatever the browser
  will let a wasm module grow its linear memory to. `Memory::high_water` already tracks
  the guest's true footprint, so the page can likely boot in far less than `main.rs`'s
  `RAM_SIZE`; measure it rather than assuming 128 MiB is fine.

**RED**: The differential check itself — assert a wasm-side boot's `state_hash()` and
UART bytes equal the native side's for the same budget. Expect it to pass; run it
because a silent disagreement is the failure mode, and a passing differential is cheap
insurance against a class we otherwise cannot see.
**GREEN**: Whatever the check turns up. Possibly nothing — that is a result, not a
wasted step.
**MUTATE**: N/A (no new production logic).
**REFACTOR**: N/A.
**Done when**: The wasm32 suite runs green and the native/wasm differential agrees;
human approves commit.

**Outcome (2026-08-10): done, and it found something.**

*The premise holds.* snemu's core does not merely build for wasm32 — it **runs**
correctly on `wasm32-unknown-unknown`, the real browser target, under Node via
`wasm-pack test --node`. No `Instant::now`-style trap of the kind that bit `~/c/slay`.

*The emulator is width-clean on the paths exercised.* A hand-assembled RV64 program
(`snemu-wasm/src/probe.rs`) building a value wider than 2^32, a guest address above
2^31, an 8-byte store/load round-trip and a UART MMIO byte write produces **identical**
registers, UART bytes and instret on both targets. The `as usize` hazard this step was
written to hunt did not materialise here — consistent with the spot-checks
(`fetch_cache.rs` masks before casting; `mem.rs::span` uses a *checked*
`usize::try_from`, so an out-of-range address returns `OutOfRange` rather than
truncating).

*What did diverge: `Machine::state_hash()`.* Byte-identical machine state hashes
differently on the two targets. Isolated rather than inferred — `probe::hash_diagnostics`
hashes three things and compares each across targets: a bare `u64` **agrees**, raw bytes
via `Hasher::write` **agree**, and only `<[u8]>::hash` **differs**, because slice hashing
length-prefixes with a `usize` (8 bytes on the host, 4 in the browser). So it is not
`SipHash`, not the toolchain, and not snemu's arithmetic — it is one width-dependent
length prefix. snemu's own doc already says the value is "not a cross-toolchain-stable
digest"; this measures exactly where that boundary falls.

*Consequences.* The differential asserts the architectural result (portable) and pins
the hash host-only. Both facts are executable: a wasm test asserts the hash *differs*,
so if snemu's digest is ever made width-independent that test fails and tells us the
differential can be widened. Making it width-independent is a small change (hash lengths
as an explicit `u64`; `Hasher::write` for byte slices) and its consumers are all
same-process comparisons — snapshot-tree fork verification and `snemu_audit`, no
committed baselines — but nothing needs it today, so it stays a deliberate non-change.

*Known gap.* The wasm test is **not in `cargo xtask test`**. It needs Node on `PATH`
plus the `wasm-bindgen` CLI, and the local Node 16 is too old (the toolchain emits
`externref`; Node 24 works). Run it by hand for now:
`PATH="$HOME/.nvm/versions/node/v24.18.0/bin:$PATH" wasm-pack test --node snemu-wasm`.
Wiring it into the gate needs a decision about that toolchain dependency.
**Decided 2026-08-11: later.** It stays a manual check for now rather than putting a
Node version floor on `cargo xtask test`; revisit once the page exists and the wasm
side is carrying real logic.

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

**Outcome (2026-08-11): done. 7 tests, no survivors.**

`Cursor { consumed: usize }` with `drain<'a>(&mut self, buf: &'a [u8]) -> &'a [u8]`,
in `snemu-wasm/src/cursor.rs`. One design point the plan did not anticipate: `drain`
is **total**, because a *shrinking* buffer is reachable by design rather than
paranoia. The page is meant to grow a control that reboots into a different
`workload=`, and a new `Machine` starts with an empty output buffer; `&buf[consumed..]`
from a stale offset would panic, and a panic inside a `requestAnimationFrame` callback
is a dead tab. So it clamps and re-syncs, and a test names that scenario.

*Mutation report.* `cargo mutants -p snemu-wasm --file "**/cursor.rs"` found **3
mutants, 3 caught** — but all three were whole-body return replacements
(`Vec::leak(vec![0])` and friends). It generated nothing for the interesting boundary,
because it does not mutate arbitrary method calls like `.min()`. The automated score
was therefore not evidence about the thing the plan flagged, so the three that matter
were applied by hand and each confirmed killed:

| mutation | outcome |
|---|---|
| `.min()` → `.max()` | killed by 4 tests, incl. the shrink case |
| `self.consumed = buf.len()` → `= start` | killed by 4 tests |
| `consumed.saturating_sub(1)` (the off-by-one) | killed by 4 tests |

Worth carrying into steps 3–5: a green `cargo mutants` run on a small pure type here
is weak evidence on its own. Read the mutant *list*, and hand-apply what the tool's
operator set misses.

**REFACTOR**: assessed, declined — the body is three lines and the clamp is the point.

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

**Outcome (2026-08-25): done. `snemu-wasm/src/budget.rs`, 12 tests, no survivors.**

`Status::{Running, Halted, Trapped}` each carrying the instret **this slice** retired,
`Budget` (limit + anchor), and `run(&mut Machine, limit) -> Status`.

Three things the step's design notes did not anticipate, all confirmed in snemu's
source rather than assumed:

- **A step can retire *zero* instructions.** With every hart parked on `wfi` and no
  armed timer, `step_round` returns `Ok(())` having moved nothing — the idle
  fast-forward at `machine.rs:234` is guarded by `if let Some(deadline)`, and the
  comment there notes such a hart can only be woken by an IPI, which cannot arrive
  while every hart idles. So it is terminal, and a naive `while spent < budget`
  spins forever. That is the `Halted` variant, and the reason this module exists.
- **`instret()` is cumulative over the machine's life**, so a budget must anchor and
  compare a *delta*. Comparing the raw counter against a per-frame limit would report
  the budget exhausted before the guest moved, on every frame after the first. Same
  absolute-vs-delta trap `Cursor` exists to avoid on the output buffers.
- **`limit` is a floor, not a ceiling.** A step is atomic, so a slice overshoots by
  whatever the last step retired.

*Mutation report.* First run: **2 missed, 4 timeouts** — and reading the list mattered
more than the score, exactly as step 2 warned. The timeouts were mutants that stop the
budget ever exhausting, so `run` span and the harness hung; cargo-mutants scores a hang
as "caught", but a test that hangs instead of failing is a weak kill, and a hang *is*
the browser-freeze failure mode. Two fixes:

1. `run`'s loop is now `for _ in 0..limit` rather than `loop`. The cap is not
   defensive padding — it restates the loop's own invariant (every iteration retires
   ≥1 instret or returns `Halted`), so correct code never reaches it. It makes "cannot
   spin" a property of the *shape* instead of the arithmetic being right, which is
   worth more here than usual: inside a `requestAnimationFrame` callback an infinite
   loop is not an interruptible hang, it is a tab you have to kill.
2. A test that pins **instret, not step-calls** — the step's headline warning, which
   had no test. Measured through public API: with `set_block_jit(true)` a compiled
   block retires **5** instructions in one `step()`, so a step-counting implementation
   overruns ~5x and the test catches it.

Second run: **8 caught, 0 missed, 0 timeouts**, and the suite went 2m → 16s because
nothing hangs any more.

*It also found a hole in step 1b.* Crate-wide mutants flagged `replace check_portable
with ()` as surviving: the probe's oracle lives in `src/` (deliberately — the host and
wasm32 tests compile separately and must share one set of expectations), so hollowing
it to a no-op left every caller passing vacuously. Fixed with two negative controls
that assert the oracle *rejects* a wrong probe. This is the "a control that cannot
discriminate" lesson and "verify the instrument" in one: an oracle nothing tests is an oracle that cannot
discriminate. Crate-wide now **19 mutants, 17 caught, 2 unviable, 0 survivors**, with
one documented equivalent mutant (`with_capacity`'s argument — capacity is a
performance hint) registered in `.cargo/mutants.toml` per project convention.

**REFACTOR**: assessed, declined.

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

### Step 6: The page — boot log in a real terminal, live spans, no frozen tab

**Acceptance criteria**: A static page (`web/`, no bundler,
`wasm-pack build --target web`) fetches `kernel.elf`, constructs the machine, and runs
a rAF loop calling `step_budget(~2M)` per frame, writing drained UART bytes straight to
an **xterm.js** terminal via `term.write()` and drained frames to a span/metric view.
**The tab stays responsive** — a spinning element or a clickable button proves it. Boot
reaches heartbeat. Two loads produce byte-identical UART output.

Mirror `~/c/slay/www/` for the page mechanics rather than rediscovering them:

- **Vendor xterm.js** into `web/xterm/` (`xterm.js` + `xterm.css`, ~290 KB total) and
  load it with a plain `<script>` tag. No CDN — the page must work offline — and no
  bundle step.
- **Size the terminal from real font metrics.** An off-screen probe span measures one
  character cell; derive `cols`/`rows` from `window.innerWidth/Height` and re-fit on
  `resize`. (M1's guest never learns the size, so this is cosmetic here — but M3's
  interactive Stitch REPL will want it, and it costs ten lines now.)
- **Keep `#terminal` at `visibility: hidden` until after the first fit**, then reveal.
  Otherwise the page flashes a wrongly-sized terminal on every load.
- **Bail out on mobile** with a notice rather than shipping something unusable.
- **Serve over HTTP** — `python3 -m http.server` or equivalent. `file://` cannot load
  ES modules, so a double-clicked `index.html` will fail confusingly.
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
  is a build artifact, not a repo file. `~/c/slay` answers the equivalent question by
  **committing** its built `www/pkg/*.wasm` (1.1 MB) so GitHub Pages needs no CI — and
  pays for it in commits literally titled "Update wasm binary" and "Update wasm". This
  project already knows that failure mode by name: a VF2 "regression" is a missed
  `cargo xtask image` until proven otherwise. Here it would be **two** derived artifacts
  (the wasm module and the kernel ELF) with no compiler to notice they disagree.
  So: prefer a `cargo xtask web` verb that builds and stages both, and — whichever way
  the commit-or-generate call goes — **the page must show the kernel build's
  fingerprint**, which is why that is now an acceptance criterion rather than a nicety.
  **Decided 2026-08-11: `cargo xtask web` it is** — build and stage the kernel ELF and
  the wasm module next to the page, with the fingerprint displayed. Not committed
  artifacts.
- **Should the page drive `workload=` selection?** `dtb.rs` already patches bootargs in
  a firmware role, so a `<select>` that reboots into `workload=smp` is nearly free and
  is a genuinely good demo. Tempting scope creep; decide explicitly rather than
  drifting into it. (Step 2 already paid part of its cost: `Cursor::drain` is total so
  that swapping in a fresh `Machine` cannot panic the rAF loop.)
- ~~**Is `snemu` missing from `run_clippy`'s `-p` list deliberate?**~~ Settled: it was
  an oversight, and all three gate lists are metadata-derived now. `snemu-wasm` needs no
  clippy-list edit — only the `MUTANT_CRATES` entry noted in the quality gate below.

## Pre-PR quality gate

1. Mutation testing — run the `mutation-testing` skill on `snemu-wasm`. (The
   `MUTANT_CRATES` characterisation list was already updated in step 1; it is
   hardcoded, unlike the test list.)
2. Refactoring assessment — run the `refactoring` skill.
3. `cargo xtask clippy` and `cargo xtask test` pass.
4. `cargo xtask links` passes — this plan and the design doc both link relatively, and
   a link check is the only thing that catches a broken one.

---
*On completion, `git mv` this file to `plans/legacy/` (per CLAUDE.md, this project
keeps the historical record rather than deleting plans) and re-run
`cargo xtask links` — a moved file breaks links in both directions.*
