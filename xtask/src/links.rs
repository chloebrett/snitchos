//! Markdown link checking — the guard against a moved file leaving a dead link.
//!
//! Moving a doc down a directory (`plans/x.md` → `plans/legacy/x.md`) silently
//! breaks two things the compiler never sees: every inbound link to it, and every
//! *outbound* `../` link it carries (from `plans/legacy/`, `../docs/` resolves to
//! `plans/docs/`, which has never existed). That second one has bitten every
//! `git mv` sweep this repo has done. This check makes it fail the gate instead.
//!
//! Scope: relative `.md` targets only. External URLs, anchors, and images are
//! skipped — the bug class is repo-internal file moves.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Repo-relative path prefixes holding no navigable documentation.
///
/// The `.claude/` entries are prompt libraries, not docs: an agent or command
/// definition contains *illustrative* markdown (`[API Reference](docs/api.md)`
/// inside a table showing what good docs look like), which was never meant to
/// resolve. `.claude/CLAUDE.md` itself is real documentation and stays checked.
const SKIP_PREFIXES: &[&str] =
    &[".git", "target", ".claude/agents", ".claude/commands", ".claude/skills"];

/// Is this repo-relative path outside the documentation we navigate?
fn is_skipped(rel: &Path) -> bool {
    SKIP_PREFIXES.iter().any(|p| rel.starts_with(p))
}

/// A markdown link worth checking: a relative `.md` target plus the 1-based line
/// it appeared on (for a clickable `file:line` report).
#[derive(Debug, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub line: usize,
}

/// Verify every relative `.md` link in the repo resolves to a file that exists.
///
/// Wired into `cargo xtask test` beside the generated-diagram drift check, and
/// for the same reason: a link is a contract the compiler can't see, so nothing
/// else notices when a `git mv` breaks it.
pub fn check() -> ExitCode {
    let root = workspace_root();
    let mut files = Vec::new();
    if let Err(e) = collect_md_files(&root, &root, &mut files) {
        eprintln!("doc links: {e}");
        return ExitCode::from(1);
    }
    files.sort();

    let mut broken = Vec::new();
    for file in &files {
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("doc links: {}: {e}", file.display());
                return ExitCode::from(1);
            }
        };
        let dir = file.parent().unwrap_or(&root);
        for lnk in extract_md_links(&content) {
            if !dir.join(&lnk.target).exists() {
                let shown = file.strip_prefix(&root).unwrap_or(file).display().to_string();
                broken.push(format!("{shown}:{} -> {}", lnk.line, lnk.target));
            }
        }
    }

    if broken.is_empty() {
        eprintln!("doc links: {} files, all links resolve", files.len());
        return ExitCode::SUCCESS;
    }
    eprintln!("doc links: {} broken link(s):", broken.len());
    for b in &broken {
        eprintln!("  {b}");
    }
    eprintln!(
        "\nA moved file breaks links in both directions: inbound ones still name the old\n\
         path, and the moved file's own `../` links now resolve one level too high.\n\
         (From `plans/legacy/`, `../docs/` means `plans/docs/`, which never existed.)"
    );
    ExitCode::from(1)
}

fn collect_md_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let rel = path.strip_prefix(root).unwrap_or(&path);
        if is_skipped(rel) {
            continue;
        }
        if path.is_dir() {
            collect_md_files(root, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace-root parent")
        .to_path_buf()
}

/// Extract checkable link targets from markdown.
///
/// Pure: the caller resolves each target against the containing file's directory.
pub fn extract_md_links(content: &str) -> Vec<Link> {
    content
        .lines()
        .enumerate()
        .flat_map(|(i, text)| targets_in_line(text).map(move |t| Link { target: t, line: i + 1 }))
        .collect()
}

/// The checkable `](target)` occurrences on one line.
fn targets_in_line(text: &str) -> impl Iterator<Item = String> + '_ {
    text.match_indices("](").filter_map(|(open, _)| {
        let rest = &text[open + 2..];
        let close = rest.find(')')?;
        checkable(&rest[..close])
    })
}

/// A target we can verify, with any `#anchor` stripped — or `None` if it isn't
/// ours to check (external URL, bare anchor, or a non-`.md` file).
fn checkable(target: &str) -> Option<String> {
    // `scheme://…` and `mailto:` are somebody else's problem. Checking the path
    // part before `#` keeps `foo.md#why` in scope.
    if target.contains("://") || target.starts_with("mailto:") {
        return None;
    }
    let path = target.split('#').next()?;
    if !path.ends_with(".md") {
        return None;
    }
    Some(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(target: &str, line: usize) -> Link {
        Link { target: target.to_string(), line }
    }

    #[test]
    fn extracts_a_relative_md_link_with_its_line() {
        let content = "intro\nsee [the plan](plans/foo.md) for more\n";
        assert_eq!(extract_md_links(content), vec![link("plans/foo.md", 2)]);
    }

    #[test]
    fn skips_external_urls() {
        // A URL is not ours to verify, and hitting the network in the test gate
        // would be a different (and much worse) kind of check.
        let content = "[blog](https://determinate.systems/blog/qemu-fix/) [x](a.md)";
        assert_eq!(extract_md_links(content), vec![link("a.md", 1)]);
    }

    #[test]
    fn skips_pure_anchors() {
        assert_eq!(extract_md_links("[jump](#the-section)"), vec![]);
    }

    #[test]
    fn strips_the_anchor_from_a_file_link() {
        // `foo.md#section` — the file is checkable even though the anchor isn't.
        assert_eq!(extract_md_links("[x](../docs/foo.md#why)"), vec![link("../docs/foo.md", 1)]);
    }

    #[test]
    fn skips_non_markdown_targets() {
        // Images and directory links rot too, but the bug this guards is `.md`
        // files moving between directories. Staying narrow keeps it honest.
        let content = "![shot](posts/a.png) [dir](redesign-reviews/) [x](a.md)";
        assert_eq!(extract_md_links(content), vec![link("a.md", 1)]);
    }

    #[test]
    fn walks_real_documentation_including_the_excluded_learning_tree() {
        assert!(!is_skipped(Path::new("docs/ipc-design.md")));
        assert!(!is_skipped(Path::new("plans/legacy/v0.9-ipc.md")));
        assert!(!is_skipped(Path::new("posts/post-12.md")));
        // `learning/` is excluded from the cargo workspace but its docs are real.
        assert!(!is_skipped(Path::new("learning/v0.9-ipc/cheat-sheet.md")));
        // CLAUDE.md is project documentation and its links are real.
        assert!(!is_skipped(Path::new(".claude/CLAUDE.md")));
    }

    #[test]
    fn skips_build_output_and_vcs() {
        assert!(is_skipped(Path::new("target/debug/build/x/out/README.md")));
        assert!(is_skipped(Path::new(".git/something.md")));
    }

    #[test]
    fn skips_prompt_libraries_whose_markdown_is_illustrative() {
        // An agent/command/skill definition is a *prompt*. Its markdown is example
        // content shown to the model — `[API Reference](docs/api.md)` in a table
        // demonstrating good docs — not navigation anyone follows. Checking it
        // reports failures for files that were never meant to resolve.
        assert!(is_skipped(Path::new(".claude/agents/docs-guardian.md")));
        assert!(is_skipped(Path::new(".claude/commands/pr.md")));
        assert!(is_skipped(Path::new(".claude/skills/tdd/SKILL.md")));
    }
}
