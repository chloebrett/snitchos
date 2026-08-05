# Post 69 — the bug was where no test could reach

- this was a structural session, not a feature one. i set out to tidy — retire finished plans to `plans/legacy/`, garden a stale debt register — and the tidying kept turning up the same shape underneath: **a claim nobody had tested, standing unexamined, and wrong.** three of them, big ones. the split was "for speed" (it wasn't). the ELF loader was "fine" (it had a writable-executable page one `static mut` away). the userspace pin dodged "an opt≥2 UB class" (there was no UB). each was wrong in the comfortable direction, and each fix was the same move: *drag the thing to where a test — or a measurement — can reach it.*

- posts 56 and 57 are this session's siblings and own two of its beats already: 56 is why stale docs only ever drift toward *underclaiming*, 57 is how a false *reason* breeds copies of itself down the crate list. this post is the third leg — the actual engineering the tidying uncovered, and the one lesson that ties all three sessions together: **the compiler, the review, the test suite — none of them check what you've put out of their reach. so the danger lives exactly there.**

## the crate that meant "the host-testable bits"

- `kernel-core` was where the host-testable kernel logic lived — the intern table, the scheduler bookkeeping, the page-table math — split out years ago from the bare-metal `kernel` binary so `cargo test` could reach it. it had grown to 27 modules and ten thousand lines, and the name had stopped describing anything: it meant "the bits that happen to build for the host," which is not a concept, it's a leftover.

- the obvious reason to split it further is speed — a one-line change recompiles all ten thousand lines. so i measured that first, and it **did not survive.** the tests already ran in **0.01 seconds**; there is no test runtime to recover. pulling out the largest quarter (`kernel-mem`, 2445 lines) shed **23% of the crate's lines for 2.5% of its build time** — because build cost is not proportional to lines, it's dominated by a fixed ~5–6s per-test-binary floor. more crates means *more* floors. the honest full-suite number after a split is slightly *worse*.

- so i did it anyway, on the real reason: **a crate boundary is enforced; a module boundary is discipline.** nothing stops someone adding `use crate::sched::TaskId` to `mmu.rs` tomorrow — it compiles, review misses it, and the layering rots. as separate crates that same edit needs a `Cargo.toml` line: a visible, reviewable decision, and a cycle becomes a hard error instead of a judgement call. that's the identical argument the repo already made for `kernel::sync` + its `disallowed_types` lint, and for the itest-harness carve-out. build time was a cost i chose to pay for a boundary the compiler enforces.

- five crates, grouped by what they *mean*, not by dependency convenience: `kernel-mem` (memory bookkeeping), `kernel-obs` (how the kernel talks about itself — the only one that touches the wire format), `kernel-devices` (device protocol state machines, no MMIO), `kernel-boot` (bootarg + trap-cause decisions), `kernel-proc` (tasks, authority, lifecycle). grouping by concept had a bonus i didn't plan: an earlier sketch needed a `kernel-ids` crate purely to break `cap → ipc/notify/sched` on three ID newtypes — but those five modules land in **one** crate (`kernel-proc`), so the coupling is internal and the junk-drawer crate never gets created. three of the five turn out to be **dependency-free** — most of what was called "core" has nothing to do with the wire format, a fact that was undiscoverable while it was one crate.

- the method was **facade-then-delete**, and it's the part worth stealing. each extraction was a pure `git mv` behind a `pub use kernel_x::…` line in `kernel-core`, so `kernel_core::mmu` still resolved and the kernel's ~200 call sites needed zero edits — every intermediate state compiled. then one final sweep deleted the `pub use` lines, and the *compiler named every site to repoint.* a big scary refactor became a sequence of zero-diff moves plus one deletion the compiler drives. at the end `kernel-core` held no code and got deleted. (it left tooling casualties — a test gate that silently dropped a crate — but 57 tells that half.)

## the boundary test was not `unsafe`-freedom

- before splitting `kernel-core` *up*, i swept the `kernel` binary the other way: what pure logic is still stranded in there that *should* be host-testable? my instinct was to grep for `unsafe`-free code. **that was the wrong test, and finding out why was the most useful thing in the session.**

- 2665 lines of `kernel/` carry no `unsafe` at all — and nearly all of it *belongs* in the binary. it's glue: it locks a static, threads a `TrapFrame`, calls an already-extracted function. `syscall/ipc.rs` has zero `unsafe` and cannot run on the host, because it reads a `TrapFrame` and locks `proc.caps`. `unsafe`-freedom tells you nothing about testability. the real question is narrower and sharper: **does it touch statics, `TrapFrame`, or MMIO?** if not, it belongs in a `kernel-*` crate — and the sting in the tail is that **pure logic often hides *inside* an `unsafe` function**, where no file-level search for "safe code" will ever find it.

## the writable page nobody could test

- which is exactly where the one real find was. the userspace ELF loader, `load()`, is an `unsafe` function — it ends in `copy_nonoverlapping`. but wrapped around those three lines of effect (`alloc`, `map`, `copy`) is pure arithmetic: parse the segments, union their permissions per page, split the file bytes into per-page copy windows. that planning had no business being un-testable. it was only un-testable because it lived inside the `unsafe` function the effects needed.

- i moved the planning to `kernel_proc::elf` — `page_perms`, `copy_windows` — where a test could reach it. and reaching it is what found the bug: **`load()` unioned the permissions of every segment that shared a page, with no W^X check anywhere in the codebase.** two `PT_LOAD`s can legitimately share a 4 KiB page (R-X code + R-- rodata unions to R-X, which is fine). but nothing asserted that a *writable* segment sharing a page with an *executable* one — which unions to RWX — was refused.

- i didn't argue it, i demonstrated it. add one initialised mutable static to `init` (`static mut X: u64 = 0xDEADBEEF` → a `.data` section), revert a linker alignment, and the *real toolchain* produced:

  ```
  PT_LOAD  R-X  @ 0x10000000   ─┐
  PT_LOAD  R--  @ 0x100006B0   ─┼─ all three land in page 0x10000000
  PT_LOAD  RW-  @ 0x10000908   ─┘   union = R + W + X
  ```

  a fully **writable-and-executable page holding every byte of the program's code**, mapped silently. the guard now refuses it (`PlanError::WxViolation`), `user.ld` page-aligns `.data` so real programs never trip it, and — the part i like — the itest suite now *guards the alignment*: pull it out and `init` won't load, so boot panics and the suite goes red. the intent the linker script had documented since v0.7a is finally enforced by something that fails.

- this is the split's whole thesis paying its rent in a single bug. pure logic hidden inside an `unsafe` function was invisible to review *and* to the test suite — a live security hole — until it was moved four inches to the left, into a crate a test could reach.

- the same move, in the same file, turned up two more the instant the code was reachable. `elf::parse` validated `file_size <= mem_size` but never bounded `mem_size` itself — a malicious image declaring `2^60` would make `page_perms` build a ~2^48-entry `BTreeMap` and **hang the kernel before allocating a frame**, which is *worse* than the panic the module's own doc comment promised was impossible. bounded now (`ImageTooLarge`), summed across segments so 65535 tiny segments can't multiply back to absurd. and `MmioRegions` + the `satp` encode/decode moved to `kernel-mem`, where a round-trip test finally pins the mode-shift and the PPN-mask constants *together* — they'd been sitting ten lines apart with nothing asserting they agreed, and a mismatch silently loads the wrong address space. moving `satp_for` also revealed it had been **open-coded a second time inside `mmu::enable`**: the same encode written twice, either fixable without the other. one host-tested source now.

## the UB that wasn't

- the third unexamined claim was the biggest, and i got it wrong myself before it got fixed. the kernel pins the embedded userspace to `opt-level=1` in `build.rs`, with a reason written down: *"there's a latent opt≥2 UB class in the userspace crates — talc OOM-loop, hang."* i went to root-cause it so the pin could come off.

- first question: is it even real, or a snemu emulator artifact? snemu had just had a genuine page-straddle fetch bug fixed, and the timeline was suggestive — pin on the 10th, straddle fix on the 17th. so i tested that hypothesis and rejected it: **QEMU reproduces it too.** QEMU has real RISC-V semantics and never had the straddle bug, so a hang there proves the bug is real, not an emulator ghost. good. except — and this is the error i'd catch only later — **"reproduces on the oracle" proves *real*, it does not prove *UB*.** a *correct* optimization deleting dead code also reproduces on the oracle. i let one claim stand in for the other.

- to find the loop, i built tooling: a proper `--opt hi` / `--opt max` ladder (opt-2 and opt-3 as first-class build regimes, replacing an env-var poke), and a `--user-detail` mode for the guest profiler that splits userspace out per-PC instead of collapsing it to one bucket. then i profiled `spawn-reap` at opt-2 expecting a hot userspace address to dominate — a spin-loop's signature.

- it didn't. the profile was **kernel-side spawn/reap churn** — `memset`, `prepare_switch`, `page_perms`, telemetry serialization — with **no `[user:…]` PC anywhere near the top.** i recorded that observation correctly ("kernel-side, not a userspace spin") and then, in the debt register, *mis-summarized it anyway* as "a userspace task is stuck in a loop." that's where the session paused.

- the resolution came in a later pass, and it's a clean one. the failing scenario is `spawn-reclaims-memory`; the workload is `memhog`, a *test program* that reserves 4 MiB so the reaper can prove `Exit` reclaims the frames. it guarded the reservation like this:

  ```rust
  let buf = Vec::<u8>::with_capacity(4 * 1024 * 1024);
  // read capacity back so the reservation can't be optimized away
  exit_with((buf.capacity() != 0) as i32);
  ```

  **that guard does nothing.** `capacity()` is a field load LLVM constant-folds back to the requested size without ever touching the *allocation*, and Rust's allocator calls carry LLVM attributes that make them removable when the result is unused. so at opt≥2 the allocation is dead code, and the optimizer correctly deletes it. no `MapAnon` syscall, no committed frames, nothing to reclaim, scenario fails. **measured, not inferred** — `rust-objdump` on `memhog` shows the `MapAnon` `ecall` present at opt-1 and *gone* at opt-2. the fix is one line, `core::hint::black_box(buf.as_ptr())`, in a test program. no kernel change. **130/130 at opt-2 and opt-3; the pin is gone.**

- so there was never any UB. the whole hunt had the label upside down. and my "genuine UB, not a snemu artifact" was the crux error, spelled out: QEMU reproducing it told me the *symptom* was real, and i let that carry me straight to a *bug class* — undefined behavior in the kernel — that a two-line `objdump` would have refuted on day one. the allocation simply wasn't there. see [[feedback_dead_allocation_is_not_ub]].

## what i learned

- **the boundary test is not `unsafe`-freedom.** most of `kernel/` is glue with no `unsafe` and belongs there; the question is *does it touch statics, `TrapFrame`, or MMIO.* and the corollary is the one that actually bit: pure logic hides *inside* `unsafe` functions, invisible to a search for safe code — which is exactly where a security hole was living.

- **move it to where a test can reach it — that's the whole through-line.** the W^X hole, the unbounded `mem_size`, the two `satp` constants that never agreed: none of them were found by cleverness. each was found the *instant* the logic crossed into a crate a `#[test]` could touch. reachability is the variable; the bugs were already there.

- **a crate boundary is enforced; a module boundary is discipline.** the split cost build time and bought a wall the compiler patrols. that trade is worth naming out loud, because the seductive-but-wrong reason ("it'll be faster!") measured false, and the real reason is unglamorous.

- **facade-then-delete makes a scary refactor boring.** every step a zero-diff `git mv` behind a `pub use`, so every intermediate compiles; then one sweep deletes the facade and the compiler enumerates the repoint sites. a ten-thousand-line dissolution with no cliff.

- **"reproduces on the oracle" proves real, not UB.** those are different claims and i collapsed them. a correct optimization removing dead code reproduces on real hardware too. the oracle tells you the symptom exists; it says nothing about which *class* of cause produced it.

- **a label nobody measured is probably wrong, in the comfortable direction.** "for speed," "bare-metal fights pedantic" (that's 57's), "opt≥2 UB class" — three unexamined claims this session, all inverted by a measurement that took ten minutes. plausible is exactly where an unchecked claim hides, because plausible is what stops you checking.

- **demonstrate, don't argue.** the W^X hole was one `static mut` and an `objdump` away from a live, reproducible repro. showing the RWX page beats asserting it could exist — and the same discipline, applied to the *other* side, is what finally killed the UB story: an `objdump` showing the allocation gone.

## what's next

- the debt register that started this is down to honest deferrals now, and the opt-1 pin — the loose end this session left dangling — is gone. more than that: these five crates turned out to be the floor everything since has been built on. the port to real hardware, the audio server, the on-target model runner — all of them lean on `kernel-mem`'s address math and `kernel-proc`'s process model as *stable, tested, separately-compiled* pieces. the wall the compiler patrols held. that's the payoff a build-time cost was buying, and it's only visible in hindsight.
