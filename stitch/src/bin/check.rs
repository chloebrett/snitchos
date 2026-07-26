//! `check <file.st>…` — run each file through the corpus gate and report the
//! stage it died at.
//!
//! Deliberately tiny. Per `plans/corpus-mvp-spike.md` S4 this exists so sixty
//! hand-pasted candidates become sixty one-line commands; if it grows candidate
//! extraction, a recipe loader, or anything that calls a model, the harness has
//! arrived early through the side door.

use std::process::ExitCode;

use stitch::gate::{self, Outcome};

fn main() -> ExitCode {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: check <file.st>…");
        return ExitCode::FAILURE;
    }

    let mut rejected = 0usize;
    for path in &paths {
        let src = match std::fs::read_to_string(path) {
            Ok(src) => src,
            Err(error) => {
                eprintln!("{path}: cannot read: {error}");
                rejected += 1;
                continue;
            }
        };
        let outcome = gate::run(&src);
        if !outcome.accepted() {
            rejected += 1;
        }
        println!("{path}: {}", describe(&outcome));
    }

    if paths.len() > 1 {
        println!("\n{}/{} accepted", paths.len() - rejected, paths.len());
    }
    if rejected == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Parse(error) => format!("parse — {error}"),
        Outcome::Type(errors) => format!("type — {}", errors.join("; ")),
        Outcome::Tests { failed, passed } => {
            format!("tests — {passed} passed, failed: {}", failed.join(", "))
        }
        Outcome::Ok { tests } => format!("ok — {tests} tests passed"),
    }
}
