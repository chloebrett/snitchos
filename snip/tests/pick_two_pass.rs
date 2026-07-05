use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use snip::{Candidate, ClaudeCfg, Status, pick_two_pass};

fn candidate(path: &str) -> Candidate {
    // A real two-hunk diff so a pass-2 full prompt has something to label.
    let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,1 +1,2 @@\n a\n+b\n";
    Candidate { path: path.to_string(), status: Status::Modified, diff: diff.to_string() }
}

/// A fake `claude` that answers the pass-1 (triage) prompt and the pass-2 (full)
/// prompt differently, keyed on a marker only the triage prompt contains.
fn fake_claude(name: &str, triage_result: &str, full_result: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("snip-2pass-test");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    // The triage prompt is the only one containing "settled_include"; branch on it.
    let script = format!(
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in\n  *settled_include*) cat <<'T'\n{}\nT\n  ;;\n  *) cat <<'F'\n{}\nF\n  ;;\nesac\n",
        envelope_with_string(triage_result),
        envelope_with_string(full_result),
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Build a claude JSON envelope whose `result` string is `inner` (JSON-escaped).
fn envelope_with_string(inner: &str) -> String {
    let escaped = inner.replace('\\', "\\\\").replace('"', "\\\"");
    format!(r#"{{"is_error":false,"result":"{escaped}","usage":{{"input_tokens":100,"output_tokens":10}}}}"#)
}

fn cfg(program: &Path) -> ClaudeCfg {
    ClaudeCfg { program: program.to_string_lossy().into_owned(), ..ClaudeCfg::default() }
}

#[test]
fn settling_everything_in_pass_one_skips_the_full_pass() {
    let triage = r#"{"settled_include":["a.rs"],"settled_exclude":["b.rs"],"needs_diff":[]}"#;
    let program = fake_claude("all-settled.sh", triage, "SHOULD_NOT_BE_USED");
    let candidates = [candidate("a.rs"), candidate("b.rs")];

    let (sel, usage) = pick_two_pass("msg", &candidates, &cfg(&program)).expect("two-pass ok");

    assert!(sel.include.iter().any(|i| i.path == "a.rs"));
    assert!(sel.exclude.iter().any(|e| e.path == "b.rs"));
    assert!(sel.is_confident(), "an all-settled triage is auto-stageable");
    assert_eq!(usage.total(), 110, "only ONE call was made (pass 1)");
}

#[test]
fn a_needs_diff_file_triggers_the_full_pass() {
    let triage = r#"{"settled_include":["a.rs"],"settled_exclude":[],"needs_diff":["c.rs"]}"#;
    let full = r#"{"include":[{"path":"c.rs","reason":"the parser change","confidence":"high"}],"exclude":[],"overall":"high"}"#;
    let program = fake_claude("escalate.sh", triage, full);
    let candidates = [candidate("a.rs"), candidate("c.rs")];

    let (sel, usage) = pick_two_pass("msg", &candidates, &cfg(&program)).expect("two-pass ok");

    assert!(sel.include.iter().any(|i| i.path == "a.rs"), "settled file kept");
    assert!(sel.include.iter().any(|i| i.path == "c.rs"), "escalated file resolved");
    assert_eq!(usage.total(), 220, "TWO calls were made (pass 1 + pass 2)");
}
