# Post 9 — Moving the kernel without breaking it

- v0.4 starts with the memory milestone: page tables, higher-half, frame allocator, kernel heap. This post is just the first part — moving the kernel to higher-half virtual addresses without losing the ability to boot. Frame allocator and heap are still ahead.
- went sideways twice. ended up writing a findings doc, throwing away an attempt, and landing the same goal in three small checkpoints. the dev arc is the post.

## what higher-half is and why bother

- conventional kernel layout: kernel maps itself at high virtual addresses (Sv39: `0xffffffff80000000+`), userspace gets the low half. every user process sees the kernel at the same VAs.
- payoff is in v0.6 (userspace). every process's address space contains the kernel mapping at the same VAs, so the trap path uses the same code addresses regardless of which process was running. without this, every context switch would need to bounce through a per-process trampoline.
- doing it now is **preparation**. v0.4 has no userspace yet. but the moment v0.6 lands, the kernel must already be at higher-half — retroactively moving an entire running kernel mid-milestone is much worse than doing it once when there's nothing else moving.
- also gets us: null deref faults (no mapping at VA 0), Linux-style layout for transferability of mental models, and matches what the page-fault diagnostics literature assumes.

## the original step list, before reality

| # | step | what i thought |
|---|---|---|
| 1 | satp on, identity-only | small |
| 2 | higher-half move + trampoline + identity unmap | medium |
| 3 | frame allocator | medium |
| 4 | kernel heap | medium |
| 5 | allocator telemetry | small |

step 2 was supposed to be one chunk. it became four sessions and a findings doc.

## the first attempt that didn't work

- changed the linker `ORIGIN` to higher-half. kernel image's link addresses now `0xffffffff80200000+`. LMA stayed at `0x80200000` (where OpenSBI loads us).
- kernel didn't boot. no output. no panic.
- spent hours adding markers, removing markers, second-guessing whether `auipc` does what i think. found out things were silently breaking everywhere — the `dtb::timebase_hz` chain crashed pre-MMU, the `dtb.all_nodes()` iteration crashed pre-MMU, `span!()` crashed pre-MMU, formatted `println!` crashed pre-MMU.
- root cause finally clicked: **anywhere the compiler stores an absolute symbol address as a value**, the linker fills it in at the higher-half VA. with the MMU off, dereferencing those goes nowhere. examples:
  - vtables for `dyn Trait` (function pointers)
  - `fmt::Arguments::args` slice — every formatted `println!("foo {}", x)` builds an `Arguments` struct on the stack that contains a fn pointer to `<T as Display>::fmt`. you can monomorphize dyn dispatch away. you cannot eliminate `fmt::Arguments`.
  - `trap_entry as *const () as usize` — the trap vector address
  - `fn` items stored anywhere
- and the kicker: PC-relative addressing (`auipc + addi` under code-model `medium`) DOES give physical addresses at runtime when the code is loaded at physical, *because runtime PC is physical and the offset to the symbol is the same in either address space*. so `&static as usize` works fine. but `fn_name as *const ()` is a different code path — it's stored as an absolute relocation, baked at link time, points at higher-half.
- the user (me) asked: **"is this actually progress?"** — fair question. lost two test scenarios. gained a half-built infrastructure that didn't actually change runtime behavior.

## the findings doc

- went `git stash`. wrote `plans/v0.4-memory-findings.md` enumerating what i'd learned and what i'd burned time on.
- key things in there that paid off later:
  - **don't use bare-MMIO UART pokes for debug**. they skip the LSR-ready check and drop chars at speed, so hex dumps lie. use the `Uart16550` driver.
  - **match the test harness's QEMU invocation** when running manually. without `-chardev/-device virtconsole`, `virtio_console::init` returns `NotFound` and looks exactly like a paging regression.
  - **anything that walks the DTB pre-MMU crashes** under higher-half link. we never isolated why. mitigation is just: don't walk the DTB pre-MMU.
  - **`span!()` pre-MMU is unreliable**. monomorphizing FrameSink helped but didn't fix it.
  - **the linker change cascades**. it's never "just the linker."
