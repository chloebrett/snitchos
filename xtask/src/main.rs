use std::process::{Command, ExitCode};

const KERNEL_TARGET: &str = "riscv64gc-unknown-none-elf";
const KERNEL_BIN: &str = "target/riscv64gc-unknown-none-elf/debug/kernel";
const HOST_READER_BIN: &str = "target/debug/host-reader";
const TELEMETRY_SOCKET: &str = "/tmp/snitch-telemetry.sock";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");

    match cmd {
        "up" => up(),
        "build" => build(),
        "reader" => reader(&args[1..]),
        "help" | "-h" | "--help" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown subcommand: {other}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!("usage: cargo xtask <subcommand>");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  build   build the kernel ELF");
    eprintln!("  up      build the kernel and run it in QEMU");
    eprintln!("  reader  build and run the host-reader");
}

fn build() -> ExitCode {
    let status = Command::new("cargo")
        .args(["build", "-p", "kernel", "--target", KERNEL_TARGET])
        .status()
        .expect("failed to invoke cargo");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn up() -> ExitCode {
    if build() != ExitCode::SUCCESS {
        return ExitCode::from(1);
    }

    // Clean up any stale socket from a previous run so QEMU can bind.
    let _ = std::fs::remove_file(TELEMETRY_SOCKET);

    // wait=on blocks QEMU at startup until a telemetry client connects.
    // Run `cargo xtask reader` in another terminal to satisfy that wait.
    let chardev_arg =
        format!("socket,path={TELEMETRY_SOCKET},server=on,wait=on,id=telemetry");

    let status = Command::new("qemu-system-riscv64")
        .args([
            "-machine", "virt",
            "-cpu", "rv64",
            "-smp", "1",
            "-m", "128M",
            "-nographic",
            "-bios", "default",
            "-kernel", KERNEL_BIN,
            // Force modern virtio-mmio (version 2). Without this, QEMU
            // exposes the legacy (version 1) layout for backward compat,
            // which has a different register set we don't implement.
            "-global", "virtio-mmio.force-legacy=false",
            // Telemetry channel: a virtio-console wired to a Unix domain
            // socket on the host. host-reader connects to this socket.
            "-chardev", &chardev_arg,
            "-device", "virtio-serial-device",
            "-device", "virtconsole,chardev=telemetry",
        ])
        .status()
        .expect("failed to invoke qemu-system-riscv64");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn reader(extra_args: &[String]) -> ExitCode {
    let build = Command::new("cargo")
        .args(["build", "-p", "host-reader"])
        .status()
        .expect("failed to invoke cargo");
    if !build.success() {
        return ExitCode::from(1);
    }

    let status = Command::new(HOST_READER_BIN)
        .args(extra_args)
        .status()
        .expect("failed to invoke host-reader");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
