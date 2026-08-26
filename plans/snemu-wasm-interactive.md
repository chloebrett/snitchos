# Plan: an interactive guest in the browser (milestone 2)

**Branch**: main (this project works directly on main; the human commits)
**Status**: Active

Follows [plans/legacy/snemu-wasm.md](legacy/snemu-wasm.md), which put a booting kernel
and live telemetry in a tab. This makes it a machine you can *use*: pick what it
boots, and type at it.

## Goal

A browser tab running the Stitch REPL with Tab completion answered by the trained
model — the thing `cargo xtask snemu boot --interactive --workload stitch-drivel`
already does locally.

## Decisions taken before writing this

- **The page picks the workload**, not the build. snemu already plays a firmware role
  (`dtb::set_bootargs`), so a `<select>` that reboots into `workload=…` is nearly
  free, and it demonstrates the runtime-workload design rather than hiding it.
- **The browser kernel carries the drivel weights.** Measured: 2.09 MB → **6.57 MB**
  with `--features kvetch-drivel`. A one-off cacheable fetch, and the alternative is
  a demo that can only babble.

## What the investigation already settled

- **`itest-workloads` is not needed.** There are zero `cfg(feature =
  "itest-workloads")` gates in `kernel/src/trap/user.rs`, so the Stitch workloads are
  selectable from a lean build. Only the storm bodies need that umbrella, and only
  `kvetch-drivel` gates the weights.
- **`stitch-kvetch` is *babble*, not the trained model** — it spawns
  `KVETCH_SERVER` (rung 0, no weights). The workload that answers from a checkpoint
  is `stitch-drivel`. Easy to confuse; the names do not say which is which.
- **`Cursor::drain` is already total across a machine swap.** Milestone 1's step 2
  made it re-sync on a shrinking buffer specifically so rebooting into a new workload
  could not panic the animation-frame loop. That bill is paid.

## The risk worth settling first

**Wall-clock pacing and interactive compute pull in opposite directions, and this
milestone is where that stops being theoretical.**

snemu's clock *is* its instruction counter: `time += 1` per retired instruction, and
the DTB declares a 10 MHz timebase, so one guest second costs 10M instructions.
Milestone 1's pacer buys exactly that per real second — which makes the guest's
*timers* faithful, and its *CPU* about 10 MIPS. A real U74 runs ~1.5 GHz. So a paced
guest computes roughly **two orders of magnitude slower than the hardware it models**,
and a Tab completion is compute-bound: the measured split puts 72–97% of it in the transformer.

A completion that takes a fraction of a second on the VF2 could therefore take tens of
seconds in a paced tab. Unpaced it would be ~4x faster than paced (38.9 vs 10 MIPS)
and cost a full core.

**Step 0 measures this before anything is built on top of it**, because if a paced
completion is unusable the shape of the whole milestone changes.

Two mitigations, cheapest first:

1. **A speed control** — paced (faithful timers, ~43% of a core) vs unpaced (fast,
   100%). Honest, trivial, and lets the user choose.
2. **An idle-aware pacer** — run flat out while the guest has work, pace only when
   every hart is parked. This is what we actually want: milestone 1's CPU burn was an
   *idle* guest heartbeating, not a working one. `Hart::is_idle()` exists but is
   `pub(crate)`, so it needs a small accessor on `Machine`. Better, and worth doing
   if step 0 says pacing hurts.

## Acceptance criteria

- [ ] A `<select>` on the page offers the runtime workloads and reboots the guest
      into the chosen one, with no rebuild.
- [ ] Typing in the terminal reaches the guest: the Stitch REPL echoes and evaluates.
- [ ] Tab at the REPL prompt returns a completion from the trained model.
- [ ] Switching workload mid-run cannot wedge or panic the page.
- [ ] The interactive path is usable — a keystroke is visible in well under a second.
- [ ] `cargo xtask web` stages the drivel-capable kernel, and the page still reports
      its fingerprint.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code without
a failing test. Per this project's CLAUDE.md, tests go at the lowest level that gives
confidence — Rust unit tests for logic, Vitest for the app's own decisions, Playwright
only for what genuinely needs a browser.

### Step 0: Measure what a paced Tab completion costs

