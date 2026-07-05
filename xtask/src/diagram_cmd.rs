//! `cargo xtask diagram <target>` — the I/O shell around the `diagram` crate.
//! This module owns the side effects (shelling out to `cargo metadata`, reading
//! and writing `docs/generated/`, the `--check` diff); all projection logic
//! lives in `diagram` and is host-tested there. See `docs/diagrams-design.md`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// Committed artifacts, relative to the workspace root.
const DEPS_DOC: &str = "docs/generated/deps.md";
const ITEST_MATRIX_DOC: &str = "docs/generated/itest-matrix.md";
const CAPS_DOC: &str = "docs/generated/caps.md";

/// Instruction-step window with no new `CapEvent` after which the `caps` boot is
/// considered settled and stops. Large enough to span init's setup + first
/// round of IPC (reply caps trickle in bursts), small enough to skip the long
/// heartbeat tail. `--steps` remains the hard ceiling.
const CAP_QUIESCENCE_STEPS: u64 = 10_000_000;

/// Verify every committed diagram in `docs/generated/` is up to date. Called
/// from the `cargo xtask test` gate so a stale diagram fails the suite. Runs
/// every target (each prints its own status) and fails if any is stale.
pub fn check_all() -> ExitCode {
    let deps_ok = deps(true) == ExitCode::SUCCESS;
    let matrix_ok = itest_matrix(true) == ExitCode::SUCCESS;
    if deps_ok && matrix_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Generate (or, with `check`, verify) the integration-test scenario/workload
/// matrix from xtask's `SCENARIOS` registry.
pub fn itest_matrix(check: bool) -> ExitCode {
    let scenarios: Vec<diagram::itest_matrix::ScenarioMeta> = crate::itest::SCENARIOS
        .iter()
        .map(|s| diagram::itest_matrix::ScenarioMeta {
            name: s.name.to_string(),
            workload: s.workload.map(str::to_string),
            tags: s.tags.iter().map(|t| (*t).to_string()).collect(),
            cpu_bound: matches!(s.cpu_profile, itest_harness::CpuProfile::Cpu),
        })
        .collect();
    let table = diagram::itest_matrix::matrix_table(&scenarios);
    let doc = render_doc(
        "Integration-test scenario / workload matrix",
        "itest-matrix",
        &table.to_markdown(),
    );

    let path = workspace_root().join(ITEST_MATRIX_DOC);
    if check {
        check_up_to_date(&path, &doc, "itest-matrix")
    } else {
        write_doc(&path, &doc)
    }
}

/// Hand-drawn (bucket A) diagram docs — each a self-contained `.md` with one
/// mermaid block. `svg` renders these to local SVGs. Add new hand-authored
/// diagrams here.
const HAND_DRAWN: &[&str] =
    &["docs/memory-map.md", "docs/context-switch.md", "docs/boot-handoff.md"];

/// Render every hand-drawn diagram doc's mermaid to a local SVG (gitignored)
/// via `mmdc` (mermaid-cli). The committed `.md` stays the source of truth and
/// renders on GitHub in-diff; the SVG is a local-viewing convenience — mermaid
/// (flowchart/sequence) can't go through graphviz like the graph targets do.
pub fn svg() -> ExitCode {
    if !command_exists("mmdc") {
        eprintln!(
            "diagram svg: `mmdc` (mermaid-cli) not found — install it with \
             `npm install -g @mermaid-js/mermaid-cli`, then re-run"
        );
        return ExitCode::from(1);
    }
    let root = workspace_root();
    let mut failures = 0u32;
    for rel in HAND_DRAWN {
        let md_path = root.join(rel);
        let md = match std::fs::read_to_string(&md_path) {
            Ok(md) => md,
            Err(e) => {
                eprintln!("diagram svg: reading {rel}: {e}");
                failures += 1;
                continue;
            }
        };
        let blocks = diagram::markdown::extract_mermaid(&md);
        if blocks.is_empty() {
            eprintln!("diagram svg: no mermaid block in {rel}");
            failures += 1;
            continue;
        }
        for (i, block) in blocks.iter().enumerate() {
            let svg_path = if blocks.len() == 1 {
                md_path.with_extension("svg")
            } else {
                md_path.with_extension(format!("{i}.svg"))
            };
            if !render_mermaid_svg(block, &svg_path) {
                failures += 1;
            }
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Pipe one mermaid block through `mmdc` to `out`. Returns `true` on success.
fn render_mermaid_svg(mermaid: &str, out: &Path) -> bool {
    let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("diagram");
    let tmp = std::env::temp_dir().join(format!("snitch-{stem}.mmd"));
    if let Err(e) = std::fs::write(&tmp, mermaid) {
        eprintln!("diagram svg: writing temp mmd: {e}");
        return false;
    }
    let status = Command::new("mmdc").arg("-i").arg(&tmp).arg("-o").arg(out).status();
    let _ = std::fs::remove_file(&tmp);
    match status {
        Ok(s) if s.success() => {
            eprintln!("diagram svg: wrote {}", out.display());
            true
        }
        Ok(s) => {
            eprintln!("diagram svg: mmdc failed for {}: {s}", out.display());
            false
        }
        Err(e) => {
            eprintln!("diagram svg: invoking mmdc: {e}");
            false
        }
    }
}

fn command_exists(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Generate the capability derivation tree: boot under snemu, fold its
/// `CapEvent` frames into a `parent_cap_id → cap_id` graph. A runtime snapshot,
/// not a contract — so it's written (never `--check`ed) and left out of the
/// `docs/generated/` gate.
pub fn caps(workload: Option<&str>, steps: u64) -> ExitCode {
    eprintln!(
        "diagram caps: booting {} under snemu (≤{steps} steps, early-stop after \
         {CAP_QUIESCENCE_STEPS} idle)…",
        workload.unwrap_or("init (default)")
    );
    let (frames, ran) = match crate::snemu_diff::collect_frames_until_cap_quiescence(
        workload,
        steps,
        CAP_QUIESCENCE_STEPS,
    ) {
        Ok(collected) => collected,
        Err(err) => {
            eprintln!("diagram caps: {err}");
            return ExitCode::from(1);
        }
    };
    let cap_events =
        frames.iter().filter(|f| matches!(f, protocol::stream::OwnedFrame::CapEvent { .. })).count();
    if cap_events == 0 {
        eprintln!(
            "diagram caps: no CapEvent frames in {} decoded — the boot may not have \
             reached userspace; try a larger --steps",
            frames.len()
        );
        return ExitCode::from(1);
    }

    let graph = diagram::caps::derivation_tree(&frames);
    let body = format!("```mermaid\n{}```\n", graph.to_mermaid());
    let doc = render_doc("Capability derivation tree", "caps", &body);

    let path = workspace_root().join(CAPS_DOC);
    let written = write_doc(&path, &doc);
    if written != ExitCode::SUCCESS {
        return written;
    }
    eprintln!("diagram caps: folded {cap_events} CapEvent frames (stopped at {ran} steps)");
    render_svg(&graph.to_dot(), &path.with_extension("svg"));
    ExitCode::SUCCESS
}

/// Generate (or, with `check`, verify) the workspace crate-dependency graph.
pub fn deps(check: bool) -> ExitCode {
    let json = match cargo_metadata() {
        Ok(json) => json,
        Err(err) => {
            eprintln!("diagram deps: {err}");
            return ExitCode::from(1);
        }
    };
    let members = match diagram::deps::parse_cargo_metadata(&json) {
        Ok(members) => members,
        Err(err) => {
            eprintln!("diagram deps: parsing cargo metadata: {err}");
            return ExitCode::from(1);
        }
    };
    let graph = diagram::deps::workspace_graph(&members);
    let body = format!("```mermaid\n{}```\n", graph.to_mermaid());
    let doc = render_doc("Workspace crate graph", "deps", &body);

    let path = workspace_root().join(DEPS_DOC);
    if check {
        return check_up_to_date(&path, &doc, "deps");
    }
    let written = write_doc(&path, &doc);
    if written != ExitCode::SUCCESS {
        return written;
    }
    // Local-dev convenience: also render an SVG via graphviz. Best-effort —
    // the committed, reviewable source of truth is the mermaid .md; a missing
    // `dot` never fails the command.
    render_svg(&graph.to_dot(), &path.with_extension("svg"));
    ExitCode::SUCCESS
}

/// Pipe DOT to `dot -Tsvg`, writing an SVG next to the `.md`. Graphviz layout
/// is version-dependent, so this artifact is gitignored (not `--check`ed);
/// it's just something to open in a browser during local dev. Absent `dot`,
/// warn and carry on.
fn render_svg(dot: &str, out: &Path) {
    let Ok(mut child) = Command::new("dot")
        .args(["-Tsvg", "-o"])
        .arg(out)
        .stdin(Stdio::piped())
        .spawn()
    else {
        eprintln!(
            "diagram: `dot` not found — skipping SVG (install graphviz, e.g. \
             `brew install graphviz`); the mermaid .md is written regardless"
        );
        return;
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(e) = stdin.write_all(dot.as_bytes())
    {
        eprintln!("diagram: writing to dot: {e}");
        return;
    }
    match child.wait() {
        Ok(status) if status.success() => eprintln!("diagram: wrote {}", out.display()),
        Ok(status) => eprintln!("diagram: dot exited with {status}"),
        Err(e) => eprintln!("diagram: waiting on dot: {e}"),
    }
}

/// `cargo metadata --no-deps` — packages are exactly the workspace members,
/// each still listing its declared dependency names.
fn cargo_metadata() -> Result<String, String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|e| format!("failed to invoke cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("cargo metadata output not utf-8: {e}"))
}

/// Wrap a pre-formatted body (a fenced mermaid block, a markdown table, …) in
/// the generated-doc envelope: a provenance header so nobody hand-edits it, and
/// a title. The body already carries its own trailing newline.
fn render_doc(title: &str, target: &str, body: &str) -> String {
    format!(
        "<!-- generated by: cargo xtask diagram {target} — do not edit -->\n\n\
         # {title}\n\n\
         {body}"
    )
}

fn write_doc(path: &Path, doc: &str) -> ExitCode {
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("diagram: creating {}: {e}", parent.display());
        return ExitCode::from(1);
    }
    match std::fs::write(path, doc) {
        Ok(()) => {
            eprintln!("diagram: wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("diagram: writing {}: {e}", path.display());
            ExitCode::from(1)
        }
    }
}

fn check_up_to_date(path: &Path, expected: &str, target: &str) -> ExitCode {
    let actual = std::fs::read_to_string(path).unwrap_or_default();
    if actual == expected {
        eprintln!("diagram {target}: {} is up to date", path.display());
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "diagram {target}: {} is stale — regenerate with `cargo xtask diagram {target}`",
            path.display()
        );
        ExitCode::from(1)
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace-root parent")
        .to_path_buf()
}
