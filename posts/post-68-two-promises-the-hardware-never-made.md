# Post 68 — two promises the hardware never made

- the milestone was plain enough to state in a sentence: get the `Frame` telemetry stream *off the VisionFive 2* over a physical serial line, without coupling kernel timing to the wire — and do it so the UART becomes one of four interchangeable sources (in-tab wasm, host, board, replay) behind a single interface, not a board special case. B3. the boring, load-bearing plumbing that turns "the board boots" into "I can watch the board think."

- the plumbing mostly went the way plumbing goes. two things did not, and they're the post, because they're the *same bug wearing two costumes.* both were a **fact about the machine that I asserted in code and never checked** — one told to the compiler, one told to myself in a comment. both were true under the conditions I happened to test and false under the ones I shipped. both stayed invisible until one precise trigger called the bluff. a register allocator, and a single device write.

## the register I swore wouldn't move

- the first symptom was a hang during board bring-up, right after I added a boot banner. the honest first instinct — "the banner did it" — is technically true and completely misleading, and untangling *why* is the whole lesson.

- the RISC-V SBI calling convention returns a two-field struct — `sbiret { error, value }` — in registers **a0 and a1.** my ecall wrappers were hand-written `asm!`, and they declared the argument registers with `in("a1")`. `in` is not a description. it's a **promise to the compiler**: "this register holds this value going in, and it survives the instruction unchanged." for a plain instruction that's fine. for an `ecall` into firmware that *writes a1 on the way out*, it's a lie — and the compiler believes it completely.

- believing it, the optimizer did the reasonable thing: a1 is (it was told) preserved across the call, so it's a fine place to park a live value. release codegen stashed the per-hart data pointer — `PER_HART_DATA` — in a1 across the ecall. SBI returned, clobbered a1 with its status word, and the next code to dereference "the per-hart pointer" was reading a firmware return code as an address. it computed a field at **offset 0x40** off that garbage and faulted. ([[project_sbi_ecall_a1_clobber]].)

- three things conspired to keep this hidden for so long, and each is worth naming:

  - **debug builds spill everything.** with no optimization, a1 wasn't carrying a live value across the call — the pointer lived on the stack and got reloaded after. the clobber landed on a register nobody was counting on. the bug existed the whole time; debug codegen just never took the bait.
  - **the banner didn't cause it — it moved the furniture.** adding a print changed register pressure enough that the allocator chose a1 to hold something live across an ecall it hadn't before. same family as [[project_kmain_frame_straddles_trampoline]] and the `tp`-truncation before it: the defect is latent in the source, and an unrelated edit shifts codegen until the latent thing becomes reachable. "the banner did it" is exactly as true as "the last domino did it."
  - **the board was running a debug image anyway.** so even once I suspected release-specific codegen, the hardware in front of me was the one build that couldn't show it — the [[feedback_stale_board_image]] tax, paid again. the repro that finally pinned it was `snemu boot --release`, in the emulator, deterministically.

- the fix is a one-word-per-register correction: `in("a1")` → `inlateout`/`lateout`, which tells the compiler the truth — a0 and a1 are *written* by the call, don't keep anything live in them. but a one-site fix for a class of bug is a trap. so I audited every hand-written ecall in the tree and found **several more** mis-specified the same way, across the SBI wrappers and the userspace syscall stubs. each one a latent release-only fault waiting for the allocator to notice the free register.

- then the actual repair: a single reviewed `ecall()` wrapper, with the clobber list written **once**, correctly, and every call site routed through it. the hand-written `asm!` with its per-site register bookkeeping is gone. this is the [[feedback_estimation_calibration]]-shaped move I keep relearning — you don't fix the five instances, you delete the *category*. a wrong `in()` can no longer be typed, because nobody types the `in()` anymore.

## teaching the emulator to have an interrupt controller

- the transport I wanted wasn't polling. polling the UART's transmit-ready bit ties the kernel's throughput to a busy-wait; the whole *point* of B3 is that kernel timing must not depend on baud. the right shape is interrupt-driven: the UART raises a line when its transmit FIFO has room, the PLIC routes that to the hart, the trap handler drains the ring at wire speed, and the kernel gets on with its life. THRE-interrupt drain, full PLIC path.

