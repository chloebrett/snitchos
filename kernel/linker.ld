ENTRY(_start)
MEMORY {
  ram (rwx) : ORIGIN = 0x80200000, LENGTH = 128M
}
SECTIONS {
  .text : {
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