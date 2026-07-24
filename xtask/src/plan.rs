//! Host gate machinery: which workspace crates get tested / linted / documented,
//! and the `test` command's unit-test driver.
//!
//! Extracted from `itest` (Step 1a of `plans/xtask-lean-test-binary.md`) so the
//! lean commands (`test`/`clippy`/`mutants`) that this module implements don't
//! pull in the snemu-linked itest runner. Pure planning logic — no snemu, no
//! `itest-harness`, no QEMU.

use std::process::{Command, ExitCode};

/// Crates the host gate deliberately does **not** `cargo test`, each with the
/// reason. Every other workspace member is tested by default: the gate derives
/// its list from `cargo metadata`, so a new crate is host-tested the moment it
/// joins the workspace, and this list is the only way out. Opting out is a
/// decision someone has to write down.
///
/// The inverse (an allow-list) let a crate be silently never-tested by simple
/// omission — which is exactly how the `kernel-core` rename slipped through.
pub(crate) const NOT_HOST_TESTED: &[(&str, &str)] = &[
    ("kernel", "no_std/no_main, riscv64-only — won't link for the host; its logic lives in the kernel-* crates"),
    ("snitchos-user", "riscv64-only userspace runtime (crt0 + syscall bindings)"),
    ("snitchos-std", "riscv64-only userspace std"),
    ("hello", "riscv64-only userspace binaries"),
    ("fs", "riscv64-only userspace FS server"),
];

/// Userspace wrappers allowed to hand-roll `asm!("ecall", …)` **permanently**,
/// each with the reason. Everything else must go through the
/// `ecall(nr, [usize; 7])` helper in `user/runtime`, which declares every
/// argument register `inlateout` so "this register survives the call" is not
/// expressible — the promise that produced seven bugs in one day.
pub(crate) const RAW_ECALL_INTENTIONAL: &[(&str, &str)] = &[
    ("ecall", "the helper itself — the one place the syscall register ABI is written down"),
    ("exit_with", "divergent (`options(noreturn)`); nothing returns, so nothing can be clobbered"),
    ("yield_now", "no operand registers to mis-declare, and a hot path the helper would pessimise"),
];

/// Wrappers still carrying a hand-rolled `ecall` that simply haven't been ported
/// yet. **This number may only ever go down.** It is a ratchet, not a budget: it
/// grandfathers what exists so an *eighth* instance of the clobber bug can't be
/// added silently, without demanding a big-bang rewrite of correct code.
pub(crate) const RAW_ECALL_GRANDFATHERED: usize = 22;

/// Names of the functions containing a hand-rolled `asm!("ecall", …)` in `src`.
///
/// Deliberately crude — a line scan tracking the most recent `fn`, keyed on the
/// `"ecall"` string literal (which only ever appears as asm text; prose uses
/// backticks). A real parser would be more precise and far more machinery than a
/// ratchet warrants.
pub(crate) fn raw_ecall_sites(src: &str) -> Vec<&str> {
    let mut current = "";
    let mut found = Vec::new();
    for line in src.lines() {
        if let Some(name) = fn_name_on(line) {
            current = name;
        }
        if line.contains("\"ecall\"") && !found.contains(&current) {
            found.push(current);
        }
    }
    found
}

/// The function name declared on `line`, if it declares one.
fn fn_name_on(line: &str) -> Option<&str> {
    let after = line.split_once("fn ")?.1;
    let name = after.split(['(', '<', ' ']).next()?;
    (!name.is_empty()).then_some(name)
}

/// Extra `cargo test` args a crate's suite needs (features it can't get from
/// its defaults). Entries naming a departed crate are an error, not a no-op.
pub(crate) const EXTRA_TEST_ARGS: &[(&str, &[&str])] = &[
    // `protocol::stream` (decoder + OwnedFrame) is behind `std`.
    ("protocol", &["--features", "std"]),
    // `--features testing` exposes `stitch::testing` so the integration tests
    // (e.g. the stim FSM in `tests/stim_fsm.rs`) can run the interpreter.
    ("stitch", &["--features", "testing"]),
];

