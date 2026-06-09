/* U-mode entry stub. The kernel sets the initial PC to `_start` (read
 * from the ELF's e_entry) and drops to U-mode via `sret`. We set up our
 * own stack, then call into Rust. `rust_main` never returns; the trailing
 * loop is belt-and-suspenders. */
.section .text._start
.globl _start
_start:
    la sp, __stack_top      # medany: pc-relative, valid at our link VA
    call rust_main
1:  j 1b
