use std::path::Path;
use std::process::{Command, Stdio};

/// The bare U-mode target the userspace programs link for — the same target
/// as the kernel (their own `user.ld` places them at a fixed low-half VA).
const USER_TARGET: &str = "riscv64gc-unknown-none-elf";

/// The embedded userspace programs: `(binary name, env var)`. The build embeds
/// each freshly-built ELF under its env var, consumed by
/// `include_bytes!(env!(...))` in `src/trap/user.rs`. One row per program — the
/// single place the build enumerates them (the kernel-side registry pairs with
/// this; see the workload-registry refactor).
const USER_PROGRAMS: &[(&str, &str)] = &[
    ("hello", "SNITCHOS_USER_ELF"),
    ("faulter", "SNITCHOS_FAULTER_ELF"),
    ("bad-ptr", "SNITCHOS_BAD_PTR_ELF"),
    ("span-flood", "SNITCHOS_SPAN_FLOOD_ELF"),
    ("worker_a", "SNITCHOS_WORKER_A_ELF"),
    ("worker_b", "SNITCHOS_WORKER_B_ELF"),
    ("heap-grow", "SNITCHOS_HEAP_GROW_ELF"),
    ("user_hog", "SNITCHOS_USER_HOG_ELF"),
    ("syscall_hog", "SNITCHOS_SYSCALL_HOG_ELF"),
    ("console_echo", "SNITCHOS_CONSOLE_ECHO_ELF"),
    ("stitch_repl", "SNITCHOS_STITCH_REPL_ELF"),
    ("probe", "SNITCHOS_PROBE_ELF"),
    ("spawner", "SNITCHOS_SPAWNER_ELF"),
    ("spawnee", "SNITCHOS_SPAWNEE_ELF"),
    ("supervisor", "SNITCHOS_SUPERVISOR_ELF"),
    ("spinner", "SNITCHOS_SPINNER_ELF"),
    ("init", "SNITCHOS_INIT_ELF"),
    ("ep_maker", "SNITCHOS_EP_MAKER_ELF"),
    ("reaper", "SNITCHOS_REAPER_ELF"),
    ("memhog", "SNITCHOS_MEMHOG_ELF"),
    ("ipc-sender", "SNITCHOS_IPC_SENDER_ELF"),
    ("ipc-receiver", "SNITCHOS_IPC_RECEIVER_ELF"),
    ("rpc-client", "SNITCHOS_RPC_CLIENT_ELF"),
    ("rpc-server", "SNITCHOS_RPC_SERVER_ELF"),
    ("badge-mint", "SNITCHOS_BADGE_MINT_ELF"),
    ("badge-handout-server", "SNITCHOS_BADGE_HANDOUT_SERVER_ELF"),
    ("badge-handout-client", "SNITCHOS_BADGE_HANDOUT_CLIENT_ELF"),
    ("fs-server", "SNITCHOS_FS_SERVER_ELF"),
    ("fs-server-seeded", "SNITCHOS_FS_SERVER_SEEDED_ELF"),
    ("fs-client", "SNITCHOS_FS_CLIENT_ELF"),
    ("spawn-image-demo", "SNITCHOS_SPAWN_IMAGE_DEMO_ELF"),
    ("notify_waiter", "SNITCHOS_NOTIFY_WAITER_ELF"),
    ("notify_signaller", "SNITCHOS_NOTIFY_SIGNALLER_ELF"),
    ("iface-reader", "SNITCHOS_IFACE_READER_ELF"),
];

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/src/entry.S");

    build_and_embed_user(&dir);
}

/// Build the userspace programs (the `hello` + `fs` crates' binaries) for their
/// bare U-mode target and embed the freshly-built ELFs via `rustc-env` (consumed
/// by `include_bytes!(env!(...))` in `src/trap/user.rs`), one per [`USER_PROGRAMS`] row.
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
    let bin_dir = format!("{user_target_dir}/{USER_TARGET}/{profile}");

    // Run a userspace cargo build for `packages`, sharing the target/flags. We
    // build in two phases — `hello` first (it provides `spawnee`), then `fs` — so
    // the `spawnee` ELF can be copied into the fs image *before* `fs-server-seeded`
    // bakes its seed at compile time (otherwise the executable wouldn't be in the
    // filesystem to run via `SpawnImage`).
    let build = |packages: &[&str]| {
        let mut cmd = Command::new(&cargo);
        cmd.current_dir(ws).arg("build");
        for p in packages {
            cmd.args(["-p", p]);
        }
        cmd.args(["--target", USER_TARGET, "--target-dir", &user_target_dir]);
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
    };

    build(&["hello"]);
    // Publish `spawnee` into the fs image so it's runnable via `SpawnImage` (the
    // shell/a client reads it from the filesystem and spawns the bytes). The
    // build-time injection of a Rust executable into the seed.
    copy_if_different(&format!("{bin_dir}/spawnee"), &ws.join("fs-image/bin/spawnee"));
    // `manifest_demo` carries a `#[entry(in, out, uses)]` manifest in its
    // `.snitch.iface` note — the fs seed extracts it into a `user.iface` xattr, so
    // this is the end-to-end data source for the typed-interface path.
    copy_if_different(
        &format!("{bin_dir}/manifest_demo"),
        &ws.join("fs-image/bin/manifest_demo"),
    );
    build(&["fs"]);

    for (bin, env_var) in USER_PROGRAMS {
        embed(&format!("{bin_dir}/{bin}"), env_var);
    }

    // Rebuild the embed whenever a user program — or its only dependency, the
    // `abi` crate — changes. Directory paths are watched recursively by cargo,
    // so this covers every source without enumerating files.
    for p in [
        "user/hello/src",
        "user/hello/Cargo.toml",
        "user/hello/build.rs",
        "user/fs/src",
        "user/fs/Cargo.toml",
        "user/fs/build.rs",
        "fs-image",
        "ramfs/src",
        "user/runtime/src",
        "user/runtime/user.ld",
        "user/runtime/build.rs",
        "user/runtime/Cargo.toml",
        "fs-core/src",
        "fs-proto/src",
        "abi/src",
        "abi/Cargo.toml",
    ] {
        println!("cargo:rerun-if-changed={}", ws.join(p).display());
    }
}

/// Copy `src` → `dst` only when the contents differ — so a rebuild that produces
/// an identical ELF doesn't touch `dst`'s mtime (which would needlessly retrigger
/// the `fs` seed rebuild that watches `fs-image/`). Creates `dst`'s parent dir.
fn copy_if_different(src: &str, dst: &Path) {
    let new = std::fs::read(src).unwrap_or_else(|e| panic!("read {src}: {e}"));
    if std::fs::read(dst).ok().as_deref() == Some(new.as_slice()) {
        return;
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).expect("create fs-image/bin");
    }
    std::fs::write(dst, &new).unwrap_or_else(|e| panic!("write {}: {e}", dst.display()));
}

fn embed(path: &str, env_var: &str) {
    assert!(
        Path::new(path).exists(),
        "expected userspace artifact at {path} after a successful build"
    );
    println!("cargo:rustc-env={env_var}={path}");
}