/// The crates the riscv gate lints: exactly the [`NOT_HOST_TESTED`] set, in
/// member order. The two lists coincide for one reason — a crate is exempt from
/// the host gate *because* it only builds for riscv64 — so that one axis decides
/// both gates rather than each keeping its own list to drift.
///
/// A stale entry is an error, for the same reason it is in [`unit_test_plan`].
pub(crate) fn riscv_only_plan<'a>(
    members: &[&'a str],
    riscv_only: &[(&str, &str)],
) -> Result<Vec<&'a str>, String> {
    let stale: Vec<&str> =
        riscv_only.iter().map(|(name, _)| *name).filter(|name| !members.contains(name)).collect();
    if !stale.is_empty() {
        return Err(format!(
            "riscv-only policy names crates that are not workspace members: {}. \
             Renamed or removed? Update NOT_HOST_TESTED in xtask/src/plan.rs.",
            stale.join(", ")
        ));
    }

    Ok(members
        .iter()
        .filter(|name| riscv_only.iter().any(|(excluded, _)| excluded == *name))
        .copied()
        .collect())
}

/// Crates that deliberately do **not** inherit the workspace lint table
/// (`[lints] workspace = true`), each with the reason.
///
/// Unlike the test/clippy/mutation gates, this one cannot be derived: cargo has
/// no workspace-wide lint inheritance, so `[lints] workspace = true` is
/// structurally per-crate and omitting it is invisible. The
/// `every_workspace_member_opts_in_or_has_a_written_reason` test is what turns
/// that silent omission into a written decision.
///
/// **Being bare-metal is not a reason.** This list used to say "full pedantic
/// fights register/address idioms" and name `kernel`, `hello` and `fs`. Measured,
/// that reason was false: `hello` produces 8 hits and `fs` a handful, **none**
/// idiom-related, so both now opt in. The workspace's `cast_*` and
/// `unreadable_literal` allows *are* the bare-metal accommodation — their own
/// comments say so ("~200×", "mirror the linker script + Sv39 layout") — so by
/// the time pedantic runs, that friction is already gone. What's left everywhere
/// is ordinary style, doc-backticks above all.
///
/// The reason mattered: while it stood, it was the template for three further
/// bogus exemptions (`snitchos-user`, `snitchos-std`, `snitchos-user-macros`) that
/// were never justified either. A false reason propagates.
/// **Empty is the goal state, and we are there.** `kernel` was the last holdout;
/// its 104 hits turned out to be ~54 mechanical doc-backticks plus ordinary
/// style, and it now opts in like everything else. Keep it empty unless a crate
/// earns an entry with a *measured* reason — "it's bare-metal" is not one.
pub(crate) const LINTS_EXEMPT: &[(&str, &str)] = &[];

/// Whether a manifest inherits the workspace lint table.
///
/// Hand-scanned rather than parsed: xtask has no `toml` dependency and this is
/// the only thing that would need one. The key must sit *inside* a `[lints]`
/// section — `edition.workspace = true` under `[package]` is the common idiom
/// and must not read as a lints opt-in.
fn opts_into_workspace_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_lints = line == "[lints]";
            continue;
        }
        if in_lints && line == "workspace = true" {
            return true;
        }
    }
    false
}

/// The crates that neither inherit the workspace lint table nor have a written
/// reason not to, given each member's `(name, manifest text)`. Empty is the
/// healthy state. A stale exemption is an error, as everywhere else.
fn lints_optin_gaps<'a>(
    manifests: &[(&'a str, String)],
    exempt: &[(&str, &str)],
) -> Result<Vec<&'a str>, String> {
    let stale: Vec<&str> = exempt
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !manifests.iter().any(|(member, _)| member == name))
        .collect();
    if !stale.is_empty() {
        return Err(format!(
            "lint policy names crates that are not workspace members: {}. \
             Renamed or removed? Update LINTS_EXEMPT in xtask/src/plan.rs.",
            stale.join(", ")
        ));
    }

    Ok(manifests
        .iter()
        .filter(|(name, text)| {
            !opts_into_workspace_lints(text) && !exempt.iter().any(|(e, _)| e == name)
        })
        .map(|(name, _)| *name)
        .collect())
}

/// Run `cargo metadata --no-deps` and return the parsed JSON.
///
/// Inherits stderr rather than capturing it (as `.output()` does by default), so
/// cargo's "Blocking waiting for file lock on package cache" — printed when
/// rust-analyzer or another cargo holds the lock — is visible instead of leaving
/// `x test` looking like a silent hang before the first `=== unit tests ===`
/// line. stdout is still captured, since that's the JSON we parse.
fn cargo_metadata_json() -> Result<serde_json::Value, String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err("cargo metadata failed".to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse cargo metadata: {e}"))
}