**Acceptance criteria**: A number, recorded: wall-clock seconds from Tab to completion
under `workload=stitch-drivel`, paced and unpaced. Enough to say whether mitigation 1
suffices or mitigation 2 is required. **No production code in this step.**
**RED**: N/A — a measurement, and saying so beats inventing a test for it.
**GREEN**: N/A.
**MUTATE**: N/A.
**Done when**: The number is in this plan and the mitigation is chosen with the human.

**Outcome (2026-08-26): measured, and it changes the shape of the milestone.**

Guest instret, from the existing itest scenarios via `--record-instret` (deterministic
under snemu, so these reproduce exactly):

| scenario | instret |
|---|---|
| `stitch-reads-a-line` (REPL boot + read a line) | 24.5M |
| `stitch-kvetch-completes` (+ **babble** Tab completion) | 27.3M |
| `stitch-drivel-completes` (+ **trained-model** Tab completion) | **441.2M** |

Subtracting: REPL boot ≈ **24.5M**, a babble completion ≈ **2.8M**, and a drivel
completion ≈ **416.7M** — 150x the babble one, and 17x the entire boot.

Converted to wall clock at the two rates the browser can run:

| | paced (10 MIPS) | unpaced (38.9 MIPS) |
|---|---|---|
| REPL boot | 2.4s | 0.6s |
| Babble Tab | **0.28s** | 0.07s |
| Drivel Tab | **42s** | **11s** |

Three conclusions:

1. **Pacing is fine for everything except the drivel completion.** A babble Tab at
   0.28s paced is genuinely interactive; boot at 2.4s is honest. Nothing here argues
   against milestone 1's pacer in general.
2. **A paced drivel completion is unusable**, and a speed toggle alone does not save
   it — even flat out it is ~11s. Mitigation 1 is therefore *insufficient on its own*.
3. **Mitigation 2 is the right one**: run flat out while the guest has work, pace only
   when every hart is parked. It gets the drivel completion to its 11s floor, keeps
   babble snappy, and keeps an idle tab cheap. Milestone 1's CPU burn was an idle
   guest heartbeating; this targets exactly that and nothing else.

*Caveat on the 11s figure*: 38.9 MIPS was measured on the idle `init` heartbeat loop.
A transformer forward pass is a different instruction mix (FP- and load-store-heavy),
so the real number could move either way. It is an estimate until measured in-tab.

*Investigated and rejected — `native-ops`.* The itest harness runs
`speed[idle-skip,native-ops,tlb,jit-A,reg-cache]`, and the browser build does not
enable `native-ops`. It should stay that way: unlike the other accelerators it is
**not** instret-transparent. It collapses a guest `memset`/`memcpy` and charges the
clock an *estimate* of what the interpreter would have retired — `memop_charge`'s own
doc records `real/charged = 1.011` over an `init` boot, and on-off agreement "within
~1%" on 94 of 110 scenarios. That is a fine trade for a 134-scenario throughput budget
and the wrong one for a page whose whole claim is that it is the real machine,
deterministic and faithful.

Worth noting the limit of the guard here: `probe::the_speedups_change_nothing_but_speed`
would **not** have caught this. Its program is 13 instructions with no memops, so it
cannot discriminate an accelerator that only affects them. The A/B tests cover the
three accelerators actually enabled; they are not a general licence.

**Follow-up (2026-08-26): mitigation 2 does not apply. Measured.**

Idle-aware pacing rests on the guest having an idle state to detect. It does not.

The first attempt sampled `Machine::all_harts_idle()` at each slice boundary and read
100% of a core — because with idle-skip the machine jumps *through* an idle wait and
resumes inside the same slice, so the parked state barely exists when sampled. The
fix for that was a cumulative counter instead of an instantaneous check
(`Machine::fast_forwards()`, which counts rounds where no hart retired and the clock
was jumped to the next deadline).

That counter then answered the real question: **a booted `init` guest logs zero
fast-forwards over 60M instructions.** It never idles at all. The cause is in
SnitchOS, not the emulator — the kernel's idle task is
`loop { wfi; yield_now(); }`, so it retires instructions between waits rather than
parking. The 43% measured under plain pacing was never an idle guest being throttled;
it was a genuinely busy one.

