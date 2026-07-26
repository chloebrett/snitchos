//! `cargo xtask cram --eval` — the ladder's scoreboard.
//!
//! Replaces the standalone `parse-rate` bin, which measured one rung on one
//! metric and could not say what the floor was. This prints the floor rows and
//! any trained rung together, through one scoring path, with samples beside
//! every generated number.

use std::path::PathBuf;
use std::time::Instant;

use cram::AccelerateGemm;
use cram_eval::generate::{Generator, parse_rate};
use cram_eval::{Babble, Predictor, Report, Uniform, corpus, score};
use kvetch_model::Model;
use kvetch_vocab::Vocab;

/// Tokens per model sample. babble's printed programs average ~27 tokens, so
/// this is several programs' worth — long enough that a model which only learned
/// how to *start* a program cannot score well.
const SAMPLE_TOKENS: usize = 96;

pub struct EvalOptions {
    pub corpus_root: PathBuf,
    pub samples: usize,
    pub checkpoint: Option<PathBuf>,
    pub vocab: Option<PathBuf>,
}

pub fn run(options: &EvalOptions) -> std::io::Result<()> {
    let paths = corpus::find_stitch_files(&options.corpus_root);
    let held_out = corpus::load(&paths);

    println!(
        "held-out   {} programs, {} bytes, from {}",
        held_out.programs.len(),
        held_out.bytes(),
        options.corpus_root.display()
    );
    for (path, why) in &held_out.rejected {
        println!("           REJECTED {} — {why}", path.display());
    }
    if held_out.programs.is_empty() {
        println!("\nno Stitch to score against.");
        return Ok(());
    }
    for program in &held_out.programs {
        println!("           {:>7} bytes  {}", program.source.len(), program.path.display());
    }
    println!();

    // The gate metric. Both rows come from the same `score`, which is the whole
    // reason the `Predictor` trait exists.
    let started = Instant::now();
    let sources = held_out.sources();
    let rungs: Vec<Box<dyn Predictor>> = vec![Box::new(Babble::default()), Box::new(Uniform)];
    let reports: Vec<Report> = rungs.iter().map(|rung| score(rung.as_ref(), &sources)).collect();
    println!("held-out masked NLL  ({:.1}s)\n", started.elapsed().as_secs_f64());
    print_nll_table(&reports);
    let disagreed = print_oracle_disagreements(&reports, &sources);

    print_floor_verdict(&reports);

    // Generative metrics. Not the gate, and labelled so they cannot be quoted
    // as one.
    println!("\nunconstrained parse rate  ({} samples)\n", options.samples);
    let babble_rate = parse_rate(&cram_eval::generate::Babble, options.samples);
    print_parse_rate(&babble_rate, "100% by construction — the mask guarantees it");

    if let (Some(checkpoint), Some(vocab_path)) = (&options.checkpoint, &options.vocab) {
        let model = Model::decode(&std::fs::read(checkpoint)?)
            .ok_or_else(|| bad_input(format!("{}: not a checkpoint this build reads", checkpoint.display())))?;
        let vocab = Vocab::decode_vocab(&std::fs::read(vocab_path)?)
            .ok_or_else(|| bad_input(format!("{}: not a vocab this build reads", vocab_path.display())))?;

        println!(
            "\nmodel      {} params, {} vocab",
            model.config().param_count(model.vocab()),
            vocab.len()
        );
        let rate = parse_rate(&Checkpoint { model, vocab }, options.samples);
        print_parse_rate(&rate, "meaningful against other trained rungs, not against babble");

        println!(
            "\nNOTE  this checkpoint has no masked-NLL row. Scoring a model on the\n\
             \x20     gate metric needs the class -> vocab-token mask (increment 6), and a\n\
             \x20     train/held-out split (increment 2) — the loader above is every real\n\
             \x20     `.st` file, which a trained model has very likely seen."
        );
    }

    if disagreed {
        // Nonzero exit, but only after everything else has printed: the numbers
        // are still worth reading, they are just measured over a corpus with a
        // hole in it.
        std::process::exit(1);
    }
    Ok(())
}

