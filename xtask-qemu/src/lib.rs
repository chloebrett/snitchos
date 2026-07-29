//! Shared QEMU invocation helpers used by both `cargo xtask boot` and
//! the integration-test harness. Keeping the args in one place means
//! a QEMU flag change (e.g. for higher-half kernels in v0.4) only
//! needs to be made once.

use std::process::Command;

pub const KERNEL_BIN: &str = "target/riscv64gc-unknown-none-elf/debug/kernel";
pub const KERNEL_TARGET: &str = "riscv64gc-unknown-none-elf";

/// Default guest RAM (MiB) — the machine size for every workload except the
/// deliberately-small ones (see `snemu_diff::ram_mb_for`).
pub const DEFAULT_RAM_MB: u32 = 128;

/// Build a `Command` pre-loaded with every QEMU arg that is common to
/// all invocations, with `ram_mb` guest RAM. The caller finishes it with
/// `.status()` or `.spawn()` and any additional stdio config.
///
/// Headless (`-nographic`), no `ramfb` device — the shape every existing
/// caller (itest harness, `measure`, `snemu-diff`) needs. For a ramfb-
/// enabled and/or on-screen invocation, use [`base_command_ex`].
pub fn base_command(chardev_arg: &str, ram_mb: u32, opt: OptLevel) -> Command {
    base_command_ex(chardev_arg, ram_mb, false, None, opt)
}

/// Like [`base_command`], with two independent extras:
/// - `ramfb`: add `-device ramfb`, so the guest sees an `etc/ramfb`
///   `fw_cfg` file. Off by default — most invocations (itests, measure,
///   snemu-diff) don't touch the display path and shouldn't pay for it.
/// - `display`: a QEMU `-display` backend (e.g. `"cocoa"`, `"gtk"`) to
///   show an actual window, replacing `-nographic` (the two conflict —
///   `-nographic` forces `-display none`). `None` keeps the existing
///   headless behaviour.
pub fn base_command_ex(
    chardev_arg: &str,
    ram_mb: u32,
    ramfb: bool,
    display: Option<&str>,
    opt: OptLevel,
) -> Command {
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
    ]);
    cmd.args(["-m", &format!("{ram_mb}M")]);
    match display {
        Some(backend) => {
            cmd.args(["-display", backend]);
        }
        None => {
            cmd.args(["-nographic"]);
        }
    }
    cmd.args([
        "-bios", "default",
        // The kernel ELF for this opt regime: `debug/kernel` for Low, `release/kernel`
        // for Mid/High. Matches whatever `build_kernel_profiled(_, opt)` just wrote.
        "-kernel", kernel_bin(opt.is_release()),
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
    if ramfb {
        cmd.args(["-device", "ramfb"]);
    }
    cmd
}

/// Invoke `cargo build -p kernel` with optional cargo features.
/// Returns the exit status.
///
/// The kernel's `build.rs` builds and embeds the userspace programs itself
/// (into an isolated target dir), so there is no separate user-build step
/// and nothing to pass in — a fresh embed falls out of building the kernel.
/// Optimization regime for a kernel build. The three levels have *distinct*
/// failure scenarios, which is why they're worth flicking between (esp. under
/// `snemu-itest`):
/// - **Low** — debug, opt-level 0 everywhere. The faithful correctness floor: the
///   whole suite (supervision included) is green here, so a Low failure is a real
///   logic bug, not a codegen artifact. (The old note that "supervision fails under
///   Low" predates supervision being built — it's now green.)
/// - **Mid** — release kernel (opt-3) with the embedded userspace pinned to opt-1
///   (the `build.rs` default, which dodges the userspace opt≥2 UB class). Fast (the
///   former `--release`), but this is where **release-codegen-vs-debug divergences
///   surface under snemu**: a scenario green under Low + QEMU can still fail here.
/// - **Hi** — release kernel, userspace at opt-**2**. Distinct from `Max` so a
///   bisect can tell "opt-2 already broke it" (fewer transforms to blame) from
///   "only opt-3 breaks it" — which is exactly how debt #16 was closed.
///   (This was described as "the first level the userspace opt≥2 UB class appears
///   (talc OOM loop / hang)". There was no UB class: see docs/debt-register.md #16.
///   Both `Hi` and `Max` are 130/130 green as of 2026-07-29.)
/// - **Max** — release everywhere, userspace at opt-3 too. Was `High`.
///
/// The ladder is monotonic in userspace opt-level: Low(0) → Mid(1) → Hi(2) → Max(3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OptLevel {
    Low,
    #[default]
    Mid,
    Hi,
    Max,
}

