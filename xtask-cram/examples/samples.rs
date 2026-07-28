//! Scratch: read a checkpoint and print what it writes.
//!
//! `cargo xtask cram --eval` reports a parse *rate* and shows three samples,
//! which is the right amount for a scorecard and the wrong amount for reading.
//! This prints as many as you ask for, at whatever length, with the parse
//! verdict beside each — the qualitative half of the same measurement.
//!
//! usage: cargo run --release -p xtask-cram --example samples -- \
//!            <checkpoint> <vocab> [count] [tokens] [temperature]

use cram::AccelerateGemm;
use kvetch_model::Model;
use kvetch_vocab::Vocab;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(checkpoint), Some(vocab)) = (args.first(), args.get(1)) else {
        eprintln!("usage: samples <checkpoint> <vocab> [count] [tokens] [temperature]");
        std::process::exit(2);
    };
    let count: usize = args.get(2).and_then(|arg| arg.parse().ok()).unwrap_or(10);
    let max_tokens: usize = args.get(3).and_then(|arg| arg.parse().ok()).unwrap_or(200);
    let temperature: f32 = args.get(4).and_then(|arg| arg.parse().ok()).unwrap_or(1.0);

    let weights = std::fs::read(checkpoint).expect("read checkpoint");
    let model = Model::decode(&weights).expect("not a checkpoint this build reads");
    let table = std::fs::read(vocab).expect("read vocab");
    let vocab = Vocab::decode_vocab(&table).expect("not a vocab this build reads");

    println!("{checkpoint}: {} vocab, {max_tokens} tokens, temperature {temperature}\n", vocab.len());

    // Prompted with a newline, the same way `eval::Checkpoint` does: the
    // training text separates programs with blank lines, so a newline is what
    // "start of a program" looks like to this model.
    let prompt = vocab.encode("\n");
    let mut parsed = 0;
    for seed in 0..count as u64 {
        let drawn =
            cram::run::sample(&model, &AccelerateGemm, &prompt, max_tokens, temperature, seed);
        let text = String::from_utf8_lossy(&vocab.decode(&drawn)).into_owned();
        let parses = stitch::parser::parse_program(&text).is_ok();
        parsed += usize::from(parses);
        println!(
            "─── seed {seed} ({}) {}\n{}",
            if parses { "parses" } else { "does not parse" },
            "─".repeat(40),
            text.trim_start_matches('\n')
        );
    }

    println!("\n{parsed}/{count} parsed");
}
