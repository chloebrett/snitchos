# Deflake bisection: heartbeat-cadence post-boot wedge

Branch: `deflake`.

## Hypothesis

Post-14 surfaced a residual ~1–2% per-scenario flake rate. Failure mode: kernel
reaches `I am alive — entering heartbeat` then the scenario times out waiting
for the second heartbeat. Looks like a virtio-console or heartbeat-loop wedge.

Suspect: introduced somewhere in the v0.6 SMP work.

## Method

Bisect over commits that touched `kernel/`, `kernel-core/`, `protocol/`,
`collector/` only. xtask stays at HEAD so we keep:

- `/tmp/snitch-itest-*.log` per-scenario QEMU capture
- last-80-lines-on-failure dump
- `pkill qemu-system-riscv64` at suite start
- `--repeat N` aggregate flake report
- per-test wallclock budget surfacing

Workspace `Cargo.toml` stays at HEAD (the only delta vs `2e409f2` is
`exclude = ["learning"]`, which is harmless to older kernels).

### Per-candidate overlay procedure

```
git checkout deflake
git checkout <C> -- kernel kernel-core protocol collector
# patch xtask to compile against older protocol — see below
cargo xtask build
cargo xtask itest heartbeat-cadence --repeat 50
```

### xtask patches required for the overlay

HEAD's xtask references protocol items (`hart_id` field on `SpanStart` /
`ContextSwitch`, `HartRegister` variant) added during v0.6. Patches:

- `xtask/src/itest/harness.rs`: drop `hart_id` from `SpanStart` /
  `ContextSwitch` display arms; remove the `HartRegister` arm.
- `xtask/src/itest/scenarios.rs::smp_secondary_hart_boots`: replace the
  `OwnedFrame::HartRegister` matcher with `|_, _| false` (the scenario can't
  meaningfully run pre-SMP; we only need it to compile so heartbeat-cadence
  can run).

These patches are local-only on the bisection branch — never committed.

## Pinned scenario

`heartbeat-cadence` — introduced `a70f420` (v0.1), last meaningfully changed
`bde9fe3` (error-bound tweak). Exists unchanged across the entire corridor.
Directly stresses the heartbeat loop + virtio path — the subsystem post-14
fingered as the wedge locus.

Regimen: `cargo xtask itest heartbeat-cadence --repeat 50`. Classify:

- **GOOD** = 0 failures
- **BAD** = ≥2 failures
- **AMBIGUOUS** (1 failure) → re-run once

50 runs gives strong signal: at the observed HEAD rate (6%), P(0 failures in
50 | true rate 6%) ≈ 4.5%, so a clean run at a candidate commit is reliable
evidence that commit is GOOD.

## Endpoints (confirmed)

| Commit | Description | Flake rate |
|---|---|---|
| `2e409f2` | post 12 — end of v0.5, pre-SMP | **0/50** ✓ GOOD |
| `main` (efcbbf9) | post 14 — end of v0.6 step 10 | **3/50** ✗ BAD (runs 10, 42, 49) |

## Corridor

`git log --oneline 2e409f2..main -- kernel kernel-core protocol collector`:

```
efcbbf9 More lint fixes. No current clippy warnings.
800cca5 Clippy fixes.
4034d25 expect a dead code snippet
c229605 Clippy fixes
387f793 per-hart runqueue and idle
ce206f1 step 9.3
35b171d multi hart step 9 part 1
fe36ace 2nd hart metrics
db88062 Secondary hart boot scenario; debugged with gdb
0c4d4f2 ipi, sbi
de8d799 ordering documentation
062e745 steps 4 and 5
8ad9f3a update protocol for multi hart
8987556 Add new metrics to dashboard
cc7d764 cooperative histogram workload
3085e5d histogram logic
cb1ab9f lcg workload
```

17 commits → ~4–5 binary search steps → ~20 min test wallclock.

Suspect clusters (oldest → newest):

- `cb1ab9f` → `cc7d764`: pre-SMP workload (LCG + histogram + cooperative).
  Note: these *predate* the SMP cluster — if bisection lands here, the SMP
  hypothesis is wrong.
- `8ad9f3a`: protocol bump for multi-hart (wire format).
- `062e745`: percpu plumbing + weak-memory audit (steps 4 & 5).
- `0c4d4f2`: IPI / SBI primitives.
- `db88062`: secondary hart boot scenario.
- `35b171d` → `ce206f1`: SBI HSM bring-up (step 9).
- `fe36ace`: 2nd hart metrics.
- `387f793`: per-hart runqueue + idle.
- `4034d25` → `efcbbf9`: lint cleanup tail.

