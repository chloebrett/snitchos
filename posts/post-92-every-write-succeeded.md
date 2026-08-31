# Post 92 — every write succeeded

- the board booted, ran four harts, printed a banner, evaluated Stitch, and produced **no telemetry at all**. three independent causes, stacked. and not one of them failed: every function returned, every MMIO write landed, every frame encoded. the system was silent in the way a correct program is silent.
- [post 88](post-88-presented-is-not-drawn.md) was a proxy that cannot fail. [post 89](post-89-nothing-to-report.md) was an observable that could not fire. [post 90](post-90-the-same-silence.md) was two failures returning the same value. this one is the next member of that family and the least comfortable: **operations that succeed and accomplish nothing.** there is no wrong value to catch, because there is no value.
- the diagnosis, in the end, came from a line that wasn't printed — an oracle that had been sitting in `main.rs` for weeks, doing exactly the job it was written for, which I read past twice.
- and the session's other lesson is about a claim I have made in plans myself: **"landed and gate-green" can be true of a crate and false of a system.**

## what actually shipped

| | |
|---|---|
| **telemetry over a real UART** | the first SnitchOS frames ever to reach a host over serial |
| `kernel-devices::console` | `ConsoleRing::push_all` (whole-frame atomic), `RebootDetector`, `REBOOT_TOKEN` |
| `kernel::console` | `tx_push_all`, `flush_tx_blocking`, the RX-side reboot watch |
| `kernel::tracing` | `init_uart_sink` + a third arm in `transmit_bytes`; drops counted and reported |
| `kernel::plic` | the boot hart's S-context, **computed** — see below |
| `kernel::sbi` | `system_reset_cold_reboot` (SRST, EID `0x5352_5354`) |
| `snemu` | `StepError::SystemReset`, keeping `reset_type` **and** `reason` |
| `xtask-board` | `thrash` (the reboot guard) + `cargo xtask board reboot` |
| itest | `console-reboot-requests-srst` — passes plain **and** scrambled |

mutants: `kernel-devices/src/console.rs` **39/39 caught**; `xtask-board/src/thrash.rs` 4 caught, 1 unviable, **0 survivors**.

board facts banked: **RJ45 #2 is GMAC1**; GMAC1's version register reads `0x52` (dwmac-5.20), confirming an offset [vf2-gmac-design.md](../docs/vf2-gmac-design.md) flagged as never checked against a datasheet; U-Boot's TX ring validates our hand-transcribed TDES encoding **and** uses a 64-byte descriptor stride — a non-zero `DSL` that appears nowhere in our design.

## three silences

**one: the frames had nowhere to go.** `KernelSink` routes to exactly two transports — the UDP batcher and the virtio-console. a VisionFive 2 has neither. so `tracing::emit_log` encoded each frame into a 520-byte scratch buffer, called `transmit_bytes`, and dropped it into a device that wasn't there. `workload=gmac-probe` — a workload written *specifically for this board* — reported its entire register dump into that void. `kernel/src/device/console.rs` states the rule three lines from the bug:

> early output must never depend on the frame sink (the stale-image / `ph!`-markers lessons)

the probe depends on it.

**two: the sink fed a ring nothing drained.** wiring `init_uart_sink` was necessary and insufficient. `tx_push_all` queues into a 512-byte ring that empties on the THRE interrupt — and the THRE interrupt never arrived.

**three: the interrupt went to somebody else's bitmap.** `plic::init` hardcoded QEMU `virt`'s numbers behind a standing `// board: derive from DTB` note. two were wrong: the JH7110 numbers UART0 as source **32**, not 10; and its S-mode context is not where symmetry would put it, because the JH7110 is one S7 monitor core plus four U74s and **the S7 contributes only an M-mode context**, shifting every U74 down one.

this is the silent-by-construction one. enabling a source in the wrong context enables it *somewhere* — a real register, a legal write, a successful store. nothing faults. the failure surfaces as a working UART attached to a ring that fills and never empties, which is indistinguishable from "the sink isn't wired" — the bug I had just finished fixing.

## the line that wasn't printed

`main.rs`, right after enabling external interrupts:

```rust
unsafe { trap::enable_external_interrupts() };
for &b in b"tx-irq-ok\n" {
    console::tx_push(b);
}
```

