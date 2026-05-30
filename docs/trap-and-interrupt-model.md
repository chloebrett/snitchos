# ⚡ Trap & interrupt model — reference

# Floating-point registers and context switch
The FPU has its own large register set, separate from the general-purpose registers. Policy: **kernel code is integer-only** (see Concepts & findings). Because the kernel never uses floats, the trap path can *skip saving/restoring FP registers* on a kernel trap — a real saving on the hottest path.

But userspace *does* use floats (audio DSP especially). So the **context-switch** path must correctly save and restore FP registers *for userspace threads* — the kernel does not use floats but must preserve them across switches, or it corrupts the float state of whatever thread was interrupted.

Optimization worth knowing: **lazy FP save** — do not save/restore FP state on every context switch; only when a thread actually touches the FPU, detected by a trap on first FP use after a switch. Threads that never use floats never pay the FP-save cost. A real trap/scheduler design choice the audio work may pull in, and good blog material.

# HAL note
Trap entry/exit assembly, the CSRs, and the trap-frame layout are all RISC-V-specific — they live below the HAL line in `arch-riscv64`. The portable kernel sees a dispatch entry point and a `TrapFrame` abstraction; aarch64 implements the same shape with its own exception-level machinery.
