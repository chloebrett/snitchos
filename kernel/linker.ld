OUTPUT_ARCH(riscv)
ENTRY(_start)

/* OpenSBI on QEMU `virt` lives at 0x8000_0000 and hands off to the kernel
 * at 0x8020_0000 by convention. RAM is configurable via -m; reserve 128M. */
MEMORY {
    RAM (rwx) : ORIGIN = 0x80200000, LENGTH = 128M
}

SECTIONS {
    . = ORIGIN(RAM);

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    } > RAM

    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    } > RAM

    .data : ALIGN(8) {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } > RAM

    .bss : ALIGN(8) {
        __bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(8);
        __bss_end = .;
    } > RAM

    . = ALIGN(16);
    __stack_bottom = .;
    . += 64K;
    . = ALIGN(16);
    __stack_top = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
