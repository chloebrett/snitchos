//! Loading a training corpus from real `.st` files on disk.
//!
//! Until now a run's only corpus was babble — generated, so infinite and always
//! grammatical. Real files are neither, which is what this module is for: it
//! decides *which* files a run trains on and *which* it never sees.
//!
//! Two rules it exists to enforce:
//!
//! - **The split is per source, not over the pile.** Holding out 20% of a
//!   concatenation of 38 hand-written and 536 generated programs would put
//!   almost none of the hand-written ones in the held-out set. Splitting each
//!   source separately keeps both represented on both sides.
//! - **The split is a stride, not a suffix.** Batch files are numbered in
//!   generation order, which is recipe order — ten consecutive files share a
//!   domain. Taking the last fifth would hold out twenty whole recipes and
//!   measure generalisation to *domains*; taking every fifth file measures
//!   generalisation to *programs*, which is the question.

use std::path::{Path, PathBuf};

use cram_eval::corpus::find_stitch_files;

/// A corpus, already split, with enough provenance for a run to report what it
/// was actually trained on.
pub(crate) struct Loaded {
    pub(crate) train: Vec<String>,
    pub(crate) held_out: Vec<String>,
    /// Held-out paths, so a run can copy them somewhere `--eval` can score them.
    pub(crate) held_out_paths: Vec<PathBuf>,
    /// `(label, train files, held-out files)` per source.
    pub(crate) sources: Vec<(String, usize, usize)>,
}

/// Gather every requested source, split each one, and read the files.
///
/// `real_root` is walked by [`cram_eval::corpus::find_stitch_files`], which
/// already knows which directories are not human-written corpus — reusing it
/// means the training side and the eval side cannot disagree about what counts
/// as a real `.st` file.
pub(crate) fn load(
    real_root: Option<&Path>,
    batch_dirs: &[PathBuf],
    drop_stages: &[&str],
    held_out_every: usize,
    held_out_root: Option<&Path>,
    strip: bool,
) -> Result<Loaded, String> {
    let mut loaded =
        Loaded { train: Vec::new(), held_out: Vec::new(), held_out_paths: Vec::new(), sources: Vec::new() };

    // A frozen held-out set replaces the split rather than adding to it: the
    // sources contribute training text only, and anything already held out is
    // taken back out of it below.
    let frozen = match held_out_root {
        Some(root) => {
            let paths = find_stitch_files(root);
            if paths.is_empty() {
                return Err(format!("{}: no .st files under it", root.display()));
            }
            for path in &paths {
                loaded.held_out.push(read(path)?);
            }
            loaded.held_out_paths = paths;
            loaded.sources.push((format!("held out {}", root.display()), 0, loaded.held_out.len()));
            true
        }
        None => false,
    };
    let held_out_every = if frozen { 0 } else { held_out_every };

    if let Some(root) = real_root {
        let paths = find_stitch_files(root);
        if paths.is_empty() {
            return Err(format!("{}: no .st files under it", root.display()));
        }
        add(&mut loaded, &format!("real {}", root.display()), &paths, held_out_every)?;
    }

    for dir in batch_dirs {
        let paths = batch_paths(dir, drop_stages)?;
        if paths.is_empty() {
            return Err(format!("{}: every candidate was dropped", dir.display()));
        }
        add(&mut loaded, &format!("batch {}", dir.display()), &paths, held_out_every)?;
    }

    if frozen {
        let kept = without(&loaded.train, &loaded.held_out);
        let leaked = loaded.train.len() - kept.len();
        if leaked > 0 {
            println!("corpus     {leaked} training programs were already held out — dropped");
        }
        loaded.train = kept;
    }

    // Both sides, always. A model trained on code alone and scored against text
    // that is half prose is being measured on a distribution it never saw.
    if strip {
        loaded.train = loaded.train.iter().map(|text| strip_comments(text)).collect();
        loaded.held_out = loaded.held_out.iter().map(|text| strip_comments(text)).collect();
    }

    Ok(loaded)
}

