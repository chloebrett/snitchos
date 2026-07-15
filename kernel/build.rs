use std::path::Path;
use std::process::{Command, Stdio};

/// The bare U-mode target the userspace programs link for — the same target
/// as the kernel (their own `user.ld` places them at a fixed low-half VA).
const USER_TARGET: &str = "riscv64gc-unknown-none-elf";

/// The embedded userspace programs: `(binary name, env var)`. The build embeds
/// each freshly-built ELF under its env var, consumed by
/// `include_bytes!(env!(...))` in `src/trap/user.rs`. One row per program — the
/// single place the build enumerates them (the kernel-side registry pairs with
/// this; see the workload-registry refactor). It is also the source of truth for
/// rebuild-watching: [`seed_packages_for_embedded_bins`] seeds the dependency
/// closure from the owning package of each bin named here, so a new bin added to
/// this table extends `rerun-if-changed` coverage automatically.
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
    ("supervised", "SNITCHOS_SUPERVISED_ELF"),
    ("cap-reporter", "SNITCHOS_CAP_REPORTER_ELF"),
    ("supervised-ipc", "SNITCHOS_SUPERVISED_IPC_ELF"),
    ("ipc-echo-server", "SNITCHOS_IPC_ECHO_SERVER_ELF"),
    ("ipc-echo-client", "SNITCHOS_IPC_ECHO_CLIENT_ELF"),
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
    ("satisfier", "SNITCHOS_SATISFIER_ELF"),
    ("spawn-image-demo", "SNITCHOS_SPAWN_IMAGE_DEMO_ELF"),
    ("notify_waiter", "SNITCHOS_NOTIFY_WAITER_ELF"),
    ("notify_signaller", "SNITCHOS_NOTIFY_SIGNALLER_ELF"),
    ("iface-reader", "SNITCHOS_IFACE_READER_ELF"),
    ("viewer", "SNITCHOS_VIEWER_ELF"),
    ("view-demo", "SNITCHOS_VIEW_DEMO_ELF"),
    ("shell", "SNITCHOS_SHELL_ELF"),
];

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/src/entry.S");
    // The embedded-userspace opt override (`snemu-itest --opt=high`); rebuild the
    // embed when it flips so mid↔high actually re-optimizes the userspace.
    println!("cargo:rerun-if-env-changed=SNITCHOS_USERSPACE_OPT");

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
            // Pin the embedded userspace to opt-level 1 even in a release kernel
            // build. At opt-level >= 2, LLVM exposes a latent UB *class* in the
            // userspace crates (confirmed in `snitchos-user`, plus at least one
            // more): talc's OOM handler loops mapping 68 KiB regions until the
            // per-process heap cap, then the program hangs in the panic handler —
            // the FS server and spawn/reap paths wedge and their itests time out.
            // The release-itest speedup is kernel-dominated (userspace is a tiny
            // instret fraction), so opt-1 here costs ~nothing and sidesteps the
            // whole class. The kernel itself stays at the workspace release
            // opt-level (3). Root-causing the userspace UB is a separate follow-up
            // (see notes/release-build-exposes-timer-death-and-uart-corruption.md).
            //
            // `SNITCHOS_USERSPACE_OPT` overrides the pin — `snemu-itest --opt=high`
            // sets it to `3` to *reproduce* the UB class on purpose (vs `--opt=mid`,
            // which leaves it unset and gets the safe opt-1). `rerun-if-env-changed`
            // (in `main`) rerebuilds when flicking between the two.
            let us_opt = std::env::var("SNITCHOS_USERSPACE_OPT").unwrap_or_else(|_| "1".into());
            cmd.args(["--config", &format!("profile.release.opt-level={us_opt}")]);
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
    // `fs-probe` is the child the `manifest-satisfy` workload's satisfier reads off
    // the FS: its `.snitch.iface` note (extracted to a `user.iface` xattr) declares
    // the `needs` the satisfier matches, and its ELF is what `SpawnImage` loads.
    copy_if_different(&format!("{bin_dir}/fs-probe"), &ws.join("fs-image/bin/fs-probe"));
    // `fs-hungry` is the *unsatisfiable* child (needs a cap the satisfier lacks) —
    // the satisfier reads its needs, refuses, and never `SpawnImage`s it.
    copy_if_different(&format!("{bin_dir}/fs-hungry"), &ws.join("fs-image/bin/fs-hungry"));
    // `fs-warden` is the *exact-match* child (needs `MINT|SEND`, what the satisfier
    // holds) → a `Use` grant, vs `fs-probe`'s narrower `SEND` → an attenuating `Mint`.
    copy_if_different(&format!("{bin_dir}/fs-warden"), &ws.join("fs-image/bin/fs-warden"));
    build(&["fs"]);

    for (bin, env_var) in USER_PROGRAMS {
        embed(&format!("{bin_dir}/{bin}"), env_var);
    }

    // Rebuild the embed whenever any source the bins are built from changes,
    // derived from the real dependency graph (below) rather than a hand list — so
    // adding a dependency to a bin can never silently embed a stale kernel, the
    // failure mode a maintained allow-list invites.
    //
    // Two inputs the graph walk can't see, so they are watched explicitly here:
    //  * `fs-image` — data baked into `fs-server-seeded` at compile time, not a crate.
    //  * `Cargo.lock` — a `cargo update` can bump an *external* (registry)
    //    dependency of a bin without touching any in-tree source. The walk only
    //    watches workspace crates (registry sources are immutable per lockfile),
    //    so the lockfile is what re-triggers on a resolved-version change.
    println!("cargo:rerun-if-changed={}", ws.join("fs-image").display());
    println!("cargo:rerun-if-changed={}", ws.join("Cargo.lock").display());
    watch_bin_dependency_closure(ws);
}

