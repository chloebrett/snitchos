//! `cargo xtask web` — build and stage everything the browser page needs.
//!
//! The page has three inputs that are all **derived artifacts**: the kernel ELF, the
//! `wasm-pack` output, and a manifest describing them. Committing derived artifacts
//! is the alternative (it is what `~/c/slay` does, so GitHub Pages needs no CI), and
//! it is the alternative this project already knows the cost of by name: a VF2
//! "regression" is a missed `cargo xtask image` until proven otherwise. Two artifacts
//! that can silently disagree with their sources — and with each other — is worse
//! than one, so they are generated, not committed.
//!
//! A generated artifact can still go stale in the other direction: you edit the
//! kernel, forget to re-run this, and read yesterday's boot log while debugging
//! today's change. Nothing in a browser will tell you. Hence the manifest — the page
//! displays the fingerprint of the kernel it loaded, so "I changed the kernel and the
//! fingerprint didn't move" is visible rather than mysterious.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use xtask_qemu as qemu;

/// The browser app (a Vite + React + TypeScript project).
const WEB_DIR: &str = "web";
/// Files served verbatim by Vite, at the site root. The kernel and its manifest are
/// fetched at runtime rather than bundled — a multi-MB ELF has no business in a JS
/// bundle, and staging it here means the dev server and the built site resolve it the
/// same way.
const PUBLIC_DIR: &str = "web/public";
/// `wasm-pack` output. Under `src/` because the app imports the generated JS glue as
/// a module and the `.wasm` beside it as a URL asset, which is Vite's job, not the
/// static server's.
const PKG_DIR: &str = "web/src/pkg";
/// The crate `wasm-pack` compiles — the browser shim over the emulator.
const WASM_CRATE: &str = "snemu-wasm";

/// The workspace root, independent of where the command was invoked from.
///
/// Same trick as `links.rs` and `loc.rs`, and for the same reason: every path this
/// command touches is a repo path, and resolving them against the *caller's* cwd
/// makes `cargo xtask web` silently mean different things from different
/// directories. It cost a confusing "crate directory is missing a `Cargo.toml`"
/// before it was written this way.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask-cmds has a workspace-root parent")
        .to_path_buf()
}

/// A short, readable identifier for a build artifact's contents.
///
/// Not a cryptographic digest and not trying to be: nothing here defends against a
/// forged kernel, and the job is only to answer "are these the same bytes as last
/// time?" at a glance. `DefaultHasher` is deterministic for given input within a
/// toolchain, which is exactly the guarantee that question needs — and it is in
/// `std`, so it costs no dependency.
#[must_use]
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The manifest the page fetches to describe what it is running.
///
/// Hand-formatted rather than serialized: three scalar fields, and a `serde` derive
/// here would be more machinery than the thing it describes.
#[must_use]
pub fn manifest_json(kernel_bytes: usize, kernel_fingerprint: &str, git_rev: &str) -> String {
    format!(
        r#"{{"kernel_bytes":{kernel_bytes},"kernel_fingerprint":"{kernel_fingerprint}","git_rev":"{git_rev}"}}"#
    )
}

/// The current commit, or `"unknown"` — a dirty tree, a tarball, or no `git` on
/// `PATH` are all ordinary, and none of them justify a manifest that guesses.
fn git_rev() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}