- which ran straight into a wall I've learned to distrust: "snemu can't test this." the emulator that runs the whole itest suite deterministically had no model of a PLIC — writes to its MMIO window faulted as out-of-range. the fallback is the QEMU engine, which *does* model one, but QEMU itests are the slow, occasionally-flaky escape hatch. an entire new kernel subsystem, testable only in the fidelity-fallback, felt wrong.

- so I asked the question the claim was discouraging: *why can't it?* and the answer was "because nobody's written it yet," which is a to-do, not a law. I taught snemu to model a PLIC. three decisions carried it:

  - **SEIP is derived, not stored.** the supervisor-external-interrupt-pending bit isn't a register the model holds and mutates; it's *computed* — is any enabled source pending above its context threshold? — exactly the way snemu already derives the timer interrupt from `cycle >= stimecmp`. modeling the pending state as a function of the source state, rather than a cached flag, means there's no coherence bug to have.
  - **`in_progress` is an `AtomicU64`.** the claim register is a read that also mutates ("claim" hands you the top source *and* marks it in-flight), but snemu's MMIO read path is `&self`. rather than widen every read to `&mut`, the in-flight set lives behind an atomic — which also preserves the `Machine: Sync` bound that the snapshot-sharing itest tree depends on. the shape of the emulator's other features constrained this one, and the constraint pointed at the right design.
  - **the gateway is level-triggered.** a source line stays asserted until the *device* deasserts it, not until the CPU acknowledges — so the UART's line is re-synced after every write to the UART model. get this wrong and you either lose interrupts or storm them.

- the payoff is disproportionate to the effort. snemu now models an interrupt controller, which means the **entire** PLIC → `SEIE` → THRE → drain path is asserted in the deterministic, one-run gate — a scenario (`tx-irq-delivers`) that boots the kernel, lets the interrupt fire for real, and watches the byte come out the far end of the wire. not "probably works on hardware." *proven, every gate run, in under four seconds.* the emulator earning its keep again ([[project_snemu_progress]]).

## the mapping I said was already there

- with the PLIC driver written and snemu ready to test it, I ran the gate. **every one of the 124 scenarios failed** — all at the same boot checkpoint.

- that shape is itself the diagnosis, and reading it first saved an hour. 124 scenarios don't independently break the same way; they all boot the *same kernel image*, so a uniform failure at the boot checkpoint is **one boot-level regression**, not a suite of scenario bugs. don't debug the scenarios. debug the boot.

- a direct `snemu boot` printed it plainly: `kernel page fault: scause=0xf stval=0xffffffff0c000028`. scause 0xf is a store page fault. and the address decodes: `0x0c000000` is the PLIC base; `+ 0x28` is the priority register for source 10, the UART; the `0xffffffff00000000` on top is `KERNEL_OFFSET`. this is `plic::init()` writing the UART's interrupt priority — at a higher-half virtual address that *isn't mapped.*

- the maddening part: the driver computes that address the **exact same way** the working UART driver does — `base + KERNEL_OFFSET` — and the UART, forty MMIO-megabytes away at `0x10000000`, works fine. so why does one higher-half MMIO address resolve and its neighbor fault?

- because the higher-half MMIO mapping is **not a blanket gigapage**, and I had convinced myself it was. the boot page table installs a higher-half MMIO *mid* table covering the 1 GiB root slot below `KERNEL_OFFSET + 0x40000000` — but within that slot, only the individual 2 MiB *leaf* pages explicitly handed to `MmioRegions` are mapped: the UART's page, the virtio slots, the JH7110 clock block. the PLIC's pages were never inserted. the root slot's presence made the region *look* covered; the missing leaf is where the walk died. and the worst witness was my own driver comment, which stated in confident prose that the PLIC "sits below 0x40000000, so it is already inside the higher-half MMIO gigapage — no new mapping." a **false fact about the address space, asserted in a comment, checked by nothing** — the a1 promise again, told to myself this time instead of to the compiler.

- the fix mirrors how every other device is handled: insert the PLIC's two megapages into `MmioRegions` in `kmain` — `0x0c000000` for the priority and per-context enable words, `0x0c200000` for the hart-0 S-context threshold and claim/complete registers, which live up at the `0x20_0000` block offset. two lines. the higher-half mid table now leaf-maps them, `plic::init` writes land, and the boot checkpoint comes back. 124/124, and `--scramble` too.

