# Secondary-hart entry point. Called by OpenSBI after a successful
# sbi_hart_start. On entry:
#   a0 = hartid (SBI handoff contract — the platform mhartid)
#   a1 = opaque (we pass the logical hart id; same as a0 today)
#   MMU is off, interrupts are off, sp is undefined.
#
# Order of operations (must run with MMU off, then transition to
# higher-half before calling any Rust function whose symbol the
# linker resolved at higher-half VA):
#
#   1. Set sp to the secondary's pre-allocated stack (PA from `la`).
#   2. Load SECONDARY_SATP (hart 0 wrote it before bring-up) and
#      install it. Both identity and higher-half mappings are live
#      until hart 0 calls unmap_identity (which is gated on
#      SECONDARY_READY, so we're safe up to and including the
#      trampoline).
#   3. Trampoline to higher-half: shift sp + jump target by
#      KERNEL_OFFSET.
#   4. Call secondary_main(hartid).
#
# Why a separate stack: hart 0's __stack_top is in use by hart 0.
# Hart 1 can't share it without corrupting hart 0's frames.

.section .text.boot
.globl _secondary_start
_secondary_start:
    # sp = value stored at SECONDARY_STACK_TOP (the actual top-of-stack
    # address, written by prepare_for_secondary from hart 0).
    la  t0, SECONDARY_STACK_TOP
    ld  sp, 0(t0)

    # SATP from the static hart 0 stashed before calling sbi_hart_start.
    la  t0, SECONDARY_SATP
    ld  t0, 0(t0)
    csrw satp, t0
    sfence.vma

    # Trampoline to higher-half. KERNEL_OFFSET is a 64-bit constant
    # that doesn't fit in any immediate, so load it from a static.
    la  t1, KERNEL_OFFSET_VALUE
    ld  t1, 0(t1)
    # sp was loaded from SECONDARY_STACK_TOP, which prepare_for_secondary
    # filled with `&SECONDARY_STACK + STACK_SIZE` — taken via `&raw const`,
    # which yields a higher-half VA already. No shift needed.
    lla t0, 1f
    add t0, t0, t1               # t0 = higher-half VA of 1f
    jr  t0
1:
    # Now at higher-half PC + sp. Calls resolve to higher-half VAs
    # by linker. a0 still holds the hartid SBI handed us; a1 holds
    # opaque (the logical hart id we passed).
    call secondary_main
    # secondary_main is `-> !` so this never returns. Park anyway.
2:  wfi
    j   2b
