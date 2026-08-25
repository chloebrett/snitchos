//! Counter-registry drift — the guard against a metric nothing ever drains.
//!
//! A `DeferredCounter` is only half a metric. Declaring one and incrementing it does
//! nothing observable; it reaches the wire only if it also appears in
//! `kernel::counter::COUNTERS`, which the heartbeat walks. Miss that second step and
//! you get a counter that climbs forever in silence — live code, correct arithmetic,
//! no output.
//!
//! Two of these existed when this check was written. `snitchos.audio.xruns_total` —
//! whose own doc comment calls it "the marquee real-time observable" — was never
//! registered, and neither was `snitchos.smp4.worker_ticks_total`. Nothing caught
//! either, because nothing *can*: the compiler sees two correct statements, and a
//! metric's absence from the wire looks exactly like a system that had nothing to
//! report. The `XRun` one survived a further layer of camouflage — its sibling
//! `AudioXRun` *frame* worked, so the fault looked observable from outside.
//!
//! Same rationale as [`crate::links`] and the generated-diagram drift check: a
//! contract nothing compiles needs a test, or it rots invisibly.
//!
//! Scope: `kernel/src`, and deliberately source-level. The kernel is `no_std`/`no_main`
//! and cannot host a `#[test]`, so the registry cannot check itself from the inside.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Every `static NAME: DeferredCounter` declared in one file's source.
///
/// Pure: the caller decides which files to feed it.
#[must_use]
pub fn declared_counters(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in src.match_indices("static ") {
        let rest = &src[at + "static ".len()..];
        let Some(colon) = rest.find(':') else { continue };
        let name = rest[..colon].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            continue;
        }
        // The declared type: everything between the `:` and the initializer. Checking
        // the *type* rather than the initializer catches a counter declared with a
        // path-qualified `crate::obs::counter::DeferredCounter::new`, and avoids
        // matching prose about `DeferredCounter` in a nearby doc comment.
        let after = &rest[colon + 1..];
        let end = after.find('=').unwrap_or(after.len());
        let ty = after[..end].trim();
        // Must *be* a counter, not merely mention one: `COUNTERS`'s own type is
        // `&[&DeferredCounter]`, and a substring match reports the registry as an
        // unregistered counter — a permanent false positive in the one report people
        // need to trust.
        if ty.ends_with("DeferredCounter") && !ty.contains('[') {
            out.push(name.to_string());
        }
    }
    out
}

