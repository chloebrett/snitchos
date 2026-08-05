# Post 72 — the unit was already on

- post 65 ended with a guard I was slightly pleased with and slightly embarrassed by. userspace floating point worked, but the kernel couldn't yet save FP registers across a context switch — so a second FP process would silently share the first's 32 registers. rather than let that corrupt quietly, I made it refuse: one FP process at a time, snitched.
- I wrote, in that post, that it was a guard "against a case nobody has". it had a customer within two days. and fixing it turned up something worse than the thing it was guarding against — a fact about the machine the whole lazy-FP design silently depended on, which nobody had checked, and which is **false on the board I'm actually targeting**.

## the case nobody had

- REPL tab completion breaks "only the REPL uses FP" **structurally**, not incidentally.
- the Stitch grammar oracle runs on *both* sides of the wire. the kvetch server samples a completion through it, and `ModelCompleter` re-validates the suggestion through it — because a client must not trust another process to police its own output. that's not belt-and-braces, it's the whole security posture of the powerbox pattern.
- two processes running the Stitch lexer means two processes that will eventually parse a float literal (`babble`'s `FLOATS` → `TokenKind::Float(f64)` → `dec2flt`). **no ordering satisfies a one-holder rule.** the second one to touch a float dies, whichever it is.
- measured, 2026-07-28: tab 1 completes and inserts at the prompt. tab 2 kills the server.

```
fp refused: task 4 — another process holds the FP registers …
user fault: task 4 killed by illegal instruction (scause=2 sepc=0x100280bc)
```

- so the guard was correct, loud, attributable, and had to go. that's the good version of being wrong: the refusal named itself, in one line, two days later.

## two design calls

### save at the switch, not at the trap

- the obvious implementation is to grow `TrapFrame` by 33 words and let FP ride the existing save/restore. it's wrong, and the reason is who pays.
- the kernel is zero-FP by design. **nothing between trap entry and a context switch can disturb a task's FP registers** — only another task actually running FP can. so the save belongs where the disturbance is.
- putting it in `TrapFrame` charges 264 bytes of copying to every syscall and every timer tick, including the overwhelming majority that return to the *same* task and could not possibly have lost anything. saving at the switch charges only switches, which is the population the cost belongs to.
- bonus: it keeps the change out of the trap entry/exit asm, which every scenario in the suite leans on.

### all 32 registers, not just `fs0`–`fs11`

- `switch` only saves `s0`–`s11` today, and there's a good reason: it *is* a function call, so the C ABI already lets the compiler treat caller-saved registers as clobbered across it. transfer that reasoning to FP and you save `fs0`–`fs11` and stop.
- **that is wrong, and preemption is why.** a task descheduled by the timer never made a call. no instruction boundary in it is a clobber point. `ft0`–`ft11` and `fa0`–`fa7` are live and must survive.
- the integer side gets this for free and I nearly didn't notice: `TrapFrame` already saves all 31 GPRs at trap entry, so the preempted case is covered before `switch` is reached. FP has no such frame — by call 1, deliberately — so the full set has to live in the task.
- `fcsr` goes with them. it holds the rounding mode, and a task that sets one must not have it changed under it by another task's arithmetic.

### and `sstatus.FS` is the filter

- save only when the outgoing task's `FS` is `Dirty` — it has actually written FP since its last restore. `Clean` means the in-memory copy is still good. `Off` means there's nothing there. this is exactly what the Clean/Dirty tracking from post 65's emulator work exists to make cheap.
- two hazards worth writing down, both of the "the tool is made of the thing it's operating on" kind:
  - **the copy code itself needs `FS != Off`** — `fsd`/`fld` *are* FP instructions. the switch has to raise `FS` around its own copy and set the final value afterwards.
  - **a task with no FP must be switched in with `FS = Off`**, or its first FP instruction quietly stops trapping — and the entire lazy authority check stops running. the enable path is only reachable while `FS` is `Off`. get this backwards and FP becomes ambient without anything failing.
- `sched.S` ended up untouched, which I did not expect. the copy lives in Rust either side of the `switch` call, and that works because `switch` **returns in the task that called it** — so "save the outgoing" and "restore the incoming" are one task's before and after.

## the negative oracle earned its keep

- the feature test is the obvious one: two processes, distinct patterns in all 32 registers, yield and preempt repeatedly, assert the values read back. it failed with 32 mismatches before the fix, which is the falsifiability check done properly.
- the *other* test is the boring half: an integer-only boot must save **nothing**. a feature test proves the register file is carried when it must be; only the negative proves it isn't carried when it needn't be.
- it failed on the first QEMU run. `context_saves_total = 2`. one spurious save per hart, on a boot with no FP anywhere.

### firmware does not hand the kernel `FS = Off`

- OpenSBI leaves the FP unit **on**. I had assumed the reset value all the way through the design without ever writing the assumption down, let alone testing it.
- harmless where it surfaced — two pointless register copies. not harmless where the same assumption is load-bearing: **a user task inherits `FS` from the kernel context that enters it.** on a machine whose firmware leaves the unit on, userspace gets floating point as *ambient authority*. no trap. no `enable_decision`. no `Log` naming the grant. the entire authority story from post 65 just doesn't run, and nothing anywhere reports that it didn't.
- fixed with a `sched::fp_init_hart()` called per hart in `kmain` and in secondary bring-up. two lines. the bug was in the assumption, not the code.

- **which engine found it is the part I want to remember.** snemu resets `FS` to `Off` — faithful to the hardware reset value, and *not* an snemu bug. it's a scope boundary: snemu models a machine, not a firmware handoff. so the deterministic engine I trust for everything was correct and silent, and the flaky one I keep as a fidelity escape hatch was the only thing that could see it.
- the VF2 boots through OpenSBI too. this was a real-board bug, sitting in the one gap between my two engines, found by the cheapest test in the plan. the kernel now depends on neither behaviour, which is what makes the difference stop mattering.

## what's still open

- **is FP state per-task or per-process?** the switch reads `Process::fp_enabled`, which is per-process, while the registers belong to an execution *context*. those are the same thing only while each process has exactly one task — true today, and not verified anywhere.
- if a process ever gets two tasks, both claim to own one register file and each restores the other's saved state. the fix is a flag on the `Task`; the reason it isn't done is that setting it lives in `try_enable`, which runs in trap context and would have to reach the scheduler lock to find the current task.
- written down here rather than left as a known-unknown in a plan file, because that's the shape of thing that gets rediscovered expensively.