fn add(
    loaded: &mut Loaded,
    label: &str,
    paths: &[PathBuf],
    held_out_every: usize,
) -> Result<(), String> {
    let (train, held_out) = split_held_out(paths, held_out_every);
    loaded.sources.push((label.to_string(), train.len(), held_out.len()));

    for path in train {
        loaded.train.push(read(&path)?);
    }
    for path in held_out {
        loaded.held_out.push(read(&path)?);
        loaded.held_out_paths.push(path);
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// The `.st` files a batch directory contributes, sorted by candidate index.
///
/// A directory with no `manifest.json` contributes every `.st` file in it —
/// a batch that predates the manifest is still a corpus, it just cannot be
/// filtered by gate stage.
fn batch_paths(dir: &Path, drop_stages: &[&str]) -> Result<Vec<PathBuf>, String> {
    let manifest = dir.join("manifest.json");
    if !manifest.exists() {
        if !drop_stages.is_empty() {
            return Err(format!(
                "{}: --drop-stage needs a manifest.json, and this batch has none",
                dir.display()
            ));
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|error| format!("{}: {error}", dir.display()))?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "st"))
            .collect();
        paths.sort();
        return Ok(paths);
    }

    let kept = kept_indices(&read(&manifest)?, drop_stages)?;
    Ok(kept
        .into_iter()
        .map(|index| candidate_path(dir, index))
        // A crashed batch can record a verdict for a candidate whose files were
        // never written. Missing is not an error — the manifest is the record of
        // what happened, and what happened may be "the process died here".
        .filter(|path| path.exists())
        .collect())
}

/// The candidate indices a batch manifest says to keep, sorted.
fn kept_indices(manifest_json: &str, drop_stages: &[&str]) -> Result<Vec<usize>, String> {
    let value: serde_json::Value =
        serde_json::from_str(manifest_json).map_err(|error| format!("manifest: {error}"))?;
    let candidates = value
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "manifest: no `candidates` array".to_string())?;

    let mut kept = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let index = candidate
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "manifest: a candidate has no `index`".to_string())?;
        let stage = candidate
            .get("stage")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "manifest: a candidate has no `stage`".to_string())?;

        if !drop_stages.contains(&stage) {
            kept.push(usize::try_from(index).map_err(|_| "manifest: index too large".to_string())?);
        }
    }

    kept.sort_unstable();
    Ok(kept)
}

/// `program` with its `//` comments removed.
///
/// 47% of batch9's tokens are comment text, and a rung this size cannot model
/// English — so half the training budget competes with the grammar for the same
/// parameters and buys prose like `getAmnestyPolicyToBmnesty`. Stripping is a
/// flag rather than a policy because the comments are the *point* of the corpus
/// for anything above syntax; this exists to measure what they cost.
///
/// A line that was only a comment is removed entirely rather than left blank:
/// blank lines separate top-level items in Stitch, so leaving one behind would
/// teach a structure the program never had.
fn strip_comments(program: &str) -> String {
    let mut out = String::with_capacity(program.len());
    // `split('\n')` rather than `lines()`, so a trailing newline survives the
    // round trip instead of being silently eaten.
    let mut lines = program.split('\n').peekable();

    while let Some(line) = lines.next() {
        let code = code_before_comment(line).trim_end();
        // Only drop a line that *became* empty. One that was already blank is
        // layout, and layout is what the model is here to learn.
        if !code.is_empty() || line.trim().is_empty() {
            out.push_str(code);
            if lines.peek().is_some() {
                out.push('\n');
            }
        }
    }

    out
}

/// The part of `line` before its `//`, if any — skipping over string literals,
/// because Stitch programs print URLs and `"//---//"` separators.
fn code_before_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut escaped = false;

    for (at, &byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' && in_string {
            escaped = true;
        } else if byte == b'"' {
            in_string = !in_string;
        } else if !in_string && byte == b'/' && bytes.get(at + 1) == Some(&b'/') {
            return &line[..at];
        }
    }

    line
}

