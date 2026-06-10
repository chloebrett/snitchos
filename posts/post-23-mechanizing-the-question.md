# Post 23 — Mechanizing the question

- post 20 ended on a line: _a grep is a question, not a verdict_. the audit skill kept asking that question by hand — and the hand kept slipping. a per-symbol table that a stray `\r` truncated. single-letter `PtePerms` flags (`R`/`W`/`X`) matching as words and flooding the counts. `dead_code` that's blind to `pub` items in `pub` modules — exactly the shape of every leaf crate here.
- so this session I built the tool to ask the question reliably, ran it across all five crates, and learned the thing the title gives away: mechanizing the _question_ doesn't mechanize the _answer_.

## the tool

- `cargo xtask audit <crate>` — a mechanical evidence-gatherer for the skill. for one crate it prints a per-`pub`-symbol table (`ext` = sibling-crate callers, `int` = this crate, `test` = test-only), the zero-caller candidates, the debt markers, and `cargo machete`'s unused deps. `--json` so the skill ingests it.
- I thought about parsing Rust properly — `syn`, rust-analyzer, rustdoc. then realized the split: `syn` fixes _symbol extraction_ (the annoying-to-regex part) but not _caller resolution_ (the part that was actually wrong). resolution needs name analysis `syn` doesn't do; the correct tools (rust-analyzer, rustdoc JSON) are heavy and already exist. so the tool stays line/word-boundary heuristics — deliberately a **lower bound on deadness**, never a verdict.
- it's `loc.rs`'s altitude, and it literally shares `loc`'s code: the `#[cfg(test)]` block detector got lifted into one `test_line_mask` both consume.
- first run, first crate: it flagged `spin` as an unused dependency in `kernel-core` — which my own hand-audit two days earlier had waved through as "load-bearing." the tool found something the human missed on day one. `grep -rw spin` confirmed it: one doc comment, zero `spin::`. removed.

## what a grep can't count

- the tool over-counts and under-counts, both structurally, and naming each failure mode is most of the value.
- **over-count by name collision.** `pub fn new` reads as `ext=455` because every `new` in the workspace matches. `is_empty`, `len`, `count` — same. so a _high_ count on a common name proves nothing; only a **zero** is trustworthy. the tool is high-precision on unique names, silent on common ones. that's the right trade for "flag candidates a human verifies."
- **under-count three ways** — proven by actually acting on them. (1) a **re-export alias**: `push_with_timeout` is consumed as `push_otlp_with_timeout`, so counting by declared name misses it. (2) a **type used positionally in a public signature**: a caller does `BaselineFile::load_path()` returning `Result<_, BaselineError>` without ever naming `BaselineError` — so it reads `ext=0` despite being public API. (3) **required by a lint, not a caller**: `Runqueue::is_empty` has zero callers, but `clippy::len_without_is_empty` demands it exist next to the public `len`.
- the move that turns all three blind spots into hard signals: the **privatization sweep**. demote the whole `ext=0` set to `pub(crate)`, rebuild _and clippy_ the crate plus its consumers, re-promote exactly what breaks. a re-export alias fails E0364; a positionally-used type fails `private_interfaces`; a lint-required item fails its own lint. the compiler is the oracle, not the grep.
- ran it on `itest-harness`: **99 → 56 public items**, 43 demotes, the 12 re-promotions all falling into the classes above. ran it on `kernel-core`: `branch_pte`, `split_va`, `PRE_INIT_BYTES` demoted; `is_empty` tried, tripped `len_without_is_empty`, reverted. each false positive is now written into the skill so the next sweep expects it.

## the part the tool can't do

- here's the thing the whole session kept teaching: `cargo xtask audit` only finds the _subtractive_ wins. dead code, unused deps, over-broad visibility. the additive wins — the abstractions — are invisible to it, and they were the highest-value findings every time.
- `kernel-core` came back mechanically **clean**: no dead modules, no long functions (clippy pedantic: zero `too_many_lines`), the `PtMem` trait already a tidy host-testable port. the tool had nothing. but reading `mmu.rs` by hand:
  - PTEs were raw `u64` pushed through six free functions (`leaf_pte`, `branch_pte`, `pte_is_branch`, …). everything in the module is a `u64` — a PTE, a physical address, perms-bits — so the compiler couldn't stop you passing the wrong one. wrapped it as `Pte(u64)`, `#[repr(transparent)]`, with `Pte::leaf` / `.is_branch()` / `.child_pa()`. the clincher: the crate **already had** a `PtePerms(u64)` newtype — this just finished a pattern it had started.
  - `split_va` returned `(usize, usize, usize, usize)`, destructured at eight sites, where a `vpn1`/`vpn0` swap is a silent bug. named it `Sv39Va { vpn2, vpn1, vpn0, offset }` — now a swap is a field-name error.
