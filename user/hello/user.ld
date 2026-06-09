/* User program link layout. Position-dependent, fixed at a low-half VA
 * (the kernel maps user segments at p_vaddr verbatim — no relocations).
 * 0x10000000 is well clear of the kernel's higher-half space and the
 * Sv39 low-half is otherwise empty in v0.7a. Keep this VA in sync with
 * the kernel-side loader; the ELF's e_entry is read from here. */
ENTRY(_start)
MEMORY {
  ram (rwx) : ORIGIN = 0x10000000, LENGTH = 16M
}
SECTIONS {
  . = 0x10000000;
  .text : {
    KEEP(*(.text._start))   /* entry stub first, by convention */
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
    /* Reserve the user stack INSIDE .bss so it lands in a loadable
     * segment with memsz > filesz (zero-filled at load). Reserving it
     * outside any section only advances the VA counter and produces no
     * PT_LOAD — the stack would be unmapped and sp would fault. */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 0x4000;             /* 16 KiB user stack */
    __stack_top = .;
    __bss_end = .;
  } > ram

  /DISCARD/ : {
    *(.eh_frame .eh_frame.*)
    *(.eh_frame_hdr)
    *(.note .note.*)
    *(.comment)
  }
}
