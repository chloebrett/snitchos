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
  /* The `#[entry(in, out, uses)]` manifest note. KEEP so `--gc-sections` can't
   * drop it; named outside `.note.*` so the discard below doesn't swallow it.
   * The seed step reads these bytes from the ELF to populate the `user.iface`
   * xattr. (Loadable today for simplicity; a non-alloc section is a refinement.) */
  .snitch.iface : {
    KEEP(*(.snitch.iface))
  } > ram
  /* Page-align the writable segment so it never shares a 4 KiB page with
   * R-X code — lets the kernel map each PT_LOAD with real W^X. The RW
   * PT_LOAD starts at whichever of .data/.bss comes first, so BOTH must be
   * aligned: .data is empty in every program today, which is the only
   * reason aligning .bss alone sufficed. One initialised mutable static
   * would have started the RW segment mid-page — sharing page 0 with R-X
   * code and silently producing an RWX page. (The linker does not
   * page-align new PT_LOADs for us here: today's .rodata segment starts
   * mid-page at 0x100006B0.) The kernel now refuses such an image
   * outright — kernel_core::elf::page_perms, PlanError::WxViolation — so
   * this alignment is what keeps that refusal from firing at boot.
   *
   * NOTE: the alignment MUST be the section's ALIGN() attribute (after the
   * `:`). A bare `. = ALIGN(0x1000);` between sections is silently ignored
   * when sections use a `> region` (`> ram` here): the section start comes
   * from the region's allocation pointer, not the location counter. */
  .data : ALIGN(0x1000) {
    *(.data .data*)
    *(.sdata .sdata*)
  } > ram
  .bss : ALIGN(0x1000) {
    __bss_start = .;
    *(.bss .bss*)
    *(.sbss .sbss*)
    /* Reserve the user stack INSIDE .bss so it lands in a loadable
     * segment with memsz > filesz (zero-filled at load). Reserving it
     * outside any section only advances the VA counter and produces no
     * PT_LOAD — the stack would be unmapped and sp would fault. */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 0x80000;            /* 512 KiB user stack — the Stitch tree-walk
                              * interpreter is deeply recursive (recursive-descent
                              * parse + recursive eval over the whole prelude); 16
                              * KiB silently overflowed it. */
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
