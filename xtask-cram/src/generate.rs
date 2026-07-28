//! `cargo xtask cram --gen` — generate candidate Stitch programs against a local
//! model server and report the funnel.
//!
//! The harness itself is `cram-gen`; this is the driver. It lives in
//! `xtask-cram` rather than lean `xtask` for the same reason `cram` and `itest`
//! do: an edit to the generator must not recompile the tool that runs
//! `cargo xtask test`.
//!
//! **The run reports on itself**, same as training does. A generation batch's
//! failure modes are quiet — a server silently re-enabling thinking, throughput
//! halving, a whole batch dying at one stage — so the funnel, the timings and
//! the per-candidate verdicts all go to the terminal as they happen and to a
//! manifest beside the candidates for later.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use cram_gen::{CandidateRecord, LmStudio, Sampling, Tally, describe, run_once, run_once_capped};

pub struct GenOptions {
    pub model: String,
    /// Which recipe sheet the batch draws its axes from. Recorded in the
    /// manifest: a corpus whose axes cannot be named later is a corpus whose
    /// findings cannot be read back onto them.
    pub recipes: String,
    pub count: usize,
    pub out: Option<PathBuf>,
    pub endpoint: Option<String>,
    /// Rewinds allowed per candidate. When the continuation oracle says no
    /// token can rescue the program, generation goes back to just before the
    /// fatal text and resumes. 0 disables it entirely.
    pub corrections: usize,
    /// Ceiling on the accumulated program, in bytes. 0 disables it. Only the
    /// correction path can exceed it — without rewinds, `max_tokens` already
    /// bounds a candidate.
    pub max_bytes: usize,
    pub sampling: Sampling,
}

