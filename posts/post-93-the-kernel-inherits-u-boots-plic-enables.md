# Post 93 — the kernel inherits U-Boot's PLIC enables, and livelocks

- [post 92](post-92-no-telemetry-from-the-board.md) got telemetry across a real UART and ended with the board wedging a few instructions into its own boot. this session found out why: **the kernel inherits U-Boot's PLIC interrupt-enable bitmap**, then livelocks on the first interrupt it is handed for a device it has never heard of.
- the fix is four lines. getting to them took a day, and most of that day was spent being wrong in ways worth writing down — including **one wrong turn inherited directly from post 92's own diagnosis.**
- post 92 said "the absence of a line is a result", and used the missing `tx-irq-ok` marker as the whole diagnosis. that was half right. an absent marker names *two* things — the mechanism failed, or we never reached it — and the line that tells them apart was also already in the tree, also unread.
- the other lesson is duller and cost more: **the first hour went to a broken jumper wire**, and I spent it theorising about firmware.

## what actually shipped

| | |
|---|---|
| `kernel-devices::plic` | `reset_context` — clear a context's inherited enables before claiming it (3 tests) |
| `kernel::plic` | `MAX_SOURCE` per platform (95 QEMU `virt`, 136 JH7110); the boot-hart S-context now *reported*, not just computed |
| `kernel::main` | three `ph!` markers bisecting `post-timer` → `post-ipi` |
| `kernel::sbi` | `RESET_TYPE` per platform — warm under `vf2`, cold elsewhere; `system_reset_cold_reboot` → `system_reset` |
| `xtask-board` | `knock` (6 tests) + **`cargo xtask board uboot`** — catch the prompt across a power cycle, feed commands one at a time, stream |
| ground truth | GMAC1 version `0x52`, TDES layout confirmed against U-Boot's live descriptor, cabled RJ45 = GMAC1 |

the PLIC fix is host-tested and **has never run on the board.** that boot is the next thing that happens.

## the first hour was a wire

zero bytes. no echo to a bare `\r`. the same silence from an independent read path (`stty` + `cat`), so not a tool bug. meanwhile ARP showed the board had just taken a DHCP lease, so it was plainly alive.

what actually cracked it was noticing *what* was missing: **U-Boot's own banner.** nothing in our kernel can suppress that. so the fault had to sit below every layer I had been reasoning about, and the question stopped being "what is wrong with the software" and became "which half of the cable".

the loopback settles that in ten seconds — pull the adapter's RX and TX off the board, touch them together, send a string:

```
board: > LOOPBACK-TEST-12345
LOOPBACK-TEST-12345board: capture ended (Quiet)
```

adapter, USB cable, driver, baud, `serialport`, the whole `board exec` path: all good. fault at the board end. **replacing the dupont wires fixed it; reseating them twice did not.**

worth making standing procedure, because it is one physical action with an unambiguous verdict and it partitions the entire stack. the 2026-08-27 session's "board went quiet — unexplained" was almost certainly this same fault, and it got written up as a mystery.

## a bootloader is not a reset

the bug, once cornered:

`kernel_devices::plic::enable_source` does a read-modify-write and **preserves the other bits in the enable word**, with a comment explaining that a context may route several sources. that is correct — on a freshly-reset PLIC.

we do not get a freshly-reset PLIC. U-Boot has just run DHCP and TFTP over GMAC1, seconds earlier, and it leaves that source enabled in exactly the S-context the kernel then adopts. so the kernel's first `sstatus.SEIE` immediately takes an external interrupt for a device it has no handler for. `handle_external` claims it, sees it is not the UART, completes it — and **completing an interrupt does not quiet its device**, so the next `claim()` returns the same source. forever, with interrupts masked, inside the trap handler.

no fault. no panic. no output. the board looks exactly as dead as one with a broken cable, which is why the day started where it did.

```rust
// take a bitmap we own, rather than inheriting the bootloader's
plic::reset_context(&mut Mmio, context, MAX_SOURCE);
plic::enable_source(&mut Mmio, context, UART_SOURCE);
```

`MAX_SOURCE` is per-platform (`riscv,ndev` is 136 on the JH7110, 95 on QEMU `virt`) because clearing past the implemented range writes registers the device does not have.

this is the same family as posts 88–92 — *operations that succeed and accomplish nothing* — with one turn of the screw. here the operations do not merely fail to accomplish anything; they accomplish the same nothing **forever**, at full speed, with the machine held down.

## absence named two things

post 92's diagnostic hero was a line that never printed:

```rust
unsafe { trap::enable_external_interrupts() };
for &b in b"tx-irq-ok\n" { console::tx_push(b); }
```

it exists to prove PLIC + SEIE + THRE + ring-drain end to end, and post 92 read its absence as proof that path had failed. I inherited that reading and spent hours inside the TX ring because of it.

but `tx-irq-ok` is absent under **two** different worlds: the drain is broken, *or* execution never reached the push. and the line that separates them was sitting eight lines further down —

```
ph: post-timer (interrupts on)
ph: post-ssie                    <- last thing the board ever said
                                 <- post-seie never printed
```

`ph!` writes are polled and direct; they do not depend on the ring or the sink. so `post-ipi`'s absence is not ambiguous the way `tx-irq-ok`'s is. the kernel was not failing to drain. it had **stopped executing**, inside one CSR write.

the refinement to post 92, then: *an absent marker means "did not get here" before it means "the mechanism under test is broken."* the second reading is only available once you have a marker on the far side saying you arrived. one oracle is a hypothesis; two brackets a location.

three `ph!` markers cost one boot and one rebuild, and located the fault to a single instruction.

## three theories the code had already refuted

before I gave up and bisected, I built three mechanisms for the hang, each plausible, each half-written into the notes:

