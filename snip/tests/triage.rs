use snip::{
    Candidate, Confidence, Excluded, Included, Selection, Status, Triage, build_triage_prompt,
    merge_triage, parse_triage,
};

fn cand(path: &str, diff: &str) -> Candidate {
    Candidate { path: path.to_string(), status: Status::Modified, diff: diff.to_string() }
}

#[test]
fn triage_prompt_lists_files_and_change_sizes_but_no_diff_bodies() {
    let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,1 +1,2 @@\n keep\n+SECRETBODY\n";
    let candidates = [cand("kernel/x.rs", diff)];

    let prompt = build_triage_prompt("a message", &candidates);

    assert!(prompt.contains("a message"));
    assert!(prompt.contains("kernel/x.rs"), "file is named");
    assert!(!prompt.contains("SECRETBODY"), "diff bodies are NOT sent in pass 1");
    assert!(prompt.contains("+1"), "change size (added lines) is shown");
    // The three-bucket contract.
    assert!(prompt.contains("settled_include"));
    assert!(prompt.contains("settled_exclude"));
    assert!(prompt.contains("needs_diff"));
}

#[test]
fn parse_triage_buckets_known_paths() {
    let candidates = [cand("a.rs", ""), cand("b.rs", ""), cand("c.rs", "")];
    let raw = r#"{"settled_include":["a.rs"],"settled_exclude":["b.rs"],"needs_diff":["c.rs"]}"#;

    let t = parse_triage(raw, &candidates);

    assert_eq!(t.settled_include, ["a.rs"]);
    assert_eq!(t.settled_exclude, ["b.rs"]);
    assert_eq!(t.needs_diff, ["c.rs"]);
}

#[test]
fn parse_triage_drops_unknown_paths_and_escalates_the_unmentioned() {
    let candidates = [cand("a.rs", ""), cand("forgotten.rs", "")];
    // Model names a hallucinated path and forgets `forgotten.rs`.
    let raw = r#"{"settled_include":["a.rs","ghost.rs"],"settled_exclude":[],"needs_diff":[]}"#;

    let t = parse_triage(raw, &candidates);

    assert_eq!(t.settled_include, ["a.rs"], "ghost path dropped");
    assert_eq!(t.needs_diff, ["forgotten.rs"], "unmentioned files escalate to a closer look");
}

fn triage(include: &[&str], exclude: &[&str], needs: &[&str]) -> Triage {
    let v = |s: &[&str]| s.iter().map(ToString::to_string).collect();
    Triage { settled_include: v(include), settled_exclude: v(exclude), needs_diff: v(needs) }
}

#[test]
fn merge_with_no_pass2_makes_settled_files_high_confidence() {
    let t = triage(&["a.rs"], &["b.rs"], &[]);
    let sel = merge_triage(t, None);

    assert_eq!(sel.include.len(), 1);
    assert_eq!(sel.include[0].path, "a.rs");
    assert!(matches!(sel.include[0].confidence, Confidence::High));
    assert_eq!(sel.exclude.len(), 1);
    assert!(matches!(sel.exclude[0].confidence, Confidence::High));
    assert!(matches!(sel.overall, Confidence::High));
    assert!(sel.is_confident(), "an all-settled triage is auto-stageable");
}

#[test]
fn merge_folds_in_the_pass2_selection_and_takes_its_overall() {
    let t = triage(&["a.rs"], &[], &["c.rs"]);
    let pass2 = Selection {
        include: vec![Included {
            path: "c.rs".into(),
            reason: "only the parser hunk".into(),
            confidence: Confidence::Medium,
            hunks: Some(vec!["H1".into()]),
        }],
        exclude: vec![Excluded {
            path: "d.rs".into(),
            reason: "unrelated".into(),
            confidence: Confidence::High,
            omitted: false,
        }],
        overall: Confidence::Medium,
        note: Some("c.rs was mixed".into()),
    };

    let sel = merge_triage(t, Some(pass2));

    assert!(sel.include.iter().any(|i| i.path == "a.rs" && i.hunks.is_none()));
    assert!(sel.include.iter().any(|i| i.path == "c.rs" && i.hunks.as_deref() == Some(&["H1".to_string()][..])));
    assert!(sel.exclude.iter().any(|e| e.path == "d.rs"));
    assert!(matches!(sel.overall, Confidence::Medium), "overall follows pass 2");
    assert_eq!(sel.note.as_deref(), Some("c.rs was mixed"));
    assert!(!sel.is_confident(), "a medium pass-2 include blocks auto-stage");
}
