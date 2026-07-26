//! The held-out side of the eval: real Stitch, as written by a human.
//!
//! This is the loader, not a placeholder for one. Increment 2 adds *sources*
//! (the `examples/` corpus), and the layers that sit on top of loading —
//! augmentation, the deterministic split, `MinHash` dedup, per-source token
//! counts. It does not need a second walker: reading `.st` files, keeping the
//! ones that parse, and reporting what was dropped is the same job whoever
//! asks.
//!
//! **What is missing here is the split, not the loading.** babble's floor row
//! needs no split, because babble was never trained on anything. The moment a
//! *model* is scored on masked NLL, a train/held-out split is mandatory and
//! this module does not provide one — [`load`] returns everything it was given.

use std::path::{Path, PathBuf};

/// A source file that survived loading, and one that did not.
pub struct Corpus {
    pub programs: Vec<Program>,
    /// Files that failed to parse, with why. Reported rather than skipped: a
    /// silently-dropped source is a corpus that shrinks without anyone noticing.
    pub rejected: Vec<(PathBuf, String)>,
}

pub struct Program {
    pub path: PathBuf,
    pub source: String,
}

impl Corpus {
    /// Total bytes of Stitch that made it in.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.programs.iter().map(|program| program.source.len()).sum()
    }

    /// The programs, as the scorer wants them.
    #[must_use]
    pub fn sources(&self) -> Vec<String> {
        self.programs.iter().map(|program| program.source.clone()).collect()
    }
}

/// Load every path given, keeping the ones that parse.
///
/// A file that does not parse cannot be scored — the oracle would reject its
/// tokens and every decision after the first error would be dead. It is
/// reported, not silently dropped: an unparseable file in the real corpus is a
/// finding about the corpus (or about a language change that broke it), and the
/// ladder doc explicitly wants "this change broke N% of the corpus" to be a
/// visible signal rather than a quiet shrink.
#[must_use]
pub fn load(paths: &[PathBuf]) -> Corpus {
    let mut programs = Vec::new();
    let mut rejected = Vec::new();

    for path in paths {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                rejected.push((path.clone(), error.to_string()));
                continue;
            }
        };
        match stitch::parser::parse_program(&source) {
            Ok(_) => programs.push(Program { path: path.clone(), source }),
            Err(error) => rejected.push((path.clone(), format!("{error:?}"))),
        }
    }

    Corpus { programs, rejected }
}

/// Directories that hold Stitch which is not corpus.
///
/// `target/` is the one that bit: the first real eval run scored three of this
/// crate's own test fixtures as human-written Stitch, because they live under
/// `target/tmp/`. Walking it is also where the search spends nearly all of its
/// time. Dot-directories are excluded for the same reason one rung up — `.git`
/// holds every past version of every file.
const NOT_CORPUS: &[&str] = &["target", "corpora", "checkpoints"];

/// Every `.st` file under `root`, depth-first and in sorted order.
///
/// Sorted so a corpus is a function of its directory rather than of the order
/// the filesystem happened to return — the same determinism rule the split in
/// increment 2 will need.
#[must_use]
pub fn find_stitch_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect(root, &mut found);
    found.sort();
    found
}

fn collect(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !is_excluded(&path) {
                collect(&path, found);
            }
        } else if path.extension().is_some_and(|extension| extension == "st") {
            found.push(path);
        }
    }
}

fn is_excluded(dir: &Path) -> bool {
    dir.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
        name.starts_with('.') || NOT_CORPUS.contains(&name)
    })
}