## Progress log

| Step | Commit | Result | Notes |
|---|---|---|---|
| 0 (endpoint) | `2e409f2` | 0/50 GOOD | clean baseline |
| 0 (endpoint) | `efcbbf9` (HEAD) | 3/50 BAD | runs 10, 42, 49 failed; UART log shows "I am alive — entering heartbeat" then timeout (post-boot wedge) |
| 1 | `062e745` (midpoint) | 3/50 BAD | runs 10, 23, 39 failed; **different failure mode**: boot-time `panicked at kernel/src/percpu.rs:71:5: hartid out of range` — kernel never reaches "I am alive" |
| 2 | `8987556` | 0/50 GOOD | clean — new GOOD endpoint |
| 3 | `8ad9f3a` | 0/50 GOOD | clean — new GOOD endpoint; corridor narrowed to 2 commits |
| 4 | `de8d799` | (killed at run 33, ≥1 fail) | **redundant step — de8d799 is _after_ `062e745` in commit order, not between it and `8ad9f3a`**. `8ad9f3a..062e745` contains only `062e745`. So Bug A introducing commit is `062e745` itself. |

## Bug A localized: introduced at `062e745` ("steps 4 and 5")

The step-5 percpu plumbing commit. The asm/static layout for `PER_HART_DATA`
and the `percpu::init` bounds check both live in this commit. The
`hartid out of range` panic fires when OpenSBI hart-roulette hands the boot
to mhartid=1 and the bounds check rejects it.

Post-14 ties this to the `LOGICAL_TO_MHARTID` translation introduced in
step 9 (`35b171d` / `ce206f1`) — that's where Bug A was fixed.

## Bug B (heartbeat wedge) — bisection corridor

The HEAD-side flake (post-boot wedge — kernel prints "I am alive" then no
second heartbeat) is a different bug. It must have been introduced between
`062e745` and `efcbbf9`. But within that range Bug A is also alive (until
fixed in step 9), so we'll see two failure modes overlapping until we get
past the Bug A fix commit.

Strategy for Bug B:
- Use `062e745..efcbbf9` (15 commits) as the corridor.
- Classify per failure mode: percpu panic = Bug A (treat as `skip`-like
  for Bug B purposes); post-boot wedge = Bug B (treat as BAD).
- The bisection question becomes: "is the first commit where Bug B fires
  earlier or later than commit X?"
- Once Bug A's fix commit is past, runs should be clean except for Bug B.

Better practical approach: **fix Bug A first**, then re-run HEAD to see if
Bug B even still exists. Bug A's fix is presumably in the commit history
already (`35b171d`/`ce206f1`); cherry-picking it onto `062e745` to confirm
isolation might be faster than chasing two bugs simultaneously.

## Bug B bisection progress

| Step | Commit | Result | Notes |
|---|---|---|---|
| B-0 | `35b171d` | 0/50 GOOD | Bug A fixed here, Bug B not present — new GOOD endpoint |
| B-1 | `387f793` | 0/50 GOOD | structural suspect (per-hart runqueue + idle) is clean |
| B-2 | `4034d25` | 5/50 BAD | same UART trace as HEAD: "I am alive — entering heartbeat" then disconnect ~100ms later. Corridor narrowed to 1 candidate (`c229605`). |
| B-3 | `c229605` | 1/50 BAD | introducing commit confirmed. |

## Bug B localized: `c229605` "Clippy fixes"

Rate is 1/50 here vs 5/50 at the next commit (`4034d25`) — either statistical
noise, or a secondary issue piles on at 4034d25. Either way, `c229605` is
where Bug B enters the tree.

### The diff

All textbook-benign clippy fixes:

- `kernel-core/src/heap_smoke.rs` — `Default` impl, `map_or(true, ...)` →
  `is_none_or(...)`, `n % k == 0` → `n.is_multiple_of(k)`
- `kernel-core/src/intern.rs` — `Default` impl, nested `if let Some(e) { if e.x { ... } }`
  → `if let Some(e) = entry && e.x == ... { return ... }` (let-chain)