/// Build the kernel and the wasm module, then stage both into `web/` with a manifest.
///
/// The kernel is built **release**: the browser pays snemu's interpretation cost on
/// top of the guest's own work, and a debug kernel retires far more instructions to
/// reach the same milestone.
pub fn web(run_e2e: bool) -> ExitCode {
    // Release, so the page boots in a second or two rather than tens.
    let opt = qemu::OptLevel::Max;
    let root = workspace_root();

    println!("web: building the kernel ({opt:?})…");
    match qemu::build_kernel_profiled(&[], opt) {
        Ok(s) if s.success() => {}
        Ok(_) => return ExitCode::from(1),
        Err(e) => {
            eprintln!("web: building the kernel: {e}");
            return ExitCode::from(1);
        }
    }

    println!("web: building the wasm module…");
    // `--target web` (not `bundler`): the emitted glue takes a URL and instantiates
    // by streaming, so the multi-MB module stays a separately-fetched asset and needs
    // no bundler plugin to handle a wasm ESM import.
    // Run *from inside* the crate rather than naming it: `wasm-pack build` (0.13)
    // ignores a positional crate path and looks for `Cargo.toml` in the working
    // directory, reporting the confusing "crate directory is missing a Cargo.toml"
    // when given one. (`wasm-pack test` does accept the path, which is what makes it
    // a trap.) `--out-dir` is therefore relative to the crate.
    let out = format!("../{PKG_DIR}");
    let status = Command::new("wasm-pack")
        .args(["build", "--target", "web", "--out-dir", &out])
        .current_dir(root.join(WASM_CRATE))
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(_) => return ExitCode::from(1),
        Err(e) => {
            // `Command` reports a missing *program* and a missing *working directory*
            // identically (`os error 2`), so say which one is actually absent rather
            // than making the reader guess.
            // `Command` reports a missing *program* and a missing *working
            // directory* identically (`os error 2`), so name which one is absent
            // rather than making the reader guess.
            eprintln!(
                "web: running wasm-pack: {e}\n\
                 web: crate dir {} exists: {}\n\
                 web: otherwise, install wasm-pack with `cargo install wasm-pack`",
                root.join(WASM_CRATE).display(),
                root.join(WASM_CRATE).is_dir(),
            );
            return ExitCode::from(1);
        }
    }

    // Read back through `kernel_bin_for`, which cannot disagree with the level the
    // kernel was just built at — the stale-binary trap its own docs describe.
    let elf_src = root.join(qemu::kernel_bin_for(opt));
    let elf = match std::fs::read(&elf_src) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("web: reading {}: {e}", elf_src.display());
            return ExitCode::from(1);
        }
    };

    let dest = root.join(PUBLIC_DIR);
    let print = fingerprint(&elf);
    let manifest = manifest_json(elf.len(), &print, &git_rev());

    if let Err(e) = std::fs::create_dir_all(&dest)
        .and_then(|()| std::fs::write(dest.join("kernel.elf"), &elf))
        .and_then(|()| std::fs::write(dest.join("build.json"), &manifest))
    {
        eprintln!("web: staging into {PUBLIC_DIR}/: {e}");
        return ExitCode::from(1);
    }
    println!(
        "web: staged {} KiB kernel (fingerprint {print}) into {PUBLIC_DIR}/",
        elf.len() / 1024
    );

    println!("web: installing node dependencies…");
    if !yarn(&["install", "--immutable"]) {
        return ExitCode::from(1);
    }

    println!("web: building the site…");
    if !yarn(&["build"]) {
        return ExitCode::from(1);
    }

    if run_e2e {
        return e2e();
    }

    println!(
        "web: done. `cd {WEB_DIR} && yarn dev` to iterate, or `yarn preview` to serve \
         the built site. `--e2e` runs the browser acceptance tests."
    );
    ExitCode::SUCCESS
}

/// Whether the gate can run the web tests, given what is installed.
///
/// Pure, and separated from the filesystem probe below, because a skip-clean path is
/// the one kind of failure that looks exactly like success: a policy that always
/// skipped would report a green gate while running nothing, forever, and no test of
/// the *runner* would notice. So the policy is stated once and pinned by tests.
///
/// Both inputs matter. No `yarn` means no Node toolchain at all; no `node_modules`
/// means a clone that has never run `cargo xtask web`, where installing would be a
/// multi-minute surprise inside what is meant to be the fast gate.
#[must_use]
pub fn can_run_web_tests(deps_installed: bool, yarn_present: bool) -> bool {
    deps_installed && yarn_present
}

/// Probe the machine for what [`can_run_web_tests`] decides on.
fn web_toolchain_ready() -> bool {
    let root = workspace_root();
    let deps = root.join(WEB_DIR).join("node_modules").is_dir();
    let yarn = Command::new("yarn")
        .arg("--version")
        .current_dir(root.join(WEB_DIR))
        .output()
        .is_ok_and(|o| o.status.success());
    can_run_web_tests(deps, yarn)
}

