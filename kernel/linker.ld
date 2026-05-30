ENTRY(_start)
MEMORY {
  ram (rwx) : ORIGIN = 0x80200000, LENGTH = 128M
}
SECTIONS {
  .text : { *(.text .text*) } > ram
  .rodata : { *(.rodata .rodata*) } > ram
  .data : { *(.data .data*) } > ram
  .bss : { *(.bss .bss*) } > ram
  . = ALIGN(16);
  __stack_bottom = .;
  . += 0x10000;
  __stack_top = .;

  /DISCARD/ : {
    *(.eh_frame .eh_frame.*)
    *(.eh_frame_hdr)
    *(.note .note.*)
    *(.comment)
  }
}