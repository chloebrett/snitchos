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

/// One file in five is held out. A fifth is enough to measure against and small
/// enough that training does not miss it — and taking it as a *stride* over
/// sorted files, per source, is what keeps both corpora represented on both
/// sides of the split. See `corpus::split_held_out`.
const DEFAULT_HELD_OUT_EVERY: usize = 5;

mod corpus;
mod eval;
mod generate;

struct Options {
    rung: Rung,
    programs: usize,
    vocab_size: usize,
    layout: Layout,
    config: TrainingConfig,
    /// Real `.st` files to train on. `None` keeps the babble corpus, which is
    /// what every run did before there was a real corpus to prefer.
    real_root: Option<PathBuf>,
    /// `cram-gen` batch directories to train on.
    batch_dirs: Vec<PathBuf>,
    /// Gate stages whose candidates are excluded from a batch.
    drop_stages: Vec<String>,
    /// One file in `held_out_every` never reaches training.
    held_out_every: usize,
    /// Copy the held-out files here, so `--eval --corpus-root` can score them.
    write_held_out: Option<PathBuf>,
    /// Drop `//` comments from every program, on both sides of the split.
    strip_comments: bool,
    /// Reuse a held-out set written earlier instead of splitting a fresh one.
    /// Two runs over different training sets are only comparable if they are
    /// measured against the *same* held-out data.
    held_out_root: Option<PathBuf>,
    /// Train against a frozen vocab instead of a fresh probe. Two runs sharing
    /// a vocab have comparable losses; two runs that each trained their own do
    /// not, however similar the numbers look.
    vocab_file: Option<PathBuf>,
    /// Train a vocab over the training split, write it, and stop.
    save_vocab: Option<PathBuf>,
    /// Names this run's checkpoint, vocab and curve. Defaults to
    /// `<rung>-<seed>`, which collides the moment two runs of one rung differ by
    /// anything except the seed — a step sweep overwrites itself, and the run it
    /// overwrites is the one you wanted to compare against.
    name: Option<String>,
    /// Score instead of train. Both live behind one verb because they share
    /// every path a run depends on — rung, checkpoint naming, vocab — and a
    /// separate binary is how `parse-rate` drifted into measuring one rung on
    /// one metric with no floor to compare it to.
    eval: Option<eval::EvalOptions>,
    /// Generate corpus candidates instead of training. Same verb for the same
    /// reason `--eval` is: it shares the rung/corpus vocabulary, and a separate
    /// binary is how `parse-rate` drifted out of sight.
    generate: Option<generate::GenOptions>,
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
    if let Some(generate) = &options.generate {
        return generate::run(generate);
    }

    // Train and tokenize on the *programs*, never the corpus file. The file's
    // `\n\x1e---\n` separators are not Stitch, and a model trained on them
    // learns to emit them — which it did, and every sampled program was legal
    // Stitch interrupted by a delimiter that is not.
    let (programs, held_out_programs) = gather(&options)?;

    let vocab = match &options.vocab_file {
        Some(path) => load_vocab(path)?,
        // Trained over the training split alone. A vocab is learned from text,
        // so training one over the held-out files would let their lexicon reach
        // the model by another route.
        None => build_vocab(&programs, options.vocab_size),
    };
    let tokens = encode(&vocab, &programs);

    if let Some(path) = &options.save_vocab {
        report_vocab(&vocab);
        std::fs::write(path, vocab.encode_vocab())?;
        println!("vocab      wrote {}", path.display());
        return Ok(());
    }

    let held_out_tokens = if held_out_programs.is_empty() {
        Vec::new()
    } else {
        encode_held_out(&vocab, &held_out_programs)
    };

    let checkpoint_dir = PathBuf::from(CHECKPOINT_DIR);
    std::fs::create_dir_all(&checkpoint_dir)?;
    let stem = options
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-{}", options.rung.name(), options.config.seed));
    let curve_path = checkpoint_dir.join(format!("{stem}.tsv"));
    let checkpoint_path = checkpoint_dir.join(format!("{stem}.kvetch"));

    announce(&options, &vocab, tokens.len());

    let mut curve = std::fs::File::create(&curve_path)?;
    writeln!(curve, "{}", Progress::HEADER)?;

