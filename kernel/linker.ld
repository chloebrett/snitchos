ENTRY(_start)
MEMORY {
  ram (rwx) : ORIGIN = 0xffffffff80200000, LENGTH = 128M
}
/* VMA in higher-half (matches Linux RISC-V's PAGE_OFFSET). LMA is the
 * physical address OpenSBI loads us at; AT(0x80200000) anchors LMAs
 * on .text, and subsequent sections follow contiguously. */
SECTIONS {
  __kernel_start = ORIGIN(ram);
  .text : AT(0x80200000) {
    /* `_start` must be at PA 0x80200000 — that's where OpenSBI jumps
     * the boot hart. `.text.entry` is the dedicated section just for
     * the entry stub (entry.S) so it's never reordered relative to
     * other boot-time asm (`.text.boot`, e.g. secondary.S). */
    KEEP(*(.text.entry))
    KEEP(*(.text.boot))
    *(.text .text*)
  } > ram
  .rodata : {
    *(.rodata .rodata*)
    *(.srodata .srodata*)
  } > ram
  .data : {
    *(.data .data*)
    *(.sdata .sdata*)
  } > ram
  .bss : {
    __bss_start = .;
    *(.bss .bss*)
    *(.sbss .sbss*)
    __bss_end = .;
  } > ram
  /* Boot stack (task 0), with a 4 KiB guard page below it. The page is
   * page-aligned so a single PTE covers it; after MMU enable the kernel splits
   * the 2 MiB kernel-image leaf covering it and unmaps this page, so a boot-stack
   * overflow faults here instead of corrupting .bss. See kernel-stack-hardening. */
  . = ALIGN(4096);
  __boot_stack_guard = .;
  . += 0x1000;
  __stack_bottom = .;
  . += 0x10000;
  __stack_top = .;
  __kernel_end = .;

  /DISCARD/ : {
    *(.eh_frame .eh_frame.*)
    *(.eh_frame_hdr)
    *(.note .note.*)
    *(.comment)
  }
}