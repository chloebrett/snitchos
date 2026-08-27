//! Plan status-header hygiene — the guard against an index that quietly lies.
//!
//! `plans/README.md` is the five-second answer to "what is actually live?", and
//! nothing checks it. Every sweep this repo has done found it stale, always in
//! the same direction: correct when written, wrong within weeks, and wrong
//! *precisely* where work was happening. The individual plan headers were
//! accurate every time; only the index drifted.
//!
//! So this checks the two things a machine can check without guessing at
//! meaning: every plan carries a **dated** status header, and every plan is
//! reachable from the index. It deliberately does **not** fail on a header's
//! age — a gate that turns red through the passage of time alone teaches people
//! to ignore it.

use crate::links::workspace_root;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

/// How far into a plan the status header may sit.
///
/// Bounded on purpose: `stim-v1.md` and `visionfive2-port.md` both carry
/// per-step `**Status**:` notes deep in the body (a step's state, not the
/// plan's). A whole-file scan reads the first of those as the plan's status,
/// which is exactly the wrong answer.
const HEADER_LINES: usize = 15;

/// The `YYYY-MM-DD` from a plan's `**Status (…)**:` header, if it has one.
pub fn status_date(content: &str) -> Option<&str> {
    content.lines().take(HEADER_LINES).find_map(dated_status)
}

/// The date in one `**Status (YYYY-MM-DD)**:` line.
fn dated_status(line: &str) -> Option<&str> {
    let date = line.strip_prefix("**Status (")?.split_once(")**:")?.0;
    is_iso_date(date).then_some(date)
}

/// Verify every plan carries a dated status header and is reachable from the index.
///
/// Two failures, both caused by a human and both invisible until someone reads
/// the index and believes it:
///
/// - **An undated status header.** A bare status asserts a freshness it cannot
///   back up. Every stale entry this repo has found was true when written.
/// - **A plan the index doesn't link.** A plan nothing points at is work nobody
///   will find.
///
/// Deliberately **not** checked: how old a date is. A gate that reddens through
/// the passage of time alone gets ignored, and the only way to green it without
/// doing the work is to lie about the date — which would corrupt the one signal
/// this whole convention exists to carry.
pub fn check() -> ExitCode {
    let plans = workspace_root().join("plans");

    let readme = match std::fs::read_to_string(plans.join("README.md")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("plan status: cannot read plans/README.md: {e}");
            return ExitCode::from(1);
        }
    };
    let indexed = linked_plans(&readme);

    let entries = match std::fs::read_dir(&plans) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("plan status: cannot read plans/: {e}");
            return ExitCode::from(1);
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_markdown(n) && !is_reference_doc(n))
        .collect();
    names.sort();

    let mut dated: Vec<(String, String)> = Vec::new();
    let mut undated: Vec<String> = Vec::new();
    let mut unindexed: Vec<String> = Vec::new();

    for name in &names {
        let content = match std::fs::read_to_string(plans.join(name)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("plan status: cannot read plans/{name}: {e}");
                return ExitCode::from(1);
            }
        };
        match status_date(&content) {
            Some(d) => dated.push((d.to_owned(), name.clone())),
            None => undated.push(name.clone()),
        }
        if !indexed.contains(name.as_str()) {
            unindexed.push(name.clone());
        }
    }

    // ISO dates sort chronologically as plain strings, so the stalest float to
    // the top for free — no calendar arithmetic, no date dependency.
    dated.sort();
    for (date, name) in &dated {
        println!("  {date}  plans/{name}");
    }

    for name in &undated {
        eprintln!("plans/{name}: status header is missing or undated");
        eprintln!("  expected a line matching `**Status (YYYY-MM-DD)**:` in the first {HEADER_LINES} lines");
    }
    for name in &unindexed {
        eprintln!("plans/{name}: not linked from plans/README.md");
    }

    if undated.is_empty() && unindexed.is_empty() {
        println!(
            "plan status: {} plans, all dated and indexed ({} reference docs skipped)",
            dated.len(),
            NOT_PLANS.len()
        );
        return ExitCode::SUCCESS;
    }
    ExitCode::from(1)
}

/// Files under `plans/` that are not plans, each with the reason.
///
/// A deny-list with written reasons, not an allow-list: this repo has already
/// been bitten by allow-lists-by-omission, where a new crate was silently never
/// linted because nobody remembered to add it. Here the default is "you are a
/// plan and you need a dated header", and opting out is a decision someone has
/// to write down.
const NOT_PLANS: &[(&str, &str)] = &[
    ("README.md", "the index itself — it describes plans, it isn't one"),
    ("v0.4-memory-findings.md", "CLAUDE.md cites it as required reading before touching boot order or address translation"),
    ("scaling-corners.md", "CLAUDE.md cites it as the corners v0.1 sidesteps"),
    ("stitch-examples-findings.md", "the lab notebook behind the 30-program corpus"),
    ("stitch-language-improvements.md", "the proposal catalogue other plans derive from"),
    ("corpus-recipe-axes.md", "a data spec, shipped as `batch9.toml` and cited from `cram-gen/src/recipe.rs`"),
];