    let started = Instant::now();
    let model = train(
        &tokens,
        &held_out_tokens,
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
  --eval-every <n>    steps between held-out loss reports      (default 200)
  --eval-batch <n>    sequences in the held-out batch          (default 64)
  --name <stem>       names the checkpoint/vocab/curve      (default <rung>-<seed>)

training on real .st files instead of babble (either flag opts in; both compose):

  --real-root <d>       include every real .st file under <d>
  --batch-dir <d>       include a cram-gen batch directory (repeatable)
  --drop-stage <s>      batch gate stages to exclude, comma-separated
                        (parse | type | tests | ok) — needs the batch manifest
  --held-out-every <n>  1 file in n is held out, per source    (default 5; 0 = off)
  --write-held-out <d>  copy the held-out files to <d>, for --eval --corpus-root
  --strip-comments      drop // comments from every program, both sides of the
                        split — ~47% of batch9's tokens are comment text
  --held-out-root <d>   reuse the held-out set in <d> instead of splitting a
                        fresh one, and drop its programs from training — the
                        only way two runs over different corpora compare
  --vocab-file <p>      train against a frozen vocab instead of a fresh probe
  --save-vocab <p>      train a vocab over the training split, write it, and stop

evaluation (replaces the old `parse-rate` bin):

  --eval              score rungs instead of training
  --corpus-root <d>   where to find real .st files             (default .)
  --samples <n>       programs sampled per generative metric   (default 200)
  --checkpoint <p>    a trained rung to include in the report
  --eval-vocab <p>    the vocab that checkpoint was trained against

corpus generation (talks to a local OpenAI-compatible server, e.g. LM Studio):

  --gen               generate candidates instead of training
  --model <name>      model id as the server knows it            (required)
  --recipes <sheet>   which recipe sheet supplies the axes    (default batch10;
                      batch9 is the frozen 100 that produced corpora/batch9)
  --count <n>         candidates to generate                     (default 10)
  --out <dir>         save each candidate's raw + extracted form
  --temp <f>          sampling temperature                       (default 0.7)
  --top-p <f>         nucleus sampling                           (default 0.8)
  --max-tokens <n>    hard cap per candidate                     (default 1200)
  --endpoint <url>    server base URL              (default http://localhost:1234/v1)
  --correct <n>       rewinds per candidate: when the continuation oracle
                      says no token can rescue the program, go back to just
                      before the fatal text and resume        (default 0, off)";

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
        config: TrainingConfig { eval_every: 200, ..TrainingConfig::default() },
        real_root: None,
        batch_dirs: Vec::new(),
        drop_stages: Vec::new(),
        held_out_every: DEFAULT_HELD_OUT_EVERY,
        write_held_out: None,
        strip_comments: false,
        held_out_root: None,
        vocab_file: None,
        save_vocab: None,
        name: None,
        eval: None,
        generate: None,
    };
    let mut generating = false;
    let mut gen_options = generate::GenOptions {
        model: String::new(),
        recipes: cram_gen::recipe::DEFAULT.to_string(),
        count: 10,
        out: None,
        endpoint: None,
        corrections: 0,
        sampling: cram_gen::Sampling::default(),
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
            "--eval-every" => options.config.eval_every = number(&value()?)?,
            "--eval-batch" => options.config.eval_batch = number(&value()?)?.max(1),
            "--real-root" => options.real_root = Some(PathBuf::from(value()?)),
            "--batch-dir" => options.batch_dirs.push(PathBuf::from(value()?)),
            "--drop-stage" => options
                .drop_stages
                .extend(value()?.split(',').map(str::trim).map(str::to_string)),
            "--held-out-every" => options.held_out_every = number(&value()?)?,
            "--write-held-out" => options.write_held_out = Some(PathBuf::from(value()?)),
            "--held-out-root" => options.held_out_root = Some(PathBuf::from(value()?)),
            "--strip-comments" => options.strip_comments = true,
            "--vocab-file" => options.vocab_file = Some(PathBuf::from(value()?)),
            "--save-vocab" => options.save_vocab = Some(PathBuf::from(value()?)),
            "--name" => options.name = Some(value()?),
            "--seed" => options.config.seed = number(&value()?)? as u64,
            "--lr" => {
                let text = value()?;
                options.config.learning_rate = text
                    .parse()
                    .map_err(|_| format!("--lr: {text:?} is not a number"))?;
            }
            "--gen" => generating = true,
            "--model" => gen_options.model = value()?,
            "--recipes" => gen_options.recipes = value()?,
            "--count" => gen_options.count = number(&value()?)?,
            "--out" => gen_options.out = Some(PathBuf::from(value()?)),
            "--endpoint" => gen_options.endpoint = Some(value()?),
            "--correct" => gen_options.corrections = number(&value()?)?,
            "--max-tokens" => gen_options.sampling.max_tokens = number(&value()?)? as u32,
            "--temp" | "--top-p" => {
                let text = value()?;
                let parsed: f64 = text
                    .parse()
                    .map_err(|_| format!("{name}: {text:?} is not a number"))?;
                if name == "--temp" {
                    gen_options.sampling.temperature = parsed;
                } else {
                    gen_options.sampling.top_p = parsed;
                }
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

    if generating {
        // A model id the server does not know fails per-request rather than up
        // front, which turns one mistake into `count` identical errors.
        if gen_options.model.is_empty() {
            return Err(String::from("--gen needs --model"));
        }
        // Same reasoning for the sheet: a name that does not exist has to fail
        // here rather than after the first model call, and must never fall back
        // to the default — a batch generated against axes nobody chose looks
        // exactly like one that was.
        cram_gen::recipe::sheet(&gen_options.recipes)?;
        options.generate = Some(gen_options);
        return Ok(options);
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

    // A stride of one holds out everything, leaving nothing to train on. It is
    // always a typo, and it fails here rather than after the corpus is read.
    if options.held_out_every == 1 {
        return Err(String::from("--held-out-every 1 would hold out every file"));
    }
    if !options.drop_stages.is_empty() && options.batch_dirs.is_empty() {
        return Err(String::from("--drop-stage only means something with --batch-dir"));
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

/// The programs a run trains on, and the ones it is measured against.
///
/// With no `--real-root` and no `--batch-dir` this is the babble corpus and an
/// empty held-out set, exactly as every run behaved before real files were an
/// option — babble is generated, so a held-out split of it measures the
/// generator rather than the model.
fn gather(options: &Options) -> std::io::Result<(Vec<String>, Vec<String>)> {
    if options.real_root.is_none() && options.batch_dirs.is_empty() {
        let corpus = load_corpus(options.programs, options.layout)?;
        return Ok((parse_corpus(&corpus), Vec::new()));
    }

    let dropped: Vec<&str> = options.drop_stages.iter().map(String::as_str).collect();
    let loaded = corpus::load(
        options.real_root.as_deref(),
        &options.batch_dirs,
        &dropped,
        options.held_out_every,
        options.held_out_root.as_deref(),
        options.strip_comments,
    )
    .map_err(std::io::Error::other)?;

    for (label, train, held_out) in &loaded.sources {
        println!("corpus     {label}: {train} train, {held_out} held out");
    }
    if !dropped.is_empty() {
        println!("           dropped stages: {}", dropped.join(", "));
    }
    if options.strip_comments {
        println!("           comments stripped from both sides of the split");
    }

    if let Some(dir) = &options.write_held_out {
        write_held_out(dir, &loaded)?;
    }

    Ok((loaded.train, loaded.held_out))
}

/// Copy the held-out files somewhere `--eval --corpus-root` can walk.
///
/// Names are flattened from the source path rather than taken from it: two
/// sources both offering `001.st` would otherwise overwrite each other, and the
/// one that survived would be silently half a held-out set.
fn write_held_out(dir: &Path, loaded: &corpus::Loaded) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (path, text) in loaded.held_out_paths.iter().zip(&loaded.held_out) {
        let flattened: String = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .filter(|part| part != "." && part != "..")
            .collect::<Vec<_>>()
            .join("_");
        std::fs::write(dir.join(flattened), text)?;
    }
    println!("held out   {} files → {}", loaded.held_out.len(), dir.display());
    Ok(())
}

/// The longest tokens a vocab learned.
///
/// The check that catches an oversized vocab, and it costs nothing: BPE spends
/// its tail on whatever is frequent, so a tail of whole phrases means the vocab
/// is memorising this corpus rather than learning the language's lexemes. Bytes
/// per token alone cannot tell those apart — both look like better compression.
/// Same report `cram-corpus`'s `train-vocab` bin prints, for the same reason.
fn report_vocab(vocab: &Vocab) {
    const SHOWN: usize = 12;

    let mut learned: Vec<String> = (kvetch_vocab::BYTE_TOKENS..vocab.len())
        .map(|id| String::from_utf8_lossy(&vocab.decode(&[id as u16])).into_owned())
        .collect();
    learned.sort_by_key(|token| core::cmp::Reverse(token.len()));

    println!("           longest {SHOWN} learned tokens (phrases here mean an oversized vocab):");
    for token in learned.iter().take(SHOWN) {
        println!("             {token:?}");
    }
}

fn load_vocab(path: &Path) -> std::io::Result<Vocab> {
    let bytes = std::fs::read(path)?;
    let vocab = Vocab::decode_vocab(&bytes).ok_or_else(|| {
        std::io::Error::other(format!("{}: not a vocab this build reads", path.display()))
    })?;
    println!("vocab      {} tokens from {}", vocab.len(), path.display());
    Ok(vocab)
}

/// The held-out token stream. Same join as training, so the two streams differ
/// only in which programs they contain.
fn encode_held_out(vocab: &Vocab, programs: &[String]) -> Vec<u16> {
    let tokens = tokenize(vocab, &training_text(programs));
    println!("held out   {} tokens over {} programs", tokens.len(), programs.len());
    tokens
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
            grammar_digest: Manifest::grammar_digest(),
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