/// Every workspace member's `(name, manifest text)`, via `cargo metadata`'s
/// `manifest_path`. Only the lint-policy test needs the file bodies — the other
/// gates work from names alone.
#[cfg(test)]
fn workspace_manifests() -> Result<Vec<(String, String)>, String> {
    let json = cargo_metadata_json()?;
    let packages = json["packages"].as_array().ok_or("cargo metadata: no packages array")?;
    packages
        .iter()
        .map(|p| {
            let name = p["name"].as_str().ok_or("cargo metadata: package with no name")?;
            let path = p["manifest_path"]
                .as_str()
                .ok_or_else(|| format!("cargo metadata: {name} has no manifest_path"))?;
            let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
            Ok((name.to_owned(), text))
        })
        .collect()
}

/// The workspace's package names, straight from `cargo metadata --no-deps`.
pub(crate) fn workspace_members() -> Result<Vec<String>, String> {
    let json = cargo_metadata_json()?;
    let packages = json["packages"].as_array().ok_or("cargo metadata: no packages array")?;
    Ok(packages.iter().filter_map(|p| p["name"].as_str().map(str::to_owned)).collect())
}

/// Decide which crates the host gate tests, and with what args: every member
/// that isn't excluded, in member order. Both lists must describe crates that
/// actually exist — a stale entry (renamed or deleted crate) is an error, so a
/// rename can't quietly drop a crate out of the gate.
pub(crate) fn unit_test_plan<'a>(
    members: &[&'a str],
    excluded: &[(&str, &str)],
    extra_args: &[(&'static str, &'static [&'static str])],
) -> Result<Vec<(&'a str, &'static [&'static str])>, String> {
    let stale: Vec<&str> = excluded
        .iter()
        .map(|(name, _)| *name)
        .chain(extra_args.iter().map(|(name, _)| *name))
        .filter(|name| !members.contains(name))
        .collect();
    if !stale.is_empty() {
        return Err(format!(
            "unit-test policy names crates that are not workspace members: {}. \
             Renamed or removed? Update NOT_HOST_TESTED / EXTRA_TEST_ARGS in xtask/src/plan.rs.",
            stale.join(", ")
        ));
    }

    Ok(members
        .iter()
        .filter(|name| !excluded.iter().any(|(excluded, _)| excluded == *name))
        .map(|name| {
            let args = extra_args
                .iter()
                .find(|(crate_name, _)| crate_name == name)
                .map_or(&[] as &'static [&'static str], |(_, args)| *args);
            (*name, args)
        })
        .collect())
}

/// Run every host-side check, in order: each workspace crate's unit tests, the
/// loom model-checks, and the generated-diagram drift check. Returns `SUCCESS`
/// only if all pass. Bails out on first failure (no point continuing if a
/// foundation crate is broken).

pub fn run_unit_tests() -> ExitCode {
    let members = match workspace_members() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("unit tests: {e}");
            return ExitCode::from(1);
        }
    };
    let names: Vec<&str> = members.iter().map(String::as_str).collect();
    let plan = match unit_test_plan(&names, NOT_HOST_TESTED, EXTRA_TEST_ARGS) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("unit tests: {e}");
            return ExitCode::from(1);
        }
    };
    // One `cargo nextest run` for every host suite: nextest compiles all the test
    // binaries in **one parallel build** off a shared graph and runs them with its
    // own parallel runner — versus the old per-crate `cargo test` loop, which
    // serialized each crate's compile behind the previous crate's *run* and paid
    // cargo's startup + freshness-check overhead once per crate. Per-crate features
    // (`EXTRA_TEST_ARGS`) fold into namespaced `pkg/feature` flags, since nextest
    // resolves features once for the whole invocation. (No workspace doctests exist,
    // so nextest's not-running-doctests is a non-issue; loom is a separate `--cfg`
    // build below either way.)
    eprintln!("=== unit tests (nextest, {} host suites) ===", plan.len());
    let mut nextest_args: Vec<String> = vec!["nextest".into(), "run".into()];
    for (crate_name, _) in &plan {
        nextest_args.push("-p".into());
        nextest_args.push((*crate_name).to_string());
    }
    let features: Vec<String> = plan
        .iter()
        .filter_map(|(name, extra)| match extra {
            ["--features", feat] => Some(format!("{name}/{feat}")),
            _ => None,
        })
        .collect();
    if !features.is_empty() {
        nextest_args.push("--features".into());
        nextest_args.push(features.join(","));
    }
    let arg_refs: Vec<&str> = nextest_args.iter().map(String::as_str).collect();
    if !run_cargo_test("all host suites", &arg_refs, &[]) {
        return ExitCode::from(1);
    }
    // The loom model-check tests (kernel-devices/tests/loom_tx.rs) live
    // behind `--cfg loom`, where loom swaps in its own Mutex/thread/
    // UnsafeCell. They need a separate compilation with that cfg set; a
    // normal `cargo test` compiles the file to nothing. The config-level
    // rustflags are riscv-target-scoped, so overriding RUSTFLAGS for this
    // host build clobbers nothing. Kept as a plain `cargo test` (its own cfg
    // build, distinct from the nextest run above).
    eprintln!("=== loom model-check ===");
    if !run_cargo_test(
        "kernel-devices (loom)",
        &["test", "-p", "kernel-devices", "--test", "loom_tx", "--quiet"],
        &[("RUSTFLAGS", "--cfg loom")],
    ) {
        return ExitCode::from(1);
    }
    // The collector's *core* — decode, span/cap state, the projections — must keep
    // compiling for wasm, because the browser front-end runs it in-tab with no
    // backend at all (snemu already compiles to wasm32 unmodified, and it would be
    // absurd for the collector to become the reason a server is required). The
    // exporters that speak HTTP (`ureq`, `tiny_http`) are native-only by nature and
    // live behind the default `native` feature; this builds the core without them.
    // A build check rather than a test: there is nothing to assert beyond "it still
    // compiles for that target", and that is exactly what rots silently.
    // See docs/uart-telemetry-design.md §"Where this is going".
    eprintln!("=== portability ===");
    if !run_cargo_test(
        "collector core (wasm32)",
        &[
            "build",
            "-q",
            "-p",
            "collector",
            "--lib",
            "--no-default-features",
            "--target",
            "wasm32-unknown-unknown",
        ],
        &[],
    ) {
        return ExitCode::from(1);
    }
    // Generated diagrams (docs/generated/) are contract artifacts: a stale one
    // means the source of truth moved without the committed diagram noticing.
    // The drift check now lives in the `xtask-itest` crate (its `diagram_cmd`
    // reads the `SCENARIOS` registry + folds snemu frames), running as a test in
    // the nextest phase so the lean tool never links snemu — see
    // plans/xtask-lean-test-binary.md and xtask-itest's `diagram_drift_tests`.
    // Same reasoning as the diagrams: a markdown link is a contract nothing
    // compiles. Every `git mv` sweep this repo has done has left dead links
    // behind — most often the moved file's own `../` links, which now resolve
    // one directory too high. Cheap to check, invisible otherwise.
    eprintln!("=== doc links ===");
    if crate::links::check() != ExitCode::SUCCESS {
        return ExitCode::from(1);
    }
    // The sibling of the markdown check above, for the links the *compiler*
    // owns. Rustdoc resolves intra-doc links but only **warns** on a broken one,
    // and nothing ran rustdoc — so they rot invisibly. The kernel-core split
    // alone dangled four (`crate::frame::Bitmap` and friends, whose modules had
    // moved to other crates), and a rename left `[`span_start`]` pointing at a
    // function that no longer existed. `cargo build` sees none of it.
    eprintln!("=== rustdoc ===");
    if !check_rustdoc(&names) {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// Rustdoc lints the gate deliberately does **not** enforce, each with the reason.
///
/// The gate denies *every* doc-build warning (`-D warnings`, see [`check_rustdoc`])
/// and this list allows specific lints back — the same opt-out shape as the
/// workspace clippy table (pedantic `warn`, named lints `allow`ed back) and
/// [`LINTS_EXEMPT`] (every crate linted, named crates excused). A lint earns a
/// spot here only with a written, ideally *measured*, reason.
///
/// The previous design was the inverse: an opt-*in* list naming the single lint to
/// enforce (`broken_intra_doc_links`). Everything nobody thought to add —
/// `bare_urls`, `unescaped_backticks`, `invalid_html_tags` — was ignored by
/// default, the same silent-omission failure the crate lists above exist to
/// prevent. Opt-out means a new rustdoc lint is enforced the moment the toolchain
/// ships it, and turning one off is a decision someone has to write down.
pub(crate) const RUSTDOC_EXEMPT: &[(&str, &str)] = &[
    (
        "private_intra_doc_links",
        "a public item's docs linking to a private one: the link names something \
         real, it just doesn't render as a link (cosmetic). Measured at ~10× in \
         `snemu` when this policy was written; drop the exemption if a re-measure \
         reaches zero. Prefer demoting the offending link to a plain `code span` \
         over widening this exemption.",
    ),
];

/// The `RUSTDOCFLAGS` value: deny every doc-build warning, then allow back exactly
/// the lints named in `exempt`. Pure so the flag string is unit-testable; the
/// allow-backs follow the deny so they win (rustc applies flags left-to-right).
fn rustdoc_deny_flags(exempt: &[(&str, &str)]) -> String {
    let mut flags = String::from("-D warnings");
    for (lint, _) in exempt {
        flags.push_str(" -A rustdoc::");
        flags.push_str(lint);
    }
    flags
}

/// `cargo doc` every crate, denying all doc-build warnings so a broken link, bare
/// URL, or malformed doc fails the gate instead of rotting. `RUSTDOC_EXEMPT` is
/// the only way to quiet a specific lint, and every entry there carries a reason.
///
/// Same two-target split as `run_clippy`, for the same reason: the bare-metal
/// crates can't be documented for the host. `--no-deps` because we're checking
/// *our* prose, not our dependencies'.
fn check_rustdoc(names: &[&str]) -> bool {
    let flags = rustdoc_deny_flags(RUSTDOC_EXEMPT);
    let deny = &[("RUSTDOCFLAGS", flags.as_str())];
    let host_plan = match unit_test_plan(names, NOT_HOST_TESTED, EXTRA_TEST_ARGS) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("rustdoc: {e}");
            return false;
        }
    };
    // Reuses the test gate's per-crate feature args: a crate that needs
    // `--features std` to compile needs it to document, too.
    let host = host_plan.iter().all(|(crate_name, crate_args)| {
        let mut args = vec!["doc", "--no-deps", "-q", "-p", *crate_name];
        args.extend_from_slice(crate_args);
        run_cargo_test(&format!("  {crate_name}"), &args, deny)
    });
    let riscv_plan = match riscv_only_plan(names, NOT_HOST_TESTED) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("rustdoc: {e}");
            return false;
        }
    };
    let riscv = riscv_plan.iter().all(|crate_name| {
        let args = ["doc", "--no-deps", "-q", "-p", crate_name, "--target", crate::qemu::KERNEL_TARGET];
        run_cargo_test(&format!("  {crate_name} (riscv)"), &args, deny)
    });
    host && riscv
}

