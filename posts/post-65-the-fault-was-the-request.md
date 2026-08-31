# Post 65 — the fault was the request

- post 64 ended on a loose thread: typing `1.5 + 1.5` at the Stitch REPL took down the kernel. one line of arithmetic, whole machine gone.
- this post is the fix, which turned out to be five increments deep and to run through the emulator, the trap handler, the ELF loader and the scheduler. userspace floating point works now — `1.5 + 1.75` evaluates to `3.25` on the metal, on both engines.
- along the way the design doc I'd written for this was **wrong twice**, in ways that would have shipped as bugs. both were caught by measuring instead of reasoning. that's most of what's worth recording here.

## three problems wearing one costume

- the symptom was one thing but the causes were separable, and separating them was most of the design work:
  1. **Stitch's floats cannot run on target.** the language supports them fully; nothing on the metal had ever executed one, because every fixture is integer-only.
  2. **a user program can panic the kernel with one illegal instruction.** `sstatus.FS` is never set, so any FP instruction traps as illegal — and the trap dispatcher's catch-all `panic!` fired regardless of privilege.
  3. **snemu hid it.** under the emulator the guest just stopped emitting frames; QEMU printed the panic.
- (2) is the serious one and has nothing to do with floating point. any user program could halt the machine with any instruction the kernel didn't have a handler for.

### the doc said "as other user faults do". other user faults did not.

- the plan was "an unhandled user trap should kill the process, as other user faults do". I went to copy what the page-fault path did and found it was `loop { wfi }` — park the hart forever — under a comment saying v0.7a has no process teardown.
- teardown had existed for milestones by then. and parking is worse than it sounds: a fault on hart 0 stops the heartbeat, so **the isolation firewall working looked exactly like the kernel dying**.
- so the fix wasn't "route the new case into the existing kill path", it was "make both paths kill". the disposition is now a pure function: an *exception* with no handler from U-mode kills the process — blanket over every cause code, so a code we've never seen can't panic on a user program's behalf — the same from S-mode panics, and an unknown *interrupt* panics either way, because an interrupt isn't attributable to the task it merely interrupted.

### problem (3) was mostly not real

- before building anything for it I reverted the fix and re-ran the new scenario. snemu printed the panic perfectly well: the harness dumps the guest UART tail when a scenario fails.
- the original blindness was that `stitch-kvetch-completes` was **unregistered**. nothing failed, so nothing dumped the tail, so the guest just looked wedged. an unregistered-scenario blind spot, not a fidelity gap. deleted that increment before writing it.

## snemu had no idea what "illegal" meant

- to test the kernel's new behaviour the emulator had to *produce* an illegal-instruction trap, and it couldn't: there was no `scause = 2` anywhere in it. an unknown opcode became `StepError::Unimplemented { pc, instr }`, which halts the run host-side.
- which is **correct**, for what it was built for. the rule I'd written down: *a gap in snemu must never become guest-visible behaviour.* a gap surfacing as a guest trap is indistinguishable from real hardware refusing the instruction, so it sends you debugging the kernel. that's the hour I lost in post 64. a host-side halt naming the gap costs thirty seconds.
- but the *inverse* also has to hold. an instruction that is genuinely illegal for the guest must trap the guest, or a correct fault handler is untestable. one match-arm fallthrough was carrying both meanings.
- so: `is_guest_illegal`, scoped to the encodings the spec *guarantees* illegal forever — the all-zero and all-ones instruction words. no judgement about snemu's coverage is involved, and the set can't rot as snemu grows.

### the test found all-zeros executing

- writing that test red first paid immediately. all-zeros wasn't halting the run — it was **executing**. it reached the compressed-instruction path, `expand` cheerfully decoded it as `c.addi4spn` with a reserved zero immediate, and the pc advanced by 2.
- precisely the failure the rule exists to prevent, sitting in the emulator the whole time.
- the JIT needed the same treatment: a block containing an illegal word must end *before* it. that one gets an internal JIT-on/JIT-off A/B rather than a `snemu diff`, because a mis-compiled block is the one failure the differential oracle structurally cannot see — both sides would be running snemu's own code and QEMU never gets a look in.

## the FP unit, and what the doc got wrong

