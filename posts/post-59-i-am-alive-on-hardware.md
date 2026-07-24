# Post 59 — I am alive, on hardware

- three words scrolled past on a serial console this afternoon and I sat there grinning at them like an idiot:

```
memory: 0x40000000 (4294967296 bytes)
timebase: 4000000 Hz
uart: 0x10000000
virtio-console: init failed: NotFound
I am alive
```

- `I am alive` is not new. it's a `println!` that has been firing on every QEMU boot for months; I didn't touch it for this. nothing about that line changed. **what changed is who said it** — a StarFive JH7110 on my desk, four SiFive U74 cores, RAM at `0x4000_0000`, running a kernel I linked at an address QEMU has never used. every line above it is a claim the hardware finally graded, and it graded them pass. this post is about the four things that stood between "code-complete" and those five words, and the one bug that turned out to be three bugs wearing a trench coat.

## the emulator is a comfortable liar

- I went into this with the port code-complete and **121/121 integration tests green** under snemu. that number felt like evidence. it was evidence — of everything except the things that mattered, because four of the load-bearing facts in my kernel were true only inside QEMU's imagination: there is a **virtio-console** to send telemetry to; there is an **fw_cfg** device at `0x1010_0000`; RAM starts at **`0x8000_0000`**; the boot hart is **0, or maybe 1**.

- none of those are true on a VisionFive 2. and the delicious part is that the _first_ of them announced itself politely — `virtio-console: init failed: NotFound`, exactly as designed, boot continues — while the second one hung the machine dead silent. same category of wrongness, wildly different manners.

## three failures before a single kernel instruction ran

- I hadn't appreciated that the boot handoff is a contract with **three separate parties** — the linker, the bootloader, and the firmware — and each validates a different thing. I failed all three in sequence.

- **the header was silently misaligned.** the RISC-V `Image` format is a 64-byte header whose first field, `code0`, must be a 4-byte instruction that jumps over the rest. I wrote `j real_start` and the assembler, being helpful, emitted a **2-byte compressed `c.j`** — shifting every subsequent field by two. `text_offset` still landed correctly at offset 8 (alignment padding, coincidence), so the hex dump looked _almost_ right, which is the dangerous kind of wrong. the tell was `magic2` sitting at offset **54** instead of **56**. `.option norvc` around the jump plus explicit `.4byte`/`.8byte` instead of `.word`/`.quad` fixed it. I'd been reading directive sizes as facts when they're negotiations.

- **`Image lacks image_size field, error!`** — I had set `image_size = 0`, with what felt like an airtight argument: we `tftpboot` to exactly `RAM_BASE + text_offset`, so no relocation is needed, so the size is irrelevant. every step of that is true and it did not matter even slightly. **U-Boot doesn't validate my reasoning, it validates the field.** the fix was a linker-symbol difference (`__kernel_end - _start`) resolved at link time.

- **`Failed to reserve memory for fdt`** — I'd been passing `${fdtcontroladdr}`, U-Boot's own control devicetree, on the theory that it's free and it's the board's _real_ DTB. it is both of those. it also lives in the high memory U-Boot has reserved **for itself**, so when `booti` tried to reserve that region to hand to the kernel it collided with the reservation already there. copy it down to `fdt_addr_r` (`0x4600_0000`) first and it's fine.

- three failures, zero of them in my kernel. the delivery path deserved to be its own milestone and I'd been treating it as a footnote.

## a hang and a fault are different animals

- then the kernel actually ran, printed `I am alive`, and stopped. no panic. nothing. and this is where I want to slow down, because the distinction I needed next is one I'd been using sloppily for years.

- a **fault** is the CPU refusing an instruction — a bad address, an illegal op. the hardware _diverts_: it records the cause in `scause`, the faulting PC in `sepc`, the offending address in `stval`, and jumps to your handler. a fault is **loud and located.** it hands you a report.

- a **hang** is the CPU executing perfectly happily and **going nowhere** — a spin on a flag that will never flip, a deadlock, an interrupt that re-fires forever. nothing traps. nothing is recorded. it is **silent and unlocated**, and from outside it is indistinguishable from "very slow."

- which means they want **opposite tools**. a fault you _read_. a hang you _bisect_. and the tell for which one you're holding is simply: did anything print? no panic on a kernel that has a working panic handler means nobody faulted — somebody is spinning.

