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

    Ok(())
}

fn bad_input(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn print_nll_table(reports: &[Report]) {
    println!("  {:<12} {:>10} {:>10} {:>12} {:>10} {:>8}", "rung", "nll", "free-nll", "perplexity", "decisions", "forced");
    for report in reports {
        println!(
            "  {:<12} {:>10.4} {:>10.4} {:>12.2} {:>10} {:>8}",
            report.rung,
            report.masked_nll,
            report.free_nll,
            report.free_perplexity(),
            report.decisions,
            report.forced,
        );
    }
    for report in reports {
        assert!(
            report.rejected_by_oracle.is_empty(),
            "{}: the oracle rejected {} human-written tokens — that is a `stitch` bug, \
             and every number above is measured over a corpus with holes in it",
            report.rung,
            report.rejected_by_oracle.len()
        );
    }
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
        println!(
            "  NOTE  uniform-over-legal beat babble's tuned tables. Those tables were\n\
             \x20       tuned for termination, not for resembling real Stitch, so this is a\n\
             \x20       finding about the tables rather than about the harness."
        );
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
    fn name(&self) -> &str {
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
