use snip::{Candidate, Status, build_prompt};

fn candidate(path: &str, status: Status, diff: &str) -> Candidate {
    Candidate { path: path.to_string(), status, diff: diff.to_string() }
}

#[test]
fn prompt_includes_message_every_candidate_and_the_json_contract() {
    let candidates = [
        candidate("kernel/src/trap.rs", Status::Modified, "@@ trap diff body @@"),
        candidate("new/file.rs", Status::Added, "+brand new"),
    ];
    let message = "kernel: guard-page fault reporting";

    let prompt = build_prompt(message, &candidates);

    assert!(prompt.contains(message), "commit message must appear");
    assert!(prompt.contains("kernel/src/trap.rs"));
    assert!(prompt.contains("@@ trap diff body @@"));
    assert!(prompt.contains("new/file.rs"));
    assert!(prompt.contains("+brand new"));

    // The output contract the model must follow.
    assert!(prompt.contains("\"include\""));
    assert!(prompt.contains("\"exclude\""));
    assert!(prompt.contains("\"confidence\""));
    assert!(prompt.contains("\"overall\""));

    // The framing that makes triage accurate.
    assert!(
        prompt.to_lowercase().contains("parallel") || prompt.to_lowercase().contains("concurrent"),
        "must frame the several-unrelated-changes problem"
    );
}

#[test]
fn prompt_labels_hunks_and_documents_partial_staging() {
    let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,1 +1,2 @@\n a\n+b\n@@ -9,1 +10,2 @@\n c\n+d\n";
    let candidates = [candidate("f", Status::Modified, diff)];

    let prompt = build_prompt("some change", &candidates);

    assert!(prompt.contains("[H1]"), "first hunk is labelled");
    assert!(prompt.contains("[H2]"), "second hunk is labelled");
    assert!(prompt.contains("\"hunks\""), "output contract mentions hunks");
    assert!(prompt.to_lowercase().contains("partial"), "explains partial staging");
}

#[test]
fn prompt_reflects_each_candidate_status() {
    let candidates = [candidate("gone.rs", Status::Deleted, "")];
    let prompt = build_prompt("cleanup", &candidates);
    assert!(prompt.to_lowercase().contains("delet"), "status should be conveyed");
}
