//! `cargo xtask cram --rung drivel` — the push-button training run.
//!
//! Everything a run needs is derived from flags with working defaults: the
//! corpus is generated or reused from cache, the probe vocab is trained on it,
//! and the model trains from there. Nothing to prepare by hand, and nothing that
//! needs a second command to make sense of afterwards.
//!
//! **The run reports on itself.** A training loop's failure modes are quiet —
//! loss plateaus, throughput halves, the schedule never warms up — so progress
//! goes to the terminal as it happens and to a TSV beside the checkpoint for
//! later. Same principle as the rest of the system: the component that could be
//! silently wrong files a report instead.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use cram::AccelerateGemm;
use cram::run::{Progress, TrainingConfig, train};
use cram_corpus::{Layout, Manifest, parse_corpus, render_corpus, tokenize, training_text};
use kvetch_model::Rung;
use kvetch_vocab::Vocab;

const CORPUS_DIR: &str = "corpora";
const CHECKPOINT_DIR: &str = "checkpoints";

/// Programs in the probe corpus when none is asked for. ~24M tokens: the
/// Chinchilla-ish 20 tokens per parameter for a 1M model.
const DEFAULT_PROGRAMS: usize = 1_000_000;

/// Entries in the probe vocab. babble's lexicon saturates around 571, so asking
/// for more is harmless — the trainer stops when there is nothing left to merge.
const DEFAULT_VOCAB: usize = 1024;

mod eval;

struct Options {
    rung: Rung,
    programs: usize,
    vocab_size: usize,
    layout: Layout,
    config: TrainingConfig,
    /// Score instead of train. Both live behind one verb because they share
    /// every path a run depends on — rung, checkpoint naming, vocab — and a
    /// separate binary is how `parse-rate` drifted into measuring one rung on
    /// one metric with no floor to compare it to.
    eval: Option<eval::EvalOptions>,
}

