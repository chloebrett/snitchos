//! `cargo xtask cram --gen` — generate candidate Stitch programs against a local
//! model server and report the funnel.
//!
//! The harness itself is `cram-gen`; this is the driver. It lives in
//! `xtask-cram` rather than lean `xtask` for the same reason `cram` and `itest`
//! do: an edit to the generator must not recompile the tool that runs
//! `cargo xtask test`.

use std::io::Write;
use std::path::{Path, PathBuf};

use cram_gen::{LmStudio, Sampling, Tally, describe, run_once};

/// The task, until the recipe axes in `plans/corpus-recipe-axes.md` are wired
/// in. One recipe is enough to measure a yield; the axes earn their way in once
/// the loop runs at all.
const TASK: &str = "\
Write a sauna booking module: rooms are booked for exclusive use over a time
window, so the core of the problem is detecting when two bookings overlap.

Shape: a module — `ext` items, no `main`.
Size: a small module — 1 to 4 types and 2 to 6 functions. Tests are extra and do
not count toward that. This is a rough guide, not a limit: never delete working
code or drop a test to hit it, and if the program naturally wants to be bigger,
let it be.
Use these constructs: prod, Maybe, |>
Name things for what they do.";

pub struct GenOptions {
    pub model: String,
    pub count: usize,
    pub out: Option<PathBuf>,
    pub endpoint: Option<String>,
    pub sampling: Sampling,
}

pub fn run(options: &GenOptions) -> std::io::Result<()> {
    let mut model = LmStudio::new(options.model.clone());
    model.sampling = options.sampling.clone();
    if let Some(endpoint) = &options.endpoint {
        model.base_url = endpoint.clone();
    }

    // Pinned settings go on the record: a later bulk run compared against this
    // one is meaningless if temperature drifted in between.
    println!(
        "model={} temp={} top_p={} max_tokens={}\n",
        model.model, model.sampling.temperature, model.sampling.top_p, model.sampling.max_tokens
    );

    let mut tally = Tally::default();
    for index in 1..=options.count {
        println!("── {index:03}/{} ──────────────", options.count);
        // Stream the model's output as it arrives. A candidate takes tens of
        // seconds to minutes; a silent terminal for that long is indistinguishable
        // from a hang, and the text itself is the most useful progress there is —
        // a repetition spiral is obvious on sight long before the token cap hits.
        let mut on_chunk = |chunk: &str| {
            print!("{chunk}");
            let _ = std::io::stdout().flush();
        };
        match run_once(&model, TASK, &mut on_chunk) {
            Ok(run) => {
                println!();
                let extra = if run.extra_blocks > 0 {
                    tally.extra_blocks += run.extra_blocks;
                    format!(" (+{} extra block(s))", run.extra_blocks)
                } else {
                    String::new()
                };
                println!("\n{index:03}: {}{extra}\n", describe(&run.outcome));
                tally.record(&run.outcome);
                if let Some(dir) = &options.out {
                    save(dir, index, &run)?;
                }
            }
            Err(error) => {
                eprintln!("\n{index:03}: model error: {error}\n");
                tally.record_error();
            }
        }
    }

    println!("\n{}", tally.funnel(options.count));
    if tally.extra_blocks > 0 {
        println!("{} extra fenced block(s) — a prompt problem, not a Stitch one", tally.extra_blocks);
    }
    if let Some(dir) = &options.out {
        println!("candidates in {}", dir.display());
    }
    Ok(())
}

/// Both halves are kept. The raw response is the only record of what the model
/// actually said; the extracted program is what the gate saw. Keeping the
/// failures matters most — model-produced broken code plus its diagnostic is the
/// scarcest input the RL branch has (`docs/kvetch-rl-design.md` §5).
fn save(dir: &Path, index: usize, run: &cram_gen::Run) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let stem = dir.join(format!("{index:03}"));
    std::fs::write(stem.with_extension("raw.md"), &run.raw)?;
    std::fs::write(stem.with_extension("st"), &run.program)?;
    Ok(())
}
