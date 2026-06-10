# Post 24 — Built wrong on purpose

- v0.7a: the first userspace process. It runs in U-mode in its own address space, makes exactly one syscall, and snitches a metric to Grafana — then tries to read kernel memory and gets slapped down. The syscall has **no permission check**: any user code can call it, the Unix way. That wrongness is the point. v0.7b rewrites it into a capability invocation, and you only feel why if you first build the version that doesn't have one. ~600 lines, two asm sequences I'd been dreading, and a harness bug that ran the wrong kernel for an hour.

## there is no "enter user mode" instruction

- I kept looking for it. There isn't one. RISC-V gives you `sret` — "supervisor return *from trap*" — and that's the only door into U-mode. It restores the PC from `sepc` and the privilege from one bit, `sstatus.SPP`. A trap into S-mode can only have come from U or S, so "where did we come from" is a single bit: 0=User, 1=Supervisor.
- so you don't *enter* userspace. you **forge a trap-return into a userspace you were never in.** set `SPP=0`, point `sepc` at the program's entry, `sret`. the CPU "returns" to a place it never left. it's the same instruction the trap handler already runs on every exit — I'd been executing the mechanism for three milestones without seeing it could go *down* as easily as it goes back.

```
    csrw satp, {satp}      # switch to the process's address space
    sfence.vma
    csrc sstatus, {clear}  # SPP=0 (→U), SUM=0, FS=Off, SIE=0
    csrs sstatus, {set}    # SPIE=1  → interrupts back on after sret
    csrw sscratch, sp      # park a kernel stack for the trap to come
    csrw sepc, {entry}
    sret                   # "return" into a program that never ran
```

## the untrusted stack

- here's what changes the instant a trap can come *from* U-mode: `sp` is now whatever the user program set it to. today's trap handler's first move is `addi sp, sp, -288` — carve a frame out of the current stack. fine when every trap came from the kernel. catastrophic when a hostile program sets `sp = 0xdeadbeef` and traps: the kernel, at full privilege, starts writing its registers to an attacker-chosen address. that's an arbitrary kernel **write-what-where** — and a trap doesn't touch the GPRs, so `ra` (the value being written) is attacker-controlled too. both halves theirs.
- the fix is the `sscratch` CSR — a scratch register U-mode can't name. convention: it holds `0` while in the kernel, and this thread's kernel stack pointer while in user. trap entry's *first* instruction swaps `sp` with it:

```
trap_entry:
    csrrw sp, sscratch, sp   # atomic swap — one instruction
    bnez  sp, 1f             # sp != 0 → came from user, sp is now the kernel stack
    csrrw sp, sscratch, sp   # sp == 0 → came from kernel, undo the swap
1:
```

- the swap, not a copy, because we need both things at once: land on the kernel stack *and* preserve the user's `sp` (now parked in `sscratch`) to save into the frame. `csrrw` is self-inverse, so the from-kernel path undoes itself in one instruction and the `sscratch=0` convention heals.
- the load-bearing ordering, learned the careful way: in the enter sequence above, `SIE` is cleared *before* `sscratch` is armed. arm `sscratch` with interrupts still on and a stray timer IRQ fires while we're in S-mode with a nonzero `sscratch` — trap entry takes the from-user branch and switches onto a bogus stack. mask first, arm second, `sret` restores interrupts atomically as it drops to U.

## one syscall, no permission

- the program does one thing: `ecall` with a syscall number in `a7`, a value in `a0`. the kernel's trap dispatcher already had an arm for "environment call from U-mode" — it was a `panic!`. now it reads `a7`, recognizes `EmitMetric`, emits the metric on the program's behalf, and **advances `sepc` by 4** so `sret` doesn't land back on the `ecall` and trap forever.
- there is no check. the program holds no handle, presents no token, the kernel asks no questions. `snitchos.user.telemetry_total` shows up in Grafana because a process *said so*. that's ambient authority, and it's exactly the model v0.7b exists to kill — the kernel surface becomes "invoke a capability you were granted," and this syscall becomes the first thing rewritten. building it wrong first means the rewrite has a before to point at.

## a page table per process