/// The browser app's unit tests (Vitest), for the host gate.
///
/// **Skips cleanly when the toolchain is absent**, the same contract
/// `cargo xtask itest` has for a missing `qemu-system-riscv64`: a Rust-only clone
/// must still be able to run the gate. The skip says what it skipped and how to
/// enable it, so a silent pass is never mistaken for a real one.
///
/// Only the unit tests. The Playwright suite needs a built site *and* a staged
/// kernel *and* a downloaded browser, which is `cargo xtask web --e2e`'s job — an
/// order of magnitude more setup than belongs in the fast gate.
pub fn test() -> ExitCode {
    if !web_toolchain_ready() {
        eprintln!(
            "web tests: skipped — no `web/node_modules` or no yarn on PATH.\n\
             web tests: run `cargo xtask web` once to set the app up."
        );
        return ExitCode::SUCCESS;
    }
    if yarn(&["install", "--immutable"]) && yarn(&["test"]) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The browser acceptance tests (Playwright).
///
/// These are the only place three of the milestone's criteria are checkable at all —
/// that the guest reaches its heartbeat, that the tab stays responsive while it does,
/// and that two loads produce byte-identical output. None are reachable from
/// `cargo nextest` (no browser) or `wasm-pack test --node` (no DOM, no animation
/// frames).
///
/// Assumes [`web`] has already run: the suite serves the built site and fetches the
/// staged kernel.
fn e2e() -> ExitCode {
    println!("web: running the browser acceptance tests…");
    if yarn(&["e2e"]) {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "web: if this failed to launch a browser, run \
             `cd web && yarn playwright install chromium` once."
        );
        ExitCode::from(1)
    }
}

/// Whether a `yarn --version` string is a Yarn this project can use.
///
/// **This exists because the wrong Yarn does not fail — it succeeds differently.**
/// Yarn Classic (1.x) ignores `packageManager`, ignores `.yarnrc.yml` (so
/// `nodeLinker` never applies), rewrites `yarn.lock` into the v1 format, and then
/// runs the build perfectly well. Nothing errors; the reproducibility guarantees
/// simply stop holding. It has silently rewritten this project's lockfile twice.
///
/// The trap is ordinary rather than exotic: `corepack enable` installs its shims
/// into the *active* Node's bin directory, so a machine whose default Node predates
/// the one you enabled corepack under still resolves a global
/// `/usr/local/bin/yarn` first.
///
/// Unparseable output is treated as unsupported. A Yarn that cannot say what it is
/// has not earned the benefit of the doubt.
#[must_use]
pub fn is_supported_yarn(version: &str) -> bool {
    version
        .trim()
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 2)
}

