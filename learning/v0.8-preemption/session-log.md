# v0.8 Preemption — Learning Session Log

## Session 1 — 2026-06-14 (~30 min)

**Format:** quiz-first (learner is the author of the code; goal was to surface gaps, not deliver).

**Topics covered (all reached Analyze/Evaluate; the gap reached Create):**
- The register paradox: two save layers (`trap_entry` full `TrapFrame` vs `switch` 14 callee-saved). Bloom: Analyze.
- `SPP` (Supervisor Previous Privilege) as the preemption gate; corrected misconception that it's an address check. Bloom: Understand→Apply.
- Lock-safety argument: why kernel is non-preemptible (preempt-while-holding-lock → hard deadlock on single hart); why no `preempt_count` needed yet. Bloom: Analyze.
- Priority + aging math; starvation; tie-break on `enqueued_tick`. Bloom: Apply (math correct).

**Performance:**
- Q3 (aging/starvation): correct, incl. the ~2s figure. Named the failure mode after prompting (starvation).
- Q1 (register paradox): right shape ("switch only callee-saved, preemptive path saves caller-saved first") but fuzzy on *where* and said caller-saved when it's *all* registers. Corrected.
- SPP: genuine knowledge gap (didn't know what SPP was; guessed address-based). Taught from scratch; landed cleanly.
- **Asked a sharp design question unprompted:** "can a userspace program dodge preemption by spamming syscalls?" Tutor (me) wrongly confirmed it as a real gap and walked through a `need_resched` fix. On verifying the code before implementing, it turned out **NOT** to be a real gap: traps run with `SIE==0` (interrupts masked) and SnitchOS never re-enables them mid-syscall, so a timer can't fire during a syscall — it fires immediately on `sret` to U-mode (`SPP==0`), normal preemption applies. The learner's `need_resched` design reasoning was sound *conditionally* (it's the right fix IF interrupts were ever enabled mid-syscall). Correction recorded in `plans/v0.8c-need-resched-on-syscall-return.md`.
- **Tutor error logged for honesty:** I asserted the gap was real and exploitable during the lesson without checking the interrupt-masking discipline first. Lesson reinforced: verify against code before confirming a "bug," especially in someone's own kernel.
- Final Feynman explanation: clean and complete. Owns the model.

**Confidence calibration:** entered "I mostly get it" (quiz-me); performance confirmed strong conceptual grasp with two genuine gaps (SPP specifics, register save location). Self-assessment was well-calibrated.

**Still-open threads (optional next time):**
- Q1b: trace the exact `ret`→`trap_entry`→`sret` resume chain in `trap.S` line by line.
- How `prepare_switch` re-enqueues the current task and builds `Candidate`s under the scheduler lock.
- SMP angle: preemption + per-hart runqueues (`runqueues[me]`).

**Artifacts produced:**
- `cheat-sheet.md`
- `plans/v0.8c-need-resched-on-syscall-return.md` (the discovered gap, as a real follow-up)

**Follow-on work this session (learner chose to build the guard):**
- Added `workload=syscall-hog` + scenario `syscall-hog-still-preempted` as a
  regression guard pinning "syscall-heavy tasks are still preempted." TDD: host
  `bootargs` parse test RED→GREEN first; kernel/userspace wiring; QEMU scenario
  10/10 under `--repeat`. Files: `kernel-core/src/bootargs.rs`,
  `user/hello/src/bin/syscall_hog.rs`, `kernel/{build.rs,src/trap/user.rs,src/main.rs,src/obs/heartbeat.rs}`,
  `xtask/src/itest.rs` + `scenarios.rs`. (Note: kernel modules live at
  `kernel/src/trap/user.rs` and `kernel/src/obs/heartbeat.rs`, not the top level.)

**Gaps tagged for spaced review (ask next session):**
- "Where does `switch`'s `ret` land, and what restores the full user state?" (Q1b — was never fully answered)
- "What single hardware bit makes the user/kernel preemption distinction cheap?" (should answer "SPP" instantly)
