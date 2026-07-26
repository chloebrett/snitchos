//! Generate (or reuse) a cached babble corpus.
//!
//! `cargo run --release -p cram-corpus -- <seed> <count> [dir]`
//!
//! Thin I/O glue over the library: decide, generate, write. The decision —
//! "is this cache still good?" — lives in [`cram_corpus::Manifest`] where it is
//! host-tested; this file only moves bytes.
//!
//! **Run it in release.** Generation re-parses the prefix on every emitted
//! token, so a debug build is roughly an order of magnitude slower for no
//! reason.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cram_corpus::{Layout, Manifest, generate_reported, parse_corpus, render_corpus};

const DEFAULT_DIR: &str = "corpora";

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some((seed, count)) = parse_args(&args) else {
        eprintln!("usage: cram-corpus <seed> <count> [flat|printed] [dir]");
        std::process::exit(2);
    };
    let layout = match args.get(2).map(String::as_str) {
        None | Some("flat") => Layout::Flat,
        Some("printed") => Layout::Printed,
        Some(other) => {
            eprintln!("unknown layout {other:?}: expected `flat` or `printed`");
            std::process::exit(2);
        }
    };
    let dir = args.get(3).map_or(PathBuf::from(DEFAULT_DIR), PathBuf::from);
    let (corpus_path, manifest_path) = paths(&dir, seed, count, layout);

    if let Some(programs) = read_fresh_cache(&corpus_path, &manifest_path, seed, count, layout) {
        report("reused", &corpus_path, &programs, None);
        return Ok(());
    }

    let started = Instant::now();
    let corpus = generate_reported(seed, count, layout);
    let elapsed = started.elapsed();
    let programs = corpus.programs;

    std::fs::create_dir_all(&dir)?;
    std::fs::write(&corpus_path, render_corpus(&programs))?;
    std::fs::write(
        &manifest_path,
        Manifest {
            format_version: cram_corpus::FORMAT_VERSION,
            seed,
            program_count: count,
            layout,
            probe_digest: Manifest::probe_digest(layout),
            grammar_digest: Manifest::grammar_digest(),
        }
        .render(),
    )?;

    report("generated", &corpus_path, &programs, Some(elapsed));
    // A printer that changes a program is a correctness bug in the corpus, not
    // a statistic — so it is stated on its own line, and stated even at zero,
    // because "no line appeared" and "the check did not run" look the same.
    println!("  {} programs kept the flat rendering (printer round-trip failed)", corpus.unfaithful);
    Ok(())
}

fn parse_args(args: &[String]) -> Option<(u64, usize)> {
    Some((args.first()?.parse().ok()?, args.get(1)?.parse().ok()?))
}

/// The layout is part of the filename, not just the manifest: two layouts of the
/// same seed and count are different corpora, and sharing a path would make one
/// silently overwrite the other.
fn paths(dir: &Path, seed: u64, count: usize, layout: Layout) -> (PathBuf, PathBuf) {
    let stem = format!("babble-{seed}-{count}-{}", layout.as_str());
    (
        dir.join(format!("{stem}.corpus")),
        dir.join(format!("{stem}.manifest")),
    )
}

/// The cached corpus, if one exists and still answers this request.
///
/// Every failure path — missing file, unreadable, unparseable manifest, stale
/// generator — means "regenerate". A corpus is reproducible from its seed, so
/// discarding a doubtful cache costs time and nothing else.
fn read_fresh_cache(
    corpus_path: &Path,
    manifest_path: &Path,
    seed: u64,
    count: usize,
    layout: Layout,
) -> Option<Vec<String>> {
    let manifest = Manifest::parse(&std::fs::read_to_string(manifest_path).ok()?)?;

    if manifest.is_stale_for(seed, count, layout) {
        return None;
    }

    Some(parse_corpus(&std::fs::read_to_string(corpus_path).ok()?))
}

fn report(verb: &str, path: &Path, programs: &[String], elapsed: Option<std::time::Duration>) {
    let bytes: usize = programs.iter().map(String::len).sum();
    let words: usize = programs
        .iter()
        .map(|program| program.split_whitespace().count())
        .sum();

    println!("{verb} {} programs -> {}", programs.len(), path.display());
    println!(
        "  {bytes} bytes, {words} whitespace-separated lexemes, {:.1} lexemes/program",
        words as f64 / programs.len().max(1) as f64
    );

    if let Some(elapsed) = elapsed {
        println!(
            "  {:.2}s ({:.1} programs/s)",
            elapsed.as_secs_f64(),
            programs.len() as f64 / elapsed.as_secs_f64()
        );
    }
}
