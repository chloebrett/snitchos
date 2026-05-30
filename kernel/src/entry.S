/* OpenSBI hands control here in S-mode with:
 *   a0 = hart id
 *   a1 = DTB physical address
 * MMU is off, interrupts off. Nothing has set sp.
 *
 * Job of this stub: set sp, zero .bss, jump to Rust. Keep a0/a1 intact so
 * kmain receives them as its first two arguments. */
.section .text.boot, "ax"
.globl _start
_start:
    la      sp, __stack_top

    /* Zero .bss before any Rust runs. */
    la      t0, __bss_start
    la      t1, __bss_end
1:  bgeu    t0, t1, 2f
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       1b
2:

    call    kmain

    /* kmain is `-> !`, but if it ever returns, park. */
3:  wfi
    j       3b
