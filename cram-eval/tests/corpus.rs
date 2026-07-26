//! Loading held-out Stitch. Small surface, but the place a corpus silently
//! shrinks if nobody is watching.

use std::path::PathBuf;

use cram_eval::corpus;

fn fixture_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("corpus-fixture");
    std::fs::create_dir_all(dir.join("nested")).expect("fixture dir");
    std::fs::write(dir.join("b.st"), "let count = 1\n").expect("write");
    std::fs::write(dir.join("a.st"), "prod Config(host: Str)\n").expect("write");
    std::fs::write(dir.join("broken.st"), "let count = = =\n").expect("write");
    std::fs::write(dir.join("nested/c.st"), "let total = 2\n").expect("write");
    std::fs::write(dir.join("notstitch.md"), "# not Stitch\n").expect("write");
    dir
}

#[test]
fn only_stitch_files_are_found_and_the_order_is_stable() {
    // A corpus must be a function of its directory, not of the order the
    // filesystem happened to return — the same determinism the increment-2
    // split will depend on.
    let dir = fixture_dir();
    let found = corpus::find_stitch_files(&dir);
    let names: Vec<String> = found
        .iter()
        .map(|path| path.strip_prefix(&dir).unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.st", "b.st", "broken.st", "nested/c.st"]);
    assert_eq!(corpus::find_stitch_files(&dir), found, "twice should give the same answer");
}

#[test]
fn a_file_that_does_not_parse_is_reported_rather_than_dropped() {
    // The whole reason `rejected` exists. An unparseable file cannot be scored
    // — the oracle would reject its tokens and every decision after the first
    // error would be dead — but dropping it silently means the corpus shrinks
    // without anyone noticing. The ladder doc wants "this change broke N% of
    // the corpus" to be a visible signal.
    let dir = fixture_dir();
    let loaded = corpus::load(&corpus::find_stitch_files(&dir));

    assert_eq!(loaded.programs.len(), 3, "the three parseable files");
    assert_eq!(loaded.rejected.len(), 1, "the broken one is reported");
    assert!(loaded.rejected[0].0.ends_with("broken.st"));
    assert!(loaded.bytes() > 0);
}

#[test]
fn a_missing_file_is_reported_not_a_panic() {
    let loaded = corpus::load(&[PathBuf::from("/nonexistent/nope.st")]);
    assert!(loaded.programs.is_empty());
    assert_eq!(loaded.rejected.len(), 1);
}
