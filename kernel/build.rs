use std::path::Path;
use std::process::{Command, Stdio};

/// The bare U-mode target the userspace programs link for — the same target
/// as the kernel (their own `user.ld` places them at a fixed low-half VA).
const USER_TARGET: &str = "riscv64gc-unknown-none-elf";

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/src/entry.S");

    build_and_embed_user(&dir);
}

/// Build the userspace programs (`hello` + `faulter`) for their bare U-mode
/// target and embed the freshly-built ELFs via `rustc-env` (consumed by
/// `include_bytes!(env!(...))` in `src/user.rs`).
///
/// Self-contained on purpose: a plain `cargo build -p kernel --target
/// riscv64gc-unknown-none-elf` produces a current embed with no `xtask`
/// pre-step and no checked-in binaries. If the user build fails we **panic**
/// — refusing to embed a stale program is the structural form of the
/// post-23 lesson (a swallowed build failure once shipped a stale kernel).
fn build_and_embed_user(kernel_dir: &str) {
    let ws = Path::new(kernel_dir)
        .parent()
        .expect("kernel manifest dir has a parent (the workspace root)");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());

    // Isolated target dir: the user programs build for the SAME target as
    // the kernel, so reusing the workspace target dir would deadlock on
    // cargo's per-target-dir build lock — held by this outer kernel build
    // while build.rs runs. A private dir under OUT_DIR sidesteps it.
    let user_target_dir = format!("{out_dir}/user-target");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(ws).args([
        "build",
        "-p",
        "hello",
        "-p",
        "fs",
        "--target",
        USER_TARGET,
        "--target-dir",
        &user_target_dir,
    ]);
    if profile == "release" {
        cmd.arg("--release");
    }
    // Don't leak the outer kernel build's flags into the user build — let it
    // resolve config exactly like a standalone `cargo build -p hello`.
    cmd.env_remove("CARGO_ENCODED_RUSTFLAGS").env_remove("RUSTFLAGS");
    // Cargo prints human/compiler output to stderr (inherited → failures are
    // visible); keep its stdout off OUR stdout, which cargo parses for the
    // `cargo:` directive lines.
    cmd.stdout(Stdio::null()).stderr(Stdio::inherit());

    let status = cmd
        .status()
        .expect("failed to invoke cargo to build the userspace programs");
    assert!(
        status.success(),
        "userspace program build failed (errors above) — refusing to embed a stale binary"
    );

    let bin_dir = format!("{user_target_dir}/{USER_TARGET}/{profile}");
    embed(&format!("{bin_dir}/hello"), "SNITCHOS_USER_ELF");
    embed(&format!("{bin_dir}/faulter"), "SNITCHOS_FAULTER_ELF");
    embed(&format!("{bin_dir}/span-flood"), "SNITCHOS_SPAN_FLOOD_ELF");
    embed(&format!("{bin_dir}/worker_a"), "SNITCHOS_WORKER_A_ELF");
    embed(&format!("{bin_dir}/worker_b"), "SNITCHOS_WORKER_B_ELF");
    embed(&format!("{bin_dir}/heap-grow"), "SNITCHOS_HEAP_GROW_ELF");
    embed(&format!("{bin_dir}/user_hog"), "SNITCHOS_USER_HOG_ELF");
    embed(&format!("{bin_dir}/syscall_hog"), "SNITCHOS_SYSCALL_HOG_ELF");
    embed(&format!("{bin_dir}/ipc-sender"), "SNITCHOS_IPC_SENDER_ELF");
    embed(&format!("{bin_dir}/ipc-receiver"), "SNITCHOS_IPC_RECEIVER_ELF");
    embed(&format!("{bin_dir}/rpc-client"), "SNITCHOS_RPC_CLIENT_ELF");
    embed(&format!("{bin_dir}/rpc-server"), "SNITCHOS_RPC_SERVER_ELF");
    embed(&format!("{bin_dir}/badge-mint"), "SNITCHOS_BADGE_MINT_ELF");
    embed(&format!("{bin_dir}/badge-handout-server"), "SNITCHOS_BADGE_HANDOUT_SERVER_ELF");
    embed(&format!("{bin_dir}/badge-handout-client"), "SNITCHOS_BADGE_HANDOUT_CLIENT_ELF");
    embed(&format!("{bin_dir}/fs-server"), "SNITCHOS_FS_SERVER_ELF");
    embed(&format!("{bin_dir}/fs-client"), "SNITCHOS_FS_CLIENT_ELF");

    // Rebuild the embed whenever a user program — or its only dependency, the
    // `abi` crate — changes. Directory paths are watched recursively by cargo,
    // so this covers every source without enumerating files.
    for p in [
        "user/hello/src",
        "user/hello/user.ld",
        "user/hello/Cargo.toml",
        "user/hello/build.rs",
        "user/fs/src",
        "user/fs/user.ld",
        "user/fs/Cargo.toml",
        "user/fs/build.rs",
        "user/runtime/src",
        "user/runtime/Cargo.toml",
        "fs-core/src",
        "fs-proto/src",
        "abi/src",
        "abi/Cargo.toml",
    ] {
        println!("cargo:rerun-if-changed={}", ws.join(p).display());
    }
}

fn embed(path: &str, env_var: &str) {
    assert!(
        Path::new(path).exists(),
        "expected userspace artifact at {path} after a successful build"
    );
    println!("cargo:rustc-env={env_var}={path}");
}