/// Run one `cargo test` invocation, printing `ok`/`FAILED` for `label`.
/// On failure surfaces the captured stderr so the user needn't re-run
/// with `--verbose`. `env` overrides are applied to the child (e.g.
/// `RUSTFLAGS=--cfg loom`). Returns `true` iff the suite passed.
fn run_cargo_test(label: &str, args: &[&str], env: &[(&str, &str)]) -> bool {
    // Cargo's stderr is inherited, not captured: compiling is what the wall-clock
    // is actually spent on (suites *run* in ~0-3s), so cargo's own `Compiling …`
    // progress is the live signal. Capturing it to replay only on failure is what
    // made the gate look hung for minutes at a time. The label goes on its own
    // line so cargo's output streams beneath it rather than trailing it.
    eprintln!("  {label}");
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit());
    for (key, value) in env {
        cmd.env(key, value);
    }
    match cmd.status() {
        Ok(status) if status.success() => {
            eprintln!("    ok");
            true
        }
        // Cargo already streamed the failure to the terminal — don't replay it.
        Ok(_) => {
            eprintln!("    FAILED");
            false
        }
        Err(e) => {
            eprintln!("    FAILED to invoke cargo: {e}");
            false
        }
    }
}

#[cfg(test)]
mod raw_ecall_ratchet_tests {
    use super::{RAW_ECALL_GRANDFATHERED, RAW_ECALL_INTENTIONAL, raw_ecall_sites};