- the doc is the most valuable artifact of the failed attempt. when i came back to it, i didn't repeat the things in there.

## the second attempt: three small checkpoints

### checkpoint 1 — `va_to_pa` at every device-DMA boundary

- the realization that unlocked the whole thing: **devices don't have an MMU**. when `virtio_console::transmit` writes a buffer pointer to the virtio queue's descriptor, the virtio device reads that value as a *physical* address. it has no idea what `satp` is set to.
- with the kernel running at identity PC: `bytes.as_ptr() as u64` gives a physical stack address. device reads from there. works.
- with the kernel running at higher-half PC (after the trampoline): `bytes.as_ptr() as u64` gives a higher-half VA. device tries to read from there. physical address `0xffffffff80...` doesn't exist. silent failure.
- four sites in `virtio_console.rs` pass an address to the device: three in `setup_queue` (the desc/avail/used ring addresses) and one in `transmit` (the buffer ptr).
- added a single helper:

```rust
pub const fn va_to_pa(va: usize) -> usize {
    if va >= KERNEL_OFFSET { va - KERNEL_OFFSET } else { va }
}
```

- the key property: **no-op when `va` is already physical**. lets us land it *before* the trampoline. tests stay green. by the time the kernel actually moves to higher-half, the translation is already in place at every dma site.

### checkpoint 2 — the trampoline

- six lines of inline asm in `kmain`:

```rust
asm!(
    "lla  t0, 1f",         // t0 = identity-PC VA of label 1
    "add  t0, t0, {off}",  // t0 = higher-half VA of label 1
    "add  sp, sp, {off}",  // sp = higher-half VA of stack top
    "jr   t0",
    "1:",
    off = in(reg) mmu::KERNEL_OFFSET,
    out("t0") _,
    options(nostack),
);
```

- the subtlety: this **must be inline**. wrapping it in `fn jump_to_higher_half()` doesn't work because the `ret` at the end of the function jumps back to the caller's `ra`, which was set when the call was made — at identity PC. the entire point is to leave identity-PC space, and `ret` undoes it.
- `lla` is "load local address" — pc-relative. at runtime `t0 = current_pc + (link_va(1) - link_va(lla))`. that offset is small (within the kernel image). result: t0 = identity VA of `1:`. add KERNEL_OFFSET, get higher-half VA. jr to it. now PC is higher-half. sp gets the same treatment.
- after the trampoline returns control to subsequent rust code, every `auipc`-derived address resolves to higher-half (because runtime PC matches link-time PC now). `&static as usize` gives higher-half VAs. and that's *exactly why checkpoint 1 had to come first* — the moment the kernel hits any `&queue_field as u64` and hands it to the device, it'd be over.

### checkpoint 3 — identity unmap (partial)

- walked the boot table's root, cleared entry 2 (the gigapage containing the kernel image at identity), `sfence.vma`.
- now: any access to a kernel-image identity VA faults. the kernel image, its stack, the DTB region — all only reachable via higher-half VAs.
- kept identity MMIO. `CONSOLE` and `UART` still store physical addresses; the panic handler's emergency UART poke is hardcoded at physical `0x10000000`. removing identity MMIO needs higher-half MMIO mappings + patched statics + patched panic handler — a real chunk that's its own checkpoint.

## the test arc

| state | kernel-core | integration |
|---|---|---|
| before v0.4 | 37 | 4 |
| step 2a (dual-map only) | 38 | 5 |
| step 2b (verify scenario) | 38 | 5 |
| first 2c/2d attempt | (mid-flight) | regressed to 3 |
| this session, checkpoint 1 | 39 | 4 |
| checkpoint 2 (trampoline) | 39 | 3 |
| checkpoint 3 (identity unmap) | 40 | 3 |

