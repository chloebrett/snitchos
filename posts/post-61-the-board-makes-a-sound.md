# Post 61 — The board makes a sound

- I plugged an earbud into the VisionFive 2's headphone jack this afternoon, booted a kernel, and heard a tone. A buzzy, slightly-wrong 440 Hz — more sawtooth than square — but unmistakably *a note*, coming out of a 3.5mm jack, driven by a microkernel that brought the DAC up from nothing. Two months ago this board could say `I am alive` ([[project_vf2_m1_code_complete]]). Now it can hum.

- what's actually happening under that tone: no codec, no DMA, no external chip. The JH7110 has an on-chip **PWMDAC** — a PWM-to-analog converter — and the kernel is doing programmed I/O, writing one 16-bit sample at a time into a register at `0x100b_0000`, paced by the timer at 8000 a second. A square wave from a lookup of exactly two values, out a filter, into my ear. It is the least sophisticated audio stack imaginable and I'm delighted with it.

- this is the story of getting there, which is really the story of the same lesson post 59 taught me wearing new clothes: **the emulator gets you to "the code runs." The hardware bills you for a wire.**

## the emulator was green, and green meant nothing I could hear

- I built this the way I build everything now: a pure, host-tested layer first (register encodings, the sample-rate table, the tone generator, the pacing math — all sourced from the mainline Linux driver, not guessed), then a **snemu device model** so the whole thing was exercisable off-hardware, then the kernel glue, then the board.

- and snemu earned its keep before the board had power. I taught it to model the PWMDAC — which I *had* to do, because snemu **halts on an unmapped MMIO write** ([[project_snemu_progress]]), so a guest that pokes `0x100b_0000` against an unmodeled bus just stops dead. Once the device existed, the integration test went green: boot the audio workload, the sample counter climbs, the whole clock-bringup → configure → write loop runs. I could even point snemu's `--audio-out` at a boot and get a **24.6-second, 196,632-sample WAV** out of a *deterministic emulator run* — open it, listen, a clean 440 Hz. Bit-for-bit reproducible.

- here's the trap I walked into anyway, eyes open: **a green audio test with a faithful device model and a listenable WAV still says nothing about whether sound comes out.** Because "green" meant *the samples were written*. It could not mean *the samples reached a pin*, for a reason that is almost funny once you see it.

## snemu can model a device. it cannot model a pad.

- the digital path, snemu did catch a real bug in. My first cut used raw physical addresses for the SYSCRG clock registers, and the itest died exactly the way post 59's `kmain` fault did:

```
kernel page fault: scause=0xd stval=0x13020278 sepc=0xffffffff...
```

- `scause=0xd` is a load page fault; `stval` is `SYSCRG_BASE + 0x278` — a *physical* address, read after `unmap_identity` tore the identity map down. Same family as the trampoline bugs ([[project_kmain_frame_straddles_trampoline]]): MMIO lives at a higher-half VA (`pa + KERNEL_OFFSET`), and I'd reached for the physical one. The emulator found it in seconds, host-side, because it's a bug *in what I wrote*.

- then the board ran, printed both its breadcrumbs, kept heartbeating — and made no sound at all. Earbuds fine, tested on my Mac. And this is the part snemu could never have told me: **the PWMDAC's output isn't on dedicated pins. It's routed through the SoC's pin-mux to GPIO 33 and 34, and I never configured the mux.** The DAC was running, the FIFO draining, samples flowing — into a pad that was wired to nothing.

- snemu has a PWMDAC device. It does not have *pads*. It has no notion of a physical pin, a mux, or a wire to a jack — because those are not addresses, and an emulator is built out of addresses. So a peripheral routed to an unconfigured pin is not a bug snemu can represent. It's precisely the shape of thing post 59 named: **the emulator finds the bugs in what you wrote; the hardware finds the bugs in what you believed** — and I believed the DAC's output went somewhere.

- the fix came straight out of the mainline board devicetree, which spells out the exact pin group: GPIO 33 = `PWMDAC_LEFT`, 34 = `PWMDAC_RIGHT`, written through `SYS_IOMUX` at `0x13040000`. Four register writes I'd simply never made. Added them, reflashed, and the earbud sang.

## the breadcrumbs made a silent board a five-minute bug

- the reason this cost an afternoon and not a week is a thing I did *before* powering on, and it's the observability thesis paying rent. The audio task prints exactly two lines:

```
audio: bringing up PWMDAC...
audio: PWMDAC up — playing 440 Hz @ 8000 Hz (500 ticks/sample)
```

