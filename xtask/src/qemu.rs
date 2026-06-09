//! Shared QEMU invocation helpers used by both `cargo xtask boot` and
//! the integration-test harness. Keeping the args in one place means
//! a QEMU flag change (e.g. for higher-half kernels in v0.4) only
//! needs to be made once.

use std::process::Command;

pub const KERNEL_BIN: &str = "target/riscv64gc-unknown-none-elf/debug/kernel";
pub const KERNEL_TARGET: &str = "riscv64gc-unknown-none-elf";
/// The first userspace program, built for the same bare riscv target as
/// the kernel but linked at a low-half VA via its own `user.ld`. The
/// kernel embeds this ELF and loads it in v0.7a Step 4.
pub const USER_HELLO_BIN: &str = "target/riscv64gc-unknown-none-elf/debug/hello";

/// Build a `Command` pre-loaded with every QEMU arg that is common to
/// all invocations. The caller finishes it with `.status()` or
/// `.spawn()` and any additional stdio config.
pub fn base_command(chardev_arg: &str) -> Command {
    let mut cmd = Command::new("qemu-system-riscv64");
    cmd.args([
        "-machine", "virt",
        "-cpu", "rv64",
        // v0.6: two harts. Hart 0 is the boot hart; hart 1 is
        // parked in OpenSBI until kmain calls sbi_hart_start.
        "-smp", "2",
        // Multi-thread TCG. Default `thread=single` multiplexes all
        // VCPUs on one host thread, which under -smp 2 starves
        // whichever hart isn't currently executing — the symptom
        // we hit was hart 0's timer IRQ skipping 8 sim-seconds
        // because hart 1 dominated emulation. `thread=multi` gives
        // each VCPU its own host thread, restoring fair timer
        // delivery. Required for reliable suite runs.
        "-accel", "tcg,thread=multi",
        "-m", "128M",
        "-nographic",
        "-bios", "default",
        "-kernel", KERNEL_BIN,
        // Force modern virtio-mmio (version 2). Without this, QEMU
        // exposes the legacy (version 1) layout for backward compat,
        // which has a different register set we don't implement.
        "-global", "virtio-mmio.force-legacy=false",
        // Telemetry channel: a virtio-console wired to a Unix domain
        // socket on the host. The collector connects to this socket.
        "-chardev", chardev_arg,
        "-device", "virtio-serial-device",
        "-device", "virtconsole,chardev=telemetry",
    ]);
    cmd
}

/// Invoke `cargo build -p kernel` with optional cargo features.
/// Returns the exit status.
pub fn build_kernel(features: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "-p", "kernel", "--target", KERNEL_TARGET]);
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    cmd.status()
}

/// Build the `hello` userspace program for the bare riscv target. The
/// kernel embeds the resulting ELF (`USER_HELLO_BIN`) and loads it; build
/// this before the kernel so the embedded copy is current.
pub fn build_user() -> std::io::Result<std::process::ExitStatus> {
    Command::new("cargo")
        .args(["build", "-p", "hello", "--target", KERNEL_TARGET])
        .status()
}