- both TDD'd behind the existing mmu tests + the riscv build + four boot scenarios. net-neutral on lines. pure type-safety gain.
- so I made the abstraction pass **mandatory** in the skill — never empty by omission. the tool finds what to delete; you still have to ask what's _missing_. and I put the `Pte`/`Sv39Va` story in as a worked example, framed plainly: _this is the kind of finding to actively hunt for, not a bonus — the point._

## the tool audits itself

- last crate: `xtask`, the orchestrator that the tool lives inside. a binary, consumed by nobody, so `ext=0` is meaningless — the privatization angle doesn't apply. mechanically clean. the finding was architectural, by hand.
- **`Cmd::Itest` was a god-subcommand**: 24 fields, eight of them mutually-exclusive _modes_ (prune, export, push, promote, discard, recover, adopt, baseline-show) dispatched by a precedence ladder of early `return`s before the actual test run. flags pretending to be subcommands.
- split them into `cargo xtask baseline {show,promote,discard,recover,adopt,prune,export,push}`. the ladder is gone — the modes are now mutually exclusive _in the type_; you can't pass `--prune-runs` to a run because it no longer exists there. then the implementation followed the CLI: the eight verbs moved to `itest/baseline.rs` (`itest.rs` **785 → 414**), and the duplicated load-baseline-and-push core that the auto-push path and the explicit push both inlined collapsed into one `load_and_push`.
- finding two: `itest::run` took **ten positional args** behind an `#[allow(too_many_arguments)]` whose own excuse said "refactor when more land." more had landed. → a `RunConfig` struct, allow retired.
- and a phantom: `itest --help` advertised `--keep-existing-qemus`, a flag clap never defined — it was prose in a doc comment promising behaviour ("stale QEMUs are killed before the suite") that the code never did (it only warns). help that lies about its own flags. dropped.

## a detour — the timeout that wasn't a failure

- mid-session, watching an itest run scroll by: `smp-tlb-shootdown-visible` printed `[timeout: last 8 of 64 frames]` and then `ok`. a timeout that passed?
- it's an _inverted oracle_. the scenario waits 5s for `tlb_stale_reads > 0` — the bad event — and the window elapsing clean _is_ the success. but `wait_for`'s timeout branch dumps an alarming capture on what's actually a pass.
- added `Harness::assert_absent`: the clean-elapsed window logs `negative-oracle window elapsed clean` instead of a scary dump; a matching bad frame returns the failure. the harness can't tell a good timeout from a bad one — so the scenario tells it.

## what i learned

- **mechanizing the question doesn't mechanize the answer.** the tool answers "who calls this" fast and repeatably. it cannot answer "should this exist" or "what's missing." for ordinary code those collapse together; for a wire format, a reserved slot, a missing newtype, they don't.
- **name the failure modes.** "the count is a lower bound" is useless; "it under-counts re-export aliases, positionally-used types, and lint-required items" is a checklist the next sweep runs. four named classes now, each earned by a real false positive.
- **the compiler is the oracle for what the grep can't see.** demote-then-rebuild turns every blind spot into a build error. you don't have to _know_ the false positives — you have to let the type system find them.
- **the human is the oracle for what the tool can't see.** every crate's best finding was an abstraction the tool is structurally blind to. so that pass is mandatory now, with a worked example, so it doesn't depend on me being in the mood.
- **write the rule into the skill, not just the diff.** the contract-surface rule (post 20), the four FP classes, the mandatory abstraction pass, the privatization procedure — all in the skill. the next crate gets them for free.

## what's next

- all five crates are audited and clean: `kernel-core`, `protocol`, `collector`, `itest-harness`, `xtask`. the audit muscle is a tool plus a sharper skill now, so the cost of the next one is mostly the part that was always the point — reading the code and asking what isn't there.
- two crates still have no README. post 20 taught me that writing one is a verification pass — the `protocol` "length-prefixed" lie only died when I tried to explain it. so that's the next place a confident-sounding sentence gets checked against the machine.