- then the actual work: RV64FD in the emulator. register file, `fcsr`, loads/stores, arithmetic, compares, conversions, FMA, the compressed forms.
- most of it is *not* interesting, and that's the point: ordinary arithmetic is left to Rust's `f32`/`f64`, because the reference is IEEE-754 and so is the host. there is nothing to model. what needs modelling is the handful of places RISC-V and the host **deliberately differ**, and those live in one pure module with a test each.

### what the compiler actually emits

- the doc said: implement round-to-nearest-even, and **host-panic on any other rounding mode** — "which covers everything a Rust compiler emits (it emits DYN, and `frm` resets to 0)".
- that claim is load-bearing and it's false. before implementing I compiled a five-line probe:

```
$ rustc --target riscv64gc-unknown-none-elf --emit asm -O probe.rs
    fcvt.w.d  a0, fa0, rtz
    feq.d     a1, fa0, fa0
```

- **every** float→int cast emits `rtz` — Rust's cast semantics are truncation. shipping the rule as written would have halted the emulator at the first `as i32` in real guest code.
- the fix is to split the gate by *who does the rounding*: arithmetic is evaluated by the host FPU, so it's nearest-even or refuse; conversions are rounded by snemu itself, so they honour `rtz` too. the refusal still names the effective mode, resolved through `frm`.
- the same four lines of output paid twice. that `feq.d fa0, fa0` is LLVM testing for NaN around every cast — it only needs to, because RISC-V converts NaN to the **maximum positive integer** where Rust's `as` gives 0. direct evidence for a divergence I'd otherwise have had to take on trust.
- generalised and filed: a claim about what a compiler emits is an empirical claim, and the compiler is right there.

### the rest of the divergence list

- each of these is a test, and each is a thing that would have been silently wrong:
  - **`funct3` is an op selector, not a rounding mode** for sign-injection, min/max, compares and classify. check rounding across all of OP-FP and you refuse `fsgnjn` — selector 1, which reads as `rtz` — and `-x` becomes an emulator halt. the failure mode is a crash on *working* code.
  - **canonical NaN** on any generated NaN, but *not* on `fsgnj`/`fmv`, which are bit moves and must preserve a payload.
  - **`fmin`/`fmax`** skip a lone NaN rather than propagating it, and order `−0.0 < +0.0` — which compare *equal*, so `if a < b { a } else { b }` returns whichever operand came second and is wrong half the time.
  - **RV64 sign-extends every 32-bit FP→int result**, `.wu` included, so `fcvt.wu.d` of `u32::MAX` reads back as `-1`. `i64::from(u32)` zero-extends. the one bug the tests caught in that slice.
- the fused multiply-add test is the one I'm happiest with. an unfused `a * b + c` is wrong by one ulp on inputs where the product isn't representable, which is invisible on friendly numbers. so: `a = b = 2²⁷ + 1` are exact, their product needs 55 significand bits, and subtracting `2⁵⁴` exposes exactly the bit that double-rounding loses. fused gives `2²⁸ + 1`, unfused `2²⁸`. the test asserts both the right answer *and* that it differs from the naive expression.

### a test that had to be deleted