pub fn run(options: &GenOptions) -> std::io::Result<()> {
    let sheet = match cram_gen::recipe::sheet(&options.recipes) {
        Ok(sheet) => sheet,
        Err(error) => return Err(std::io::Error::other(error)),
    };
    let mut model = LmStudio::new(options.model.clone());
    model.sampling = options.sampling.clone();
    if let Some(endpoint) = &options.endpoint {
        model.base_url = endpoint.clone();
    }

    // Pinned settings go on the record: a later bulk run compared against this
    // one is meaningless if temperature drifted in between.
    // `correct` belongs here as much as temperature does: batch9's manifest
    // recorded every sampling knob and not the guard budget, so the run cannot
    // be reproduced and no later run is strictly comparable to it.
    let header = format!(
        "model={} recipes={} ({} recipes over {} domains) correct={} max_bytes={} temp={} top_p={} top_k={} presence={} max_tokens={}",
        model.model,
        sheet.name,
        sheet.count(),
        sheet.domains().len(),
        options.corrections,
        options.max_bytes,
        model.sampling.temperature,
        model.sampling.top_p,
        model.sampling.top_k,
        model.sampling.presence_penalty,
        model.sampling.max_tokens,
    );
    println!("{header}\n");

    let mut tally = Tally::default();
    let mut records = Vec::new();
    let mut warned_about_thinking = false;
    let batch_started = Instant::now();

    for index in 1..=options.count {
        // Cycle the axes: `--count 500` is five passes over all hundred domains,
        // not five hundred copies of the first one.
        let recipe = sheet.nth(index - 1);
        let task = recipe.render();
        println!(
            "── {index:03}/{}  {} · {} · {} ──────────────",
            options.count, recipe.domain, recipe.size, recipe.shape
        );
        // Stream the model's output as it arrives. A candidate takes tens of
        // seconds; a silent terminal for that long is indistinguishable from a
        // hang, and the text itself is the most useful progress there is — a
        // repetition spiral is obvious on sight long before the cap hits.
        let mut on_chunk = |chunk: &str| {
            print!("{chunk}");
            let _ = std::io::stdout().flush();
        };
        // A rewind throws away text the stream has already printed, so the
        // terminal stops matching the program that will be saved. Announce it,
        // with control characters escaped so it is unambiguous which bytes went.
        let mut on_rewind = |discarded: &str| {
            print!("\n\x1b[2m⟲ rewound: \"{}\"\x1b[0m\n", cram_gen::escape(discarded));
            let _ = std::io::stdout().flush();
        };
        let started = Instant::now();
        let outcome = if options.corrections > 0 {
            run_once_capped(
                &model,
                &task,
                options.corrections,
                options.max_bytes,
                &mut on_chunk,
                &mut on_rewind,
            )
        } else {
            run_once(&model, &task, &mut on_chunk)
        };
        let seconds = started.elapsed().as_secs_f64();

        // Say it once, plainly. The difference between "the prompt is wrong" and
        // "the server ignored `enable_thinking`" is the difference between
        // editing a prompt and editing a chat template.
        if !warned_about_thinking && matches!(&outcome, Ok(run) if run.reasoned) {
            warned_about_thinking = true;
            eprintln!(
                "\n! the server is still emitting <think> — `chat_template_kwargs` was ignored.\n\
                 ! fix it in LM Studio: model settings → Prompt Template, and put\n\
                 !   {{%- set enable_thinking = false %}}\n\
                 ! on the FIRST line. Later in the file the template has already read it.\n"
            );
        }

        match outcome {
            Ok(run) => {
                let empty = run.program.trim().is_empty();
                let (stage, detail) = if empty {
                    // Reasoning that ran into the token cap. Blaming the parser
                    // for this would point at the wrong thing entirely.
                    tally.record_empty();
                    ("empty".to_string(), "no program in response".to_string())
                } else if run.overlong {
                    // The gate will call this a parse death, and it is not one:
                    // the program was cut off mid-write. Recording it as such
                    // would put a harness decision into the Stitch statistics.
                    tally.record_long();
                    (
                        "long".to_string(),
                        format!("stopped at {} bytes — over the cap", run.program.len()),
                    )
                } else {
                    tally.record(&run.outcome);
                    (run.outcome.stage().to_string(), describe(&run.outcome))
                };
                tally.extra_blocks += run.extra_blocks;

                let guarded = match (run.corrections.len(), run.abandoned) {
                    (0, false) => String::new(),
                    (n, false) => format!(" [{n} rewind(s)]"),
                    (n, true) => format!(" [{n} rewind(s), then abandoned]"),
                };
                println!(
                    "\n{index:03}: {detail}{}{guarded} — {}",
                    extra_note(run.extra_blocks),
                    rate(run.tokens, seconds)
                );
                if empty {
                    println!("     raise --max-tokens, or disable thinking");
                }
                println!();

                records.push(CandidateRecord {
                    index,
                    domain: recipe.domain.clone(),
                    size: recipe.size.clone(),
                    shape: recipe.shape.clone(),
                    stage,
                    detail,
                    tokens: run.tokens,
                    seconds,
                    reasoned: run.reasoned,
                    extra_blocks: run.extra_blocks,
                    abandoned: run.abandoned,
                    corrections: run.corrections.clone(),
                });
                // Everything is kept, salvageable or not: model-produced broken
                // code plus its diagnostic is the scarcest input the RL branch
                // has (`docs/kvetch-rl-design.md` §5).
                if let Some(dir) = &options.out {
                    save(dir, index, &run)?;
                    // Rewrite the manifest after every candidate rather than at
                    // the end. A long batch is very likely to be interrupted —
                    // and one that was lost every verdict it had already earned,
                    // leaving a directory of programs with no record of what the
                    // gate said about any of them.
                    write_manifest(
                        dir,
                        &header,
                        batch_started.elapsed().as_secs_f64(),
                        &tally,
                        &records,
                        options.count,
                    )?;
                }
            }
            Err(error) => {
                eprintln!("\n{index:03}: model error: {error}\n");
                tally.record_error();
            }
        }
    }

    let elapsed = batch_started.elapsed().as_secs_f64();
    let tokens: usize = records.iter().map(|record| record.tokens).sum();
    println!("{}", tally.funnel(options.count));
    println!(
        "{:.1}s total, {tokens} tokens, {}",
        elapsed,
        rate(tokens, elapsed)
    );
    if tally.extra_blocks > 0 {
        println!(
            "{} extra fenced block(s) — a prompt problem, not a Stitch one",
            tally.extra_blocks
        );
    }

    if let Some(dir) = &options.out {
        write_manifest(dir, &header, elapsed, &tally, &records, options.count)?;
        println!("candidates + manifest in {}", dir.display());
    }
    Ok(())
}

fn rate(tokens: usize, seconds: f64) -> String {
    if seconds <= 0.0 {
        return format!("{tokens} tokens");
    }
    format!("{tokens} tokens in {seconds:.1}s ({:.1} tok/s)", tokens as f64 / seconds)
}

fn extra_note(extra: usize) -> String {
    if extra == 0 { String::new() } else { format!(" (+{extra} extra block(s))") }
}

/// Both halves are kept. The raw response is the only record of what the model
/// actually said; the extracted program is what the gate saw.
fn save(dir: &Path, index: usize, run: &cram_gen::Run) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let stem = dir.join(format!("{index:03}"));
    std::fs::write(stem.with_extension("raw.md"), &run.raw)?;
    std::fs::write(stem.with_extension("st"), &run.program)?;
    Ok(())
}

fn write_manifest(
    dir: &Path,
    header: &str,
    elapsed: f64,
    tally: &Tally,
    records: &[CandidateRecord],
    attempted: usize,
) -> std::io::Result<()> {
    let tokens: usize = records.iter().map(|record| record.tokens).sum();
    let manifest = serde_json::json!({
        "settings": header,
        "attempted": attempted,
        "elapsed_seconds": elapsed,
        "tokens": tokens,
        "tokens_per_second": if elapsed > 0.0 { tokens as f64 / elapsed } else { 0.0 },
        "funnel": {
            "model_errors": tally.errors,
            "empty": tally.empty,
            "long": tally.long,
            "parse": tally.parse,
            "type": tally.type_errors,
            "tests": tally.tests,
            "ok": tally.ok,
        },
        "candidates": records,
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    )
}