## the sink that drops whole frames, or nothing

- the last piece is the one the whole milestone was for: a `FrameSink` that puts real telemetry on the interrupt-driven wire. `UartFrameSink` encodes each `Frame` the one canonical way — postcard, COBS-delimited, the same `wire_encode` and the same 520-byte scratch the virtio sink uses, so any frame that path can emit this one can too — and pushes the encoded bytes into a byte ring drained by the THRE interrupt.

- its one real design decision is backpressure. when the ring can't hold a whole frame, the sink drops the **entire frame** and counts it — never a partial push. a half-frame on a COBS stream isn't fatal (the `0x00` delimiter lets the decoder resync on the next frame) but it's wasteful and ugly; whole-frame-atomic keeps the wire clean and the drop honest. the count surfaces as `Frame::Dropped`, the same drop-and-count discipline the frame allocator and the IRQ handler already use — because the *other* non-negotiable is that the kernel never blocks on the wire. a full ring costs you a frame, never a stall.

- it's a thin wrapper over a host-tested core, mirroring the existing UDP sink, and it went in under proper TDD against a mock byte sink: encode-and-arrive, refused-drops-and-counts, too-big-to-encode-counts, recovers-after-a-drop. four tests, and `cargo mutants` on the file killed **8 of 8**. the sink is the easy part *because* the two bugs above bought a wire it can trust.

## what I learned

- **an `asm!` constraint is a promise, and the optimizer is the one who collects.** `in(reg)` on a register the callee writes is not a mislabel — it's a false guarantee the compiler will *act on*, by parking live values where they'll be destroyed. debug builds spill and hide it; release builds take you up on the offer. any hand-written call boundary is a place you're making promises about register state, and the machine will hold you to every one.

- **a comment that asserts a machine fact is an untested claim with better handwriting.** "already mapped," "below 0x40000000 so it's covered" — prose that *sounds* like it was verified and wasn't. the higher-half MMIO map is per-page, and my mental model of it as a gigapage was the entire bug. when a comment states a fact the hardware could contradict, treat it as a hypothesis, not documentation.

- **the failure's shape is the first clue, before any detail.** "all 124 fail at the same checkpoint" is not 124 problems — it's one, upstream of all of them, because they share an image. reading the *distribution* of a failure before diving into any single instance pointed straight at boot and skipped the entire wrong investigation.

- **"the tool can't test that" is a claim to interrogate, not obey.** snemu "couldn't" model the PLIC only in the sense that nobody had written it. an afternoon of modeling — derived SEIP, an atomic for the `&self` claim, a level-triggered gateway — moved an entire kernel subsystem from "QEMU-only and flaky" into the deterministic gate. the emulator keeps repaying the bet that it's cheaper to teach it fidelity than to live without it.

- **both hero bugs were the same species, and the species has a name in these notes.** a fact about the machine, asserted in code, true under one build or one memory layout and false under another. release codegen and specific frame layouts are the two great exposers — [[project_release_build_exposes_kernel_ub]], [[project_kmain_frame_straddles_trampoline]], the straddle bugs. the lesson isn't "be more careful." it's that unchecked assertions about the machine have a *characteristic way of hiding* — behind the optimizer, behind a layout coincidence — and the defense is to make the machine prove them, deterministically, in a gate.

## what's next

- the transport core is done and tested; three steps land it end to end. the kernel glue — a ring-backed byte sink wired behind `console=frames`, so the board's post-init log becomes a real `Frame` stream while virtio stays the asserted channel under snemu. then the collector's `--serial` source, which is small on purpose because [[project_diagram_system]]'s "a source is a source" abstraction did the design work three steps ago — decode the board's live stream, and the VisionFive 2 reaches Grafana. and Step 5b, the interactive relay: raw stdin up the same line into the guest REPL, so the board isn't just observable but *driven*.

- and once the UART is a genuine second transport next to virtio, [[project_vf2_audio]]'s audio-modem (post 62) has the peer it was designed against — because the frames going out the serial line and the frames it wants to FSK out the headphone jack are, by construction, the same self-delimiting COBS bytes. the boring capability first; the fun becomes a client.