with a comment saying, in as many words, that it exists to prove PLIC + SEIE + THRE + ring-drain end to end, and that if the interrupt path is broken the marker never appears.

it never appeared. it was absent from every board boot log I captured, including the ones I read line by line looking for something else. **the absence of a line is a result**, and it was the whole diagnosis sitting in plain sight — its `tx-irq-delivers` scenario passes under snemu, so the marker's presence there and absence on hardware localises the fault to the platform in one comparison.

I read past it twice. what finally surfaced it was going looking for *why the drain wasn't running* rather than *what was wrong with the sink* — i.e. asking about the mechanism a layer down instead of re-examining the layer I had just touched.

## a constant that cannot be a constant

my first PLIC fix hardcoded the JH7110 context. it didn't work, and the boot log said why:

```
boot 1:  smp: hart 1 (mhartid 1) | hart 2 (mhartid 2) | hart 3 (mhartid 4)
boot 2:  smp: hart 1 (mhartid 1) | hart 2 (mhartid 3) | hart 3 (mhartid 4)
```

the kernel uses the four U74s, so the boot hart was **mhartid 3 in one boot and 2 in the next**. OpenSBI hands off to whichever hart wins the race. any compile-time context is right only by luck, and mine was unlucky.

it is now computed from `LOGICAL_TO_MHARTID[0]`, which `kmain` fills from the DTB before `plic::init` runs. the per-platform part that *is* constant — whether the S-context sits at `2m` or `2m+1` — stays a `cfg`, because that is a property of the SoC's `interrupts-extended` order rather than of this boot.

worth noticing what kind of mistake this was. I did not get a number wrong; I got the **category** wrong. I reached for a constant to describe something that varies per power cycle, and the code would have been wrong in a way no host test could see, on a board that would have booted fine and stayed quiet.

## "landed and gate-green" was true, and about the wrong thing

`kernel-obs/src/uart_sink.rs` defines `UartFrameSink` and its `ByteSink` trait. host-tested. **8/8 mutants killed.** its plan step is marked landed, and the plan's status header lists it among the steps that are done.

`UartFrameSink` appears **nowhere** in `kernel/`. nothing has ever constructed one. the module is `pub`, so no dead-code warning fires — it is public crate API with zero consumers, and its mutation score certifies code that does not run.

it went orphaned for a real reason rather than by neglect: it is a `FrameSink` that takes a `Frame` and encodes it, but the kernel's transport selection happens in `transmit_bytes`, which receives **already-encoded bytes**, because `send_hello` and `flush_pre_init` enter there too. dispatching at the frame level would open the stream on the wrong wire; nesting it under `transmit_bytes` would mean decoding bytes back into a frame so it could re-encode them. the two shapes do not compose, so the UART arm went in at the byte level and the sink stayed unused. now [debt #22](../docs/debt-register.md).

the general form is the part worth keeping: **a crate's test suite can prove a component correct. it cannot prove that anything calls it.** every green signal here was honest about the proposition it tested, and the proposition was not the one the status header claimed.

the same plan's header also *omits* a step its own section marks ✅ done — so it is wrong in both directions. `cargo xtask plan-status` gates that the header exists and is dated, not that it is true, and this session found three stale statuses in one evening. that is no longer bad luck.

## liveness without a console

for a stretch the board was completely silent, and the two obvious probes were useless: SnitchOS has no network stack, and idle U-Boot ignores ICMP, so a ping proves nothing **in any state** including perfectly healthy. I ran one anyway before noticing that.

the discriminating test came from asking what a *living* board must emit regardless of its console. `bootcmd` opens with `dhcp` and then TFTPs, so it must broadcast — and broadcasts flood every switch port including the AP, so a Wi-Fi-attached Mac sees them even though the board is on the cable:

```
21:46:44  DHCP Request from 6c:cf:39:00:56:cb
21:46:47  ARP Announcement 192.168.0.61
21:46:47  Request who-has 192.168.0.7 tell 192.168.0.61   <- the stale serverip
```

