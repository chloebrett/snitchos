/* U-mode entry stub (crt0). The kernel sets the initial PC to `_start`
 * (read from the ELF's e_entry) and drops to U-mode via `sret`. We set up
 * our own stack (`__stack_top` from the program's `user.ld`), then call
 * into Rust. `__snitchos_start` never returns (it `exit`s after `main`);
 * the trailing loop is belt-and-suspenders. Lives in the shared runtime
 * crate; every program links it. */
.section .text._start
.globl _start
_start:
    la sp, __stack_top      # medany: pc-relative, valid at our link VA
    call __snitchos_start    # runtime entry: init heap, publish caps, run main, exit
1:  j 1b
