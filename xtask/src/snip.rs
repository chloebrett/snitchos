//! `cargo xtask snip` — Sonnet-assisted staging for parallel-agent workflows.
//!
//! You write the commit message; Sonnet decides which of the many concurrent
//! working-tree changes belong to it. The pure prompt-build / reply-parse logic
//! lives in the `snip` crate; this module owns all the git I/O and the CLI.
//!
//! Flow (three explicit steps — see `plans/snip-stage-picker.md`):
//!   cargo xtask snip "<message>"   propose (asks Sonnet, writes a plan)
//!   cargo xtask snip stage         `git add` the planned files
//!   cargo xtask snip commit        `git commit` the plan's message

use std::fs;
use std::process::{Command, ExitCode};

use serde::{Deserialize, Serialize};
use snip::{
    Candidate, ClaudeCfg, Confidence, Selection, Status, Usage, build_patch, fingerprint,
    parse_hunks, parse_status, pick, pick_two_pass,
};

const PLAN_PATH: &str = ".git/snip-plan.json";

/// Context lines each side of a hunk when diffing. Fewer = smaller prompt.
/// MUST be identical in `gather` (what the model labels) and `stage_partial`
/// (what we reconstruct), or hunk boundaries — and thus the H-ids — diverge.
const DIFF_CONTEXT: &str = "-U1";

/// A file to stage only in part: named hunks of its diff, applied via
/// `git apply --cached`.
#[derive(Serialize, Deserialize)]
struct Partial {
    path: String,
    hunks: Vec<String>,
}

/// Persisted proposal: what to stage, and a fingerprint to detect drift.
#[derive(Serialize, Deserialize)]
struct Plan {
    message: String,
    /// Whole-file stages (`git add`).
    include: Vec<String>,
    /// Partial stages (`git apply --cached` of the named hunks).
    #[serde(default)]
    partial: Vec<Partial>,
    fingerprint: String,
    overall: String,
}

/// Options for the propose step.
pub struct ProposeOpts {
    /// Status-only payload (no diffs). Fastest, coarser.
    pub fast: bool,
    /// Stage immediately after proposing, regardless of confidence.
    pub yes: bool,
    /// Suppress auto-staging even when the proposal is high-confidence throughout.
    pub no_auto: bool,
    /// Proceed despite drift / low confidence when staging.
    pub force: bool,
    /// Opt into the two-pass lean-first triage (a cheap paths-only pass, then
    /// full diffs only for undecided files). Rarely a token win: `claude -p`
    /// carries a ~57k-token fixed baseline PER call, so a second call usually
    /// costs more than the diffs it avoids sending. Worth it only when the diff
    /// payload is genuinely huge. Default is a single full-diff pass.
    pub lean: bool,
}

pub fn propose(message: &str, opts: &ProposeOpts) -> ExitCode {
    timed("propose", || match run_propose(message, opts) {
        Ok((code, usage)) => (code, Some(usage)),
        Err(e) => (fail(&e), None),
    })
}

pub fn stage(force: bool) -> ExitCode {
    timed("stage", || match run_stage(force) {
        Ok(()) => (ExitCode::SUCCESS, None),
        Err(e) => (fail(&e), None),
    })
}

pub fn commit(no_verify: bool) -> ExitCode {
    timed("commit", || match run_commit(no_verify) {
        Ok(()) => (ExitCode::SUCCESS, None),
        Err(e) => (fail(&e), None),
    })
}

/// Run `f`, printing how long it took — and, when the model was called, how many
/// tokens it used — to stderr, so it never pollutes parseable stdout.
fn timed(verb: &str, f: impl FnOnce() -> (ExitCode, Option<Usage>)) -> ExitCode {
    let start = std::time::Instant::now();
    let (code, usage) = f();
    let secs = start.elapsed().as_secs_f64();
    match usage {
        Some(u) if u.total() > 0 => {
            let cached = if u.cache_read_tokens > 0 {
                format!(" ({} cached)", u.cache_read_tokens)
            } else {
                String::new()
            };
            eprintln!(
                "(snip {verb} took {secs:.1}s · {} tokens: {} in{cached} / {} out)",
                u.total(),
                u.input_tokens,
                u.output_tokens
            );
        }
        _ => eprintln!("(snip {verb} took {secs:.1}s)"),
    }
    code
}