/// `programs` minus any program the held-out set already contains.
///
/// By content, not by path. Reusing one frozen held-out set across several runs
/// is what makes their curves comparable, but the same files are still sitting
/// in the source directories — so the exclusion has to survive a copy under a
/// different name.
///
/// The key is the program with its comments stripped, so it also survives a
/// copy written out by `--strip-comments`. Comparing raw bytes would let all
/// 116 held-out programs back into training the moment the held-out set was
/// transformed — a leak that nothing downstream would report.
fn without(programs: &[String], held_out: &[String]) -> Vec<String> {
    let excluded: std::collections::HashSet<String> =
        held_out.iter().map(|text| strip_comments(text)).collect();
    programs.iter().filter(|program| !excluded.contains(&strip_comments(program))).cloned().collect()
}

/// Where `cram_gen` saved a candidate: zero-padded to three digits.
fn candidate_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("{index:03}.st"))
}

/// Split `items` into `(train, held_out)`, taking every `every`-th item for the
/// held-out side. `every == 0` holds nothing out.
fn split_held_out<T: Clone>(items: &[T], every: usize) -> (Vec<T>, Vec<T>) {
    if every == 0 {
        return (items.to_vec(), Vec::new());
    }

    let side = |held_out: bool| {
        items
            .iter()
            .enumerate()
            .filter(|(at, _)| (at % every == 0) == held_out)
            .map(|(_, item)| item.clone())
            .collect()
    };
    (side(false), side(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A batch manifest with one candidate per stage, in the shape
    /// `cram_gen::CandidateRecord` serializes to.
    fn manifest(stages: &[(usize, &str)]) -> String {
        let rows: Vec<String> = stages
            .iter()
            .map(|(index, stage)| {
                format!(r#"{{"index":{index},"stage":"{stage}","domain":"d","tokens":1}}"#)
            })
            .collect();
        format!(r#"{{"attempted":9,"candidates":[{}]}}"#, rows.join(","))
    }

    #[test]
    fn every_candidate_is_kept_when_no_stage_is_dropped() {
        let text = manifest(&[(1, "ok"), (2, "parse"), (3, "tests")]);

        assert_eq!(kept_indices(&text, &[]).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn a_dropped_stage_loses_exactly_its_own_candidates() {
        let text = manifest(&[(1, "ok"), (2, "parse"), (3, "tests"), (4, "parse")]);

        assert_eq!(kept_indices(&text, &["parse"]).unwrap(), vec![1, 3]);
    }

    #[test]
    fn several_stages_can_be_dropped_at_once() {
        let text = manifest(&[(1, "ok"), (2, "parse"), (3, "tests"), (4, "type")]);

        assert_eq!(kept_indices(&text, &["parse", "type"]).unwrap(), vec![1, 3]);
    }

    /// Indices come back sorted whatever order the manifest happened to record
    /// them in, because the corpus is a function of its directory rather than of
    /// the order a crashed run left behind — the same determinism rule
    /// `cram_eval::corpus::find_stitch_files` follows.
    #[test]
    fn kept_indices_are_sorted_regardless_of_manifest_order() {
        let text = manifest(&[(7, "ok"), (2, "ok"), (5, "ok")]);

        assert_eq!(kept_indices(&text, &[]).unwrap(), vec![2, 5, 7]);
    }

    /// A manifest that is not one is an error, never an empty corpus: silently
    /// training on nothing is the failure that looks like success.
    #[test]
    fn a_manifest_that_does_not_parse_is_an_error() {
        assert!(kept_indices("not json", &[]).is_err());
        assert!(kept_indices(r#"{"candidates":"nope"}"#, &[]).is_err());
    }

    #[test]
    fn one_in_five_is_held_out() {
        let items: Vec<usize> = (0..10).collect();

        let (train, held_out) = split_held_out(&items, 5);

        assert_eq!(held_out, vec![0, 5]);
        assert_eq!(train, vec![1, 2, 3, 4, 6, 7, 8, 9]);
    }

    /// The stride is what makes the split stratified: batch files are numbered in
    /// generation order, which is recipe order, so taking every fifth one spreads
    /// the held-out set across every domain instead of parking it in the last
    /// twenty recipes.
    #[test]
    fn the_split_spreads_across_the_whole_input() {
        let items: Vec<usize> = (0..100).collect();

        let (_, held_out) = split_held_out(&items, 5);

        assert_eq!(held_out.len(), 20);
        assert!(held_out.contains(&0) && held_out.contains(&95));
    }

    #[test]
    fn a_stride_of_zero_holds_nothing_out() {
        let items: Vec<usize> = (0..10).collect();

        let (train, held_out) = split_held_out(&items, 0);

        assert_eq!(train, items);
        assert!(held_out.is_empty());
    }

    #[test]
    fn the_split_is_the_same_every_time() {
        let items: Vec<usize> = (0..37).collect();

        assert_eq!(split_held_out(&items, 5), split_held_out(&items, 5));
    }

    /// Batch candidates are saved zero-padded to three digits, so the index a
    /// manifest records has to be spelled the same way to find its file.
    #[test]
    fn a_whole_line_comment_goes_and_takes_its_line_with_it() {
        let program = "ext a() -> Int = 1\n// why\next b() -> Int = 2\n";

        assert_eq!(strip_comments(program), "ext a() -> Int = 1\next b() -> Int = 2\n");
    }

    /// Blank lines separate top-level items, so they are structure, not
    /// whitespace. A comment block must not leave a hole where the blank lines
    /// around it were, and must not fill one either.
    #[test]
    fn blank_lines_survive_the_comments_between_them() {
        let program = "ext a() -> Int = 1\n\n// why\n// more why\n\next b() -> Int = 2\n";

        assert_eq!(strip_comments(program), "ext a() -> Int = 1\n\n\next b() -> Int = 2\n");
    }

    #[test]
    fn an_indented_comment_goes_too() {
        let program = "ext a() -> Int = {\n    // why\n    1\n}\n";

        assert_eq!(strip_comments(program), "ext a() -> Int = {\n    1\n}\n");
    }

    #[test]
    fn a_trailing_comment_goes_but_its_code_stays() {
        let program = "ext a() -> Int = 1  // why\n";

        assert_eq!(strip_comments(program), "ext a() -> Int = 1\n");
    }

    /// The one case a line scan gets wrong if it is naive, and it is not
    /// hypothetical: Stitch programs print URLs and separator strings.
    #[test]
    fn a_slash_slash_inside_a_string_is_not_a_comment() {
        let program = "let url = \"http://example.com\"\n";

        assert_eq!(strip_comments(program), program);
        assert_eq!(strip_comments("let rule = \"//---//\"  // why\n"), "let rule = \"//---//\"\n");
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let program = "let q = \"a \\\" // b\"\n";

        assert_eq!(strip_comments(program), program);
    }

    #[test]
    fn a_program_with_no_comments_is_untouched() {
        let program = "ext a() -> Int = 1\n\next b() -> Int = 2\n";

        assert_eq!(strip_comments(program), program);
    }

    /// Reusing a held-out set across runs is the only way two runs are
    /// comparable, and it is also the only way to leak it: the same files are
    /// still sitting in the source directories. Excluding by *content* rather
    /// than by path is what makes the guarantee hold — a held-out program copied
    /// under a flattened name is still the same text.
    #[test]
    fn training_drops_anything_the_held_out_set_already_contains() {
        let programs =
            vec![String::from("a"), String::from("b"), String::from("c"), String::from("b")];
        let held_out = vec![String::from("b")];

        assert_eq!(without(&programs, &held_out), vec![String::from("a"), String::from("c")]);
    }

    /// A held-out set written out with `--strip-comments` no longer matches the
    /// source files byte for byte, and matching on bytes would silently let all
    /// 116 of them back into training. The comments are not what is being held
    /// out — the program is.
    #[test]
    fn a_held_out_program_still_matches_after_its_comments_were_stripped() {
        let programs = vec![String::from("// why\next a() -> Int = 1\n"), String::from("b")];
        let held_out = vec![String::from("ext a() -> Int = 1\n")];

        assert_eq!(without(&programs, &held_out), vec![String::from("b")]);
    }

    #[test]
    fn training_is_untouched_when_nothing_is_held_out() {
        let programs = vec![String::from("a"), String::from("b")];

        assert_eq!(without(&programs, &[]), programs);
    }

    #[test]
    fn a_candidate_index_names_its_zero_padded_file() {
        let dir = std::path::Path::new("corpora/batch9");

        assert_eq!(candidate_path(dir, 1), dir.join("001.st"));
        assert_eq!(candidate_path(dir, 973), dir.join("973.st"));
    }
}