/// Does this name end in a markdown extension?
fn is_markdown(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// Is this `plans/` file a living document rather than a plan?
fn is_reference_doc(name: &str) -> bool {
    NOT_PLANS.iter().any(|(n, _)| *n == name)
}

/// Every sibling plan the index links to, deduplicated.
///
/// Sibling only: a target with a `/` in it is either an archived plan
/// (`legacy/…`) or somewhere else entirely (`../docs/…`), and neither is live
/// work this index is accountable for.
pub fn linked_plans(readme: &str) -> BTreeSet<&str> {
    readme
        .match_indices("](")
        .filter_map(|(i, _)| {
            let rest = &readme[i + 2..];
            let target = rest.split_once(')')?.0;
            (is_markdown(target) && !target.contains('/')).then_some(target)
        })
        .collect()
}

/// Shape-check only — `YYYY-MM-DD` with digits where digits belong. A real
/// calendar parse would buy nothing here: the date is a human's claim about
/// when they last checked, and no library can validate that.
fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_dated_status_header() {
        let content = "# Plan: a thing\n\n**Status (2026-08-27)**: 🟡 **PARTIAL.**\n";
        assert_eq!(status_date(content), Some("2026-08-27"));
    }

    /// The trap this module exists to avoid. `stim-v1.md` carries
    /// `**Status**: written — RED span-sequence test asserts …` at line 532: a
    /// note about one *step*, not the plan. Reading it as the plan's status
    /// would report a plan as undated that is dated, or vice versa.
    #[test]
    fn ignores_a_status_note_below_the_header_block() {
        let mut content = String::from("# Plan: a thing\n\n**Status (2026-08-27)**: ok\n");
        content.push_str(&"filler\n".repeat(40));
        content.push_str("**Status**: written — a per-step note\n");
        assert_eq!(status_date(&content), Some("2026-08-27"));
    }

    #[test]
    fn rejects_an_undated_status_header() {
        let content = "# Plan: a thing\n\n**Status**: 🟡 **PARTIAL.**\n";
        assert_eq!(status_date(content), None);
    }

    /// Both undated spellings were in use before the convention was settled, and
    /// neither carries the fact the date exists to carry.
    #[test]
    fn rejects_the_other_undated_spelling() {
        let content = "# Plan: a thing\n\n**Status:** 🟡 **PARTIAL.**\n";
        assert_eq!(status_date(content), None);
    }

    #[test]
    fn rejects_a_malformed_date() {
        let content = "# Plan: a thing\n\n**Status (August 2026)**: ok\n";
        assert_eq!(status_date(content), None);
    }

    /// The living documents that happen to live in `plans/`. They will never
    /// "finish", so demanding a plan status header of them would produce a
    /// permanent red that the only available fix is to lie about.
    #[test]
    fn reference_documents_are_not_plans() {
        assert!(is_reference_doc("scaling-corners.md"));
        assert!(is_reference_doc("v0.4-memory-findings.md"));
        assert!(is_reference_doc("corpus-recipe-axes.md"));
        assert!(!is_reference_doc("uart-telemetry.md"));
        assert!(!is_reference_doc("board-bridge.md"));
    }

    #[test]
    fn the_index_itself_is_not_a_plan() {
        assert!(is_reference_doc("README.md"));
    }

    fn linked(readme: &str) -> Vec<&str> {
        linked_plans(readme).into_iter().collect()
    }

    #[test]
    fn finds_a_plan_linked_from_the_index() {
        let readme = "| [uart-telemetry.md](uart-telemetry.md) | frames | 🟡 |\n";
        assert_eq!(linked(readme), vec!["uart-telemetry.md"]);
    }

    /// The index links out to notes, docs and posts constantly. Those are not
    /// plans and must not be demanded to have a plan status header.
    #[test]
    fn ignores_links_that_leave_the_plans_directory() {
        let readme = "see [notes](../notes/stock-take.md) and [design](../docs/x.md)\n";
        assert!(linked(readme).is_empty());
    }

    /// Archived plans are finished by definition; the index links the directory
    /// and sometimes individual files, and neither is live work.
    #[test]
    fn ignores_archived_plans() {
        let readme = "[legacy/](legacy/) and [old](legacy/v0.9-ipc.md)\n";
        assert!(linked(readme).is_empty());
    }

    #[test]
    fn counts_a_plan_linked_twice_once() {
        let readme = "[a](board-bridge.md) … [a](board-bridge.md)\n";
        assert_eq!(linked(readme), vec!["board-bridge.md"]);
    }

    /// A header past the block is not a header. Keeps `HEADER_LINES` honest.
    #[test]
    fn ignores_a_dated_header_below_the_block() {
        let mut content = String::from("# Plan: a thing\n");
        content.push_str(&"filler\n".repeat(HEADER_LINES));
        content.push_str("**Status (2026-08-27)**: too late\n");
        assert_eq!(status_date(&content), None);
    }
}
