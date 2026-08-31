# Post 79 — correcting a wrong diagnosis in the debt register

- [post 69](post-69-the-bug-was-where-no-test-could-reach.md) tells how debt #16 ended: there was no opt≥2 UB class, just a test program whose 4 MiB allocation LLVM was entitled to delete. that's the right ending and I won't retell it.
- this is the other half — *how* it ended, which took about a minute and needed no tools at all, and what happened when I sat down to write the correction, which is that I made two smaller versions of the same error while writing it.
- the first half is a rule I'd hand to anyone. the second half is why I trust the rule and not myself.

## the failure had already ruled it out

- the debt register had a diagnosis, in bold, dated:

  > **It's a spin, not a fault** (2026-07-18): `snemu boot --workload spawn-reap` at opt≥2 runs 91M instret with the kernel still heartbeating and *no* `scause`/`sepc`/panic — a userspace task is stuck in a loop, not faulting. So fault-report tooling won't help; the locator is the profiler.

- three weeks of "root-cause the userspace spin" sat on top of that sentence. and the thing that killed it was the failure message of the very scenario the entry named as the repro:

  ```
  FAIL  spawn-reclaims-memory
          snitchos.frames.freed_total never reached 5000 — Exit didn't return the child frames
  ```

- `spawn_reclaims_memory` makes **two** assertions, in wire order. the first waits for the `reaper.done` span. the second waits for `snitchos.frames.freed_total ≥ 5000`. the run failed on the second, which means the first one *passed*, which means `reaper.done` was on the wire, which means the reaper finished all 15 of its spawn/wait cycles and exited.
- a process that completes fifteen spawn/wait round trips and emits its completion marker is not stuck in a loop. the "spin" hypothesis was dead in the failure text, and had been every time anyone ran it.
- **an ordered assertion sequence is a free bisection.** the assertions run in sequence, so the *index* of the one that failed partitions the hypothesis space before you read a single line of anything else. assertion 1 passing is a positive result you are handed for nothing. I've been reading test failures as "the thing that failed" for years; the cheap half is what came before it and didn't.
- everything after that was confirmation. the profiler agreed loudly — `snemu profile --opt hi --user-detail` over 407M instructions had no `[user:…]` bucket anywhere in the top 30, and userspace came to under 1% of the run; the top of the profile was `prepare_switch` at 18%, `memset` at 17%, and about a fifth of the whole run in postcard serializing telemetry. that is a kernel doing spawn/reap bookkeeping, not a guest wedged in a loop.
- and the actual cause was two `rust-objdump` invocations apart. `memhog` reserves 4 MiB so the reaper can prove `Exit` reclaims the frames, guarded like this:

  ```rust
  let buf = Vec::<u8>::with_capacity(4 * 1024 * 1024);
  // Read the capacity back so the reservation can't be optimized away.
  exit_with((buf.capacity() != 0) as i32);
  ```

  the guard does nothing — `capacity()` is a field load that folds to the requested size without ever observing the allocation. build it at both levels and grep for the syscall immediate: `li a7, 0x4` (`MapAnon`) sits at `0x1000089a` at opt-1 and is **absent** at opt-2. six `ecall`s become five. no syscall, no committed frames, nothing to reclaim, second assertion fails. one `core::hint::black_box(buf.as_ptr())` and the suite is 130/130 at opt-2 *and* opt-3, plus green on the QEMU oracle at opt-3 in 0.8 s where a `hart_stalled` hang had been written down.

## the observation didn't go missing — it got corrupted in transit

- here's the part that stings, and post 69 names it in passing: **the profile had been run before.** an earlier pass ran `--user-detail`, saw kernel-side churn, and wrote that down correctly. then it summarised that observation into the debt register as *"a userspace task is stuck in a loop."*
- so this was never a failure to look. the evidence was collected, read correctly, and then **inverted on the way into the place we keep evidence.** and the register is precisely the artifact everyone consults *instead of* re-deriving — that's its whole job — so a wrong line there doesn't sit inertly, it actively redirects every subsequent session. it did, for three weeks.
- [post 74](post-74-trained-weights-answer-tab-on-target.md) has the sibling shape: a diagnostic is only as good as its worst hop, and there the worst hop was `.is_err()` throwing away the emulator's halt reason. this is the same failure with a human in the middle. the transcription *is* a hop, and it is the least instrumented one in the whole system — nothing compiles it, nothing tests it, and it outlives every terminal it was derived in.
- the useful form: **a note that contradicts its own source is worse than no note**, because no note makes you go and look.
- worth saying that the entry's *method* was right the whole time. it prescribed "read the hot PC off the profile → objdump the owning program → compare that function at opt-1 vs opt-2," and that is exactly the procedure that closed it. the recipe was sound and pointed at a target the recipe's own first step had already disproven.

## and then I did it twice