the board was fine; the console wiring was the entire fault. **a board with no working console is not necessarily a dead board**, and its broadcast traffic is an independent liveness oracle — one `wire::capture` structurally cannot provide, since `Timeout` cannot distinguish "hung" from "unplugged". (the `serverip` in that trace is [post 90](post-90-the-same-silence.md)'s MAC-rotation story arriving on cue.)

that capture also answered a question from a *different* agenda item by accident: the MAC in it identified which physical jack was cabled, settling **RJ45 #2 = GMAC1** without the plug-one-and-see procedure the agenda proposed. cheap broad capture beat the targeted method.

## the negative result: `exec` cannot interrupt autoboot

board-bridge step 4b — stopping the U-Boot countdown — has no host-side oracle, so this session was its gate. it fails, structurally: `exec` writes its input **once, at port-open**, then captures. catching a two-second countdown requires writing *during* it. no amount of retrying fixes a shape mismatch; it wants a write-on-match mode. recorded as a result rather than a to-do, because "we tried and it didn't work" and "it cannot work as built" are different facts.

## reboot, and the ISR that only watches

the thing this session was ultimately for: `~~~reboot` on the console now cold-reboots the board into a freshly TFTP'd image, so the loop stops needing a human at the power switch.

the one design decision worth recording is where the work happens. `drain_rx` runs in the external-interrupt handler, and the reset path emits a telemetry frame — which interns a string, which may allocate. that is the re-entry deadlock CLAUDE.md already documents for the allocator and the IRQ handler. so the ISR **only observes**: it feeds bytes to the detector and raises a flag. the heartbeat, in normal context, emits the reason, flushes the TX ring with a bounded polled drain, and calls SRST.

the order is load-bearing and it is why the flush exists at all: the ring drains on the THRE interrupt, and a reset stops that machinery, so resetting with bytes still queued truncates the very frame that explains the reboot. asserting in the itest that the `Log` **arrives** is how the flush gets tested — you cannot observe a flush directly, but you can observe that something which required it happened.

snemu now models SRST as a halt carrying `reset_type` and `reason`, which makes the whole chain gate-testable. the halt is the **pass** — the one scenario where the guest stopping is the behaviour under test, and where `halt_reason` is what separates "asked to reset" from "hit an unimplemented instruction". before snemu modelled it, the second is exactly what the first looked like.

## what I got wrong

- **I read a stale ARP entry as evidence** and reported the cabled port was GMAC0. it was #2. the entry was real; it was just from before the cable moved. an observation with no timestamp is not an observation of *now*.
- **the ping**, above — a probe that returns the same answer for every hypothesis.
- **the hardcoded PLIC context**, above — right category of fix, wrong category of value.
- **"one edit from clean"** — I said `console=frames` was all that stood between us and the milestone. it isn't: the sink opens at `main.rs:307` and the ring cannot drain until 368, so `Hello` — which carries the `timebase_hz` the host needs to interpret *every* timestamp — plus the metric registrations are dropped on every boot. the stream is not lossy, it is uninterpretable. I found that only because the claim was challenged.
- **two of my own tests were wrong rather than the code** — a miscounted free slot, and an "aborted token" case that concatenated into the exact token it claimed to reject. both failed loudly, which is the system working.
- **I batched tests and implementation** on the thrash guard, against a rule I have been given explicitly. the mutation run is the compensating evidence, not an excuse.

## a pattern across parallel sessions

five build breaks came from other streams working the same tree, and they are one shape:

| broke | defined in | obligation left stale in |
|---|---|---|
| `GmacTx` | `kernel-boot::bootargs` | `kmain`'s match |
| `DisplaySink` | `protocol` | `collector::caps` match |
| `kitsch_static` | `kitsch-render` | `user/hello` call site |
| state hash | `snemu::Bus::hash_state` | `snemu-wasm`'s pinned constant |
| `present()` | `stitch::platform` | `stitch::natives` call site |

in every case the **definition and its obligations live in different crates**, so the authoring crate's tests stay green while everyone else's build dies — and none of them are caught by the tests the author runs. the existing note about `WorkloadKind` being a cross-session chokepoint turns out to describe a class, not a variant: it generalises to enums, trait signatures, and pinned constants alike.

## what is still open

M2 is **not** closed. frames cross the wire, which is the hard part and the part that needed hardware — but the preamble is still being dropped every boot, and `cargo xtask reader --serial` into Grafana remains unexercised. the fix is an ordering change plus making frames mode start when the sink opens, which needs `console_mode` to distinguish "unset" from an explicit `console=text`.

the honest summary is that this session proved the mechanism and did not finish the milestone, and I'd rather that sentence be in the post than discovered in three weeks by someone reading a status header.
