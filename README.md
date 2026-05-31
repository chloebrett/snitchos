# snitchos

The operating system that snitches on itself 🐀

## notes

until we have xtask:

```
cargo build -p kernel --target riscv64gc-unknown-none-elf

cargo objdump -p kernel --target riscv64gc-unknown-none-elf -- -h
```

```
 - Ctrl-A x — quit QEMU.
  - Ctrl-A c — toggle to QEMU's monitor (a debug shell). Same combo again to return.
  - Ctrl-A h — help
```
