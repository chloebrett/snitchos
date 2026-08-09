# Plan: `cargo xtask image --opt` — optimized board images

**Branch**: `main` (this project lands on main; no feature branches)
**Status**: 📐 PLAN — not started

## Goal

Make an optimized VisionFive 2 image reproducible through the tool, so the board
stops running a debug kernel with an **opt-0** userspace.

## Why now

Closes [../docs/debt-register.md](../docs/debt-register.md) **#19**. `image()` calls
`qemu::build_kernel`, which is hardcoded to `OptLevel::Low`; `Low` passes no
`--release`, so `kernel/build.rs:152` sees `PROFILE=debug` and builds the embedded
userspace with no `--release` either. Every syscall, trap, and — for
`workload=stitch-drivel` — every f32 multiply of the transformer forward pass runs
unoptimised with `debug_assertions` and overflow checks on.

This is the first item in
[../notes/drivel-on-vf2-speedup-ideas.md](../notes/drivel-on-vf2-speedup-ideas.md)
that is not a guess about where time goes: it does not need the profile to justify
it, and it makes every subsequent measurement mean something.

An opt-3 image was produced by hand on 2026-07-29 (7.78 MB → 6.13 MB). **Nothing
records that it booted** — treat optimized-on-hardware as unproven.

## Acceptance criteria

- [ ] `cargo xtask image --opt max` writes a `snitchos.img` built from the release
      kernel with an opt-3 userspace.
- [ ] `cargo xtask image` at any level objcopies the ELF from **the profile it just
      built** — a release build can never emit a stale debug binary.
- [ ] `OptLevel::Mid` states its userspace opt level rather than inheriting it, so
      the ladder is monotonic Low(0) → Mid(1) → Hi(2) → Max(3) by declaration.
- [ ] The board boots the optimized image to heartbeat, verified on hardware.
- [ ] A `stitch-drivel` Tab is timed on hardware at both levels, and both numbers are
      recorded — the debug baseline **before** the change.

## Steps

### Step 1: `OptLevel::Mid` forces opt-1 explicitly

**Acceptance criteria**: `OptLevel::Mid.userspace_opt_override()` returns `Some("1")`
instead of `None`. `cargo xtask itest` (which defaults to `Mid`) stays 130/130 —
behaviour-preserving, because `kernel/build.rs:189` already defaults to `"1"`.