/// Emit `cargo:rerun-if-changed` for every **workspace crate** the embedded bins
/// transitively depend on, derived from `cargo metadata`. Watching each crate's
/// directory (recursive) covers its `src`, `Cargo.toml`, and `build.rs` without
/// enumerating files, and stays correct as dependencies come and go. Registry
/// deps are immutable per lockfile, so only workspace members are watched here —
/// `Cargo.lock` (watched by the caller) catches resolved-version changes.
///
/// KNOWN LIMIT — feature-resolution skew. The closure comes from `cargo
/// metadata`'s default feature resolution, which can differ from the actual bin
/// build's (`cargo build -p hello -p fs --target …`, run above). If an embedded
/// bin ever gains an **optional dependency behind a feature** that the real build
/// enables but metadata doesn't resolve, that edge is absent from the graph here,
/// the crate goes unwatched, and its changes silently embed a stale kernel. No
/// embedded bin has feature-gated dependencies today; **if you add one, either
/// watch its crate explicitly or move the userspace build to cargo
/// artifact-dependencies** (`-Z bindeps`), which tracks features/targets/lockfile
/// natively and would retire this whole walk. This is the one staleness edge the
/// external metadata walk cannot close from the outside.
fn watch_bin_dependency_closure(ws: &Path) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1"])
        .current_dir(ws)
        // Don't leak the outer kernel build's flags into the nested cargo — they
        // break its rustc target probe (same scrub as the user build above).
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .stderr(Stdio::inherit())
        .output()
        .expect("cargo metadata failed to run");
    assert!(out.status.success(), "cargo metadata exited non-zero");
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("cargo metadata emitted invalid JSON");

    // package id -> manifest_path, and the resolve graph's id -> [dep ids].
    let mut manifest_of = std::collections::HashMap::new();
    let mut seeds = seed_packages_for_embedded_bins(&meta, &mut manifest_of);
    let deps_of = resolve_edges(&meta);

    // BFS the closure; watch each reachable crate whose source lives in-tree.
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = seeds.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(manifest) = manifest_of.get(&id)
            && let Some(dir) = Path::new(manifest).parent()
            && dir.starts_with(ws)
        {
            println!("cargo:rerun-if-changed={}", dir.display());
        }
        if let Some(next) = deps_of.get(&id) {
            seeds.extend(next.iter().cloned());
        }
    }
}

/// Seed the closure walk with the packages that *own* an embedded bin, and fill
/// `manifest_of` for every package (the id -> manifest_path map the walk needs).
///
/// A package is a seed iff it has a `bin` target whose name is in
/// [`USER_PROGRAMS`] — so the seed set is **derived from that single source of
/// truth**, not a second hand-maintained list that could drift from it. Add a bin
/// to `USER_PROGRAMS` (even one in a brand-new userspace crate) and its owning
/// package — and therefore its whole dependency closure — is watched
/// automatically. The one standing assumption: every embedded bin resolves to an
/// in-tree package via its bin-target name, which holds for the path crates the
/// kernel embeds.
fn seed_packages_for_embedded_bins(
    meta: &serde_json::Value,
    manifest_of: &mut std::collections::HashMap<String, String>,
) -> Vec<String> {
    let embedded: std::collections::HashSet<&str> =
        USER_PROGRAMS.iter().map(|(bin, _)| *bin).collect();
    let mut seeds = Vec::new();
    for pkg in meta["packages"].as_array().into_iter().flatten() {
        let (Some(id), Some(manifest)) = (pkg["id"].as_str(), pkg["manifest_path"].as_str())
        else {
            continue;
        };
        manifest_of.insert(id.to_string(), manifest.to_string());
        let owns_embedded_bin = pkg["targets"].as_array().into_iter().flatten().any(|t| {
            let is_bin = t["kind"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k.as_str() == Some("bin"));
            is_bin && t["name"].as_str().is_some_and(|n| embedded.contains(n))
        });
        if owns_embedded_bin {
            seeds.push(id.to_string());
        }
    }
    seeds
}

/// Build the resolve graph's adjacency map: package id -> its dependency ids.
fn resolve_edges(
    meta: &serde_json::Value,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut edges = std::collections::HashMap::new();
    for node in meta["resolve"]["nodes"].as_array().into_iter().flatten() {
        let Some(id) = node["id"].as_str() else { continue };
        let deps = node["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_str().map(str::to_string))
            .collect();
        edges.insert(id.to_string(), deps);
    }
    edges
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
