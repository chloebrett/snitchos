# 🅿️ Parked — Memory model & HAL (Q27, Q28)

*Discussed at a principle level and provisionally agreed, but parked for a proper pass before the relevant milestones (memory: v0.4, HAL: starts v0.1 / matures over time). Nothing here is locked.*

# Memory model (Q27)

## Settled by principle
- **Higher-half kernel, committed from v0.4.** Kernel virtual addresses in the high half of the 64-bit space, userspace in the low half. Retrofitting this is the canonical architectural corner — commit early.
- **RISC-V Sv39** paging (39-bit, 512 GB, three-level page tables) as the sane default. High half starts around `0xFFFFFFC0_00000000`. Kernel links to and runs at high addresses.

## Q27a — kernel mapped into every address space? (provisional: YES)
- **Provisional decision: kernel is mapped into every process's address space** (high half = kernel, low half = that process's userspace).
- Rationale: a trap/syscall does not require switching page tables — cheap kernel entry, which matters for a microkernel with frequent kernel transitions. Standard choice; even seL4 does this.
- Tradeoff: this is the attack surface Meltdown exploited; the mitigation (KPTI / separate page tables) is the expensive thing this approach avoids. Judged acceptable for a learning OS not built against nation-state attackers. "How a Meltdown-class bug would force KPTI" is good teaching content.
- **This is the one real judgment call in Q27 — confirm or revisit before v0.4.**

## Q27b — allocator hierarchy (provisional: standard layered answer)
Three layers, each a trait with a trivial implementation first, swappable later:

1. **Boot/static allocation** — fixed compile-time buffers (v0.1 intern table + rings already here).
2. **Physical frame allocator** (v0.4) — hands out physical page frames. Provisional v0 implementation: free-list of frames (simplest).
3. **Kernel heap** (v0.4) — built on the frame allocator; makes `Vec`/`Box` work in kernel code. Provisional v0 implementation: linked-list allocator.

Traits: `FrameAllocator`, `Heap`.

## Q27c — physmap (provisional: YES)
A "physmap" region in the kernel's high half that linearly maps all physical RAM, so the kernel converts any physical address to virtual by adding an offset. Standard, avoids fiddly temporary-mapping code.

## Q27 net (provisional)
Higher-half (v0.4), Sv39, kernel-in-every-address-space, free-list frame allocator + linked-list heap as v0 implementations behind traits, physmap region in the high half. Mostly "adopt the standard answer"; only Q27a is a genuine judgment call.

# HAL boundary (Q28)

## Principle
The HAL is an **interface** (a set of traits); the `arch-*` crates are **implementations** / backends. Everything above the HAL is architecture-neutral. Drawing the line wrong makes the aarch64 port a rewrite instead of a new backend.

## Below the HAL (arch-specific, reimplemented per arch)
- CPU register / CSR access; trap vector and trap entry/exit assembly
- Page table *format* and MMU manipulation (Sv39 vs aarch64 translation tables)
- Context-switch assembly
- Timer and interrupt controller access (RISC-V PLIC/CLINT vs aarch64 GIC)
- Boot entry, the first instructions
- Cycle counter read (`time` CSR vs aarch64 `CNTVCT`)
- Atomic / memory-barrier intrinsics if they differ
- CPU feature detection; entropy seed instruction (`seed` CSR vs `RNDR`)

## Above the HAL (portable, written once)
- Capability system, scheduler *logic*, IPC *logic* (the policy — "who to switch to"; the mechanism "switch context" is below)
- Telemetry / observability system (protocol, ring buffers, interning all portable; only the cycle-counter *read* is below)
- Frame allocator and heap *algorithms* (writing a PTE is below; free-list logic is above)
- Page table *manager* ("map this region with these perms" is portable; "encode this arch's PTE" is below)
- VFS, FS, WASM runtime, drivers-as-userspace-components

## Key trait surface (rough)
`Cpu`, `Mmu`/`PageTable`, `TrapFrame` + trap-dispatch entry, `ContextSwitch`, `InterruptController`, `Timer`, `Clock` (monotonic cycle read), `EntropySeed`.

## The single most important HAL decision
**Split page table manipulation:** the *manager* (portable — "map this virtual range to these frames with these perms") is above the HAL; only the *leaf PTE encoding* (arch-specific bit-packing) is below. The most commonly mis-drawn line — people put whole page-table logic below the HAL and duplicate it for arch #2. Sv39 and aarch64 tables are structurally similar enough that the manager genuinely can be shared.

## Honest risk
A HAL designed against a single architecture is always slightly wrong; leaks are discovered when the second arch arrives. Mitigation: (1) draw the line using the lists above, (2) keep `arch-riscv64` strictly behind the trait surface with no leaks even while it is the only arch — discipline a single-arch project would skip, (3) accept that the aarch64 milestone *will* refactor the HAL and treat that as expected, not failure. The aarch64 port's real job is to validate and correct the HAL.

## v0.1 implication
The HAL barely exists in v0.1 (needs: cycle-counter read, trap setup, serial, boot entry). Even so, put RISC-V-specific code in an `arch-riscv64` crate behind a small interface from line one rather than scattering `riscv` calls through the kernel. Cheap now; means the HAL grows as a real boundary instead of being retrofitted.

## Q28 net (provisional)
HAL is a trait surface; `arch-*` crates are backends; line drawn per the lists above; page-table manager portable, PTE encoding arch-specific; `arch-riscv64` is its own crate from v0.1; the aarch64 port is expected to refactor the HAL.

# To resolve before locking
- Q27a: confirm kernel-in-every-address-space (the Meltdown tradeoff) before v0.4.
- Q27b/c: confirm the standard answers, or revisit if a more interesting choice appeals.
- Q28: validate the HAL trait surface once there is real v0.1 code to check it against.