**RED**: `xtask-qemu/src/lib.rs:229` — `userspace_opt_override_climbs_the_ladder`
currently asserts `Mid => None`, and its comment says *"If `Mid` ever returns
`Some("1")` instead of `None`, that is the unpinning work from
docs/debt-register.md #16 — and this assertion is where it starts."* Flip it. The
tripwire is already armed and pointed at this work.
**GREEN**: `OptLevel::Mid => Some("1")` in `userspace_opt_override`.
**MUTATE**: N/A — `xtask-qemu` is in `NOT_MUTATED` ("tooling: real suite; enrolment
candidate"). Enrolling it is worth doing and is not this change.
**REFACTOR**: None expected.
**Done when**: unit test green, `cargo xtask itest` unchanged at 130/130.

**Why first**: `Mid` is currently *defined by inheriting* `build.rs`'s pin. Until it
declares its own level, "what userspace opt does a board image get" has no answer
local to the tool, and adding `--opt` on top of an implicit default is how the four-rung
ladder silently collapses to three.

### Step 2: the image reads the profile it built

**Acceptance criteria**: one function maps an `OptLevel` to the kernel ELF path, and
`image()` uses it. `kernel_bin_for(OptLevel::Low)` is under `/debug/`; `Mid`, `Hi` and
`Max` are under `/release/`.

**RED**: a test in `xtask-qemu`'s existing `mod tests` pinning `kernel_bin_for` across
all four levels, beside `kernel_bin_selects_profile_directory`.
**GREEN**: `pub fn kernel_bin_for(opt: OptLevel) -> &'static str { kernel_bin(opt.is_release()) }`;
`xtask/src/main.rs:846` uses it.
**MUTATE**: N/A (see step 1).
**REFACTOR**: None expected.
**Done when**: unit test green; `cargo xtask image` still produces a working debug
image byte-for-byte as before.

> **Scope grew during execution, recorded for approval.** Two findings while doing it:
>
> 1. **`image()` was not the only site with the shape.** `xtask-itest/src/main.rs:1336`
>    binds `let opt = if release { Mid } else { Low }`, builds with
>    `build_kernel_profiled(…, opt)`, then reads with `kernel_bin(release)` — two
>    independent expressions of one fact, agreeing today only because `release → Mid`.
>    Latent rather than live, but the same class.
> 2. **The boolean is the trap, so it is now unreachable.** `kernel_bin(bool)` is
>    private; `kernel_bin_for(OptLevel)` is the public API. Without this the fix is a
>    one-off that anyone can rewrite; with it the bug class cannot be expressed from
>    outside the module.
>
> So the diff converts **four** call sites (`image`, `snemu boot`, `base_command_ex`,
> `snemu_diff`) rather than one. Each is a single line, and the two that already
> derived from `opt` were correct — they just now read as one idiom instead of two.
> The test is exhaustive over `OptLevel::value_variants()` rather than four hand-written
> assertions, so a rung added later is covered by existing.

**Why this is its own step, and lands before `--opt`**: the defect is a *duplicated
decision*. `image()` chooses a profile twice — once as `build_kernel` (Low) and once
as `kernel_bin(false)` — with nothing tying the two together. Add `--opt` without
fixing it and `objcopy` silently reads whatever debug ELF was last built, so the
optimized image is *exactly as slow* and the cause is invisible. That is the
[stale board image] failure mode with a new way in. Same shape as `satp_for` being
open-coded twice (debt #13) and `workload_features` being duplicated per call site.

### Step 3: `cargo xtask image --opt <level>`

**Acceptance criteria**: `cargo xtask image --opt max` parses to `OptLevel::Max` and
builds release; `--opt low` builds debug; a bare `cargo xtask image` uses the agreed
default. An unknown level is a parse error, not a silent fallback.

**RED**: CLI parse tests in `xtask/src/main.rs`'s `cli_tests`, matching the existing
`Cli::try_parse_from(["xtask", "itest", "--engine", "qemu", "sched"])` shape.
**GREEN**: `#[arg(long, value_enum, default_value_t = ...)] opt: OptLevel` on
`Cmd::Image`, threaded to `build_kernel_profiled` and `kernel_bin_for`.
**MUTATE**: N/A (see step 1).
**REFACTOR**: None expected.
**Done when**: parse tests green, both levels produce an image, `cargo xtask test` green.

> **DECIDED 2026-08-06: the default is `Max`** — opt-3 kernel *and* opt-3 userspace.
> `--opt low` stays available for bring-up.
>
> Why, given `Mid` is the better-tested level (the default itest gate runs it every
> day, where opt-3 userspace is only exercised on demand): **drivel's forward pass
> runs in userspace**, so the userspace opt level is not a detail of this plan, it is
> the plan. `Mid` would ship a release kernel wrapped around an opt-1 transformer and
> leave most of the win on the table.
>
> The cost is accepted knowingly: this changes what every existing `cargo xtask image`
> produces, and `vf2` + release is untested either way (see "Risk"). Step 4 is what
> discharges it, and it is why step 4 is a step.

### Step 4: the image knows what it was built at, and says so at boot

**Acceptance criteria**: a booted kernel reports its own build regime — kernel profile
*and* embedded-userspace opt level — on the UART (human) and on the wire (assertable).
An itest asserts the reported level matches the level the harness built at.

**RED**: an itest scenario asserting the frame appears and names the level the suite
built with, plus a host test for the string's construction. A negative control is
cheap here and worth having: build at one level, assert the scenario fails if it
claims another.
**GREEN**: `kernel/build.rs` emits `cargo:rustc-env=` for the two facts it already
knows — `PROFILE` and the `us_opt` it passes to the nested userspace build — reusing
the mechanism already at `build.rs:442`. The kernel `env!()`s them and reports at
boot, both channels per the project's standing rule: UART for the human, a frame for
anything to be asserted on.
**MUTATE**: the string construction is host-testable and worth mutating if it grows
any conditionals; a `format!` of two env vars is not.
**REFACTOR**: assess.
**Done when**: `cargo xtask itest` green, and a board boot prints the level.

> **Split into 4a / 4b during execution.** 4a is the fact and the human channel;
> 4b is the wire and the assertion. 4a stands alone (it is what step 5 reads off
> the board), so it commits separately.
>
> **4a — DONE.** `kernel_boot::build_info::userspace_opt_level` (4 tests, 3/3
> mutants caught), `kernel/build.rs` calling it to *decide and report* with one
> value, and a `REGIME_LINE` under the banner. Verified on all three arms:
>
> | build | reports |
> |---|---|
> | `cargo xtask snemu boot` | `kernel debug, userspace opt-0` |
> | `cargo xtask snemu boot --release` | `kernel release, userspace opt-1` |
> | `SNITCHOS_USERSPACE_OPT=3 cargo build --release` | `userspace opt-3` |
>
> The first row is the finding this step exists for: **that is what the board has
> been running**, and nothing said so.
>
> Two calls worth recording. `concat!` of two `env!` literals rather than
> `format!`, so the line is `&'static str` in rodata and boot allocates nothing.
> And ASCII only — the board's console is both the channel that most needs this
> line and the least able to render anything exotic (debt #18).
>
> **4b — the frame + the itest assertion.** Still to do.

**Two design points that are not incidental:**

1. **There is no single "opt level" and the report must not pretend there is.** The
   kernel profile and the userspace opt level are independent — that independence is
   the entire point of the Low/Mid/Hi/Max ladder, and collapsing them into one number
   would reintroduce exactly the ambiguity step 1 removed. Report both.
2. **Derive it in `build.rs`, not in `xtask`.** `build.rs` knows what was *actually*
   passed to the nested userspace build; `xtask` knows what it *intended*. If the
   level ever fails to propagate, a `build.rs`-derived string tells the truth and an
   `xtask`-derived one repeats the lie. The whole value of this step is being a
   witness rather than an echo.

**Why this lands before the hardware step.** Step 5 otherwise has to *trust* that the
image it flashed is the one it built — which is the assumption
[[feedback_stale_board_image]] exists because it failed, and which debt #19 records
failing again ("any later `cargo xtask image` silently overwrites it"). With this
step, step 5 reads the level off the board and the verification is real rather than
inferred. It also makes the whole opt-level story observable, which is the property
this OS is nominally about.

### Step 5: prove it on hardware, and record both numbers

Not a gate step — the gate cannot boot a board. Explicitly manual.

**Acceptance criteria**:
1. **Baseline first**, on the current debug image: `cargo xtask image --workload
   stitch-drivel`, flash, and time a Tab at the `stitch-drivel` prompt. Record it.
   Capturing this *after* the change is not possible.
2. `cargo xtask image --opt max --workload stitch-drivel` boots to heartbeat on the
   board **and reports `release` / userspace opt-3 at boot** (step 4) — so "the right
   image is on the board" is read off the board, not assumed.
3. A Tab at the prompt returns the same completion as the debug image (the sampler is
   deterministic given the boot seed, so this is checkable by eye).
4. Both timings and both image sizes recorded in
   [../notes/drivel-on-vf2-speedup-ideas.md](../notes/drivel-on-vf2-speedup-ideas.md),
   replacing the §1a estimate with a measurement.

**Done when**: the above is recorded, and debt #19 is deleted from the register.

**What to expect if it goes wrong.** Release codegen on this kernel is the regime that
produced the `tp` truncation (a `&static` hoisted across the higher-half trampoline)
and the SBI `a1` clobber — both fixed, both *hidden* precisely because board images
are debug. The `ph!` progress markers are still in the tree for exactly this. Note
also `notes/uboot.md`: `fdt_high` is required above ~3 MB, and an opt-3 image is
smaller, so a `booti` that previously needed it still will.

## Risk: what the gate does **not** cover

`cargo xtask itest --opt max` is 130/130 and `--engine qemu --opt max` passes too —
but every one of those builds a **QEMU-target** kernel. The artifact this plan ships
is `vf2`-gated: different RAM base (`0x4000_0000`), `snps,dw-apb-uart` with
`reg-shift=2`, SBI `TIME` instead of `sstc`, different console default. That
combination — `vf2` **and** release — has never been built by the gate or booted on
hardware.

The uncovered surface is small and enumerable: four `#[cfg(feature = "vf2")]` sites in
`kernel/src/main.rs`, `obs/heartbeat.rs` and `device/console.rs`, plus `RAM_BASE`.
Step 4 is the only thing that can close it, which is why it is a step rather than a
footnote.

## Gate

`cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`, plus
`cargo xtask clippy` and `cargo xtask links`. Steps 1–3 are ordinary host changes and
join it normally. Step 4 is hardware and joins nothing.

## Deliberately not in scope

- **Removing the `build.rs` opt-1 pin.** Step 1 is precondition #1 of debt #16;
  preconditions #2 (what exercises the level that stops being default) and #3 (guard
  the latent talc-OOM symptom) are untouched, and the `--config` line stays.
- **Any model or oracle optimisation.** Everything else in the speedup note is
  independent and measured against whatever baseline step 4 records.
- **Enrolling `xtask-qemu` in mutation testing.** Worth doing; not here.

---
*On completion, `git mv` this file to `plans/legacy/` (project override of the
planning skill's delete step) and run `cargo xtask links`.*
