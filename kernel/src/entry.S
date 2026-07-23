.section .text.entry
.globl _start
_start:
  # RISC-V Image header (Linux Documentation/riscv/boot-image-header.rst), so
  # U-Boot `booti` can load + enter the kernel on the VisionFive 2: booti jumps to
  # the image start (this label), so `code0` must jump past the 64-byte header to
  # the real entry. Harmless on QEMU, which enters here via the ELF `e_entry` — the
  # same jump. `text_offset` is RAM-base-relative (+2 MiB), so one header serves
  # both QEMU (`0x8020_0000`) and the board (`0x4020_0000`). See B2/M0.5 in
  # plans/visionfive2-port.md.
  # `code0` must be a *4-byte* instruction (a compressed `c.j` would shift every
  # header field), so force JAL; explicit `.4byte`/`.8byte` sizes keep the layout
  # exact.
  .option push
  .option norvc
  j real_start          # code0 (4-byte JAL): jump over the header to the real entry
  .option pop
  .4byte 0              # code1 (unused; the header data below is never executed)
  .8byte 0x200000       # text_offset: load addr = RAM_BASE + 0x200000
  .8byte __kernel_end - _start  # image_size: memory footprint. U-Boot booti rejects
                                # a zero image_size ("Image lacks image_size field").
                                # Resolved at link time (RISC-V ADD64/SUB64 relocs).
  .8byte 0              # flags (bit 0 = 0 -> little-endian)
  .4byte 0x00000002     # version: (major 0 << 16) | minor 2
  .4byte 0              # res1
  .8byte 0              # res2
  .8byte 0              # magic (deprecated in v0.2 -> 0)
  .4byte 0x05435352     # magic2 = "RSC\x05" (little-endian)
  .4byte 0              # res4 (PE/COFF header offset — unused)
real_start:             # 64 bytes past _start
  la sp, __stack_top
  la t0, __bss_start
  la t1, __bss_end
1: bgeu t0, t1, 2f
  sd zero, 0(t0)
  addi t0, t0, 8
  j 1b
2:
  call kmain
  # if kmain ever returns (it shouldn't), park
3: wfi
  j 3b
