/* S-mode trap entry, exit, and Rust dispatch.
 *
 * On any trap, the CPU jumps here (because `stvec` will be set to
 * `trap_entry`). We save all 30 GPRs + sepc + sstatus into a TrapFrame
 * on the current stack, then call the Rust dispatcher with a pointer
 * to the frame. After the dispatcher returns, we restore everything
 * and `sret`.
 *
 * Frame layout — 256 bytes of saved state + 32 bytes of headroom = 288.
 *
 *   offset  field
 *      0    ra   (x1)
 *      8    sp   (x2)  — caller's sp, saved via t0 below
 *     16    gp   (x3)
 *     24    tp   (x4)
 *     32    t0   (x5)
 *     40    t1   (x6)
 *     48    t2   (x7)
 *     56    s0   (x8)
 *     64    s1   (x9)
 *     72    a0   (x10)
 *     80    a1   (x11)
 *     88    a2   (x12)
 *     96    a3   (x13)
 *    104    a4   (x14)
 *    112    a5   (x15)
 *    120    a6   (x16)
 *    128    a7   (x17)
 *    136    s2   (x18)
 *    144    s3   (x19)
 *    152    s4   (x20)
 *    160    s5   (x21)
 *    168    s6   (x22)
 *    176    s7   (x23)
 *    184    s8   (x24)
 *    192    s9   (x25)
 *    200    s10  (x26)
 *    208    s11  (x27)
 *    216    t3   (x28)
 *    224    t4   (x29)
 *    232    t5   (x30)
 *    240    t6   (x31)
 *    248    sepc
 *    256    sstatus
 */

.section .text
.globl trap_entry
.align 4
trap_entry:
    # Get onto a trusted kernel stack before touching memory. A trap from
    # U-mode arrives with sp = the user's stack pointer (attacker-chosen);
    # spilling registers there would be a kernel write-what-where. By
    # convention sscratch == 0 in the kernel, == this thread's kernel stack
    # top while in user. Swap sp <-> sscratch atomically (one insn, so the
    # user's sp is preserved in sscratch rather than lost):
    csrrw sp, sscratch, sp
    #   from user:   sscratch held kstk -> sp = kstk (good), sscratch = usp
    #   from kernel: sscratch held 0    -> sp = 0,           sscratch = ksp
    bnez sp, 1f              # sp != 0 -> came from user (normal path at 1:)

    # --- from kernel (sp == 0; sscratch holds the kernel sp) ---
    # Run the handler on this hart's *exception stack*, not the kernel stack we
    # trapped from, so a kernel-stack-overflow guard fault can build its trap
    # frame on known-good memory instead of double-faulting on the overflowed
    # stack. The original kernel sp is saved at frame offset 8, so the shared exit
    # (`ld sp, 8(sp)`) returns onto it. See plans/kernel-stack-hardening.md.
    csrrw sp, sscratch, sp   # sp = ksp, sscratch = 0 (heal the convention)
    csrw sscratch, t0        # park t0 as the one scratch reg (ints masked here)
    ld t0, 24(tp)            # t0 = this hart's exc_stack_top (percpu EXC_STACK_TOP_OFFSET)
    beqz t0, 5f              # 0 = pre-init (tp not ready): fall back to current stack
    addi t0, t0, -288        # exception-stack frame base
    sd sp, 8(t0)             # caller sp (= ksp) -> frame offset 8 (sp still = ksp)
    mv sp, t0                # switch onto the exception-stack frame
    csrr t0, sscratch        # restore the trapped t0
    csrw sscratch, zero      # re-heal sscratch == 0
    sd ra,  0(sp)            # x1
    sd gp, 16(sp)            # x3
    sd tp, 24(sp)            # x4
    sd t0, 32(sp)            # x5
    j 3f                     # offset 8 already stored above
5:                           # pre-init fallback: build the frame on the current stack
    csrr t0, sscratch        # restore the trapped t0
    csrw sscratch, zero      # re-heal sscratch == 0
    addi sp, sp, -288
    sd ra,  0(sp)
    sd gp, 16(sp)
    sd tp, 24(sp)
    sd t0, 32(sp)
    addi t0, sp, 288         # caller sp = frame top (the kernel sp)
    sd t0, 8(sp)
    j 3f

