Serial console, before anything else:

```
ls /dev/cu.*
screen /dev/cu.usbserial-0001 115200
```

**Use `cu.*`, not `tty.*`.** On macOS `tty.*` is the call-in node — `open()` blocks
until carrier detect, which a USB-TTL adapter never asserts, so `screen` hangs with a
blank window and no error. `cu.*` is the call-out node and skips the wait.

**A blank screen has four causes and they look identical:** the terminal is on the wrong
node (above); another process holds the port (`lsof /dev/cu.usbserial-0001` names it —
a stale `screen` on the `tty.*` node blocks the `cu.*` open too, so kill it first); the
wiring is wrong (board **pin 6 → GND**, **pin 8 UART0 TX → adapter RX**, **pin 10 UART0
RX → adapter TX**, crossed, 115200 8N1); or the board genuinely isn't booting (boot-mode
switches — `RGPIO_1,RGPIO_0 = 1,1` is UART-recovery, not flash boot).

Bisect with a reset while attached: **seeing the SPL/OpenSBI banner means the cable and
port are good and the fault is ours.** Until you have that banner, nothing about the
kernel is implicated — U-Boot runs before `booti`.

U-Boot, from fresh:
```
dhcp
setenv serverip 192.168.0.7
setenv fdt_high 0x48000000
tftpboot 0x40200000 snitchos.img
setenv bootargs 'workload=stitch-drivel'
booti 0x40200000 - ${fdtcontroladdr}
```

**`fdt_high` is not optional above ~3 MB.** Without it, `booti … ${fdtcontroladdr}`
fails outright with `Failed to reserve memory for fdt at 0xff7105e0` — U-Boot tries to
reserve its own live DTB *in place*, up where it relocated itself, which is outside the
memory region LMB manages; setting `fdt_high` makes it copy the blob down into ordinary
RAM instead. It first bit at 7.78 MB (the `kvetch-drivel` image, ~4.5 MB of which is
weights) and nothing about the message points at image size.

Build the image first — `cargo xtask image --workload <name>` (validates the name against
the workload registry and prints the matching `setenv` line; the build itself is
workload-independent, since every image reads `/chosen/bootargs`). Output that doesn't
match the source is a stale `snitchos.img` until proven otherwise.

Leave `console=` off for an interactive workload. The default (`console=text`) is what
puts the kernel log and every userspace `ConsoleWrite` on this UART; `console=frames`
diverts them to the telemetry wire, which is correct for the collector and looks exactly
like a dead board on a terminal.
