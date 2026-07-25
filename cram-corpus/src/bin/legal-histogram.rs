//! Histogram the size of the oracle's legal-token set at every decision a
//! decoder would face, over babble output *and* over real Stitch.
//!
//! `cargo run --release -p cram-corpus --bin legal-histogram [programs]`
//!
//! Sizes both grammar-derived decode savings — see
//! `docs/speculative-decoding-design.md`. The two corpora are reported
//! separately and deliberately: the babble histogram is the *null model's*
//! trajectory through the grammar, real Stitch is the better proxy for a
//! trained model's, and the delta between them is a shape-statistics signal
//! rather than noise.

use std::path::{Path, PathBuf};

use cram_corpus::{Layout, generate, legal_histogram};

/// Where hand-written Stitch lives. Walked recursively for `.st` files.
const REAL_SOURCE_ROOTS: [&str; 3] = ["fs-image", "stitch/src", "plans/lang"];

fn main() {
    let programs: usize = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(2000);

    // Flat: the histogram counts *decisions*, and layout changes none of them —
    // whitespace is not a token. Using the cheaper rendering keeps the measured
    // quantity identical.
    report("babble", &generate(0, programs, Layout::Flat));
    report("real Stitch", &real_sources());
}

fn real_sources() -> Vec<String> {
    let mut sources = Vec::new();
    for root in REAL_SOURCE_ROOTS {
        collect_stitch_files(Path::new(root), &mut sources);
    }
    sources
}

fn collect_stitch_files(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        if path.is_dir() {
            collect_stitch_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "st")
            && let Ok(source) = std::fs::read_to_string(&path)
        {
            out.push(source);
        }
    }
}

fn report(label: &str, programs: &[String]) {
    if programs.is_empty() {
        println!("\n{label}: no programs found");
        return;
    }

    let histogram = legal_histogram(programs);
    let total: usize = histogram.iter().sum();
    let pct = |count: usize| 100.0 * count as f64 / total as f64;

    println!("\n=== {label} — {} programs, {total} decisions ===", programs.len());

    let forced = histogram.first().copied().unwrap_or(0) + histogram.get(1).copied().unwrap_or(0);
    println!("  n=1 (forced, zero passes): {:.1}%", pct(forced));
    for n_max in [2, 3, 5] {
        let within: usize = histogram.iter().take(n_max + 1).sum();
        println!("  n<={n_max} (draftable):          {:.1}%", pct(within));
    }

    println!("  histogram:");
    for (n, &count) in histogram.iter().enumerate() {
        if count > 0 {
            let bar = "#".repeat((pct(count) / 2.0).round() as usize);
            println!("    {n:>3}: {count:>7} {:>5.1}% {bar}", pct(count));
        }
    }
}