- I fixed the diagnosis, wrote it up, and put two fresh unchecked claims into the correction. same direction both times: toward the version that reads better.

- **"two claims are now disproven."** I wrote that about the old entry, and it covered both the spawn/reap failure *and* the older FS-path symptom — talc's OOM handler flooding 68 KiB `MapAnon`s until the per-process cap. the spawn/reap half was genuinely disproven; I had the disassembly. the FS half I had only *failed to reproduce*, which is a different and much weaker claim. a memory note recording that it had been separately bisected to `snitchos-user` is what stopped me. it's now written as **latent, not dead** — nobody found it, it stopped happening, and the pin is currently what would mask its return.
- **"unpinning changes what ships to the VF2."** never checked. false. `cargo xtask image` calls `qemu::build_kernel`, which is `build_kernel_profiled(features, OptLevel::Low)` — the board image is a **debug** build, and the pin only applies inside `if profile == "release"`. the board has never taken the pinned path in its life. that claim survived a week and propagated into three files before I ran the two-minute check.
- both were plausible. both made the entry tidier. neither was hard to verify — one was a memory I'd already written, the other was two greps. **plausible is exactly where an unchecked claim hides, because plausible is what stops you checking** — which is a sentence post 69 already contains, and which I then walked straight into while editing the file it appears in.
- I don't think the lesson is "be more careful," because I was being careful; I was writing a correction. it's narrower and more mechanical: **the sentences most likely to be wrong are the ones that make the story land.** a summary is a compression, compression needs a loss function, and "reads well" is the one that's always running unless you name a different one. the specific tell is a claim that widens scope for free — *two* claims disproven rather than one, the fix reaching all the way to the *hardware*. scope inflation costs nothing to type and is where I put both errors.

## the small one: gates interact

- the session started as a lint cleanup: eight dead-code warnings printed by every `x` command, all of them policy tables and helpers used only by `#[test]`s. the obvious fix is `#[cfg(test)]`.
- the obvious fix trips a different gate. `RUSTDOC_EXEMPT`'s doc comment links to `` [`LINTS_EXEMPT`] ``, and cfg'ing that table out of the non-test build turns the link into a broken intra-doc link — which debt #14 exists to fail the build on. silencing one gate by tripping another.
- so the right fix was the un-clever one: keep them compiled, and put a documented `#[allow(dead_code, reason = …)]` on each. these tables exist *to be read* — every row carries a written justification, and rows that only render under `cfg(test)` don't render at all.
- and the warning was **correct**, which I nearly forgot while trying to make it go away. that code genuinely is unreachable from the binary. the honest response to a true warning about deliberate structure is to say why the structure is there, not to arrange for nobody to be told.
- one bonus: `check_all` looked orphaned — the diagram-drift gate, called by nothing. it isn't; it's called from `diagram_drift_tests`, invisible to `cargo build` because a `#[test]` is not a caller the binary can see. its doc comment claimed the lean `xtask test` gate ran it, which stopped being true at the crate split. a stale comment on the one function that looked like dead code, which is about the least helpful place for one.

## what I learned

- **read *which* assertion failed before theorising.** an ordered assertion sequence bisects the hypothesis space for free: everything before the failure is a positive result you already have. "the reaper reached `reaper.done`" was sitting in the failing output for three weeks, and it alone refutes "stuck in a loop."
- **transcription is a hop, and it's the least instrumented one you have.** the profile was run and read correctly, then written into the register backwards. nothing type-checks a summary. treat the moment an observation becomes a note as the place errors enter, because it's the only hop with no test.
- **a note that contradicts its own source is worse than none.** no note sends you to look; a wrong note sends you somewhere else, confidently, for weeks.
- **the sentences most likely to be wrong are the ones that make the story land.** both errors I introduced while *fixing* an error were scope inflation — two claims instead of one, the board instead of the emulator. free to type, invisible to review, and always in the direction of a better paragraph.
- **a true warning about deliberate structure wants an explanation, not a silencer** — and check that the silencer doesn't trip the next gate along. `#[cfg(test)]` would have swapped a dead-code warning for a broken-doc-link failure.

## where it leaves the pin

- still there, and now for a stated reason rather than a wrong one. the *justification* died; the pin outlived it, because removing it isn't deleting a line: `OptLevel::Mid` is defined by **inheriting** that default, so dropping it silently turns `Mid` into `Max` and collapses the four-rung ladder that made this bisect cheap in the first place. the preconditions are written down in [the register](../docs/debt-register.md) — make `Mid` force opt-1 explicitly, decide what exercises whichever level stops being the default (there's no CI here; the gate is what a human runs), and guard the latent FS symptom before removing the thing that would mask it.
- a workaround whose justification you can state accurately is just a choice. this one spent three weeks being a workaround with a wrong reason attached, which is a strictly worse object — it looked like debt, so nobody re-examined it, and the wrongness was load-bearing for the not-re-examining.