    #[test]
    fn a_hand_rolled_ecall_is_attributed_to_its_function() {
        let src = "pub fn spawn(x: usize) {\n    asm!(\n        \"ecall\",\n    );\n}\n";
        assert_eq!(raw_ecall_sites(src), vec!["spawn"]);
    }

    #[test]
    fn an_asm_block_without_an_ecall_is_ignored() {
        // CSR twiddling and the like is not a syscall and has no ABI to mis-declare.
        let src = "pub fn sum() {\n    asm!(\"csrs sstatus, {}\", in(reg) 1);\n}\n";
        assert!(raw_ecall_sites(src).is_empty());
    }

    #[test]
    fn each_site_is_attributed_separately() {
        let src = "fn a() {\n  asm!(\"ecall\");\n}\nfn b() {\n  asm!(\"ecall\");\n}\n";
        assert_eq!(raw_ecall_sites(src), vec!["a", "b"]);
    }

    /// The ratchet. Every hand-rolled `asm!("ecall", …)` in the userspace runtime
    /// re-declares the syscall register ABI, and declaring a register `in(...)`
    /// that the kernel writes is a bug no compiler or test can catch — it fires
    /// only when codegen parks a live value there. Seven instances were found in
    /// one day (see the SBI-clobber callout in plans/visionfive2-port.md).
    ///
    /// New wrappers must route through the `ecall(nr, [usize; 7])` helper, where
    /// the mistake is not expressible. This test lets the existing unported
    /// wrappers stay while forbidding an eighth.
    #[test]
    fn no_new_hand_rolled_ecall_wrappers() {
        let src = include_str!("../../user/runtime/src/lib.rs");
        let unported: Vec<&str> = raw_ecall_sites(src)
            .into_iter()
            .filter(|f| !RAW_ECALL_INTENTIONAL.iter().any(|(name, _)| name == f))
            .collect();
        assert!(
            unported.len() <= RAW_ECALL_GRANDFATHERED,
            "{} hand-rolled `ecall` wrappers in user/runtime, budget is {}.\n\
             Route new syscalls through the `ecall(nr, [usize; 7])` helper instead \
             of writing an `asm!` block — see its doc comment for why.\n\
             If you *ported* one, lower RAW_ECALL_GRANDFATHERED; it must only go down.\n\
             sites: {unported:?}",
            unported.len(),
            RAW_ECALL_GRANDFATHERED,
        );
    }