/// Every counter named in the `COUNTERS` registry array.
///
/// Pure, and tolerant of comments inside the array — the entries are `&crate::…::NAME`
/// paths, and a `//` line is skipped rather than scanned.
#[must_use]
pub fn registered_counters(src: &str) -> Vec<String> {
    let Some(start) = src.find("COUNTERS:") else { return Vec::new() };
    let rest = &src[start..];
    let Some(open) = rest.find("&[") else { return Vec::new() };
    let body_start = start + open;
    let Some(close) = src[body_start..].find("];") else { return Vec::new() };
    let body = &src[body_start..body_start + close];

    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }
        let Some(at) = line.find("&crate::") else { continue };
        let path = line[at..].trim_end_matches(',');
        if let Some(name) = path.rsplit("::").next() {
            let name = name.trim().trim_end_matches(',');
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Verify every declared `DeferredCounter` is registered for draining.
///
/// Wired into `cargo xtask test` beside the doc-link and diagram-drift checks.
pub fn check() -> ExitCode {
    let root = workspace_root();
    let kernel_src = root.join("kernel").join("src");

    let mut files = Vec::new();
    if let Err(e) = collect_rs_files(&kernel_src, &mut files) {
        eprintln!("counter registry: {e}");
        return ExitCode::from(1);
    }
    files.sort();

    let registry_path = kernel_src.join("obs").join("counter.rs");
    let registry = match std::fs::read_to_string(&registry_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("counter registry: {}: {e}", registry_path.display());
            return ExitCode::from(1);
        }
    };
    let registered = registered_counters(&registry);
    if registered.is_empty() {
        eprintln!(
            "counter registry: found no entries in COUNTERS ({}). The array moved or \
             changed shape — this check is now blind and must be repaired, not deleted.",
            registry_path.display()
        );
        return ExitCode::from(1);
    }

    let mut unregistered = Vec::new();
    let mut declared_total = 0usize;
    for file in &files {
        let Ok(src) = std::fs::read_to_string(file) else { continue };
        for name in declared_counters(&src) {
            declared_total += 1;
            if !registered.contains(&name) {
                let shown = file.strip_prefix(&root).unwrap_or(file).display().to_string();
                unregistered.push(format!("{name} ({shown})"));
            }
        }
    }

    if unregistered.is_empty() {
        eprintln!(
            "counter registry: {declared_total} counters declared, all registered for draining"
        );
        return ExitCode::SUCCESS;
    }
    eprintln!("counter registry: {} counter(s) never drained:", unregistered.len());
    for u in &unregistered {
        eprintln!("  {u}");
    }
    eprintln!(
        "\nA `DeferredCounter` reaches the wire only via `counter::COUNTERS`, which the\n\
         heartbeat walks. One that is declared and incremented but not listed there\n\
         climbs forever in silence — and that is indistinguishable from a system with\n\
         nothing to report. Add it to COUNTERS in kernel/src/obs/counter.rs."
    );
    ExitCode::from(1)
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rs")) {
            out.push(path);
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask-cmds has a workspace-root parent")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_plainly_declared_counter() {
        let src = r#"
pub static XRUNS: DeferredCounter = DeferredCounter::new("snitchos.audio.xruns_total");
"#;
        assert_eq!(declared_counters(src), vec!["XRUNS".to_string()]);
    }

    #[test]
    fn finds_one_declared_across_two_lines_with_a_qualified_path() {
        // Both forms occur in the kernel: the initializer wraps to its own line, and
        // the type is reached through `crate::obs::counter::`. A check that only
        // matched the single-line unqualified shape would miss real counters.
        let src = r#"
pub static SMP4_WORKER_TICKS: DeferredCounter =
    DeferredCounter::new("snitchos.smp4.worker_ticks_total");
pub static FP_SAVES: crate::obs::counter::DeferredCounter =
    crate::obs::counter::DeferredCounter::new("snitchos.fp.context_saves_total");
"#;
        assert_eq!(
            declared_counters(src),
            vec!["SMP4_WORKER_TICKS".to_string(), "FP_SAVES".to_string()]
        );
    }

    #[test]
    fn ignores_statics_that_are_not_counters() {
        let src = r#"
static AUDIO_ACTIVE: AtomicBool = AtomicBool::new(false);
static PWMDAC_BASE: usize = 0x100b_0000;
"#;
        assert!(declared_counters(src).is_empty());
    }

    #[test]
    fn prose_mentioning_the_type_is_not_a_declaration() {
        // The registry file itself explains what a `DeferredCounter` is, at length.
        // Matching the initializer instead of the declared type would count the prose.
        let src = "/// A `DeferredCounter` is drained by the heartbeat.\n\
                   /// See DeferredCounter::new for the constructor.\n";
        assert!(declared_counters(src).is_empty());
    }

    /// The registry is not one of the things it registers.
    ///
    /// `COUNTERS`'s own type contains `DeferredCounter`, so a naive substring match
    /// reports the array as an undrained counter — which it did on the first live run,
    /// against a tree where the only real finding was one counter. A guard whose output
    /// includes a permanent false positive teaches people to skim past it.
    #[test]
    fn the_registry_array_is_not_itself_a_counter() {
        let src = "pub static COUNTERS: &[&DeferredCounter] = &[\n    &crate::ipi::X,\n];\n";
        assert!(declared_counters(src).is_empty());
    }

    #[test]
    fn reads_the_registry_array() {
        let src = r#"
pub static COUNTERS: &[&DeferredCounter] = &[
    &crate::ipi::RECEIVED_TOTAL,
    &crate::pwmdac::SAMPLES_EMITTED,
];
"#;
        assert_eq!(
            registered_counters(src),
            vec!["RECEIVED_TOTAL".to_string(), "SAMPLES_EMITTED".to_string()]
        );
    }

    #[test]
    fn a_comment_inside_the_registry_is_not_an_entry() {
        // The array carries explanatory comments, including ones naming counters.
        let src = r#"
pub static COUNTERS: &[&DeferredCounter] = &[
    // audio — &crate::pwmdac::NOT_A_REAL_ENTRY, described in prose
    &crate::pwmdac::SAMPLES_EMITTED,
];
"#;
        assert_eq!(registered_counters(src), vec!["SAMPLES_EMITTED".to_string()]);
    }

    /// The check must fail loudly if the array it reads ever moves or is renamed.
    /// A silent empty read would turn this guard into one that always passes.
    #[test]
    fn a_missing_registry_reads_as_empty_so_the_caller_can_refuse() {
        assert!(registered_counters("fn main() {}").is_empty());
    }
}
