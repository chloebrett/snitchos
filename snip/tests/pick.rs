use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use snip::{Candidate, ClaudeCfg, Status, pick};

fn candidate(path: &str) -> Candidate {
    Candidate { path: path.to_string(), status: Status::Modified, diff: "diff".to_string() }
}

/// Write an executable shell script into `dir`. The caller owns `dir` (a
/// `TempDir`) so each test gets its own — a shared path under `temp_dir()` is
/// mutable state between tests and races any concurrent run of this suite.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A fake `claude` that ignores stdin and prints `stdout_body`.
fn fake_claude(dir: &Path, name: &str, stdout_body: &str) -> PathBuf {
    let script = format!("#!/bin/sh\ncat >/dev/null\ncat <<'SNIP_EOF'\n{stdout_body}\nSNIP_EOF\n");
    write_script(dir, name, &script)
}

fn cfg(program: &Path) -> ClaudeCfg {
    ClaudeCfg { program: program.to_string_lossy().into_owned(), ..ClaudeCfg::default() }
}

#[test]
fn picks_files_by_shelling_out_to_the_configured_program() {
    let envelope = r#"{"type":"result","is_error":false,"result":"{\"include\":[{\"path\":\"a.rs\",\"reason\":\"yes\",\"confidence\":\"high\"}],\"exclude\":[],\"overall\":\"high\"}","usage":{"input_tokens":300,"output_tokens":20}}"#;
    let scratch = tempfile::tempdir().expect("scratch dir");
    let program = fake_claude(scratch.path(), "ok-claude.sh", envelope);
    let candidates = [candidate("a.rs"), candidate("b.rs")];

    let (selection, usage) = pick("some message", &candidates, &cfg(&program)).expect("pick succeeds");

    assert_eq!(selection.include.len(), 1);
    assert_eq!(selection.include[0].path, "a.rs");
    // b.rs was never mentioned → surfaced as excluded-by-omission.
    assert!(selection.exclude.iter().any(|e| e.path == "b.rs"));
    assert_eq!(usage.total(), 320, "usage flows out of pick");
}

#[test]
fn a_nonzero_exit_is_an_error() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let path = write_script(scratch.path(), "boom-claude.sh", "#!/bin/sh\ncat >/dev/null\nexit 3\n");

    let candidates = [candidate("a.rs")];
    assert!(pick("m", &candidates, &cfg(&path)).is_err());
}
