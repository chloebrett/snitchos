# snemu 1 — let the kernel tell you what to build

- another side project that got out of hand. SnitchOS already runs in QEMU; the temptation was to ask what we'd *gain* by writing our own RISC-V emulator. three real answers — and one method that made the whole thing tractable. this post is the arc from an empty `Cpu` struct to **SnitchOS booting on snemu, through its own MMU, running at higher-half** — all test-driven, and almost all of it dictated by the kernel itself. it's called **snemu**: the SnitchOS emulator.

## why our own emulator

- QEMU is excellent and we're not replacing it for real work. but three things make an owned emulator worth it: **startup** (QEMU's ~1s of process + firmware + DTB-gen dwarfs a short itest; snemu boots in milliseconds), **telemetry as a first-class concern** (snemu can see what the kernel can't see about *itself* — every MMIO access, instruction counts, page faults), and **I/O ownership** (we support whatever we like, however we like). and underneath all three: it's the most direct way to understand the hardware contract our kernel leans on, by being forced to *implement the other half of it*.
- the scope looks terrifying until you remember **we control the toolchain**. every guest instruction comes out of `rustc` targeting `riscv64gc`. so the envelope isn't "all of RISC-V" — it's a finite, enumerable set: complete RV64GC *user* instructions, plus only the *system* slice our kernel actually uses. whole extension families (vector, hypervisor, crypto) never appear. that one observation turns "impossible" into "a long weekend of mechanical decode plus a few genuinely hard subsystems."

## the method: run until it breaks

- the single best decision was the **unimplemented-instruction meta-loop**. the decoder's fallback arm doesn't guess — it halts and prints `pc` + the raw instruction word. so the workflow is: point snemu at the real kernel, see exactly what it hits, implement *that*, repeat. the kernel is the spec.
- this is implement-on-demand taken seriously, and it has a property I underrated: **nothing speculative ever ships.** i never implemented an instruction the kernel didn't use, so i never wrote a subtly-wrong handler that lies dormant for weeks and then detonates. every single instruction was exercised by real code the moment it landed. (the cost — that the kernel's path isn't *exhaustive* per-instruction — is real, and it's what a later riscv-tests pass is for. but the 80% is free.)
- the whole thing is TDD on top of that: each instruction gets a unit test built from its **real kernel encoding** with a hand-verified expected value, then the impl, then we run the kernel to the next stop. red, green, advance.

## the journey, in step counts

- the boot reads like a thriller told in instruction counts:
- **step 9** — the first thing the kernel does that snemu didn't know: a compressed jump (`c.j`). RISC-V's "C" extension is mandatory and its immediates are notoriously scrambled, so this was the start of a long, satisfying grind through ~29 compressed forms, each surfaced by the kernel and each tested against the exact bytes it emitted.
- **~136,000 steps** — the kernel runs *that far* of physical-PC boot (entry stub, BSS zero, frame allocator, building the page tables) and then falls off the **MMU cliff**: it reaches `mmu::enable`, turns on paging, and trampolines to a higher-half virtual address. snemu had no MMU yet, so the fetch went off the end of RAM. exactly the predicted milestone-1 boundary.
- to *prove console-out* before the cliff, the kernel grew a tiny `minimal-boot` profile — greet the UART raw and halt, before paging — and snemu printed **`Hello from snemu (minimal-boot)`**. the first words our OS ever said on our own silicon-that-isn't.

## the parts that were actually tricky

- **the null device tree sent the kernel feral.** with paging wired up, the full kernel *still* jumped to a wild higher-half address with `satp` still zero — before `mmu::enable` could possibly have run. the tell: `satp == 0` at the fault. the cause: snemu handed the kernel `a1 = 0` (no device tree), and the kernel's first act is to *parse* the DTB. parsing garbage, the fdt reader computed a bogus pointer and leapt into nowhere. the fix was to be what firmware is — dump QEMU's own `virt.dtb`, embed it, load it into RAM, and point `a1` at it. the wild jump vanished instantly.
- **the entry point is a lie you have to translate.** the kernel is *linked* at higher-half VAs but *boots* at physical PC. so the ELF's entry is `0xffffffff80200000`, but execution must start at `0x80200000`. the loader translates the entry through its segment's vaddr→paddr mapping — handling both flat and higher-half kernels without special-casing either.
- **Sv39 came alive quietly, which is how you want it.** once the DTB was real, the kernel sailed through `mmu::enable`, `satp` flipped to Sv39 mode, and snemu's page-table walker started translating every fetch and load — **147,000 instructions with paging on and not one translation fault.** the walk was right against real kernel page tables. the only thing it then tripped on was `sfence.vma` (a TLB flush — a no-op when you have no TLB) and, right after, the atomics.
- **the tests caught *me*, twice.** building each test from the kernel's real instruction bytes meant the expected value was *my* hand-decode — and twice (a `c.beqz` offset, a `c.lw` register) my arithmetic was wrong while the implementation was right. the kernel advancing past the instruction is what flagged it: the impl agreed with reality, my test didn't. exactly the independence you want — a test whose expected value you derived the *other* way.

## what i learned

- **let the target drive.** implementing an ISA blind is a recipe for dormant bugs and wasted effort. running the real kernel and implementing only what it demands is faster, safer, and the bug-finding falls out for free — every instruction is born exercised.
- **be what the layer below you is.** half a day of "why is it jumping to nowhere" evaporated the moment i realized snemu wasn't providing the device tree that firmware always does. an emulator's job isn't only the CPU — it's the *whole* contract the software was written against. the null DTB was an absence, and absence is the hardest bug to see.
- **a good oracle isn't the same as an exhaustive one.** the kernel boot validates real semantics across a huge path, but only for the bit-patterns it happens to use. that's most of the value and none of the false confidence — as long as you *say* what's still unchecked (the residual immediate bits, waiting on riscv-tests).
- **determinism is a feature you protect.** snemu has no hidden state — no JIT cache, no real-time coupling — so a run is perfectly reproducible. that made every "wait, why did the step count change?" a real signal (the kernel binary had changed under me), not noise.

## what's next

- the **A extension** — atomics and `lr`/`sc`. the kernel's sync primitives are everywhere, and on a single hart they're delightfully easy: an atomic op is just load-modify-store with the old value returned, and `sc` always succeeds because nobody else is running. that's the immediate next stop.
- then **virtio-console + CLINT**, which is where snemu stops being a curiosity and becomes *useful*: the integration suite already asserts on decoded telemetry frames, so the moment snemu can emit them, `xtask itest --snemu` runs the entire existing suite against snemu and diffs it against QEMU — a differential oracle we get for free, no new tests, no toolchain.
- and eventually the parts that were the whole point: telemetry baked into the emulator, snapshot/rewind that falls out of having no hidden state, and — because it's a pure interpreter — snemu running *inside* SnitchOS, booting a guest SnitchOS, narrating both into the same Grafana. but an emulator earns those by first booting the real thing. it boots the real thing now.
