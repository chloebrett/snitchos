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
// Fixture construction; efficiency irrelevant.
#[allow(clippy::format_collect)]
fn caps_the_prompt_under_a_global_budget_but_still_names_every_file() {
    // Ten files, each with a huge multi-hunk diff.
    let body: String = (0..400)
        .map(|i| format!("@@ -{i},1 +{i},2 @@\n ctx {i}\n+add {i}\n"))
        .collect();
    let diff = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{body}");
    let candidates: Vec<Candidate> = (0..10)
        .map(|i| candidate(&format!("f{i}.rs"), Status::Modified, &diff))
        .collect();

    let prompt = build_prompt("m", &candidates);

    assert!(prompt.lines().count() < 3000, "global budget bounds the prompt (got {})", prompt.lines().count());
    assert!(prompt.contains("truncated"), "elision is noted");
    assert!(prompt.contains("f0.rs"), "first file named");
    assert!(prompt.contains("f9.rs"), "last file still named even when its body is elided");
}

#[test]
fn prompt_reflects_each_candidate_status() {
    let candidates = [candidate("gone.rs", Status::Deleted, "")];
    let prompt = build_prompt("cleanup", &candidates);
    assert!(prompt.to_lowercase().contains("delet"), "status should be conveyed");
}