- the second line prints *after* the clock/reset bring-up returns. So its appearance is proof the bring-up completed — including the reset-status poll, the one operation snemu couldn't validate (it always reports "reset released"; real silicon might not). When both lines showed and the board still went silent, I knew in one boot: **the digital path is fine, the analog hop is the problem.** No bisecting a silent board. The bracket did it.

- and it's a small live demo of a thing I've been designing lately ([[project_board_agent_bridge]]): the heartbeat kept ticking the whole time, which proves *nothing* about the audio task — a spin in that task would be invisible to a timer-driven heartbeat. What told me the truth was a breadcrumb that happened to sit right after the risky call, encoding liveness by accident. A per-task watchdog would make that systematic instead of lucky. I got away with hand-placed luck this time.

## the sawtooth is the jack telling me how it's wired

- the tone isn't a clean square — it sags into a sawtooth. That's not a bug, it's **the jack's AC coupling**: there's a DC-blocking capacitor in series, which is a high-pass filter, so a square wave's flat tops can't hold — they droop as the cap discharges, and alternating drooping levels *is* a saw shape. Textbook, benign, and exactly what a low-frequency square does through a coupling cap. A sine has no flats to droop, so the fix for a clean tone is a sine wave — which I'd deliberately deferred. The hardware taught me its analog channel the moment I fed it a signal with sharp edges.

## what snemu bought, and what it couldn't — again

- worth writing down because it's the same division as first light, sharpened. snemu bought: a green test before hardware, a *deterministic WAV I could listen to* (you can hear a boot replay — that still delights me), and it caught the physical-address MMIO bug host-side. What it couldn't buy: the pin-mux, the AC-coupling character, the confirmation that the DAC latches `WDATA` (which turned out true, and resolved my biggest open question about whether PIO would even work). **Every gap was in something I believed, not something I typed.** The emulator and the board are not redundant; they audit different things.

## what's next

- the tone works, but the architecture underneath it is wrong on purpose: a kernel task pokes `WDATA` directly. That doesn't scale to *two* sound sources, and it makes the DAC ambient authority — anyone can make noise. The next move is **`glitch`**: a userspace audio server that holds the DAC as a *capability*, so every source of sound is a client of one disciplined thing, every play is observable, and "the right to make noise" is a cap you can grant and revoke. The beep becomes its first client. The plan's written; it's the discipline the rest hangs off. (Name: snitch · stitch · glitch. It had to.)

- and past that is the part I got carried away designing this week, so I'll just gesture at it: audio as a *telemetry output* — sonify the kernel, hear the scheduler, hear an OOM. Then, because the telemetry is already self-framing `Frame`s, **modulate that frame stream out the jack as FSK** — a modem, from scratch — and demodulate it back, deterministically, from a snemu-captured boot. Then two instances talking over an audio link, which is just cross-machine `~>` (the shipped typed, capability-checked pipe) over a new transport. And the whole thing is *testable without a speaker*: wire two snemu instances' audio buffers together digitally, inject loss as a knob, and you can TDD a two-machine acoustic protocol with zero hardware and zero sound. Real audio becomes the moneyshot, not the dependency. It's all in [[project_vf2_audio]] if future-me needs the thread.

## what I learned

- **a green test with a faithful device model still can't cross the analog gap.** snemu modeled the DAC, ran the writes, and produced a listenable WAV — and told me nothing about whether a pad was wired. Green meant "samples written," which is not "sound made." I knew this abstractly from first light; I re-learned it concretely, one layer deeper, standing over a silent board.

- **an emulator is made of addresses, so it can't model a wire.** A device at a register, snemu nails. A pin-mux routing that device's output to a physical pad is not an address — it's a belief about the world — and beliefs are exactly what emulators inherit from you rather than check. Peripheral silent but code running? Suspect the pins before the logic.

- **bracket the risky call with breadcrumbs and a silent failure locates itself.** Two prints around the one operation the emulator couldn't verify turned "no sound, no idea" into "bring-up fine, analog broken" in a single boot. The heartbeat continuing proved nothing; the breadcrumb after the poll proved everything.

- **the hardware teaches you its analog channel the first time you send it edges.** The sawtooth isn't a defect, it's the coupling capacitor introducing itself. A sine wouldn't have asked the question. Sharp edges are how you interrogate a filter you didn't know was there.

- **and sometimes the milestone is just that something with a heatsink made a sound.** The tone is buzzy, the waveform's wrong, the architecture's a placeholder. It doesn't matter. My kernel made a noise on purpose, on real silicon, and I sat there grinning at a 440 Hz sawtooth like it was a symphony.