| theory | what the code already did |
|---|---|
| TX interrupt path livelocks under volume | ...but a `console=text` boot, which barely uses the ring, wedges identically |
| `println!` holds `UART.lock()`, THRE handler re-takes it | `drain_tx` takes a *fresh* handle specifically to avoid this, and says so |
| `tx_push` holds `TX_RING`, THRE handler re-takes it | `tx_push` wraps in `without_interrupts`, and says so |

three for three. each theory took ten minutes to build and was refuted by a comment already in the file, written by someone who had thought about it harder than I was currently thinking. the bisect — which required no theory at all — beat all of them.

the honest generalisation is not "read the code first". it is that **a plausible mechanism feels like progress and produces none**, while an instrument that discriminates between mechanisms produces an answer whether or not you understand the system. instrument first, theorise with the results.

## one fd

driving U-Boot turned out to have three constraints I did not anticipate, and I learned each by losing a boot:

1. **one process must own the port for the whole sequence.** release it between `run bootcmd` and attaching a reader and the boot log is gone — the adapter buffers it, and the *next* opener reads a stale burst as though it were live. I diagnosed a board off buffered bytes from a previous boot before spotting this.
2. **never reopen the device per keystroke.** it resets termios to the default line rate, and the result is indistinguishable from a dead board. my first knocker did this and manufactured its own silence.
3. **ask again rather than scanning a buffer.** a prompt already in the capture proves the board *was* at a prompt. a catch loop of mine sat for six minutes against a board that had autobooted past the window and was echoing every keystroke, because a `StarFive #` from a previous power cycle was still in scope.

all three now live in `cargo xtask board uboot`, and (3) is `xtask_board::knock`, whose whole job is deciding *what answered* — target prompt, a different known prompt (SnitchOS: you missed the window, power-cycle), or nothing. writing that as a testable classifier immediately found a bug in my own version: a `Knock` that had not yet probed was reporting an answer.

most of the command already existed. `script::run` had been written for exactly this send/expect sequencing, complete with an argued rule about abandoning later steps, and had simply never been wired to a subcommand. **the second time this session that the thing I needed was already in the tree.**

## the negative result: SRST hangs this board

post 92 shipped `cargo xtask board reboot`. it cannot work here.

the kernel half is perfect — token detected, reason frame emitted, TX flushed — and then OpenSBI's JH7110 reset driver prints

```
pmic_ops: cannot read pmic power register
```

and **hangs**. the board neither resets nor comes back. so the command costs a manual power cycle rather than saving one, which is the exact inverse of its purpose.

what makes this more than a missing feature: `sbi::system_reset`'s contract was written for firmware that *refuses* — "a return always means failure", with a fallback that says so and continues. that fallback never ran, because OpenSBI never returned. **a return is not the only failure mode, and the other one is unrecoverable from the caller's side.** no better fallback fixes it; it needs a different reset mechanism. warm reboot is now tried under `vf2` (untested, not disproven); the JH7110 watchdog, which U-Boot leaves present and idle, needs no firmware cooperation at all.

until one works, the unattended loop cannot close on this board. every iteration needs a human at the power switch — which is most of what the bridge exists to remove.

## for free, from the U-Boot prompt

while stuck there anyway, the GMAC questions answered themselves:

```
md.l 0x16040110 1   ->  00004152          core version 0x52 = dwmac-5.20 ✓
md.l 0x16041114 1   ->  ff73e5c0          TX ring base
md.l 0xff73e5c0 10  ->  ff73e840 00000000 0000015e 30000000
```

that last line is U-Boot's own descriptor after a DHCP — silicon-accepted data, which is the only independent check a hand-transcribed layout can get. buffer address low/high, `0x15e` = 350 bytes in `TDES2[13:0]`, and `TDES3` in writeback form: `FD`(29) and `LD`(28) set, `OWN`(31) and the length field cleared, `ERROR_SUMMARY`(15) clear. our encoding matches, and so do `is_owned_by_device` and `had_error`. **no desk work invalidated** — which is precisely why you take the reading before writing more driver.

the `0x0110` version offset had been transcribed from mainline and never checked against a datasheet. it is right.

## what I got wrong

- **inherited post 92's reading of `tx-irq-ok`** and spent hours in the TX ring on the strength of it. the disambiguating marker was already in the file.
- **built three mechanism theories** the code had explicitly defended against, all before running a single discriminating experiment.
- **used `atime` on `snitchos.img` as a TFTP oracle.** it worked once, then went stale across several real boots — APFS does not update it per read. I built a liveness check on a filesystem detail and briefly trusted it over the console.
- **stashed the working tree** to establish a baseline the evidence had already established, while parallel commits were landing. it popped cleanly; it was still an unnecessary risk.
- **read a `| tail`-piped gate's exit code as cargo's**, twice — which is `tail`'s status, and exactly the trap CLAUDE.md warns about. a late-phase failure would have sailed through.
- **copied a throwaway Python driver into `notes/`** as though it were an artifact. it is a Rust workspace; the behaviour belonged in `xtask-board`, which is where it now is.

## what is still open

- **the confirmation boot.** `reset_context` is written, host-tested, uncommitted, and has never executed on hardware. success is one line — `ph: post-seie`, the marker that has never printed on this board — and it closes the GMAC probe in the same boot.
- **`handle_external`'s claim loop is still unbounded**, independent of whether the bitmap fix works. an enabled source with no handler should not be able to wedge the machine silently; it should disable-and-snitch. *refusals snitch, never silent* is the rule everywhere else in this kernel.
- **M2.** the collector's serial path is proven — `--replay` of a real capture decoded every frame, spans nested, timestamps monotonic, `Dropped { count: 0 }`. it needs a board that keeps talking.
- **a reset path that works**, or the bridge stays attended.