fn run_propose(message: &str, opts: &ProposeOpts) -> Result<(ExitCode, Usage), String> {
    let candidates = gather(opts.fast)?;
    if candidates.is_empty() {
        println!("Nothing to stage — the working tree is clean.");
        return Ok((ExitCode::SUCCESS, Usage::default()));
    }

    let strategy = if opts.lean { "lean-first" } else { "full-diff" };
    println!(
        "→ asking claude (sonnet, {strategy}) to triage {} changed file(s)…",
        candidates.len()
    );
    let cfg = ClaudeCfg::default();
    let picker = if opts.lean { pick_two_pass } else { pick };
    let (selection, usage) =
        picker(message, &candidates, &cfg).map_err(|e| format!("claude query failed: {e}"))?;

    print_selection(&selection);

    let include: Vec<String> = selection
        .include
        .iter()
        .filter(|i| i.hunks.is_none())
        .map(|i| i.path.clone())
        .collect();
    let partial: Vec<Partial> = selection
        .include
        .iter()
        .filter_map(|i| i.hunks.clone().map(|hunks| Partial { path: i.path.clone(), hunks }))
        .collect();

    let plan = Plan {
        message: message.to_string(),
        include,
        partial,
        fingerprint: fingerprint(&candidates),
        overall: confidence_str(selection.overall).to_string(),
    };
    write_plan(&plan)?;

    if plan.include.is_empty() && plan.partial.is_empty() {
        println!("\nNo files selected. Nothing written to stage.");
        return Ok((ExitCode::SUCCESS, usage));
    }

    let auto = selection.is_confident() && !opts.no_auto;
    if auto {
        println!("\n✓ high confidence throughout — auto-staging (use --no-auto to skip).");
    }
    if opts.yes || auto {
        println!();
        run_stage(opts.force)?;
    } else {
        println!("\nNext: `cargo xtask snip --stage` to stage these, then inspect `git diff --cached`.");
    }
    Ok((ExitCode::SUCCESS, usage))
}

fn run_stage(force: bool) -> Result<(), String> {
    let plan = read_plan()?;
    guard_drift(&plan, force)?;
    guard_confidence(&plan, force)?;

    if plan.include.is_empty() && plan.partial.is_empty() {
        return Err("plan selects no files".to_string());
    }

    if !plan.include.is_empty() {
        let mut args = vec!["add", "--"];
        args.extend(plan.include.iter().map(String::as_str));
        git(&args)?;
    }
    for p in &plan.partial {
        stage_partial(p)?;
    }

    println!("Staged {} file(s):", plan.include.len() + plan.partial.len());
    for p in &plan.include {
        println!("  + {p}");
    }
    for p in &plan.partial {
        println!("  + {} (hunks {})", p.path, p.hunks.join(", "));
    }
    println!("\nInspect with `git diff --cached`, then `cargo xtask snip --commit`.");
    Ok(())
}

/// Stage only the named hunks of one file: re-derive its current diff, rebuild a
/// patch of just those hunks, and `git apply --cached` it. Re-deriving (rather
/// than trusting a stored patch) means the drift guard already proved the hunk
/// ids still line up with the live diff.
fn stage_partial(p: &Partial) -> Result<(), String> {
    let diff = git_stdout(&["diff", DIFF_CONTEXT, "HEAD", "--", &p.path])?;
    let file = parse_hunks(&diff);
    let patch = build_patch(&file, &p.hunks).ok_or_else(|| {
        format!("none of the planned hunks for {} exist in its current diff", p.path)
    })?;
    git_apply_cached(&patch)
        .map_err(|e| format!("could not apply selected hunks of {}: {e}", p.path))
}

fn run_commit(no_verify: bool) -> Result<(), String> {
    let plan = read_plan()?;
    // No content-drift check here: staging legitimately changed the index since
    // propose. The guard that matters (tree changed under the AI's proposal)
    // already ran at `stage`; between stage and commit you eyeballed the diff.
    let mut args = vec!["commit", "-m", &plan.message];
    if no_verify {
        args.push("--no-verify");
    }
    git(&args)?;
    fs::remove_file(PLAN_PATH).map_err(|e| format!("committed, but could not clear {PLAN_PATH}: {e}"))?;
    Ok(())
}