fn main() -> std::io::Result<()> {
    let options = match parse(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    if let Some(eval) = &options.eval {
        return eval::run(eval);
    }

    let corpus = load_corpus(options.programs, options.layout)?;
    // Train and tokenize on the *programs*, never the corpus file. The file's
    // `\n\x1e---\n` separators are not Stitch, and a model trained on them
    // learns to emit them — which it did, and every sampled program was legal
    // Stitch interrupted by a delimiter that is not.
    let programs = parse_corpus(&corpus);
    let vocab = build_vocab(&programs, options.vocab_size);
    let tokens = encode(&vocab, &programs);

    let checkpoint_dir = PathBuf::from(CHECKPOINT_DIR);
    std::fs::create_dir_all(&checkpoint_dir)?;
    let stem = format!("{}-{}", options.rung.name(), options.config.seed);
    let curve_path = checkpoint_dir.join(format!("{stem}.tsv"));
    let checkpoint_path = checkpoint_dir.join(format!("{stem}.kvetch"));

    announce(&options, &vocab, tokens.len());

    let mut curve = std::fs::File::create(&curve_path)?;
    writeln!(curve, "{}", Progress::HEADER)?;

    let started = Instant::now();
    let model = train(
        &tokens,
        vocab.len(),
        options.config,
        &AccelerateGemm,
        |progress| {
            println!("  {}", progress.line());
            // Ignore a write failure rather than abort a long run over the log:
            // the checkpoint is the deliverable, the curve is a convenience.
            let _ = writeln!(curve, "{}", progress.row());
        },
    );

    std::fs::write(&checkpoint_path, model.encode())?;
    // Beside the weights, always: they index this token table, and loading them
    // against a different one produces a model that runs and is nonsense.
    let vocab_path = checkpoint_dir.join(format!("{stem}.vocab"));
    std::fs::write(&vocab_path, vocab.encode_vocab())?;

    println!(
        "\ndone in {:.1}s\n  checkpoint {}\n  vocab      {}\n  curve      {}",
        started.elapsed().as_secs_f64(),
        checkpoint_path.display(),
        vocab_path.display(),
        curve_path.display()
    );
    Ok(())
}

const USAGE: &str = "\
usage: cargo xtask cram [options]

  --rung <name>       drivel | quip | cliche | ballad | saga   (default drivel)
  --programs <n>      babble programs in the corpus            (default 1000000)
  --vocab <n>         vocab entries to train                   (default 1024)
  --layout <l>        flat | printed                           (default printed)
  --steps <n>         optimizer steps                          (default 2000)
  --batch <n>         sequences per step                       (default 16)
  --context <n>       tokens per sequence                      (default 128)
  --lr <f>            peak learning rate                       (default 0.003)
  --seed <n>          seeds weights and batch order            (default 0)
  --report-every <n>  steps between progress lines             (default 20)

evaluation (replaces the old `parse-rate` bin):

  --eval              score rungs instead of training
  --corpus-root <d>   where to find real .st files             (default .)
  --samples <n>       programs sampled per generative metric   (default 200)
  --checkpoint <p>    a trained rung to include in the report
  --eval-vocab <p>    the vocab that checkpoint was trained against";

fn parse(args: &[String]) -> Result<Options, String> {
    // `Printed` by default: it re-prints each program from its AST, so the
    // corpus has the newlines, indentation and tight operators real Stitch has
    // instead of babble's flat space-separated rendering. `Flat` teaches a model
    // babble's *renderer*, and it never exercises indentation — which is the
    // whole reason `kvetch_vocab::pre_tokenize` keeps whitespace runs whole.
    let mut options = Options {
        rung: Rung::Drivel,
        programs: DEFAULT_PROGRAMS,
        vocab_size: DEFAULT_VOCAB,
        layout: Layout::Printed,
        config: TrainingConfig::default(),
        eval: None,
    };
    let mut evaluating = false;
    let mut eval = eval::EvalOptions {
        corpus_root: PathBuf::from("."),
        samples: 200,
        checkpoint: None,
        vocab: None,
    };

    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        // `--flag=value` and `--flag value` both, because muscle memory differs
        // and neither spelling deserves to be a usage error.
        let (name, inline) = flag.split_once('=').map_or((flag.as_str(), None), |(n, v)| {
            (n, Some(v.to_string()))
        });
        let mut value = || {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let number = |text: &str| -> Result<usize, String> {
            text.parse()
                .map_err(|_| format!("{name}: {text:?} is not a number"))
        };

        match name {
            "--rung" => options.rung = rung_named(&value()?)?,
            "--programs" => options.programs = number(&value()?)?,
            "--vocab" => options.vocab_size = number(&value()?)?,
            "--layout" => options.layout = layout_named(&value()?)?,
            "--steps" => options.config.steps = number(&value()?)?,
            "--batch" => options.config.batch = number(&value()?)?,
            "--context" => options.config.context = number(&value()?)?,
            "--report-every" => options.config.report_every = number(&value()?)?.max(1),
            "--seed" => options.config.seed = number(&value()?)? as u64,
            "--lr" => {
                let text = value()?;
                options.config.learning_rate = text
                    .parse()
                    .map_err(|_| format!("--lr: {text:?} is not a number"))?;
            }
            "--eval" => evaluating = true,
            "--corpus-root" => eval.corpus_root = PathBuf::from(value()?),
            "--samples" => eval.samples = number(&value()?)?,
            "--checkpoint" => eval.checkpoint = Some(PathBuf::from(value()?)),
            "--eval-vocab" => eval.vocab = Some(PathBuf::from(value()?)),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }

    if evaluating {
        // Half a checkpoint is a mistake, not a mode: weights loaded against
        // the wrong token table produce a model that runs and is nonsense.
        if eval.checkpoint.is_some() != eval.vocab.is_some() {
            return Err(String::from("--checkpoint and --eval-vocab go together"));
        }
        options.eval = Some(eval);
        return Ok(options);
    }

    options.config.rung = options.rung;
    // Warmup is a fraction of the run, not a constant: 100 steps of warmup in a
    // 200-step smoke run would be half the run spent not training.
    options.config.warmup_steps = options.config.warmup_steps.min(options.config.steps / 10);
    Ok(options)
}

fn rung_named(name: &str) -> Result<Rung, String> {
    Rung::ALL
        .into_iter()
        .find(|rung| rung.name() == name)
        .ok_or_else(|| format!("unknown rung {name:?}"))
}

fn layout_named(name: &str) -> Result<Layout, String> {
    match name {
        "flat" => Ok(Layout::Flat),
        "printed" => Ok(Layout::Printed),
        other => Err(format!("unknown layout {other:?}")),
    }
}

/// The corpus, from cache when the cache still answers this request.
fn load_corpus(programs: usize, layout: Layout) -> std::io::Result<String> {
    let dir = Path::new(CORPUS_DIR);
    let stem = format!("babble-0-{programs}-{}", layout.as_str());
    let corpus_path = dir.join(format!("{stem}.corpus"));
    let manifest_path = dir.join(format!("{stem}.manifest"));

    let cached = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|text| Manifest::parse(&text))
        .is_some_and(|manifest| !manifest.is_stale_for(0, programs, layout));

    if cached {
        println!("corpus     reuse {}", corpus_path.display());
        return std::fs::read_to_string(&corpus_path);
    }

    println!("corpus     generating {programs} programs ({})…", layout.as_str());
    let started = Instant::now();
    let generated = cram_corpus::generate(0, programs, layout);
    let text = render_corpus(&generated);

    std::fs::create_dir_all(dir)?;
    std::fs::write(&corpus_path, &text)?;
    std::fs::write(
        &manifest_path,
        Manifest {
            format_version: cram_corpus::FORMAT_VERSION,
            seed: 0,
            program_count: programs,
            layout,
            probe_digest: Manifest::probe_digest(layout),
        }
        .render(),
    )?;
    println!("           {:.1}s", started.elapsed().as_secs_f64());

    Ok(text)
}

fn build_vocab(programs: &[String], size: usize) -> Vocab {
    let started = Instant::now();
    let texts: Vec<&str> = programs.iter().map(String::as_str).collect();
    let vocab = Vocab::train(&texts, size);

    println!(
        "vocab      {} tokens in {:.1}s{}",
        vocab.len(),
        started.elapsed().as_secs_f64(),
        if vocab.len() < size {
            "  (lexicon exhausted — babble cannot fill more)"
        } else {
            ""
        }
    );
    vocab
}

/// One flat token stream over every program.
///
/// The join lives in [`cram_corpus::training_text`], not here: this file having
/// its own copy of "how programs become training text" is precisely how the
/// separator bug happened — `build_vocab` used the parsed programs while this
/// path used the raw corpus file, and nothing could test either.
fn encode(vocab: &Vocab, programs: &[String]) -> Vec<u16> {
    let started = Instant::now();
    let text = training_text(programs);
    let tokens = tokenize(vocab, &text);

    println!(
        "tokens     {} in {:.1}s ({:.2} bytes/token)",
        tokens.len(),
        started.elapsed().as_secs_f64(),
        text.len() as f64 / tokens.len() as f64
    );
    tokens
}

fn announce(options: &Options, vocab: &Vocab, tokens: usize) {
    let config = options.rung.config();
    let steps = options.config.steps;
    let seen = options.config.tokens_per_step() * steps;

    let mut summary = String::new();
    let _ = write!(
        summary,
        "model      {} — {} params (d {} × {} layers × {} heads)\n\
         schedule   {steps} steps × {} seq × {} ctx = {seen} tokens ({:.2} epochs)\n",
        options.rung.name(),
        config.param_count(vocab.len()),
        config.d_model,
        config.layers,
        config.heads,
        options.config.batch,
        options.config.context,
        seen as f64 / tokens as f64,
    );
    print!("{summary}");
    println!();
}
