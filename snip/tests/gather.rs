use snip::{Candidate, Status, cap_diff, fingerprint, parse_status};

fn cand(path: &str, diff: &str) -> Candidate {
    Candidate { path: path.to_string(), status: Status::Modified, diff: diff.to_string() }
}

#[test]
fn fingerprint_is_stable_for_the_same_candidates() {
    let a = [cand("a.rs", "x"), cand("b.rs", "y")];
    let b = [cand("a.rs", "x"), cand("b.rs", "y")];
    assert_eq!(fingerprint(&a), fingerprint(&b));
}

#[test]
fn fingerprint_changes_when_a_diff_changes() {
    let a = [cand("a.rs", "x")];
    let b = [cand("a.rs", "x CHANGED")];
    assert_ne!(fingerprint(&a), fingerprint(&b));
}

#[test]
fn fingerprint_ignores_candidate_ordering() {
    let a = [cand("a.rs", "x"), cand("b.rs", "y")];
    let b = [cand("b.rs", "y"), cand("a.rs", "x")];
    assert_eq!(fingerprint(&a), fingerprint(&b));
}

#[test]
// Building fixture text; efficiency of the construction is irrelevant here.
#[allow(clippy::format_collect)]
fn cap_diff_truncates_long_diffs_with_a_marker() {
    let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
    let capped = cap_diff(&body, 200);

    let kept = capped.lines().filter(|l| l.starts_with("line ")).count();
    assert!(kept <= 200, "kept {kept} lines, expected <= 200");
    assert!(capped.contains("truncated"), "must mark truncation");
    assert!(capped.contains("line 0"), "keeps the head of the diff");
}

#[test]
fn cap_diff_leaves_short_diffs_untouched() {
    let body = "one\ntwo\nthree\n";
    assert_eq!(cap_diff(body, 200), body);
}

#[test]
fn parse_status_reads_porcelain_z_records() {
    // `git status --porcelain=v1 -z`: two status chars, space, path, NUL.
    let raw = " M kernel/src/trap.rs\0?? new/file.rs\0 D gone.rs\0";
    let entries = parse_status(raw);

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].path, "kernel/src/trap.rs");
    assert_eq!(entries[0].status, Status::Modified);
    assert_eq!(entries[1].path, "new/file.rs");
    assert_eq!(entries[1].status, Status::Untracked);
    assert_eq!(entries[2].path, "gone.rs");
    assert_eq!(entries[2].status, Status::Deleted);
}

#[test]
fn parse_status_handles_renames_with_a_source_field() {
    // A rename record carries the destination then the source as a second field.
    let raw = "R  new/name.rs\0old/name.rs\0 M other.rs\0";
    let entries = parse_status(raw);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, "new/name.rs");
    assert_eq!(entries[0].status, Status::Renamed);
    assert_eq!(entries[1].path, "other.rs");
    assert_eq!(entries[1].status, Status::Modified);
}