- the integration count went *down* because two scenarios that depended on specific span machinery became invalid as the kernel actually transitioned:
  - `mmu-enabled` asserted a span wrapping the satp write. that span uses `register_or_lookup` → emit a `StringRegister` frame. emitting pre-MMU under higher-half link consistently crashes. so the span had to go.
  - `mmu-higher-half-verify` read the first byte of `__kernel_start` through both its identity VA and `identity + KERNEL_OFFSET`. once PC is at higher-half, `&__kernel_start` resolves to the *higher-half* VA, and adding KERNEL_OFFSET wraps into non-canonical territory. the function's semantics break.
- net test count is lower than at step 2b, but the kernel does something it didn't before. existing scenarios all pass *implicitly proving* the trampoline + DMA fix-ups work — frames wouldn't arrive otherwise.

## what i learned

- **PC-relative addressing is sneakier than i remembered.** `auipc` + offset gives `current_pc + (link_target - link_pc)`. when current_pc differs from link_pc by a constant, the result also shifts by that constant. with code-model `medium`, `&static` always gives "the address of static at the current load point." this works in your favor for statics, against you for absolute fn pointers stored in vtables / `fmt::Arguments`.
- **`fmt::Arguments` is the silent killer.** every formatted println, every `eprintln!`, every `panic!("{}", x)` — all of these build an `Arguments` struct with fn pointers to type-specific formatters. monomorphization can't reach them. they're a hard floor on "what can run pre-MMU under higher-half link."
- **no-op-on-identity translation helpers** are wildly useful. `va_to_pa` doing nothing when the input is already physical means it can land before the trampoline. there's no atomic moment to coordinate.
- **trampolines must not be functions.** the failure mode is silent if you forget — the function returns happily, but to identity-PC code, and then the next anything-using-an-absolute-fn-pointer faults at a totally unrelated line.
- **don't combine the linker change, the trampoline, and the dma fix-ups in one shot.** they have to land in the order: dma fix-ups (no-op) → trampoline. doing it together looks like "everything's broken at once"; doing it in order means each step is independently verifiable.
- **a findings doc after a failed attempt is the single most leveraged thing you can write.** the second attempt didn't re-encounter any of the things in there. the third attempt (the unmap) was three small commits with no dead ends.

## what's parked

- identity MMIO mapping is still live. `CONSOLE`, `UART`, panic-handler UART poke all use physical MMIO addresses identity-mapped at root entry 0. fully removing identity requires:
  - add higher-half mappings for MMIO regions (new mid table)
  - patch `console::init` / `virtio_console::init` to store `pa + KERNEL_OFFSET`
  - patch the panic handler's hardcoded `0x10000000`
- frame allocator and kernel heap (the rest of v0.4) are orthogonal to this. they hand out physical pages either way.
- DTB-iteration-crashes-pre-MMU is not isolated. we side-stepped it by hardcoding the MMIO region and walking the DTB only post-MMU. probably worth understanding eventually.

## v0.4-step-2 status

| ✓ | thing |
|---|---|
| ✓ | kernel linked at higher-half VAs (`ORIGIN = 0xffffffff80200000`, `AT(0x80200000)`) |
| ✓ | `code-model=medium` for PC-relative symbol addressing |
| ✓ | `FrameSink` monomorphized (no vtable on the emission path) |
| ✓ | `mmu::enable` runs early in `kmain`, before any formatted `println!` |
| ✓ | dual-mapped boot table (kernel image at both identity + higher-half) |
| ✓ | `va_to_pa` at every device-DMA boundary |
| ✓ | trampoline + sp fix-up to higher-half PC |
| ✓ | identity-kernel gigapage unmapped |
| — | identity-MMIO gigapage unmap (parked) |

## what's next

- **step 3**: physical frame allocator. bitmap-based, DTB-driven memory map. orthogonal to higher-half PC.
- maybe revisit identity-MMIO removal once we've gotten v0.4 step 5 (allocator telemetry) done and have a flatter view of what cleanup is worth doing.
- the **fmt::Arguments issue** keeps coming up. there might be a kernel-side wrapper that pre-resolves formatters to a known address space, but i don't want to think about it until something forces the issue.
