//! Measure a checkpoint's **unconstrained** parse rate.
//!
//! `cargo run --release -p xtask-cram --bin parse-rate -- <checkpoint> <vocab> [samples]`
//!
//! Sample programs from the model with no oracle mask, decode them, and ask the
//! real `stitch` parser how many are legal Stitch. This is the
//! grammar-learnability probe from `plans/drivel.md`: it answers "can a model
//! this small learn the grammar when data is not the constraint".
//!
//! **It is not a comparison against babble.** babble is 100% by construction —
//! the mask guarantees it — so this can only tie or lose. The number is
//! meaningful against the *other rungs*, measured the same way, and as evidence
//! that the pipe learned anything structural at all.

use std::time::Instant;

use cram::AccelerateGemm;
use cram::run::sample;
use kvetch_model::Model;
use kvetch_vocab::Vocab;

/// Tokens per sample. babble's printed programs average ~27 tokens, so this is
/// several programs' worth — long enough that a model which only learned how to
/// *start* a program cannot score well.
const SAMPLE_TOKENS: usize = 96;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(checkpoint), Some(vocab_path)) = (args.first(), args.get(1)) else {
        eprintln!("usage: parse-rate <checkpoint> <vocab> [samples]");
        std::process::exit(2);
    };
    let samples: usize = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(200);

    let Some(model) = Model::decode(&std::fs::read(checkpoint)?) else {
        eprintln!("{checkpoint}: not a checkpoint this build can read");
        std::process::exit(1);
    };
    let Some(vocab) = Vocab::decode_vocab(&std::fs::read(vocab_path)?) else {
        eprintln!("{vocab_path}: not a vocab this build can read");
        std::process::exit(1);
    };

    println!(
        "{} params, {} vocab, {samples} samples x {SAMPLE_TOKENS} tokens\n",
        model.config().param_count(model.vocab()),
        vocab.len()
    );

    let start_of_program = vocab.encode("\n");
    let started = Instant::now();
    let mut parsed = 0;
    let mut parsed_whole = 0;
    let mut shown = 0;

    for index in 0..samples {
        // Seeded per sample so any individual one can be reproduced alone.
        //
        // Prompted with a newline rather than nothing: the corpus separates
        // programs with newlines, so a newline is what "start of a program"
        // looks like to this model. An empty prompt would ask it to continue a
        // context it never saw in training.
        let tokens = sample(
            &model,
            &AccelerateGemm,
            &start_of_program,
            SAMPLE_TOKENS,
            1.0,
            index as u64,
        );
        let text = String::from_utf8_lossy(&vocab.decode(&tokens)).into_owned();

        // Two cuts, because the difference between them is exactly the cost of
        // the token budget rather than a property of the model.
        //
        // `raw` keeps everything up to the last newline, so a sample stopped
        // mid-construct is counted as a failure. `whole` cuts back to the last
        // blank line — the boundary the training text actually uses between
        // top-level items — so only complete items are judged.
        let raw = text.rsplit_once('\n').map_or(text.as_str(), |(head, _)| head);
        let whole = text
            .rsplit_once("\n\n")
            .map_or(raw, |(head, _)| head)
            .trim_end();

        let raw_ok = stitch::parser::parse_program(raw).is_ok();
        let whole_ok = !whole.is_empty() && stitch::parser::parse_program(whole).is_ok();
        parsed += usize::from(raw_ok);
        parsed_whole += usize::from(whole_ok);
        let ok = raw_ok;

        if shown < 3 {
            shown += 1;
            println!(
                "--- sample {index} ({}) ---\n{raw}\n",
                if ok { "parses" } else { "does not parse" }
            );
        }
    }

    let percent = |count: usize| 100.0 * count as f64 / samples as f64;
    println!(
        "unconstrained parse rate\n  \
         as sampled:      {parsed}/{samples} = {:.1}%\n  \
         complete items:  {parsed_whole}/{samples} = {:.1}%\n\
         ({:.1}s)",
        percent(parsed),
        percent(parsed_whole),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
