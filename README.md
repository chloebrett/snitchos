# snitchos

The operating system that snitches on itself 🐀

## Status

**v0.1: "Hello, traced world"** — *complete*. Kernel boots on RISC-V in QEMU, emits a structured boot-phase span tree over a dedicated virtio-console channel, and host-side reader decodes and prints it.

Working:

- no_std kernel; handwritten boot stub + linker script; ns16550a UART driver
- DTB parse (memory, UART, timebase)
- virtio-console driver: discovery + modern-spec handshake + virtqueue + TX
- `protocol` crate: 7-variant postcard-encoded `Frame` enum, hosted TDD
- `collector`: connects to QEMU's Unix socket; decodes frames from a stream
- `tracing` module: timestamps from the `time` CSR, string intern table, RAII-guarded spans via the `span!` macro, pre-init buffering with a `Dropped { count }` checkpoint after flush
- `kernel.boot` opens at boot with `console_init` + `telemetry_init` sub-spans, then a `kernel.heartbeat` span emitted once per timebase tick
- `xtask` orchestration: `cargo xtask up` / `cargo xtask reader`

Up next: **v0.2 (Grafana)** — replace the stdout reader with Tempo / Prometheus / Grafana running in docker-compose; add structured metrics alongside spans.

See [posts/](posts/) for the per-milestone devlog.

## Quick start

Use two terminals:

```
# Terminal A — kernel + QEMU. Waits at the telemetry chardev until the
# reader connects in terminal B.
cargo xtask up

# Terminal B — collector. Connects to the telemetry socket,
# decodes Frames, prints them.
cargo xtask reader

# Optional: pretty (multi-line) frame output.
cargo xtask reader -- --pretty
```

Quit QEMU with `Ctrl-A x`.

## Subcommands

```
cargo xtask build      # build the kernel ELF
cargo xtask up         # build kernel + run in QEMU (telemetry waits for reader)
cargo xtask reader     # build + run collector
cargo xtask reader -- --pretty   # multi-line debug output
cargo xtask --help
```

## Reading

- [docs/README.md](docs/README.md) — design overview (the three pillars: observability, capabilities, microkernel).
- [docs/v0.1-hello-traced-world.md](docs/v0.1-hello-traced-world.md) — the v0.1 milestone plan.
- [posts/](posts/) — devlog notes as we go.
- [plans/virtio-console.md](plans/virtio-console.md) — virtio-console implementation plan.

## Workspace layout

```
kernel/         no_std RISC-V S-mode kernel; entry.S, linker.ld, drivers
protocol/       postcard-encoded telemetry Frame enum (no_std)
collector/    host-side reader; connects to the telemetry socket
xtask/          orchestration commands (this file's "Quick start")
docs/           project design + milestone plans
plans/          in-progress implementation plans
posts/          devlog notes
```

## QEMU controls

- `Ctrl-A x` — quit QEMU.
- `Ctrl-A c` — toggle to QEMU's monitor (debug shell). Same combo again to return.
- `Ctrl-A h` — help.

## Useful one-offs

Dump the QEMU `virt` machine's device tree (binary → readable):

```
qemu-system-riscv64 -machine virt -machine dumpdtb=virt.dtb
brew install dtc           # one-time
dtc -I dtb -O dts virt.dtb -o virt.dts
```

Inspect the kernel ELF's section layout:

```
cargo objdump -p kernel --target riscv64gc-unknown-none-elf -- -h
```

(needs `rustup component add llvm-tools-preview` and `cargo install cargo-binutils`)