    /// A renamed or deleted wrapper must not leave a silent entry behind — same
    /// reasoning as `NOT_HOST_TESTED`'s staleness check: a permanent exemption
    /// naming a function that no longer hand-rolls an `ecall` is an exemption
    /// nobody is checking, and it would silently cover a *future* function that
    /// happens to reuse the name.
    #[test]
    fn every_permanent_exemption_still_names_a_real_site() {
        let src = include_str!("../../user/runtime/src/lib.rs");
        let sites = raw_ecall_sites(src);
        for (name, reason) in RAW_ECALL_INTENTIONAL {
            assert!(
                sites.contains(name),
                "RAW_ECALL_INTENTIONAL lists `{name}` ({reason}) but it has no \
                 hand-rolled `ecall` — drop the entry."
            );
        }
    }

    /// Porting a wrapper must be reflected in the budget, or the ratchet silently
    /// stops ratcheting — the same failure mode as an allow-list by omission.
    #[test]
    fn the_grandfathered_budget_is_not_slack() {
        let src = include_str!("../../user/runtime/src/lib.rs");
        let unported = raw_ecall_sites(src)
            .into_iter()
            .filter(|f| !RAW_ECALL_INTENTIONAL.iter().any(|(name, _)| name == f))
            .count();
        assert_eq!(
            unported, RAW_ECALL_GRANDFATHERED,
            "RAW_ECALL_GRANDFATHERED is {RAW_ECALL_GRANDFATHERED} but {unported} \
             wrappers remain — lower it to {unported}."
        );
    }
}

#[cfg(test)]
mod unit_test_plan_tests {
    use super::{EXTRA_TEST_ARGS, NOT_HOST_TESTED, unit_test_plan};

    #[test]
    fn a_crate_nobody_mentions_is_tested_by_default() {
        let plan = unit_test_plan(&["brand-new-crate"], &[], &[]).expect("valid plan");
        assert_eq!(plan, vec![("brand-new-crate", &[] as &[&str])]);
    }

    #[test]
    fn an_excluded_crate_is_skipped() {
        let plan = unit_test_plan(&["kernel", "collector"], &[("kernel", "riscv only")], &[])
            .expect("valid plan");
        assert_eq!(plan, vec![("collector", &[] as &[&str])]);
    }