1:  # --- from user (sp = kstk, sscratch = parked user sp) ---
    addi sp, sp, -288
    sd ra,  0(sp)        # x1
    sd gp, 16(sp)        # x3
    sd tp, 24(sp)        # x4
    sd t0, 32(sp)        # x5 — t0 is safely saved now, use it as scratch
    csrr t0, sscratch    # caller sp = parked user sp
    sd t0, 8(sp)
3:

    # Continue saving the rest of the GPRs.
    sd t1, 40(sp)        # x6
    sd t2, 48(sp)        # x7
    sd s0, 56(sp)        # x8
    sd s1, 64(sp)        # x9
    sd a0, 72(sp)        # x10
    sd a1, 80(sp)        # x11
    sd a2, 88(sp)        # x12
    sd a3, 96(sp)        # x13
    sd a4, 104(sp)       # x14
    sd a5, 112(sp)       # x15
    sd a6, 120(sp)       # x16
    sd a7, 128(sp)       # x17
    sd s2, 136(sp)       # x18
    sd s3, 144(sp)       # x19
    sd s4, 152(sp)       # x20
    sd s5, 160(sp)       # x21
    sd s6, 168(sp)       # x22
    sd s7, 176(sp)       # x23
    sd s8, 184(sp)       # x24
    sd s9, 192(sp)       # x25
    sd s10, 200(sp)      # x26
    sd s11, 208(sp)      # x27
    sd t3, 216(sp)       # x28
    sd t4, 224(sp)       # x29
    sd t5, 232(sp)       # x30
    sd t6, 240(sp)       # x31

    # Save sepc and sstatus via t0 (already saved above; safe to clobber).
    csrr t0, sepc
    sd t0, 248(sp)
    csrr t0, sstatus
    sd t0, 256(sp)

    # Pass the trap frame pointer to Rust as the first arg.
    mv a0, sp

    # Hand off to the Rust dispatcher.
    call trap_handler

    # Restore sstatus and sepc via t0 as scratch. Do these *before*
    # restoring t0 itself, since these CSR writes clobber t0.
    ld t0, 256(sp)
    csrw sstatus, t0
    ld t0, 248(sp)
    csrw sepc, t0

    # Re-arm sscratch for the NEXT trap, based on where we return to
    # (SPP read from the sstatus we just restored; t0 free until the GPR
    # reload below):
    #   to user (SPP=0): park this thread's kernel stack top (frame top =
    #                    sp + 288) so the next trap entry swaps onto it.
    #   to kernel (SPP=1): sscratch is already 0 by the in-kernel
    #                      convention (the entry undo restored it) — leave it.
    ld t0, 256(sp)
    andi t0, t0, 0x100
    bnez t0, 4f
    addi t0, sp, 288
    csrw sscratch, t0
4:

    # Restore all GPRs. The sp slot at offset 8 is restored last, below,
    # from the saved caller sp (correct for both user- and kernel-origin
    # traps — see the path-dependent save at entry).
    ld ra,   0(sp)       # x1
    ld gp,  16(sp)       # x3
    ld tp,  24(sp)       # x4
    ld t0,  32(sp)       # x5 — restored to trapped value
    ld t1,  40(sp)       # x6
    ld t2,  48(sp)       # x7
    ld s0,  56(sp)       # x8
    ld s1,  64(sp)       # x9
    ld a0,  72(sp)       # x10
    ld a1,  80(sp)       # x11
    ld a2,  88(sp)       # x12
    ld a3,  96(sp)       # x13
    ld a4, 104(sp)       # x14
    ld a5, 112(sp)       # x15
    ld a6, 120(sp)       # x16
    ld a7, 128(sp)       # x17
    ld s2, 136(sp)       # x18
    ld s3, 144(sp)       # x19
    ld s4, 152(sp)       # x20
    ld s5, 160(sp)       # x21
    ld s6, 168(sp)       # x22
    ld s7, 176(sp)       # x23
    ld s8, 184(sp)       # x24
    ld s9, 192(sp)       # x25
    ld s10, 200(sp)      # x26
    ld s11, 208(sp)      # x27
    ld t3, 216(sp)       # x28
    ld t4, 224(sp)       # x29
    ld t5, 232(sp)       # x30
    ld t6, 240(sp)       # x31

    # Restore sp from the saved caller-sp slot, which also deallocates
    # the frame. Correct for BOTH paths: kernel-origin -> frame top
    # (== old `addi sp, sp, 288`), user-origin -> the parked user sp.
    ld sp, 8(sp)

    # Return from supervisor trap. CPU jumps to sepc and restores SIE
    # from SPIE.
    sret