## the sequel — answering the question for virtio

- the audit only finds two axes: what's **dead** (subtract) and what's **missing** (add). this session hit the third it's blind to — code on the **wrong side of a seam**: pure logic stranded in the bare-metal `kernel` crate where no host test can reach. the fix isn't delete or abstract, it's **relocate** — move it to `kernel-core`, leave the hardware shell behind.
- two warm-ups proved the shape. `frame.rs`'s reserved-region arithmetic → `kernel_core::frame::release_unreserved` (11/11 mutants caught). `grow_va_range`'s ceiling math → `kernel_core::heap::next_heap_top` — and moving it **surfaced a latent bug**: the in-place version multiplied `n * FRAME_SIZE` _unchecked_ before the `checked_add`. you don't see it until you write the test that pokes the overflow. _extraction is a bug-finder._
- then the big one: the entire virtio-mmio bring-up — register map, feature negotiation, queue setup, the reset→ack→driver→features-ok→driver-ok state machine — lifted into `kernel_core::virtio` behind a `MmioTransport` trait, host-tested against a `FakeVirtioDevice`. exactly the `PtMem` port shape the crate already had: **pure logic + a trait + a fake; the volatile MMIO, `va_to_pa`, and the `fence` stay in the kernel.** the win the tool can't see: `NoVersion1` and `QueueTooSmall` were **dead under QEMU** (it always offers VERSION_1 and queue-max 1024) — code that had _never run_. behind the fake, each is one assertion. _relocation makes the unreachable testable._
- mutation flagged five survivors — all `|`→`^` on the status bits. not missing tests: the bits (`0x01/0x02/0x08/0x80`) are disjoint and each set exactly once, so `^` ≡ `|`. an **equivalent mutant** you kill by understanding, not by adding a test (same class as the documented `advance_anchor` one). _a survivor is a question too._

## the flake, made impossible — then deterministic

- the payoff target: the ~2% cross-hart wedge from the deflake saga — a dropped `MutexGuard` in `virtio_console::send`. root cause, named at last: `CONSOLE: Mutex<usize>` guarded a **`Copy`** base address while the actually-shared state — the `TX_STAGING` buffer — lived _outside_ the lock. **the lock guarded the wrong thing.**
- **structural fix:** put the buffer _inside_ the mutex — `Mutex<TxStaging>`, deliberately **not `Copy`** — so the original footgun `let base = *handle.lock();` (copy-out, drop-the-guard-at-the-`;`) **no longer compiles** (E0507, can't move out of the guard). the bug class is dead by construction, not by comment.
- **deterministic regression:** a `loom` model-check (`kernel-core/tests/loom_tx.rs`, `#![cfg(loom)]`) that exhaustively explores the interleavings. the correct primitive passes; a **buggy twin** modelling the old architecture (buffer in an outside-the-lock `UnsafeCell`, guard dropped early) fails; a meta-assertion pins _both_, so the detector can't silently rot. a 2%-of-the-time flake became **won't-compile + caught on every run.** wired into `cargo xtask test`, so it gates every suite.
- a tidy-up the seam forced: `HandshakeError` lives in kernel-core, `InitError` in the kernel — nested the former into the latter (`InitError::Handshake(_)`) so the pure handshake owns its failures and the kernel adds only `NotFound`, the DTB-discovery one that can't move.

## what i learned (sequel)

- **the third question is "what's on the wrong side of a seam."** the tool finds dead and missing; it can't see logic stranded in the hardware crate. relocating it is where the host tests, the found bug, and the killed flake all came from.
- **the seam has one shape, reused every time:** pure logic in kernel-core, a trait for the hardware (`MmioTransport`, like `PtMem`), a fake for the test. the kernel keeps only what genuinely touches silicon — MMIO, `va_to_pa`, the fence, the lock discipline.
- **a flake is a design smell with a structural fix.** "guard the thing that's actually shared" turned a memory-ordering scare into a one-line type change the compiler enforces — and `loom` turned "caught it by luck at 2%" into "caught every run." the lock guarding a `Copy` value was the whole bug.
