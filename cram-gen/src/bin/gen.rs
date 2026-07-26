//! `gen <model> <n> [--out <dir>]` — generate `n` candidates against a local
//! LM Studio server and report the funnel.
//!
//! Replaces the copy-paste loop. One recipe for now: the harness earns the
//! recipe axes once it can run at all.

use std::path::PathBuf;
use std::process::ExitCode;

use cram_gen::{LmStudio, Run, run_once};

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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (model_name, count, out) = match parse(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}\nusage: gen <model> <n> [--out <dir>]");
            return ExitCode::FAILURE;
        }
    };

    let model = LmStudio::new(model_name);
    eprintln!(
        "model={} temp={} top_p={} max_tokens={}",
        model.model, model.sampling.temperature, model.sampling.top_p, model.sampling.max_tokens
    );

    let mut tally = Tally::default();
    for index in 0..count {
        match run_once(&model, TASK) {
            Ok(run) => {
                println!("{:03}: {}", index + 1, summarise(&run));
                tally.record(&run);
                if let Some(dir) = &out {
                    save(dir, index + 1, &run);
                }
            }
            Err(error) => {
                eprintln!("{:03}: model error: {error}", index + 1);
                tally.errors += 1;
            }
        }
    }

    println!("\n{}", tally.funnel(count));
    ExitCode::SUCCESS
}

fn parse(args: &[String]) -> Result<(String, usize, Option<PathBuf>), String> {
    let model = args.first().ok_or("missing <model>")?.clone();
    let count = args
        .get(1)
        .ok_or("missing <n>")?
        .parse()
        .map_err(|_| "n must be a number".to_string())?;
    let out = match args.iter().position(|arg| arg == "--out") {
        Some(at) => Some(PathBuf::from(args.get(at + 1).ok_or("--out needs a directory")?)),
        None => None,
    };
    Ok((model, count, out))
}

fn summarise(run: &Run) -> String {
    let extra = if run.extra_blocks > 0 {
        format!(" (+{} extra block(s))", run.extra_blocks)
    } else {
        String::new()
    };
    format!("{}{extra}", stage_detail(run))
}

fn stage_detail(run: &Run) -> String {
    use stitch::gate::Outcome;
    match &run.outcome {
        Outcome::Parse(error) => format!("parse — {error}"),
        Outcome::Type(errors) => format!("type — {}", errors.join("; ")),
        Outcome::Tests { failed, passed } => {
            format!("tests — {passed} passed, failed: {}", failed.join(", "))
        }
        Outcome::Ok { tests } => format!("ok — {tests} tests passed"),
    }
}

fn save(dir: &PathBuf, index: usize, run: &Run) {
    if let Err(error) = std::fs::create_dir_all(dir) {
        eprintln!("cannot create {}: {error}", dir.display());
        return;
    }
    let stem = dir.join(format!("{index:03}"));
    // Both halves are kept: the raw response is the only record of what the
    // model actually said, and the extracted program is what the gate saw.
    let _ = std::fs::write(stem.with_extension("raw.md"), &run.raw);
    let _ = std::fs::write(stem.with_extension("st"), &run.program);
}

#[derive(Default)]
struct Tally {
    parse: usize,
    r#type: usize,
    tests: usize,
    ok: usize,
    errors: usize,
}

impl Tally {
    fn record(&mut self, run: &Run) {
        match run.outcome.stage() {
            "parse" => self.parse += 1,
            "type" => self.r#type += 1,
            "tests" => self.tests += 1,
            _ => self.ok += 1,
        }
    }

    /// A funnel, never one number: the stage a candidate dies at is the
    /// diagnosis, and a single yield percentage collapses four different
    /// actions into one shrug.
    fn funnel(&self, attempted: usize) -> String {
        format!(
            "{attempted} attempted → {} model errors → parse ✗ {} → type ✗ {} → tests ✗ {} → ok {}",
            self.errors, self.parse, self.r#type, self.tests, self.ok
        )
    }
}