- (that working panic handler was not free, incidentally. the pre-init and panic UART paths were still hardcoded to QEMU's byte-spaced register layout, so on a board whose UART spaces registers 4 bytes apart they'd have polled the wrong status register and printed nothing. I fixed that _before_ powering on specifically so a failure wouldn't be invisible. it is the single highest-leverage thing I did all day, and it paid off within the hour.)

- so: bisect. I scattered `vf2`-gated phase markers through `kmain` and reset the board. the trail walked confidently past the timer, past interrupts, past the IPI, past the frame allocator, past the heap — and stopped at **`ramfb::init`**.

- ramfb rides **fw_cfg**, which is a QEMU invention. on the board there is no such device at `0x1010_0000` — but that address _is_ inside the identity-mapped MMIO gigapage, so reading it doesn't fault. it returns open-bus garbage. and the fw_cfg DMA handshake does what handshakes do: **polls a control bit until the device clears it.** a device that does not exist never clears anything. it spun forever, silently, at full speed.

- **a dead device is worse than a missing one.** absent hardware that faults is a _gift_ — it tells you immediately and precisely. absent hardware that reads back plausible garbage inside a mapped window is a trap, and any probe that waits on it needs a bounded wait, not a `while`. my port plan had actually predicted this exact thing months ago ("guard it so the board build simply doesn't call `ramfb::init`") and I still walked into it, because a prediction in a document isn't a guard in the code.

## the trampoline is a boundary the compiler can't see

- skip ramfb, reset, and the board sailed all the way through: heap, stack window, boot-stack guard, the context-switch smoke, **all three secondary harts up**, userspace realised. and then it faulted.

```
Kernel panic: kernel page fault: scause=0xf stval=0x40506de0 sepc=0xffffffff40226eb2
```

- `scause=0xf` is a store page fault. `stval` is the address it tried to write. and `sepc` — the faulting PC — **wasn't in the panic message**, because I'd never needed it before. I added it, which took thirty seconds and turned an unlocated fault into a located one. the single most useful field, missing from the report, for no reason other than that QEMU had never made me want it.

- with `sepc` in hand, `rust-objdump` around that address:

```
jalr <mmu::unmap_identity>     ← identity mapping torn down
jalr <Once::get>               ← the very next println!'s UART lookup
ld   a0, 0x410(sp)             ← a pointer, spilled in kmain's own frame
sd   a1, 0x0(a0)               ← ***fault*** — a0 = 0x40506de0, a PHYSICAL address
```

- `stval = 0x40506de0` is the **boot stack**, 544 bytes below `__stack_top` — at its _physical_ address, not its higher-half one. so: code running at a correct higher-half PC, with a correct higher-half `sp`, storing through a pointer that is physical.

- here's the mechanism, and it's the best thing I learned today. `entry.S` enters `kmain` with `sp` pointing at the **physical** `__stack_top` — the MMU isn't on yet, there is no other choice. `kmain` allocates its frame there. _then_, partway down `kmain`, the higher-half trampoline runs and does `add sp, sp, KERNEL_OFFSET`. **`kmain`'s stack frame straddles that shift.** in a debug build the frame is enormous, so the compiler materialises and spills the addresses of `kmain`'s own locals — and every one it computed before the trampoline is **physical forever**. they keep working, invisibly, for the entire boot… because the identity mapping is still live. `unmap_identity()` is the guillotine. the very next instruction that touches one of those pointers dies.

- and here is why this is the real story: **this is the third time.** the `tp` truncation ([[project_release_build_exposes_kernel_ub]]) was an `&static` whose address the optimizer hoisted _above_ the trampoline, so it materialised physical. the `entry_pa` miscompile ([[project_entry_pa_loop_invariant_miscompile]]) was a loop-invariant address that read back as **0** on the second iteration, so hart 2 launched at PC 0. now `kmain`'s frame. three bugs that presented completely differently — a masked register, a hart that never started, a store fault after a page-table edit — and they are **one shape**: an address materialised on the wrong side of the trampoline.

- one is bad luck. two is a coincidence. **three is a design property**, and design properties get fixed structurally, not case by case. the rule the codebase has been trying to teach me: _no cached address, and no stack frame, may span the trampoline._ the fix is to split `kmain` so everything after the trampoline runs in a **fresh frame** allocated with the higher-half `sp`. for now the board simply skips `unmap_identity` — a workaround I've labelled loudly as temporary, because a workaround that hides a bug this general is worse than the bug.

- the genuinely unsettling part: **none of this is board-specific.** QEMU's codegen just happens not to spill a physical self-pointer that survives to the teardown. adding a `println!` changes whether it triggers. I have been shipping this the whole time.

## what the emulator bought, and what it couldn't

- snemu ([[project_snemu_progress]]) caught the `entry_pa` miscompile **before the board ever had power** — a real kernel bug that would have bitten identically on hardware, found host-side. and it was found by instrumenting the _emulator_ rather than the guest: printing `hart_start`'s arguments and the faulting hart's registers from snemu itself, precisely because adding guest `println!`s perturbed the timing enough to make the bug vanish. an emulator you own is a debugger with a god view.

- but snemu could not have found the fw_cfg hang, or the `kmain` frame fault, or the header misalignment, or any of the three handoff failures — because those aren't about my code being wrong, they're about my **assumptions** being wrong, and an emulator is built out of my assumptions. that's the honest division: **the emulator finds the bugs in what you wrote; the hardware bills you for the bugs in what you believed.** you need both, and the second one has a longer feedback loop and a much better memory.

## the wrong turns, for the record

- I was **certain** it was the timer. `init_timer` enables interrupts and arms an SBI timer against a 4 MHz timebase — genuinely new runtime behaviour, first IRQ into the handler on real silicon, exactly the profile of a thing that hangs. I said so confidently. the markers came back: timer fine, interrupts fine, IPI fine, frame allocator fine. the hypothesis was _reasonable_ and _wrong_, and it cost one reset because I'd instrumented broadly instead of betting narrowly. bisect first, theorise second.

- the first real board output came out as a **staircase** — every line marching further right — because the kernel emits bare `\n` and a real serial terminal wants `\r\n`. QEMU's console had been silently forgiving that forever. a purely cosmetic bug that made the actual diagnostic output nearly unreadable at exactly the moment I needed to read it.

- and one small joy: the boot hartid genuinely varies run to run. I watched it come up on **mhartid 2** one boot and **mhartid 4** the next, secondaries reshuffling to match, and the DTB-driven logical-id mapping absorbed it both times without noticing. the old `1 - hart_id` arithmetic would have computed `1 - 2` and underflowed into a garbage hart id. I'd re-scoped that multi-hart rework mid-flight when it turned out _not_ to be the first-boot blocker I'd billed it as ([[project_vf2_m1_code_complete]]) — and then it quietly saved first light anyway.

## what I learned

- **a hang and a fault want opposite tools.** a fault records where it died — read `scause`/`sepc`/`stval`. a hang records nothing — bisect it with prints. the first question isn't "what broke," it's "which of the two am I holding," and "did the panic handler say anything" answers it. corollary: **make sure the panic handler works on the target before you need it.** mine printed on the wrong register stride until I fixed it the hour before first boot.

- **a dead device is worse than a missing one.** unmapped MMIO faults and tells you. mapped-but-empty MMIO returns garbage and lets your handshake spin forever. every probe of hardware you aren't certain exists needs a bounded wait, and "the plan said to guard this" is not a guard.

- **the third instance is when a bug becomes a rule.** `tp`, `entry_pa`, `kmain`'s frame — three unrelated-looking failures, one shape. I fixed the first two individually and felt clever. the third one is the repo telling me the abstraction is wrong, not the instances. **fix the property, not the symptom.**

- **validators validate; they don't read your reasoning.** `image_size = 0` was _correct_ by every argument I could make about what the loader needed. U-Boot checked the field anyway. the same energy that says "this can't matter" is the energy that skips the field.

- **the emulator finds your code's bugs; the hardware finds your beliefs'.** 121/121 green was real and meant exactly what it said — and said nothing at all about four devices that don't exist. I'd take snemu again in a heartbeat; I'd also stop treating a green suite as a prediction about silicon.

- **three words can be a whole milestone.** `I am alive` didn't change. I didn't write a line of it this week. it just got said by something with a heatsink, and that turns out to be the entire difference.