- the program is mapped at a fixed low-half VA (`0x10000000`), `U` bit set, with real W^X per segment — text R-X, rodata R, stack RW. it lives in its **own root page table**, not the kernel's. the trick that makes this cheap: the kernel's high half (Sv39 root slots 256..511 — image, linear map, heap) is *shared into* every process's root by copying those root entries. so a trap or syscall needs no page-table switch; the kernel is already mapped. seL4 does the same. it's the one real judgment call in the memory model — the surface Meltdown exploited — and acceptable for a learning OS.
- `enter` switches `satp` to the process root right before the `sret`. because the high half is shared, the enter code's own instructions and stack stay mapped across the switch — you're standing on the branch you're sawing, and it holds because both trees share that branch.
- the loader had to grow up here. `hello` was tidy — code in one page, stack page-aligned into the next. then `faulter` (the isolation probe) showed up with R-X code and R rodata **sharing the first page**, and the naive "map each segment's pages" loop tried to map `0x10000000` twice and errored. real ELF loading: union the perms over every page each segment touches, map each page once with the union, *then* copy the bytes. that loader is the durable piece — v0.10 loads programs from a filesystem through the same four steps; only the byte source changes.

## the firewall

- the payoff scenario: `faulter` emits a marker, then reads `0xffffffff80200000` — the kernel image base. that page is *mapped* in its address space (high half, shared) but carries no `U` bit. the load faults to S-mode. the kernel counts `snitchos.user.faults_total`, parks the offending hart, and hart 0 keeps heartbeating. userspace reached for the kernel and the page table said no.
- worth being honest: this firewall isn't *new* in v0.7a — the `U` bit did this the moment the first program was mapped. what's new is the **per-process** address space underneath it. for one process the difference is invisible; it's the thing that will matter the instant there are two, each with its own low half. interface before implementation, again — the isolation is real and tested before there's a second process to isolate.

## the bug that ran the wrong kernel

- first light failed. the scenario timed out, the boot log looked normal, no panic. I burned an hour before the frame dump gave it away: the running kernel was executing the **default demo**, not my userspace workload. it was a stale binary.
- two bugs stacked. one: a non-exhaustive `match` in `#[cfg(feature = "itest-workloads")]` code — invisible to my default build, broken only in the profile the test suite actually uses. two, the killer: the harness's build step was `build_kernel(...).map(|_| ())`, which threw away cargo's exit status. a compile failure became `Ok(())`, and the runner happily booted the *previous* binary and reported a scenario timeout. my code never ran. the symptom pointed nowhere near the cause.
- the fix is three lines (`if status.success()`), but the lesson is bigger than the fix: a test harness that swallows its build's exit code doesn't test your change — it tests the last thing that compiled. and the diagnostic data was sitting in `.itest-runs/` the whole time; I'd been re-running with manual log capture instead of reading the frame histogram that says, plainly, which workload ran.

## what i learned

- **the door into a lower privilege is the door back out, run backwards.** there's no special "go to userspace" — you forge a trap-return. once that clicked, the whole entry sequence is just "set up `sstatus`/`sepc`/`satp` as if you'd trapped from the program, then return to it."
- **`sscratch` exists because of a chicken-and-egg.** you need a kernel stack pointer in a register before you can save any register, and every register holds untrusted user state. one CSR the user can't touch breaks the deadlock. mask interrupts before you arm it.
- **the embedded program is debug info wearing a 700 KB coat.** the `faulter` ELF was 700 KB; its loadable bytes were 8 KB. a `-s` link-arg strips the DWARF and the committed fixture + embedded image go back to sane. check what you're actually shipping into `include_bytes!`.
- **a harness that ignores its build's exit code is worse than no harness.** it converts "your change broke the build" into "your change mysteriously doesn't work." verify the binary has your change before you debug its behavior — `strings | grep` is faster than an hour of theories.

## what's next

- v0.7a is the *before*. a process that runs, snitches, and gets firewalled — under ambient authority, with one unguarded syscall, on purpose.
- **v0.7b: capabilities.** the syscall becomes "invoke a `TelemetrySink` you were granted." per-process capability tables, unforgeable handles, root caps to `init` only — and the kernel surface collapses to one idea: *invoke a capability.* the per-process address space, the syscall dispatch, and the ABI crate are all already in place to grow a `CapTable` onto. this is the milestone where the project's second pillar lands and the deliberate wrongness gets paid off.
