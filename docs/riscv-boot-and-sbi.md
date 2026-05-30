# ⛓️ RISC-V boot & SBI — reference

*Reference material for v0.1 bring-up. The corrected mental model: on QEMU RISC-V there is no bootloader you write.*

# One-line model
On QEMU RISC-V, OpenSBI (M-mode firmware, shipped with QEMU) initializes the machine, hands your S-mode kernel two registers (hart ID, DTB pointer) with the MMU off, and **stays resident** as a callable services layer beneath you. Your "boot code" is a dozen instructions of stack setup, then Rust.

# Privilege levels
Three RISC-V modes matter:

- **M-mode (Machine)** — highest, full hardware access. Every core starts here at power-on. Nothing above it.
- **S-mode (Supervisor)** — where an OS kernel runs. Manages virtual memory, takes interrupts; some operations trap to M-mode.
- **U-mode (User)** — userspace, least privileged.

**The SnitchOS kernel is an S-mode program. It does not run in M-mode.** This is the key fact that makes RISC-V boot clean: a well-defined, standardized layer below the kernel handles the machine-level ugliness.

# The boot chain on QEMU `virt`
1. **QEMU built-in ROM** — tiny, sets a few registers, jumps onward. (Mask ROM on real hardware.)
2. **SBI firmware = OpenSBI**, runs in **M-mode**. QEMU ships it built-in — you do not provide it. Does the machine-level init you would otherwise hand-write.
3. **SnitchOS kernel**, runs in **S-mode**. OpenSBI hands control to it with a largely clean slate.

The thing called "bootloader" is really two things on RISC-V: the **SBI firmware** (OpenSBI, M-mode) and optionally a **separate bootloader** (U-Boot etc.). **For QEMU you need neither a separate bootloader nor your own SBI** — QEMU loads the kernel directly and bundles OpenSBI. Take that simplification.

# What SBI is
**SBI = Supervisor Binary Interface** — a *specification*, a standardized contract between M-mode firmware and the S-mode kernel. **OpenSBI** is the reference *implementation*. (Interface vs. implementation, again.)

SBI has two distinct jobs:

## Job 1 — initialize the machine before the kernel runs
Sets up M-mode trap handling, configures physical memory protection, gets the boot hart into a sane state, drops privilege to S-mode, jumps to the kernel. All the M-mode register fiddling — already done.

## Job 2 — stay resident, offer the kernel a runtime API
OpenSBI does **not** exit. It sits underneath the kernel for the whole run. The S-mode kernel calls *down* into it via the `ecall` instruction — the same instruction userspace will later use to call the kernel (`ecall` traps to the next level up). SBI calls the kernel will actually use:

- **Console putchar/getchar** — the legacy SBI console (modern version: the DBCN debug-console extension). **Lets the kernel print before any serial driver exists.**
- **Timer** — `sbi_set_timer` programs the next timer interrupt. Used in v0.3.
- **IPI / hart management** — the HSM extension; start/stop other harts. Used for SMP bringup, much later.
- **System reset** — `sbi_system_reset` cleanly shuts the machine down. Useful immediately: how a kernel integration test signals "done" and exits QEMU.

**Symmetry worth internalizing (good blog material):** OpenSBI is to the S-mode kernel what the kernel will be to userspace — the privileged layer underneath, offering services through a trap instruction.

# Kernel entry state (the SBI handoff)
When OpenSBI jumps to the kernel entry point:

- In **S-mode**.
- **One hart running** (the boot hart); others parked. Multi-hart bringup is the kernel's choice, later, via HSM.
- Register **`a0` = hart ID** of the boot hart.
- Register **`a1` = physical address of the Device Tree Blob (DTB)**.
- **MMU is off** — running on physical addresses. Setting up virtual memory (Sv39, higher-half) is the kernel's job: the v0.4 milestone.
- Interrupts off, caches sane.

That `a0`/`a1` contract is the entire handoff — tiny, compared to x86's inherited real/protected/long-mode + BIOS/UEFI baggage.

# The Device Tree (DTB)
The kernel cannot assume where RAM is, how big it is, where the UART registers live, or the timer frequency. The **Device Tree** — passed as the DTB at `a1` — is a tree of nodes describing the hardware: memory base/size, UART MMIO address, timebase frequency, etc.

Directly relevant to decisions already made:

- **The `timebase_hz` field in the `Hello` telemetry frame comes from the device tree.** Kernel parses the DTB, finds the timebase frequency, puts it in `Hello`.
- Same for discovering the UART address (serial driver) and RAM size (frame allocator).

So "parse the DTB" is a mandatory early-boot task. For QEMU `virt` the layout is stable so addresses *could* be hardcoded to start, but parsing the DTB properly is the right move and makes the real-hardware port far easier. `no_std` Rust crates exist (`fdt` etc.) — not a big yak-shave.

# How the kernel binary is loaded and run
- Compile the kernel to an ELF for a `riscv64` bare-metal target (`riscv64gc-unknown-none-elf` or a custom target).
- The **linker script** decides where code/data are placed and where the entry point is. On QEMU `virt`, RAM starts at physical `0x8000_0000`; OpenSBI occupies the bottom and hands off at a known address above itself (conventionally `0x8020_0000`). The linker script places the kernel there.
- QEMU is told `-kernel kernel.elf`; QEMU + OpenSBI load it and jump to its entry point.
- The entry point is a *tiny* piece of hand-written assembly — the one unavoidable bit. Its whole job: set up a stack pointer (nothing has set `sp` yet), then jump into Rust. ~A dozen instructions. After that it is all Rust.

The linker script is the most likely time-sink — hence the v0.1 rule: **one weekend hand-writing the bootstrap, then fall back to `riscv-rt`** (which provides exactly the entry assembly + linker script + stack-setup glue). Writing it by hand once is educational; fighting it for six weekends is not.

# Where this lands in the SnitchOS design
1. **SBI is part of the HAL story.** SBI calls are RISC-V-specific — aarch64 has no SBI; its equivalent is firmware + PSCI + direct hardware access. So "call SBI for timer/console/reset" lives **below the HAL line**, in `arch-riscv64`. The portable kernel says "set a timer"; the RISC-V backend implements it as an SBI call; aarch64 implements it differently. A concrete instance of the parked HAL boundary.
2. **The SBI console gives v0.1 a free head start.** "The kernel prints something" can work via the SBI debug console *before* the UART driver exists. Writing the real 16550 UART driver then becomes its own clean step rather than a prerequisite for any output. Plan: v0.1 uses the SBI console for earliest bring-up, then moves to a real UART driver for the serial channel; the *telemetry* channel is a separate virtio device regardless.

# v0.1 boot task order (implied)
1. Entry assembly: set `sp`, jump to Rust.
2. Earliest output via SBI console.
3. Parse the DTB (`a1`) — discover RAM, UART address, `timebase_hz`.
4. Bring up the real UART driver (serial channel).
5. Bring up the virtio-console telemetry channel.
6. Emit the `Hello` frame (with `timebase_hz` from the DTB).
7. Boot-phase spans, `BootComplete` marker, heartbeat loop.

# Open follow-ups (not blocking)
- Trap model detail (S-mode trap CSRs: `stvec`, `scause`, `sepc`, `sstatus`) — needed for v0.3.
- The MMU-off → Sv39 transition mechanics — needed for v0.4.
- DTB parsing crate choice (`fdt` vs alternatives).
- Custom target JSON vs the stock `riscv64gc-unknown-none-elf` target.
