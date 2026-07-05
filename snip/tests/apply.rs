//! End-to-end proof that `build_patch` output actually stages via
//! `git apply --cached`: a two-hunk change, stage only the second hunk, and
//! assert the index holds that change while the first stays unstaged.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use snip::{build_patch, parse_hunks};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git_apply_cached(dir: &Path, patch: &str) {
    let mut child = Command::new("git")
        .current_dir(dir)
        .args(["apply", "--cached", "--recount", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git apply");
    child.stdin.take().unwrap().write_all(patch.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git apply failed: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn stages_only_the_selected_hunk_via_git_apply() {
    let dir = std::env::temp_dir().join("snip-apply-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    git(&dir, &["init", "-q", "-b", "main"]);
    // Twenty lines so edits far apart produce two hunks whose 3-line context
    // regions don't touch (and thus don't merge into one hunk).
    let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.join("f.txt"), &base).unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"]);

    // Edit line 2 (hunk 1) and line 18 (hunk 2).
    let edited = base.replace("line 2\n", "line 2 EDITED\n").replace("line 18\n", "line 18 EDITED\n");
    std::fs::write(dir.join("f.txt"), &edited).unwrap();

    let diff = git(&dir, &["diff", "HEAD", "--", "f.txt"]);
    let file = parse_hunks(&diff);
    assert_eq!(file.hunks.len(), 2, "the two edits are separate hunks");

    // Stage only the second hunk.
    let patch = build_patch(&file, &["H2".to_string()]).expect("H2 patch");
    git_apply_cached(&dir, &patch);

    let staged = git(&dir, &["diff", "--cached"]);
    assert!(staged.contains("line 18 EDITED"), "H2 is staged");
    assert!(!staged.contains("line 2 EDITED"), "H1 is NOT staged");

    let unstaged = git(&dir, &["diff"]);
    assert!(unstaged.contains("line 2 EDITED"), "H1 remains unstaged");

    let _ = std::fs::remove_dir_all(&dir);
}
