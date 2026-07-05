use snip::{Candidate, Confidence, Status, parse_reply};

fn candidate(path: &str) -> Candidate {
    Candidate { path: path.to_string(), status: Status::Modified, diff: String::new() }
}

const TWO_HUNK_DIFF: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a\n+b\n c\n@@ -10,2 +11,3 @@\n d\n+e\n f\n";

fn candidate_with_diff(path: &str, diff: &str) -> Candidate {
    Candidate { path: path.to_string(), status: Status::Modified, diff: diff.to_string() }
}

#[test]
fn parses_a_clean_selection_with_reasons_and_confidence() {
    let candidates = [candidate("kernel/src/trap.rs"), candidate("stitch/src/interp.rs")];
    let raw = r#"{
        "include": [
            {"path": "kernel/src/trap.rs", "reason": "matches the trap change", "confidence": "high"}
        ],
        "exclude": [
            {"path": "stitch/src/interp.rs", "reason": "unrelated Stitch work", "confidence": "high"}
        ],
        "overall": "medium",
        "note": "trap.rs is clearly in scope"
    }"#;

    let selection = parse_reply(raw, &candidates).expect("valid reply parses");

    assert_eq!(selection.include.len(), 1);
    assert_eq!(selection.include[0].path, "kernel/src/trap.rs");
    assert_eq!(selection.include[0].reason, "matches the trap change");
    assert!(matches!(selection.include[0].confidence, Confidence::High));

    assert_eq!(selection.exclude.len(), 1);
    assert_eq!(selection.exclude[0].path, "stitch/src/interp.rs");
    assert!(matches!(selection.exclude[0].confidence, Confidence::High));

    assert!(matches!(selection.overall, Confidence::Medium));
    assert_eq!(selection.note.as_deref(), Some("trap.rs is clearly in scope"));
}

#[test]
fn drops_include_paths_that_are_not_real_candidates() {
    let candidates = [candidate("kernel/src/trap.rs")];
    let raw = r#"{
        "include": [
            {"path": "kernel/src/trap.rs", "reason": "real", "confidence": "high"},
            {"path": "kernel/src/invented.rs", "reason": "hallucinated", "confidence": "high"}
        ],
        "exclude": [],
        "overall": "high"
    }"#;

    let selection = parse_reply(raw, &candidates).expect("valid reply parses");

    assert_eq!(selection.include.len(), 1);
    assert_eq!(selection.include[0].path, "kernel/src/trap.rs");
}

#[test]
fn surfaces_candidates_the_model_never_mentioned_as_excluded_by_omission() {
    let candidates = [
        candidate("kernel/src/trap.rs"),
        candidate("kernel/src/forgotten.rs"),
    ];
    let raw = r#"{
        "include": [{"path": "kernel/src/trap.rs", "reason": "in scope", "confidence": "high"}],
        "exclude": [],
        "overall": "high"
    }"#;

    let selection = parse_reply(raw, &candidates).expect("valid reply parses");

    let omitted = selection
        .exclude
        .iter()
        .find(|e| e.path == "kernel/src/forgotten.rs")
        .expect("unmentioned candidate is surfaced as excluded");
    assert!(matches!(omitted.confidence, Confidence::Low));
    assert!(!omitted.reason.trim().is_empty());
}

#[test]
fn tolerates_a_markdown_fenced_json_reply() {
    let candidates = [candidate("a.rs")];
    let raw = "Here you go:\n```json\n{\n  \"include\": [{\"path\": \"a.rs\", \"reason\": \"x\", \"confidence\": \"high\"}],\n  \"exclude\": [],\n  \"overall\": \"high\"\n}\n```\n";

    let selection = parse_reply(raw, &candidates).expect("fenced reply still parses");

    assert_eq!(selection.include.len(), 1);
    assert_eq!(selection.include[0].path, "a.rs");
}

#[test]
fn a_whole_file_include_has_no_hunk_restriction() {
    let candidates = [candidate("a.rs")];
    let raw = r#"{"include":[{"path":"a.rs","reason":"all of it","confidence":"high"}],"exclude":[],"overall":"high"}"#;
    let selection = parse_reply(raw, &candidates).unwrap();
    assert_eq!(selection.include[0].hunks, None);
}

#[test]
fn a_partial_include_carries_the_validated_hunk_ids() {
    let candidates = [candidate_with_diff("f", TWO_HUNK_DIFF)];
    // The model asks for H2 (valid) and H9 (does not exist) — H9 is dropped.
    let raw = r#"{"include":[{"path":"f","reason":"only the second change","confidence":"med","hunks":["H2","H9"]}],"exclude":[],"overall":"med"}"#;

    let selection = parse_reply(raw, &candidates).unwrap();

    assert_eq!(selection.include.len(), 1);
    assert_eq!(selection.include[0].hunks.as_deref(), Some(&["H2".to_string()][..]));
}

#[test]
fn a_partial_include_with_no_valid_hunks_is_dropped() {
    let candidates = [candidate_with_diff("f", TWO_HUNK_DIFF)];
    let raw = r#"{"include":[{"path":"f","reason":"garbage hunks","confidence":"low","hunks":["H9"]}],"exclude":[],"overall":"low"}"#;

    let selection = parse_reply(raw, &candidates).unwrap();
    assert!(selection.include.is_empty(), "a partial include with no real hunks stages nothing");
}

#[test]
fn malformed_json_is_an_error() {
    let candidates = [candidate("a.rs")];
    assert!(parse_reply("not json at all", &candidates).is_err());
}

#[test]
fn empty_include_means_nothing_matches() {
    let candidates = [candidate("a.rs")];
    let raw = r#"{"include": [], "exclude": [], "overall": "high"}"#;
    let selection = parse_reply(raw, &candidates).expect("valid reply parses");
    assert!(selection.include.is_empty());
}