/// Collect every changed file (tracked + untracked) as a diff-capped candidate.
fn gather(fast: bool) -> Result<Vec<Candidate>, String> {
    let porcelain = git_stdout(&["status", "--porcelain=v1", "-z"])?;
    let entries = parse_status(&porcelain);

    let mut candidates = Vec::with_capacity(entries.len());
    for entry in entries {
        let diff = if fast {
            String::new()
        } else {
            candidate_diff(&entry.path, entry.status)?
        };
        // Store the full diff: `build_prompt` caps per-hunk for display, and
        // partial staging needs faithful hunk text for id validation.
        candidates.push(Candidate { path: entry.path, status: entry.status, diff });
    }
    Ok(candidates)
}

fn candidate_diff(path: &str, status: Status) -> Result<String, String> {
    match status {
        // Untracked files have no HEAD blob to diff against — show their content.
        Status::Untracked => Ok(fs::read_to_string(path).unwrap_or_else(|_| "(binary or unreadable)".to_string())),
        // Everything else: the combined staged+unstaged diff against HEAD.
        _ => git_stdout(&["diff", DIFF_CONTEXT, "HEAD", "--", path]),
    }
}

fn guard_drift(plan: &Plan, force: bool) -> Result<(), String> {
    let current = fingerprint(&gather(false)?);
    if current != plan.fingerprint && !force {
        return Err(format!(
            "working tree changed since `snip` proposed (fingerprint {} → {}). \
Re-run `cargo xtask snip \"<message>\"`, or pass --force to stage anyway.",
            plan.fingerprint, current
        ));
    }
    Ok(())
}

fn guard_confidence(plan: &Plan, force: bool) -> Result<(), String> {
    if plan.overall == "low" && !force {
        return Err(
            "Sonnet reported LOW overall confidence in this partition. \
Review the proposal, then pass --force to stage anyway.".to_string(),
        );
    }
    Ok(())
}

fn print_selection(sel: &Selection) {
    println!("\ninclude ({}):", sel.include.len());
    for i in &sel.include {
        let scope = match &i.hunks {
            Some(hunks) => format!(" (partial: hunks {})", hunks.join(", ")),
            None => String::new(),
        };
        println!("  + [{:<4}] {}{scope}  — {}", confidence_str(i.confidence), i.path, i.reason);
    }
    println!("exclude ({}):", sel.exclude.len());
    for e in &sel.exclude {
        println!("  - [{:<4}] {}  — {}", confidence_str(e.confidence), e.path, e.reason);
    }
    print!("overall: {}", confidence_str(sel.overall));
    match &sel.note {
        Some(note) => println!(" — {note}"),
        None => println!(),
    }
}

fn confidence_str(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "high",
        Confidence::Medium => "med",
        Confidence::Low => "low",
    }
}

fn write_plan(plan: &Plan) -> Result<(), String> {
    let json = serde_json::to_string_pretty(plan).map_err(|e| e.to_string())?;
    fs::write(PLAN_PATH, json).map_err(|e| format!("could not write {PLAN_PATH}: {e}"))
}

fn read_plan() -> Result<Plan, String> {
    let raw = fs::read_to_string(PLAN_PATH).map_err(|_| {
        format!("no proposal found at {PLAN_PATH}. Run `cargo xtask snip \"<message>\"` first.")
    })?;
    serde_json::from_str(&raw).map_err(|e| format!("corrupt plan {PLAN_PATH}: {e}"))
}

/// Run git, requiring success; returns stdout.
fn git_stdout(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Feed `patch` to `git apply --cached` on stdin, staging just those hunks.
fn git_apply_cached(patch: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("git")
        .args(["apply", "--cached", "--recount", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run git apply: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "no stdin handle for git apply".to_string())?
        .write_all(patch.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Run git for effect (add/commit), streaming its output.
fn git(args: &[&str]) -> Result<(), String> {
    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(|e| format!("could not run git: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {} failed", args.join(" ")))
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("snip: {msg}");
    ExitCode::from(1)
}