- one test's job was "a legal instruction snemu doesn't model halts the host", and its witness was `fadd.d`. then I implemented `fadd.d`. moved it to `fmadd.d`; then implemented that too.
- when RV64FD was complete the test had no possible witness left. so I deleted it — rather than weakening it into something vacuous — after checking both behaviours it covered still had homes: the general rule via a sibling test whose witness is `mret` (not FP, so FP work can't invalidate it), and "the FS gate doesn't fire when FS is *on*" via every FP test in the file, all of which run with FP enabled and would fail against a gate that fired regardless.
- I'd left a note in the test's doc comment months-of-context earlier — well, hours — saying exactly this should happen. nice to be able to follow my own instruction.

## the fault was the request

- now the feature. and the mechanism is my favourite part of the whole design, which I can't take credit for because it's classic lazy-FP context switching — it just happens to double as an authority check here.
- `sstatus.FS` starts `Off`, so a program's first FP instruction traps as illegal. **that trap is the request.** the handler asks: is this process authorised? if so, turn FP on in the *saved* `sstatus`, don't advance `sepc`, and return — the instruction re-executes, now legally. if not, kill it.
- no syscall. nothing declared. a program that never executes an FP instruction never traps, so never pays. and the trap that used to panic the kernel is now the decision point — the robustness fix and the feature are the same line of code.
- one ordering detail that matters more than it looks: check "is FP off" *before* "is it authorised". the other way round, an authorised process gets FP re-enabled on every genuinely-illegal instruction and retries it forever — a livelock instead of a dead process.

### the authority is real, the gate is vacuous

- authority comes from the ELF: RISC-V binaries carry their float ABI in `e_flags`, so the loader can *read* whether a program uses FP. unforgeable by accident, and unusually for an authority claim, mechanically checkable rather than taken on faith.
- then I checked what our binaries actually say:

```
$ xxd -s 48 -l 4 …/illegal
00000030: 0500 0000
```

- `0x5` = `RVC | FLOAT_ABI_DOUBLE`. `riscv64gc-unknown-none-elf` is a hard-float target, so **every** binary in the tree claims hard float — including `init` and `hello`, neither of which contains a float.
- so as a gate it refuses nothing. the doc's argument for opt-in was cost attribution — "it is wrong to tax integer-only programs for a feature they never use" — and that benefit is delivered entirely by the **laziness**, not by the flag. the flag only starts refusing anything if some program is deliberately built soft-float, which is the alternative the doc had already rejected.
- I implemented it anyway, because it's the specified design and it makes the grant *observable* — the snitch says which process and on what authority, the same way a capability grant does. but the "unauthorised" path is test-covered rather than demonstrated, and the plan now says so.

### one FP process at a time, loudly

- the honest gap: the kernel can't yet save 32 FP registers across a context switch, so a second FP process would silently share the first's register file.
- so a second request is **refused and snitched** rather than granted. one FP process at a time, observable, until FP context switching lands. silent cross-process corruption is strictly worse than a loud refusal, and refusals snitching is the trade this codebase makes everywhere else.

## the tripwire

- when increment 3 was done but the kernel side wasn't, floats still didn't work — but the *blast radius* had shrunk to one process. I wrote a scenario asserting exactly that honest intermediate state, and wrote into its doc comment that it was a **tripwire**: when lazy enable landed, `1.5 + 1.5` should print `3` and this scenario must be rewritten. if it still passed unchanged, the enable path wasn't reaching the REPL.
- first run after increment 4:

```
stitch> 1.5 + 1.5
=> 3
0/1 scenarios pass
```

- the failure is the evidence. the rewritten version asserts the enable metric, the authority log, the arithmetic (`1.5 + 1.75` → `3.25`, chosen because `3.25` appears nowhere in the boot self-test, whereas `=> 3` does), and a surviving heartbeat. green on both engines.
- which also, finally, gives FP a differential oracle. the design leaned on `snemu diff` making incremental FP safe, but until a *user* program could execute an FP instruction successfully, neither side executed one and there was nothing to diff. now there is.

## and the thing I started with, six increments ago

- `stitch-kvetch-completes` — the tab-completion scenario from post 64, unregistered because it wedged the guest — no longer wedges. no fault, no panic, runs straight through.
- and fails. its own bisect diagnostic reports *"the REPL never reached the call"*: the completion request is never sent, and the console shows the grammar-only menu.
- so there's a second, independent gap in the completion path, and FP was necessary but not sufficient. the comment above that scenario now records what's actually left, which is better than what it said this morning.

> **Correction (2026-07-28).** the paragraph above is wrong, and wrong in a way worth leaving visible. there was no second gap. re-run with the bisect scaffolding removed, the REPL *does* reach the call and the server *does* answer — and then the **second** Tab kills it on `RefuseBusy`, the one-FP-process-at-a-time guard described three sections up. FP was necessary and also sufficient; it just needed increment **4b** rather than 4.
>
> what misled me: I read the scenario's own leftover `return Err("DIAGNOSTIC: …")` instead of the frame stream, and I read the console *after* the wedge, where the last completed line is all there is — so a completion that had actually been inserted at the prompt looked like a grammar-only fallback. **the diagnostic I trusted was a hand-rolled string from someone else's abandoned bisect; the telemetry was right there and I didn't look at it.**
>
> the sequel is [post 72](post-72-fp-context-switching.md), which is largely about that guard finding its customer.
- five increments to fix a bug I found by typing four characters at a prompt. two of them were fixing the design document.