So mitigation 1 — an explicit speed control — is not the cheap fallback, it is the
only one that applies. Shipped as `Speed = "paced" | "turbo"`, defaulting to paced
(10.0 MIPS, 40.9% of a core, truthful timers) with turbo for compute-bound work.
`Pacer.budgetFor` is the same tested mechanism the idle experiment produced, driven by
the user's choice rather than a guest state that does not exist — including the credit
clamp, which turns out to be *required* here: switching turbo→paced after a long run
would otherwise leave tens of seconds of credit and freeze the guest.

`fast_forwards()` is kept in snemu: it is small, tested, and is the honest way to ask
this question of any future guest that does park.

Worth carrying forward as a general lesson: **"the guest is idle" was an assumption,
and the tab-burns-a-core symptom was compatible with it, which is why it survived
this long.** It took a counter aimed at exactly that claim to kill it.

**Proposed shape, for approval:**

- Default the page to **`stitch-kvetch`** (babble). It is the workload that is
  actually interactive, and it proves the whole path — REPL, IPC, completion server,
  Tab — end to end.
- Offer **`stitch-drivel`** as the showcase, honestly labelled as taking ~10s. The
  wait is *observable*: the telemetry pane shows `kvetch.complete` open and the
  server's `bytes_emitted_total` climb while it works. A demo where you can watch the
  model think is on-thesis for this project rather than an apology.
- Implement **idle-aware pacing** (mitigation 2) as its own step before the selector.

### Step 1: `cargo xtask web` builds and stages a drivel-capable kernel

**Acceptance criteria**: The staged `kernel.elf` is the `kvetch-drivel` build (~6.5 MB),
the manifest's fingerprint changes accordingly, and the existing acceptance suite
still passes against it — the default boot is unchanged, since the workload is a
runtime selection.
**RED**: A test on the feature list, mirroring `image_features` in `xtask/src/main.rs`
— the web kernel carries `kvetch-drivel`, and does *not* carry `itest-workloads`.
**GREEN**: Pass the feature through `build_kernel_profiled`.
**MUTATE**: `cargo mutants -p xtask-cmds --file "**/web.rs"`.
**Done when**: Criteria met, report reviewed, human approves commit.

**Outcome (2026-08-26): done.** `web_features()` returns `["kvetch-drivel"]`; the
staged kernel went 2.09 MB → **6.37 MB** and its fingerprint moved
`f1811c59…` → `3f63d515…`, which is the staleness detector demonstrating itself. Two
tests pin the policy, and the second is the one that earns its keep: the kernel must
**not** carry `itest-workloads`. "Add the feature that makes it work" is the reflex,
and here it would cost megabytes of programs no visitor can select. Acceptance suite
unchanged (4/4, 5.2s).

### Step 2: `Handle` boots a chosen workload

**Acceptance criteria**: `Handle::new(elf, ram, workload)` patches
`/chosen/bootargs` so the guest selects that workload; `None` boots the default
`init` exactly as today. A bad workload name is the *guest's* business, not the
shim's — the page must not maintain its own registry of valid names.
**RED**: Host tests for the pure part: the bootargs string for a given selection, and
that a patched DTB still parses (`fdt::Fdt::new`) and carries the expected
`/chosen/bootargs`. Then a wasm test that a `Handle` built with a workload boots.
**GREEN**: `dtb::set_bootargs` in `Handle::new`.
**MUTATE**: `cargo mutants -p snemu-wasm`.
**Done when**: Criteria met, report reviewed, human approves commit.

**Outcome (2026-08-26): done.** New `boot.rs` — `selection`, `bootargs_for`,
`dtb_for` — and `Handle::new(elf, ram, workload)` patches `/chosen/bootargs` as QEMU's
`-append` would. Patched trees are parsed back through `fdt`, the *guest's* reader,
rather than trusting our writer. No workload registry in the browser: that mapping
belongs to `kernel_boot::bootargs`, and a second copy is the shape `workload_features`
exists to prevent.

Two things worth carrying:

- **The thinness guard passed something it should not have.** `(!workload.is_empty())
  .then_some(workload)` sat in the shell — a conditional wearing a method call, which
  a keyword scan cannot see. It is a *decision* about what an empty string means, so
  it moved to `boot::selection` with tests. The guard is a ratchet, not a proof.
