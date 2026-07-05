use snip::{build_patch, parse_hunks};

const TWO_HUNK_DIFF: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a\n+b\n c\n@@ -10,2 +11,3 @@ fn x\n d\n+e\n f\n";

#[test]
fn parses_header_and_labels_hunks_positionally() {
    let fd = parse_hunks(TWO_HUNK_DIFF);

    assert!(fd.header.starts_with("diff --git a/f b/f"));
    assert!(fd.header.ends_with("+++ b/f\n"), "header stops at the first hunk");

    assert_eq!(fd.hunks.len(), 2);
    assert_eq!(fd.hunks[0].id, "H1");
    assert!(fd.hunks[0].text.starts_with("@@ -1,2 +1,3 @@"));
    assert!(fd.hunks[0].text.contains("+b"));
    assert_eq!(fd.hunks[1].id, "H2");
    assert!(fd.hunks[1].text.contains("+e"));
}

#[test]
fn header_plus_all_hunks_reconstructs_the_original_diff_byte_for_byte() {
    let fd = parse_hunks(TWO_HUNK_DIFF);
    let rebuilt = format!("{}{}{}", fd.header, fd.hunks[0].text, fd.hunks[1].text);
    assert_eq!(rebuilt, TWO_HUNK_DIFF);
}

#[test]
fn a_diff_with_no_hunks_is_all_header() {
    let fd = parse_hunks("diff --git a/f b/f\nBinary files differ\n");
    assert!(fd.hunks.is_empty());
    assert_eq!(fd.header, "diff --git a/f b/f\nBinary files differ\n");
}

#[test]
fn build_patch_keeps_only_the_selected_hunks() {
    let fd = parse_hunks(TWO_HUNK_DIFF);
    let patch = build_patch(&fd, &["H2".to_string()]).expect("one valid hunk");

    assert!(patch.starts_with("diff --git a/f b/f"), "patch keeps the file header");
    assert!(patch.contains("+e"), "keeps the selected hunk");
    assert!(!patch.contains("+b"), "drops the unselected hunk");
}

#[test]
fn build_patch_returns_none_when_no_id_is_valid() {
    let fd = parse_hunks(TWO_HUNK_DIFF);
    assert!(build_patch(&fd, &["H9".to_string()]).is_none());
}