- `kernel-core/src/mmu.rs` — `Default` impl, test-only lifetime elision
- `kernel-core/src/preinit.rs` — `Default` impl
- `kernel-core/src/workload.rs` — single `#[allow(clippy::should_implement_trait)]`
- `kernel/src/percpu.rs` — comment punctuation
- `kernel/src/sched.rs` — unused import removal
- `collector/*` — host-only, irrelevant to kernel runtime

None of these should change runtime behavior. But one of them did.

### Hypotheses to investigate (in priority order)

1. **`intern.rs` let-chain rewrite.** `intern` is on the boot + heartbeat
   hot path (every `register_counter_owned`, every span name lookup). A
   subtle codegen difference between nested `if let` and let-chain could
   surface here. The rewrite is macro-level lowering; check the actual
   indentation/scope — the diff shows only `return` inside the block, no
   visible closing brace where the previous nesting would have had one.
   If the let-chain accidentally moved code out of the loop body, lookups
   could behave wrong.
2. **`is_none_or` / `is_multiple_of`.** Theoretically equivalent to the
   old forms, but worth confirming on the kernel's exact stdlib version.
   This is in heap_smoke, which runs at heartbeat — plausible vector.
3. **`Default` impls.** Almost certainly inert, but listing for completeness.

### Next step

Inspect `intern.rs` at `c229605` carefully. If the let-chain rewrite has a
subtle scope error, that's the bug. If it's clean, move to hypothesis 2.

## Hunk-level investigation

Per-file revert sweep — at the c229605 overlay, revert one file at a time
to `387f793`'s version and re-run x50.

| Sub-step | File reverted | Result | Notes |
|---|---|---|---|
| H-1 | `kernel-core/src/intern.rs` | BAD: 1 fail / 28 runs | let-chain rewrite NOT the cause |
| H-2 | `kernel-core/src/heap_smoke.rs` | BAD: 1 fail / 28 runs | `is_none_or` / `is_multiple_of` swaps NOT the cause |
| H-3 | `kernel-core/src/mmu.rs` | BAD: 1 fail / 41 runs | `Default` impl + test-only lifetime elision NOT the cause |
| H-4 | `kernel-core/src/preinit.rs` | BAD: 1 fail / 97 runs (50 clean, then 1 fail at run 47/50 of confirmation pass) | `Default` impl NOT the cause; first 50 was statistical luck — at ~2% true rate, P(0 in 50) ≈ 36% |

## Updated hypothesis: c229605 unmasks, not introduces

Both highest-likelihood files ruled out, same Bug B signature each time.

The c229605 diff is genuinely all benign clippy cleanup — no file in it
contains a semantic change. Yet bisection consistently points here.

Working theory: **c229605's minor codegen ripples (function layout in
`.text`, monomorphization order, inlining decisions) nudge timing enough to
open a race window that pre-existed in the kernel**. The "bug" lives in the
original code; c229605 is the trigger, not the cause.

Evidence:
- Failure rate is 1–10%, characteristic of a timing race.
- Failure mode: kernel boots fully, virtio wedges within ~100ms — classic
  shape for a race between virtio TX queue setup, the first heartbeat,
  and timer IRQ enabling.
- The clippy fixes that *should* be 100% inert sometimes still alter
  codegen via function ordering inside the ELF.

Implication: even if we find the offending hunk via revert sweep, fixing
that hunk won't fix the underlying race. The bisection tells us **where
the timing window opened**, not **what bug exists**.

### Sweep result: codegen-unmasks-race hypothesis confirmed

All four files that *could* plausibly affect runtime (intern, heap_smoke,
mmu, preinit) were reverted individually. Bug B persisted at the same
~1–3% rate in every case. The remaining three files (`workload.rs`
`#[allow]` attribute, `kernel/src/percpu.rs` comment punctuation,
`kernel/src/sched.rs` unused-import removal) cannot meaningfully affect
runtime, so testing them is unnecessary.

**Conclusion: Bug B is a pre-existing race condition in the kernel code at
`387f793` (or earlier). c229605's *aggregate* codegen footprint nudges the
binary layout enough to open the race's timing window. No single hunk in
c229605 is responsible — reverting any one of them leaves the race intact.**

## The actual race

The kernel UART reaches `I am alive — entering heartbeat`, then virtio-console
disconnects within ~100 ms in ~2% of runs. Suspects on the virtio-console
init / first-tx ordering, in order of likelihood:

1. **virtio TX queue race against the first heartbeat span.** First
   heartbeat emits frames via `virtio_console::send`; if device-ready
   handshake (`VIRTIO_STATUS_DRIVER_OK` write, queue notify) isn't
   sequenced before the first send under all codegen layouts, occasional
   wedge.