impl OptLevel {
    /// Whether this builds the optimized (`--release`) kernel profile. Everything
    /// but `Low` does (they differ only in the *userspace* opt level).
    pub fn is_release(self) -> bool {
        !matches!(self, OptLevel::Low)
    }

    /// The userspace opt-level this regime forces via `SNITCHOS_USERSPACE_OPT`,
    /// or `None` to leave `build.rs`'s default (opt-1) in place. `Low`/`Mid` take
    /// the default; `Hi`/`Max` raise it. (`Hi`/`Max` were "opt in to the
    /// UB-exposing levels on purpose"; that class was disproven — debt #16.)
    fn userspace_opt_override(self) -> Option<&'static str> {
        match self {
            OptLevel::Low | OptLevel::Mid => None,
            OptLevel::Hi => Some("2"),
            OptLevel::Max => Some("3"),
        }
    }
}

pub fn build_kernel(features: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    build_kernel_profiled(features, OptLevel::Low)
}

/// Kernel features a workload needs **beyond** `itest-workloads`.
///
/// The registry is additive at runtime — one binary, `workload=` selects — and that
/// holds while programs are small. `kvetch-drivel` is the exception: it embeds ~4.5 MB
/// of trained weights, more than the rest of the image put together, enough to push a
/// 16 MiB machine out of frames at userspace load. So it is gated, and a caller that
/// boots one of its workloads has to ask for it.
///
/// **This lives here, beside `build_kernel`, because every path that builds a kernel
/// for a workload needs the same answer.** It first shipped as a private helper in the
/// itest audit, and `snemu boot --workload stitch-drivel` promptly booted a kernel
/// with an empty ELF stub and panicked `Parse(BadMagic)` — a mapping duplicated per
/// call site is a mapping that is wrong at all but one of them.
#[must_use]
pub fn workload_features(workload: Option<&str>) -> &'static [&'static str] {
    match workload {
        Some("kvetch-drivel" | "stitch-drivel") => &["kvetch-drivel"],
        _ => &[],
    }
}

/// The kernel ELF path for a given profile. Cargo writes the debug build to
/// `.../debug/kernel` and the optimized build to `.../release/kernel`; a caller
/// that built with `--release` must read from the matching directory.
#[must_use]
pub fn kernel_bin(release: bool) -> &'static str {
    if release {
        "target/riscv64gc-unknown-none-elf/release/kernel"
    } else {
        KERNEL_BIN
    }
}

/// Invoke `cargo build -p kernel` with optional cargo features, optionally in
/// the optimized (`--release`) profile.
///
/// The kernel's `build.rs` builds and embeds the userspace programs itself
/// (into an isolated target dir), so there is no separate user-build step
/// and nothing to pass in — a fresh embed falls out of building the kernel.
pub fn build_kernel_profiled(
    features: &[&str],
    opt: OptLevel,
) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "-p", "kernel", "--target", KERNEL_TARGET]);
    if opt.is_release() {
        cmd.arg("--release");
    }
    // `Hi`/`Max` lift the embedded userspace to opt-2/opt-3; the kernel's `build.rs`
    // reads this to override its default opt-1 userspace pin (so `Low`/`Mid` leave it
    // unset and get the pin). Building the UB class on purpose is the point of both.
    if let Some(us_opt) = opt.userspace_opt_override() {
        cmd.env("SNITCHOS_USERSPACE_OPT", us_opt);
    }
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    cmd.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_bin_selects_profile_directory() {
        assert!(kernel_bin(false).contains("/debug/"));
        assert!(kernel_bin(true).contains("/release/"));
        assert!(kernel_bin(true).ends_with("/kernel"));
    }

    #[test]
    fn only_low_is_debug_the_rest_are_release() {
        assert!(!OptLevel::Low.is_release());
        assert!(OptLevel::Mid.is_release());
        assert!(OptLevel::Hi.is_release());
        assert!(OptLevel::Max.is_release());
    }

    #[test]
    fn userspace_opt_override_climbs_the_ladder() {
        // Low/Mid take build.rs's default (opt-1) pin; Hi/Max opt into the UB levels.
        assert_eq!(OptLevel::Low.userspace_opt_override(), None);
        assert_eq!(OptLevel::Mid.userspace_opt_override(), None);
        assert_eq!(OptLevel::Hi.userspace_opt_override(), Some("2"));
        assert_eq!(OptLevel::Max.userspace_opt_override(), Some("3"));
    }
}
