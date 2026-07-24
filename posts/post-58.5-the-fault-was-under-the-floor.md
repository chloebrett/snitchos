# Post 58.5 — the fault was under the floor

- this one is a prequel. post 59 opens on "code-complete, 121/121 green, three words on a serial console" — and skips the day in between, where the board that just arrived refused to run *anybody's* modern software, mine included. before SnitchOS could fail on this hardware in interesting ways, stock Linux failed on it in a boring, instructive one. worth banking, because the lesson underneath it is the same bet the whole OS is built on.

- the plan was the most boring possible win. flash a stock **Ubuntu 24.04 riscv64** image to a microSD, boot it, tick the "yes, the hardware is real and works" box, move on. a sanity check you're supposed to pass without noticing. I did not pass it without noticing.

## a trap with a return address in the wrong building

- it got most of the way. SPL, DRAM init, U-Boot, the boot menu, it loaded the Ubuntu 6.17 kernel, printed `EFI stub: Exiting boot services` — and then:

```
sbi_trap_error: hart0: trap handler failed (error -2)
sbi_trap_error: hart0: mcause=0x5 mtval=0x40048060 mepc=0x40004cac
```

- `mcause=0x5` is a load access fault: something read an address the hardware refused. the instinct is to blame the thing that was just running — the kernel. but the useful field is `mepc`, the PC that faulted, and it reads **`0x40004cac`**. the kernel wasn't loaded there. RAM on this board starts at `0x4000_0000`, and the *firmware* — OpenSBI, the M-mode layer that sits **under** the S-mode kernel — lives right at the bottom of it. the faulting instruction was inside OpenSBI itself.

- that reframes everything. this isn't a kernel bug; it's a fault **under the floor** — in the layer I'd been silently trusting to be solid because it's below the part I think about. and because it's a fault, not a hang, it was *located*: three registers told me which building the crime happened in. (post 59 is largely about the opposite animal — the silent hang — so file this as the easy case: the hardware handed me a report.)

- the banner, which I'd scrolled past twice, closed it: `U-Boot SPL 2021.10 (Feb 28 2023)` / `OpenSBI v1.2`. the board shipped with **three-year-old firmware in its SPI flash.** a 2026 kernel makes SBI calls that a 2023 OpenSBI mishandles, and the mishandling is a load fault inside the firmware's own trap path. this isn't a workaround-worthy quirk either — updating the SPI firmware is a *documented Canonical prerequisite* for this board. the machine was simply older than the software it was being asked to run.

## the cure was inside the patient

- the fix I expected was tedious: find the right firmware blobs online, cross-reference versions, dd them somewhere, hope. the actual fix is much better and slightly funny — **the firmware I needed was already on the SD card, inside the OS that wouldn't boot.**

- the Ubuntu rootfs carries the board's own bootloader binaries under `/usr/lib/u-boot/starfive_visionfive2/`. and U-Boot — the *old* one, still running from flash — can read the SD card perfectly well; it just couldn't boot the kernel on it. so it can pick up the new firmware off the very filesystem it failed to launch, and write it to its own flash. from the `StarFive #` prompt (you catch it by spamming Enter during the autoboot window):

```
sf probe
load mmc 1:1 ${kernel_addr_r} /usr/lib/u-boot/starfive_visionfive2/u-boot-spl.bin.normal.out
sf update  ${kernel_addr_r} 0        ${filesize}
load mmc 1:1 ${kernel_addr_r} /usr/lib/u-boot/starfive_visionfive2/u-boot.itb
sf update  ${kernel_addr_r} 0x100000 ${filesize}
```

- SPL goes to SPI offset `0x0`; `u-boot.itb` — which bundles modern U-Boot **and** a modern OpenSBI — goes to `0x100000`. `150787` and `1359401` bytes written, clean. reset, and the new banner confirmed the flash took: `U-Boot 2025.10` (Nov 2025), board correctly identified as `VisionFive 2 v1.3B`. the trap was gone. an image self-contained enough to carry its own cure — I'd assumed "won't boot" meant "useless to me right now," and it wasn't.

## then it booted the wrong thing, and the machine told me why

- new firmware, new problem: it came up and immediately went looking for an OS in the wrong places. `Card did not respond to voltage select! : -110` (the empty eMMC connector), then a PHY timeout (no Ethernet cable). it was probing everything *except* the SD card the OS was actually on.

- the cause was self-inflicted — flashing firmware included `env default`, which wipes the StarFive boot script, so autoboot fell back to a generic order that didn't know about my card. and here's the part I want to remember: my first two fixes were **guesses about how it should work**, and the machine rejected both. `setenv boot_targets mmc1; boot` did nothing — the leftover `bootcmd` ignores `boot_targets` and reaches for an undefined `bootcmd_mmc1`. what actually worked was asking *this* U-Boot what *it* does: `bootflow scan`, the bootstd scanner, which walks every device and boots the first bootable one. it found the Ubuntu EFI loader on the SD instantly. `setenv bootcmd 'bootflow scan'` made it stick.

- the same shape had already bitten me one layer down, at the boot-mode DIP switches. the silkscreen labels are ambiguous — a numbered side and a weird-symbol side — and I confidently decoded them backwards (guessed ON = logic 1). what set me straight wasn't staring harder at the legend, it was a line the SPL prints on the way up: `Trying to boot from SPI`. that one observed fact fixed the whole truth table (**ON = weird-symbol side = logic 0**, so both switches ON = `0,0` = boot from SPI flash — the stable config). the label is a claim; the boot log is the system.

## the bet, made to me before I could make it

- three separate times in one afternoon — the fault's address, the device probe, the switch polarity — the answer came from **what the running machine reported**, and every time I'd started from a story *about* the machine (the kernel must be at fault; `boot_targets` must select the device; the silkscreen must mean what it looks like) that was wrong or beside the point.

- which is, of course, the entire thesis of this OS. SnitchOS exists because you should believe the system's own account of itself over the documentation, the diagram, or your model of it — that's what all the telemetry is *for*. it's a little on the nose that the board made me pass that exam before it would run a single line of my code. I'd been treating the firmware as floor: solid, below thought, not my problem. it had a version, and the version was three years stale, and I had to update the floor before I could build on it.

- checkbox ticked, eventually, and not boringly. full procedure with the device map and the macOS `dd` incantation is in `notes/visionfive2-first-boot-and-firmware-update.md`; the durable board facts it turned up are in [[project_visionfive2_port]]. next comes the part where my *own* code runs on it — and that's post 59.