fn bad_input(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

/// How far into a program babble is still operating in the regime it generates
/// in. Its walk winds down at 24 tokens and its programs average ~27, so a
/// decision past this point asks it something it never has to answer.
const REGIME: u32 = 50;

fn print_nll_table(reports: &[Report]) {
    println!(
        "  {:<12} {:>10} {:>10} {:>12} {:>12} {:>10} {:>8}",
        "rung", "nll", "free-nll", "perplexity", "free<50tok", "decisions", "forced"
    );
    for report in reports {
        println!(
            "  {:<12} {:>10.4} {:>10.4} {:>12.2} {:>12.4} {:>10} {:>8}",
            report.rung,
            report.masked_nll,
            report.free_nll,
            report.free_perplexity(),
            report.free_nll_before(REGIME),
            report.decisions,
            report.forced,
        );
    }
}

/// The oracle disagreeing with the parser is a `stitch` bug, and it silently
/// removes decisions from every mean above — so it is reported loudly, with
/// enough context to find it, rather than asserted away before the rest of the
/// report prints.
fn print_oracle_disagreements(reports: &[Report], sources: &[String]) -> bool {
    let Some(report) = reports.iter().find(|report| !report.rejected_by_oracle.is_empty()) else {
        return false;
    };
    println!(
        "\n  WARNING  the oracle rejected {} token(s) a human actually wrote.\n\
         \x20          Those decisions are excluded from every mean above, and the\n\
         \x20          disagreement is between `stitch`'s oracle and its parser.",
        report.rejected_by_oracle.len()
    );
    for decision in &report.rejected_by_oracle {
        let context = sources
            .get(decision.program)
            .and_then(|source| locate(source, decision.position))
            .unwrap_or_default();
        println!(
            "           program {} wrote {:?} at byte {} (oracle admitted {} classes)\n\
             \x20            {}",
            decision.program, decision.actual, decision.position, decision.legal_count, context
        );
    }
    true
}

/// A one-line window around `position`, for a diagnostic that has to be
/// actionable without opening the file.
fn locate(source: &str, position: usize) -> Option<String> {
    if position >= source.len() {
        return None;
    }
    let line_start = source[..position].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[position..].find('\n').map_or(source.len(), |index| position + index);
    let line = source.get(line_start..line_end)?;
    let number = source[..position].matches('\n').count() + 1;
    Some(format!("line {number}: {}", line.trim()))
}

/// Which row is actually the floor.
///
/// The plan names babble as the floor, on the assumption its tuned tables beat
/// uniform-over-legal. That assumption is checked here rather than trusted:
/// babble's tables were tuned to make walks *terminate*, not to resemble Stitch
/// as a human writes it, so uniform winning is a real possibility. If it does,
/// the floor is uniform — otherwise a trained rung could clear "the floor" while
/// losing to a model with no tables at all.
fn print_floor_verdict(reports: &[Report]) {
    let Some(best) = reports
        .iter()
        .min_by(|a, b| a.free_nll.total_cmp(&b.free_nll))
    else {
        return;
    };
    println!(
        "\n  floor: {} at free-nll {:.4} — a trained rung must beat this, not merely babble.",
        best.rung, best.free_nll
    );
    if best.rung != "babble" {
        let babble = reports.iter().find(|report| report.rung == "babble");
        println!(
            "  NOTE  uniform-over-legal beat babble's tuned tables. Those tables were\n\
             \x20       tuned to make walks *terminate*, not to resemble real Stitch, and\n\
             \x20       their finishing pressure saturates on a long file — a regime babble\n\
             \x20       never generates in, since its own walks stop by ~27 tokens."
        );
        if let Some(babble) = babble {
            println!(
                "  \x20       Evidence: babble scores {:.4} over the first {REGIME} tokens of a\n\
                 \x20       program and {:.4} overall. Uniform is {:.4} / {:.4} — flat, as a\n\
                 \x20       rung with no notion of position must be.",
                babble.free_nll_before(REGIME),
                babble.free_nll,
                best.free_nll_before(REGIME),
                best.free_nll,
            );
        }
    }
}

fn print_parse_rate(rate: &cram_eval::generate::ParseRate, caveat: &str) {
    println!(
        "  {:<12} as sampled {}/{} = {:.1}%   complete items {}/{} = {:.1}%",
        rate.rung,
        rate.as_sampled,
        rate.samples,
        rate.as_sampled_pct(),
        rate.complete_items,
        rate.samples,
        rate.complete_items_pct(),
    );
    println!("               ({caveat})");
    for example in &rate.examples {
        println!(
            "\n  --- {} seed {} ({}) ---\n{}",
            rate.rung,
            example.seed,
            if example.parses { "parses" } else { "does not parse" },
            example.text
        );
    }
}

/// A trained rung, generating.
struct Checkpoint {
    model: Model,
    vocab: Vocab,
}

impl Generator for Checkpoint {
    fn name(&self) -> &'static str {
        "checkpoint"
    }

    fn stops_at_budget(&self) -> bool {
        true
    }

    fn sample(&self, seed: u64) -> String {
        // Prompted with a newline rather than nothing: the corpus separates
        // programs with newlines, so a newline is what "start of a program"
        // looks like to this model. An empty prompt would ask it to continue a
        // context it never saw in training.
        let prompt = self.vocab.encode("\n");
        let tokens =
            cram::run::sample(&self.model, &AccelerateGemm, &prompt, SAMPLE_TOKENS, 1.0, seed);
        String::from_utf8_lossy(&self.vocab.decode(&tokens)).into_owned()
    }
}
