//! Scratch: how big is the real Stitch corpus, and what does drivel do if you
//! train it on that instead of babble? Curiosity probe, not a gate.
//!
//! usage: cargo run --release -p xtask-cram --example real-corpus -- [root...]

use cram::AccelerateGemm;
use cram::run::{TrainingConfig, train};
use cram_corpus::{tokenize, training_text};
use cram_eval::corpus::{find_stitch_files, load};
use kvetch_model::Rung;
use kvetch_vocab::Vocab;
use std::path::PathBuf;

const VOCAB_SIZE: usize = 1024;
const SAMPLE_TOKENS: usize = 200;

fn main() {
    let roots: Vec<PathBuf> = {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.is_empty() {
            vec![PathBuf::from("examples/stitch")]
        } else {
            args.into_iter().map(PathBuf::from).collect()
        }
    };

    let paths: Vec<PathBuf> = roots.iter().flat_map(|root| find_stitch_files(root)).collect();
    let corpus = load(&paths);

    println!("== corpus ==");
    for program in &corpus.programs {
        println!(
            "  {:<48} {:>5} lines  {:>7} bytes",
            program.path.display(),
            program.source.lines().count(),
            program.source.len()
        );
    }
    for (path, why) in &corpus.rejected {
        println!("  REJECTED {} — {}", path.display(), why);
    }

    let sources = corpus.sources();
    let lines: usize = sources.iter().map(|source| source.lines().count()).sum();
    println!(
        "\n  {} files, {} lines, {} bytes",
        sources.len(),
        lines,
        corpus.bytes()
    );

    let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
    let vocab = Vocab::train(&refs, VOCAB_SIZE);
    let text = training_text(&sources);
    let tokens = tokenize(&vocab, &text);
    println!(
        "  vocab {} entries, {} tokens ({:.2} bytes/token)",
        vocab.len(),
        tokens.len(),
        corpus.bytes() as f64 / tokens.len() as f64
    );

    let byte_level = Vocab::byte_level();
    println!("  (byte-level baseline: {} tokens)", tokenize(&byte_level, &text).len());

    let config = TrainingConfig {
        rung: Rung::Drivel,
        steps: 3000,
        report_every: 250,
        ..TrainingConfig::default()
    };
    println!(
        "\n== training drivel: {} steps, ctx {}, batch {} ==",
        config.steps, config.context, config.batch
    );
    // No held-out split: this probe trains on everything it can find, which is
    // the whole point of it. `cargo xtask cram --real-root` is the path that
    // splits.
    let model = train(&tokens, &[], vocab.len(), config, &AccelerateGemm, |progress| {
        println!("  {}", progress.line());
    });

    println!("\n== samples ==");
    for seed in 0..5 {
        let prompt = vocab.encode("\n");
        let out = cram::run::sample(&model, &AccelerateGemm, &prompt, SAMPLE_TOKENS, 1.0, seed);
        let text = String::from_utf8_lossy(&vocab.decode(&out)).into_owned();
        let parses = stitch::parser::parse_program(&text).is_ok();
        println!("\n--- seed {seed} (parses: {parses}) ---\n{text}");
    }
}
