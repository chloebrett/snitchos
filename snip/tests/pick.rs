use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use snip::{Candidate, ClaudeCfg, Status, pick};

fn candidate(path: &str) -> Candidate {
    Candidate { path: path.to_string(), status: Status::Modified, diff: "diff".to_string() }
}

/// Write an executable shell script that ignores stdin and prints `stdout_body`.
fn fake_claude(name: &str, stdout_body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("snip-test");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let script = format!("#!/bin/sh\ncat >/dev/null\ncat <<'SNIP_EOF'\n{stdout_body}\nSNIP_EOF\n");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn cfg(program: &Path) -> ClaudeCfg {
    ClaudeCfg { program: program.to_string_lossy().into_owned(), ..ClaudeCfg::default() }
}

#[test]
fn picks_files_by_shelling_out_to_the_configured_program() {
    let envelope = r#"{"type":"result","is_error":false,"result":"{\"include\":[{\"path\":\"a.rs\",\"reason\":\"yes\",\"confidence\":\"high\"}],\"exclude\":[],\"overall\":\"high\"}"}"#;
    let program = fake_claude("ok-claude.sh", envelope);
    let candidates = [candidate("a.rs"), candidate("b.rs")];

    let selection = pick("some message", &candidates, &cfg(&program)).expect("pick succeeds");

    assert_eq!(selection.include.len(), 1);
    assert_eq!(selection.include[0].path, "a.rs");
    // b.rs was never mentioned → surfaced as excluded-by-omission.
    assert!(selection.exclude.iter().any(|e| e.path == "b.rs"));
}

#[test]
fn a_nonzero_exit_is_an_error() {
    let dir = std::env::temp_dir().join("snip-test");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("boom-claude.sh");
    fs::write(&path, "#!/bin/sh\ncat >/dev/null\nexit 3\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

    let candidates = [candidate("a.rs")];
    assert!(pick("m", &candidates, &cfg(&path)).is_err());
}