    #[test]
    fn extra_args_attach_to_their_crate() {
        let plan = unit_test_plan(&["protocol"], &[], &[("protocol", &["--features", "std"])])
            .expect("valid plan");
        assert_eq!(plan, vec![("protocol", &["--features", "std"] as &[&str])]);
    }

    #[test]
    fn plan_follows_member_order() {
        let plan = unit_test_plan(&["b", "a"], &[], &[]).expect("valid plan");
        assert_eq!(plan.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec!["b", "a"]);
    }

    /// A renamed or deleted crate must not leave a silent entry behind — that is
    /// how `kernel-core`'s rename slipped past the gate.
    #[test]
    fn an_exclusion_naming_a_departed_crate_is_an_error() {
        let err = unit_test_plan(&["collector"], &[("kernel-core", "gone")], &[])
            .expect_err("stale exclusion must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    #[test]
    fn extra_args_naming_a_departed_crate_are_an_error() {
        let err = unit_test_plan(&["collector"], &[], &[("kernel-core", &["--features", "std"])])
            .expect_err("stale args must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    /// The committed lists describe the real workspace, checked against the real
    /// `cargo metadata` — this is what fails when a crate is renamed or removed.
    #[test]
    fn the_committed_lists_match_the_workspace() {
        let members = super::workspace_members().expect("cargo metadata");
        let names: Vec<&str> = members.iter().map(String::as_str).collect();
        unit_test_plan(&names, NOT_HOST_TESTED, EXTRA_TEST_ARGS).expect("committed lists are current");
    }
}

#[cfg(test)]
mod rustdoc_policy_tests {
    use super::{RUSTDOC_EXEMPT, rustdoc_deny_flags};

    /// With nothing exempt the gate denies every doc-build warning — opt-out, so a
    /// rustdoc lint nobody has named is on by default.
    #[test]
    fn with_no_exemptions_all_warnings_are_denied() {
        assert_eq!(rustdoc_deny_flags(&[]), "-D warnings");
    }

    #[test]
    fn an_exemption_is_allowed_back_under_the_rustdoc_prefix() {
        assert_eq!(
            rustdoc_deny_flags(&[("private_intra_doc_links", "noisy")]),
            "-D warnings -A rustdoc::private_intra_doc_links",
        );
    }

    /// Allow-backs follow the deny so they win, and compose in list order.
    #[test]
    fn exemptions_compose_after_the_deny() {
        assert_eq!(
            rustdoc_deny_flags(&[("a", "r1"), ("b", "r2")]),
            "-D warnings -A rustdoc::a -A rustdoc::b",
        );
    }

    /// Same discipline as the crate lists: an exemption is a written decision, so
    /// every entry carries a non-empty reason and no lint is listed twice.
    #[test]
    fn committed_exemptions_are_written_decisions() {
        let mut seen = std::collections::HashSet::new();
        for (lint, reason) in RUSTDOC_EXEMPT {
            assert!(!lint.is_empty(), "exemption with an empty lint name");
            assert!(!reason.trim().is_empty(), "the `{lint}` exemption has no reason");
            assert!(seen.insert(*lint), "`{lint}` is listed twice in RUSTDOC_EXEMPT");
        }
    }
}

#[cfg(test)]
mod lints_policy_tests {
    use super::{LINTS_EXEMPT, lints_optin_gaps, opts_into_workspace_lints};

    #[test]
    fn a_manifest_with_the_opt_in_is_detected() {
        assert!(opts_into_workspace_lints("[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n"));
    }

    #[test]
    fn a_manifest_without_a_lints_section_does_not_opt_in() {
        assert!(!opts_into_workspace_lints("[package]\nname = \"x\"\n"));
    }

    #[test]
    fn a_lints_section_that_does_not_inherit_the_workspace_does_not_opt_in() {
        assert!(!opts_into_workspace_lints("[lints]\nworkspace = false\n"));
    }

    /// `edition.workspace = true` under `[package]` is the common idiom and must
    /// not read as a lints opt-in — the key has to sit *inside* `[lints]`.
    #[test]
    fn a_workspace_key_in_another_section_is_not_a_lints_opt_in() {
        assert!(!opts_into_workspace_lints("[package]\nedition.workspace = true\n"));
        assert!(!opts_into_workspace_lints("[dependencies]\nworkspace = true\n"));
    }

    /// The `[lints]` section ends at the next section header.
    #[test]
    fn a_workspace_key_after_the_lints_section_closes_does_not_count() {
        assert!(!opts_into_workspace_lints("[lints]\n\n[dependencies]\nworkspace = true\n"));
    }

    #[test]
    fn a_crate_missing_the_opt_in_is_reported() {
        let gaps = lints_optin_gaps(&[("collector", "[package]\n".to_string())], &[])
            .expect("valid policy");
        assert_eq!(gaps, vec!["collector"]);
    }

    #[test]
    fn a_crate_with_the_opt_in_is_not_reported() {
        let gaps =
            lints_optin_gaps(&[("collector", "[lints]\nworkspace = true\n".to_string())], &[])
                .expect("valid policy");
        assert!(gaps.is_empty(), "opted-in crate should not be reported: {gaps:?}");
    }

    #[test]
    fn an_exempt_crate_missing_the_opt_in_is_not_reported() {
        let gaps = lints_optin_gaps(&[("kernel", "[package]\n".to_string())], &[(
            "kernel",
            "register idioms",
        )])
        .expect("valid policy");
        assert!(gaps.is_empty(), "exempt crate should not be reported: {gaps:?}");
    }

    #[test]
    fn an_exemption_naming_a_departed_crate_is_an_error() {
        let err = lints_optin_gaps(&[("collector", "[package]\n".to_string())], &[(
            "kernel-core",
            "gone",
        )])
        .expect_err("stale exemption must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    /// The real gate: every workspace member either inherits the workspace lint
    /// table or has a written reason not to. `[lints] workspace = true` is
    /// structurally per-crate — cargo has no workspace-wide inheritance — so this
    /// test is the only thing standing between the policy and silent drift.
    #[test]
    fn every_workspace_member_opts_in_or_has_a_written_reason() {
        let manifests = super::workspace_manifests().expect("cargo metadata");
        let borrowed: Vec<(&str, String)> =
            manifests.iter().map(|(name, text)| (name.as_str(), text.clone())).collect();
        let gaps = lints_optin_gaps(&borrowed, LINTS_EXEMPT).expect("committed list is current");
        assert!(
            gaps.is_empty(),
            "these crates neither opt into the workspace lints nor have a LINTS_EXEMPT reason: {gaps:?}"
        );
    }
}

#[cfg(test)]
mod riscv_only_plan_tests {
    use super::{NOT_HOST_TESTED, riscv_only_plan, unit_test_plan};

    #[test]
    fn returns_the_excluded_crates_in_member_order() {
        let plan = riscv_only_plan(&["collector", "kernel", "hello"], &[
            ("hello", "riscv only"),
            ("kernel", "riscv only"),
        ])
        .expect("valid plan");
        assert_eq!(plan, vec!["kernel", "hello"]);
    }

    #[test]
    fn a_host_crate_is_not_in_the_riscv_plan() {
        let plan = riscv_only_plan(&["collector"], &[]).expect("valid plan");
        assert!(plan.is_empty(), "no exclusions means nothing is riscv-only: {plan:?}");
    }

    /// Same guard the unit-test plan has: a renamed crate must not leave a
    /// silent entry behind.
    #[test]
    fn an_entry_naming_a_departed_crate_is_an_error() {
        let err = riscv_only_plan(&["collector"], &[("kernel-core", "gone")])
            .expect_err("stale entry must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    /// The invariant the hardcoded clippy allow-list used to break: every
    /// workspace member is linted by exactly one of the two gates. A crate can
    /// no longer be silently unlinted by simple omission — which is how
    /// `snemu`, `stitch`, `hitch` and eleven others drifted out.
    #[test]
    fn every_workspace_member_is_linted_by_exactly_one_gate() {
        let members = super::workspace_members().expect("cargo metadata");
        let names: Vec<&str> = members.iter().map(String::as_str).collect();
        let host: Vec<&str> = unit_test_plan(&names, NOT_HOST_TESTED, &[])
            .expect("valid plan")
            .iter()
            .map(|(name, _)| *name)
            .collect();
        let riscv = riscv_only_plan(&names, NOT_HOST_TESTED).expect("valid plan");

        for member in &names {
            let in_host = host.contains(member);
            let in_riscv = riscv.contains(member);
            assert!(in_host || in_riscv, "{member} is linted by neither gate");
            assert!(!(in_host && in_riscv), "{member} is linted by both gates");
        }
    }
}