- **Mutation testing found the A/B pair could not discriminate.** `Speedups::apply →
  ()` survived, because every test around it asserts ON and OFF *agree* — which a
  no-op satisfies perfectly. The consequence is not cosmetic: the browser would drop
  from 38.9 MIPS back to the 11 MIPS interpreter with the suite green. Killed with the
  one deterministic difference that does exist — with the block JIT on a `step()`
  retires a whole compiled block, without it exactly one instruction. **41 mutants, 36
  caught, 5 unviable, 0 survivors.**

### Step 3: `Handle` accepts keystrokes

**Acceptance criteria**: `Handle::push_input(bytes)` reaches the guest's console.
The thinness guard still passes — this is one call into `push_console_input`.
**RED**: A wasm test booting `workload=stitch-repl`, pushing a line, and asserting the
REPL's response appears in the drained UART. That is the lowest level with real
confidence: there is no host-side seam between "bytes pushed" and "guest read them".
**GREEN**: The binding.
**MUTATE**: N/A if the shell stays a single call; say so rather than skipping quietly.
**Done when**: Criteria met, human approves commit.

**Outcome (2026-08-26): done.** `Handle::push_input(text)` — one call into
`push_console_input`, so no mutants to speak of, and the thinness guard still passes.

**The plan's RED was not buildable as written.** It called for booting
`workload=stitch-repl` and asserting the REPL's reply, but the REPL lives in the
staged 6.37 MB `kernel.elf` — a gitignored build artifact, so the test would fail on a
clean clone. Instead the guest is a hand-assembled ns16550a echo loop: poll LSR for
the data-ready bit, read RBR, write THR. Same seam, nothing external.

Two tests: what is typed comes back, and three pushes with no stepping between them
all survive in order (a fast typist inside one animation frame).

The hand-assembly is where the time went, and it is worth recording why: the loop
first branched to offset 4 rather than 8, re-running `slli x6, x6, 28` and shifting
the UART base out of the register. Exactly one character echoed and then it polled
address 5 forever — which presents as "input is dropped", not "the jump is wrong".

### Step 4: The page sends keystrokes and offers a workload selector

**Acceptance criteria**: xterm's `onData` reaches `push_input`; a `<select>` reboots
into the chosen workload, resetting the terminal, the telemetry tail and the pacer,
and leaving the page responsive.
**RED**: Vitest over the pure parts — the reboot decision (what must be reset, and
that selecting the current workload is a no-op) and the input encoding. The DOM
wiring itself is glue and belongs to step 5.
**GREEN**: The component work.
**MUTATE**: `yarn test` plus a review of the pure modules.
**Done when**: Criteria met, human approves commit.

### Step 5: The demo, end to end

**Acceptance criteria**: A Playwright spec picks `stitch-drivel`, waits for the
prompt, types an expression, sees it evaluated, presses Tab and sees a completion.
This is the milestone's headline claim and the only place it is checkable.
**RED**: The spec, failing until the pieces above are wired.
**GREEN**: Whatever it turns up.
**Done when**: All acceptance criteria at the top are met; human approves commit.

## Open questions

- **Does the 6.5 MB fetch want a progress indicator?** A blank page for several
  seconds on a cold cache is a bad first impression, and the page currently says
  nothing until the kernel has loaded.
- **Should the selector list come from the guest?** Hardcoding workload names in the
  page duplicates `kernel_boot::bootargs`'s registry, which is exactly the
  duplicated-mapping shape `workload_features` was extracted to kill. Probably fine
  for a handful of demo entries, but decide it rather than drift into it.

## Pre-PR quality gate

1. Mutation testing — `mutation-testing` skill on the crates touched.
2. Refactoring assessment — `refactoring` skill.
3. `cargo xtask clippy`, `cargo xtask test`, `cargo xtask links`.
4. `yarn check`, `yarn test`, `yarn e2e`, and `yarn measure` for the standing numbers.

---
*On completion, `git mv` this file to `plans/legacy/` (per CLAUDE.md this project keeps
the historical record rather than deleting plans) and re-run `cargo xtask links` — a
moved file breaks links in both directions.*
