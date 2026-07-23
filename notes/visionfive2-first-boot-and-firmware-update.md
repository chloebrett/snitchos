# VisionFive 2 — first boot, and the firmware update that gated it

**Date:** 2026-07-23. **Board:** StarFive VisionFive 2 **v1.3B** (SN
`VF7110B1-2310-D004E000-00002350`, PCB rev b2, 4 GiB DRAM, MAC0
`6c:cf:39:00:56:ca`). **Host:** macOS.

Goal for the session was narrow: confirm the board that just arrived actually
works, by getting a stock Linux onto a microSD and booting it. It works — but
the path had one non-obvious gate (stale factory firmware) worth recording,
because the SnitchOS port ([../plans/visionfive2-port.md](../plans/visionfive2-port.md))
will boot through this same U-Boot.

## What we did, end to end

1. **Flashed Ubuntu 24.04.4 preinstalled server (riscv64 / jh7110)** to the
   microSD.
   - Image: `ubuntu-24.04.4-preinstalled-server-riscv64+jh7110.img.xz` from
     `cdimage.ubuntu.com/releases/24.04/release/`. SHA256
     `172445879d87595d149d8bbc6dd3048a70c5f3b63847c6119beaa7ff395ca23e`
     (verified against the published `SHA256SUMS`).
   - macOS identified the card as `/dev/disk5` (the built-in reader reports as
     **internal**, so `diskutil list external` misses it — use `diskutil list`).
   - Write: `xz -dc <img>.xz | sudo dd of=/dev/rdisk5 bs=4m` (raw `rdiskN` for
     speed; BSD `dd` has no progress bar — Ctrl-T prints status). Then
     `diskutil eject`.

2. **First boot: reached the kernel, then OpenSBI trapped.** The board is alive
   (SPL, OpenSBI, U-Boot, DRAM, both Ethernet MACs, EEPROM all detected), read
   the SD, ran the boot menu, loaded the Ubuntu 6.17 kernel — then:
   ```
   sbi_trap_error: hart0: trap handler failed (error -2)
   sbi_trap_error: hart0: mcause=0x5 mtval=0x40048060 mepc=0x40004cac
   ```
   A load access fault **inside OpenSBI itself** (firmware base `0x40000000`),
   right after `EFI stub: Exiting boot services`.

3. **Root cause: 2023 factory firmware vs a 2026 kernel.** The banner gave it
   away — `U-Boot SPL 2021.10 (Feb 28 2023)` / `OpenSBI v1.2`. The board shipped
   with ~3-year-old firmware in SPI flash; the modern 6.17 kernel makes SBI
   calls that old OpenSBI mishandles → the trap. Updating the SPI-flash firmware
   is a **documented Canonical prerequisite**, not a workaround.

4. **Updated SPI flash from U-Boot — no downloads needed.** The firmware binaries
   ship *inside the Ubuntu rootfs* on the SD, and U-Boot can read them. At the
   `StarFive #` prompt (spam Enter during the brief autoboot window):
   ```
   sf probe
   load mmc 1:1 ${kernel_addr_r} /usr/lib/u-boot/starfive_visionfive2/u-boot-spl.bin.normal.out
   sf update  ${kernel_addr_r} 0        ${filesize}
   load mmc 1:1 ${kernel_addr_r} /usr/lib/u-boot/starfive_visionfive2/u-boot.itb
   sf update  ${kernel_addr_r} 0x100000 ${filesize}
   env default -f -a
   env save
   ```
   SPL → SPI offset `0x0`; `u-boot.itb` (U-Boot **+ modern OpenSBI**) → `0x100000`.
   Both writes reported clean (`150787` and `1359401` bytes written). This is
   what fixed the trap.

5. **Post-update: new U-Boot booted but looked for the OS on the wrong device.**
   New banner confirmed the flash took: `U-Boot 2025.10-0ubuntu0.24.04.1 (Nov 19
   2025)`, board correctly ID'd as `VisionFive 2 v1.3B`. But `env default`
   wiped the StarFive boot script, so autoboot fell back to probing **mmc 0**
   (the empty eMMC connector → `Card did not respond to voltage select! : -110`)
   and netboot (no cable → PHY timeout). Our SD is **mmc 1**.

6. **Booted from the SD via standard-boot.** `setenv boot_targets mmc1; boot`
   did *not* help — the leftover `bootcmd` ignores `boot_targets` and looks for
   an undefined `bootcmd_mmc1`. The fix was this U-Boot's bootstd scanner:
   ```
   bootflow scan
   ```
   (This build is minimal bootstd — no `-l`/`-b` flags; plain `bootflow scan`
   scans all devices and boots the first bootable one.) It found the Ubuntu EFI
   loader on mmc 1 and booted. **Made permanent** with:
   ```
   setenv bootcmd 'bootflow scan'
   env save
   ```

## Things worth remembering

- **Boot-mode DIP switch polarity (this board): ON = the weird-symbol side =
  logic 0; the numbered (1/2) side = logic 1.** So **both switches ON = `0,0` =
  boot from SPI flash** (the recommended, stable config — SPL loads from flash,
  which then loads the kernel from SD). The tell that we were in flash mode all
  along was the SPL line `Trying to boot from SPI`. Truth table:
  `0,0`=QSPI flash · `0,1`=SD-direct · `1,1`=UART recovery (harmless dead-end).
  I initially guessed ON=1 and had it backwards; the `Trying to boot from SPI`
  evidence + "ON == weird-symbol side" resolved it. **Anchor switch decoding on
  observed boot behavior, not the ambiguous silkscreen.**
- **The card reader is "internal" on this Mac** — `diskutil list external`
  returns nothing; use `diskutil list` and match by size/FS (31 GB FAT32
  "NO NAME").
- **The Ubuntu image is self-contained** (SPL + U-Boot in its own GPT boot
  partitions), *and* carries the firmware `.bin`/`.itb` under
  `/usr/lib/u-boot/starfive_visionfive2/` — which is why the SPI update needed
  no external downloads.
- **`env default -f -a` resets the StarFive boot script**, which is what left
  the board probing the empty eMMC slot. If you reset the env, expect to set
  `bootcmd` yourself (we set it to `bootflow scan`).
- **Device map:** `mmc 0 = mmc@16010000` (eMMC connector, empty here),
  `mmc 1 = mmc@16020000` (microSD). SD is a 29.1 GiB `SD16G`, SD 3.0 High Speed,
  4-bit.

## Relevance to the port

We now have a **known-good VF2 reference target**: modern U-Boot 2025.10 at the
`StarFive #` prompt (where a SnitchOS payload will be `load`ed / `tftpboot`ed
and `booti`'d), a stock Ubuntu to diff against, and confirmed hardware. This
retires the "can the board even boot" unknown ahead of M0 (serial + TFTP loop)
in the port plan.
