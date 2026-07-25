//! Train a BPE vocab over a cached corpus and report whether it is any good.
//!
//! `cargo run --release -p cram-corpus --bin train-vocab -- <corpus> [size]`
//!
//! The report is the point. A vocab is easy to train and easy to train
//! *badly*, and the two failure modes are both legible here rather than in any
//! summary statistic:
//!
//! - **Overfit to the corpus** — the longest learned tokens are whole phrases
//!   rather than lexemes. Pre-tokenization bounds this, but a vocab still
//!   oversized for its corpus will spend its tail on rare full words.
//! - **Undertrained** — compression barely improves on byte-level, meaning the
//!   vocab is too small for the language's lexicon.
//!
//! Printing the extremes costs nothing and is the check that catches both.

use std::time::Instant;

use cram_corpus::parse_corpus;
use kvetch_vocab::{BYTE_TOKENS, TokenId, Vocab};

/// How many of the longest learned tokens to show. Enough to see the tail's
/// character; few enough to read.
const SHOWN: usize = 20;

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: train-vocab <corpus> [size]");
        std::process::exit(2);
    };
    let size: usize = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(4096);

    let corpus = parse_corpus(&std::fs::read_to_string(&path)?);
    let texts: Vec<&str> = corpus.iter().map(String::as_str).collect();
    let bytes: usize = texts.iter().map(|text| text.len()).sum();

    let started = Instant::now();
    let vocab = Vocab::train(&texts, size);
    let elapsed = started.elapsed();

    let tokens: usize = texts.iter().map(|text| vocab.encode(text).len()).sum();

    println!("{path}: {} programs, {bytes} bytes", corpus.len());
    println!(
        "trained {} tokens in {:.1}s",
        vocab.len(),
        elapsed.as_secs_f64()
    );
    println!(
        "compression: {tokens} tokens, {:.2} bytes/token",
        bytes as f64 / tokens as f64
    );

    let mut learned: Vec<String> = (BYTE_TOKENS..vocab.len())
        .map(|id| String::from_utf8_lossy(&vocab.decode(&[id as TokenId])).into_owned())
        .collect();
    learned.sort_by_key(|token| core::cmp::Reverse(token.len()));

    println!("\nlongest {SHOWN} learned tokens (phrases here mean an oversized vocab):");
    for token in learned.iter().take(SHOWN) {
        println!("  {token:?}");
    }

    Ok(())
}