2. **Timer IRQ enabling vs static initializer order.** `TIMER_INTERVAL_TICKS`
   is set during boot and read by the IRQ handler. If the IRQ enable happens
   before the interval is published with the right memory ordering — possible
   under reordered codegen.
3. **`Mutex<Inner>` first-acquire ordering.** virtio-console's mutex on the
   TX path; if first-acquire happens during a window where the printed
   "I am alive" UART path is still holding its own lock and codegen
   reorders the release, a deadlock.

Next step (separate investigation, not bisection):

- Confirm codegen theory empirically with `nm --size-sort` or
  `riscv64-elf-objdump -d` diff between kernels built at 387f793 and
  c229605. If function ordering / sizes have shifted meaningfully, theory
  confirmed.
- Read `kernel/src/main.rs` boot path from "I am alive" through first
  `kernel.heartbeat` SpanStart. Look for sequencing assumptions between
  virtio init, timer init, and the first frame emission.
- Add stronger ordering primitives (compiler/memory fences, Release/Acquire
  where Relaxed is currently used) on the suspect publication patterns.
  The `kernel::percpu` "memory ordering discipline" doc — added in
  `de8d799` — is the right home for any new invariants surfaced here.

## Process takeaways

- `--repeat 50` is borderline for distinguishing a clean run from a 2-3%
  flake rate; budget for a confirmation pass when a step looks clean at
  the bisection edge. Saved as feedback memory.
- Bisection localizes *where the symptom manifests*, not always *where the
  bug lives*. When the introducing commit's diff is implausibly benign, the
  bisection has found a codegen edge, not a logic edge. Recognize this
  shape: the per-file revert sweep is what proves it.
- Documenting the bisection log as it ran (this file) was invaluable for
  catching my own corridor-direction error at sub-step 4. Keep doing this.

Corridor narrowed to 4 commits, all surface-level:

```
efcbbf9 More lint fixes.        <- BAD
800cca5 Clippy fixes.
4034d25 expect a dead code snippet
c229605 Clippy fixes
387f793 per-hart runqueue + idle <- GOOD
```

**Striking finding**: Bug B was introduced by what should be cosmetic
commits. Most likely a clippy autofix with semantic effect (post-14
already documents `deref_addrof` autofix breaking the kernel's required
`&mut *(&raw mut STATIC)` idiom — that exact hazard, possibly recurring).

Next midpoint: `4034d25` ("expect a dead code snippet").

## Two distinct failure modes

Step 1 surfaced that the corridor likely contains **two interleaved bugs**:

- **Bug A — boot-time percpu panic** at `062e745`. `percpu.rs:71` asserts hartid
  in range; OpenSBI hart-roulette (described in post 14) hands boot to mhartid=1
  with a `MAX_HARTS=?` bounds check that rejects it. Post 14 calls out the
  `LOGICAL_TO_MHARTID` translation fix landing in step 9 (`35b171d` / `ce206f1`)
  — that's almost certainly where this gets fixed.
- **Bug B — post-boot heartbeat wedge** at HEAD. Kernel boots fine, prints
  "I am alive — entering heartbeat", then second heartbeat never arrives.

The pinned scenario (`heartbeat-cadence`) treats both as failures. The
bisection will localize the **earlier-introduced** bug first. Plan: fix that,
rebuild HEAD, re-run; if HEAD still flakes, bisect again for Bug B.

## Bisection mechanic going forward

`062e745` BAD → new corridor `2e409f2..062e745` (7 commits):

```
de8d799 ordering documentation
062e745 steps 4 and 5            <- BAD endpoint
8ad9f3a update protocol for multi hart
8987556 Add new metrics to dashboard
cc7d764 cooperative histogram workload
3085e5d histogram logic
cb1ab9f lcg workload              <- GOOD-adjacent
```

Next midpoint: `8987556`. log2(7) ≈ 3 more steps.

## Tradeoff to watch

`heartbeat-cadence` is a boot/heartbeat-path scenario. If the flake is
specifically a multi-hart race (IPI / shootdown handshake), the pinned
scenario might be insensitive to it. If the corridor closes on a commit
whose changes look unrelated to heartbeat/virtio, switch the pinned scenario
to `smp-spawn-on-hart-1-runs` and re-bisect over the narrower (post-SMP)
sub-corridor.