/// `yarn --version` as reported in `web/`, or `None` if it could not be run.
fn yarn_version() -> Option<String> {
    let out = Command::new("yarn")
        .arg("--version")
        .current_dir(workspace_root().join(WEB_DIR))
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Refuse to run if the `yarn` on `PATH` is one that would quietly do the wrong
/// thing. Returns `true` when it is safe to proceed.
fn yarn_is_usable() -> bool {
    let Some(version) = yarn_version() else {
        eprintln!(
            "web: could not run `yarn`.\n\
             web: needs Node >= 22 with corepack enabled (`corepack enable`)."
        );
        return false;
    };
    if is_supported_yarn(&version) {
        return true;
    }
    eprintln!(
        "web: `yarn --version` reports {version}, but this project pins Yarn 4 via \
         `packageManager`.\n\
         web: refusing to continue — Yarn Classic does not fail here, it succeeds \
         differently: it ignores `packageManager` and `.yarnrc.yml`, and rewrites \
         yarn.lock into the v1 format.\n\
         web: fix with `corepack enable` under Node >= 22, and remove or shadow any \
         global yarn (`which -a yarn`)."
    );
    false
}

/// Run a yarn script in `web/`, refusing outright if the wrong Yarn is on `PATH`.
fn yarn(args: &[&str]) -> bool {
    if !yarn_is_usable() {
        return false;
    }
    match Command::new("yarn").args(args).current_dir(workspace_root().join(WEB_DIR)).status() {
        Ok(s) if s.success() => true,
        Ok(_) => false,
        Err(e) => {
            eprintln!("web: running `yarn {}`: {e}", args.join(" "));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fingerprint, manifest_json};

    /// Yarn 4 and anything newer is what the project pins.
    #[test]
    fn a_modern_yarn_is_accepted() {
        assert!(super::is_supported_yarn("4.18.0"));
        assert!(super::is_supported_yarn("2.4.3"));
        assert!(super::is_supported_yarn("5.0.0"));
        assert!(super::is_supported_yarn("  4.18.0\n"), "trailing newline from stdout");
    }

    /// Yarn Classic is the case this guard exists for. It has rewritten this
    /// project's lockfile twice without erroring once.
    #[test]
    fn yarn_classic_is_refused() {
        assert!(!super::is_supported_yarn("1.22.18"));
        assert!(!super::is_supported_yarn("1.0.0"));
    }

    /// A version string that cannot be parsed gets no benefit of the doubt.
    #[test]
    fn unparseable_output_is_refused() {
        assert!(!super::is_supported_yarn(""));
        assert!(!super::is_supported_yarn("not a version"));
        assert!(!super::is_supported_yarn("v4.18.0"), "a leading v is not what yarn prints");
    }

    /// With everything installed the tests must actually run. This is the direction
    /// that matters: the opposite mistake — a policy that always skips — turns the
    /// whole phase into a green light that checks nothing.
    #[test]
    fn a_complete_toolchain_runs_the_web_tests() {
        assert!(super::can_run_web_tests(true, true));
    }

    /// And each missing piece skips, rather than failing a Rust-only clone. Same
    /// contract `cargo xtask itest` has for a missing `qemu-system-riscv64`.
    #[test]
    fn a_missing_piece_skips_rather_than_failing_the_gate() {
        assert!(!super::can_run_web_tests(false, true), "no node_modules");
        assert!(!super::can_run_web_tests(true, false), "no yarn");
        assert!(!super::can_run_web_tests(false, false), "neither");
    }

    /// A fingerprint is a function of the bytes and nothing else — the property that
    /// makes "the fingerprint didn't move" mean "the build didn't take".
    #[test]
    fn the_same_bytes_always_fingerprint_the_same() {
        assert_eq!(fingerprint(b"kernel bytes"), fingerprint(b"kernel bytes"));
    }

    /// And different bytes must look different, or the display is decoration.
    #[test]
    fn different_bytes_fingerprint_differently() {
        assert_ne!(fingerprint(b"kernel v1"), fingerprint(b"kernel v2"));
    }

    /// A one-byte change is the case that matters: a rebuild that changed almost
    /// nothing must still be visible.
    #[test]
    fn a_single_byte_change_is_visible() {
        assert_ne!(fingerprint(&[0u8; 64]), fingerprint(&[[0u8; 63].as_slice(), &[1]].concat()));
    }

    /// Short enough to read off a page at a glance, and fixed-width so two of them
    /// can be compared by eye.
    #[test]
    fn a_fingerprint_is_short_and_fixed_width() {
        let a = fingerprint(b"one");
        let b = fingerprint(&[0u8; 4096]);
        assert_eq!(a.len(), 16);
        assert_eq!(b.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "hex, so it reads as an id");
    }

    /// The manifest is a contract with the page, which has no compiler to notice a
    /// renamed key.
    #[test]
    fn the_manifest_shape_is_what_the_page_reads() {
        let json = manifest_json(1024, "abcdef0123456789", "a1b2c3d");
        assert_eq!(
            json,
            r#"{"kernel_bytes":1024,"kernel_fingerprint":"abcdef0123456789","git_rev":"a1b2c3d"}"#
        );
    }

    /// An unknown git revision is normal (a dirty tree, a tarball, no `git` on PATH)
    /// and must not produce a manifest that lies about provenance.
    #[test]
    fn an_unknown_revision_is_reported_as_unknown_not_omitted() {
        assert!(manifest_json(1, "f", "unknown").contains(r#""git_rev":"unknown""#));
    }
}
