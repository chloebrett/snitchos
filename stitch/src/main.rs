//! The `stitch` CLI: `stitch <file.st>` runs a program; `stitch` with no args
//! starts a REPL. All logic lives in `stitch::runner`; this is just wiring.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use stitch::ast::Item;
use stitch::runner::{run_program_source, run_repl_line};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next() {
        Some(path) => run_file(&path),
        None => repl(),
    }
}

/// Run a `.st` file: telemetry/result to stdout, errors to stderr, exit code.
fn run_file(path: &str) -> ExitCode {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("error: cannot read {path}: {error}");
            return ExitCode::from(2);
        }
    };
    let result = run_program_source(&source);
    print!("{}", result.stdout);
    eprint!("{}", result.stderr);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation, reason = "exit codes are 0-2")]
    ExitCode::from(result.exit_code as u8)
}

/// A line-at-a-time REPL: definitions accumulate, expressions are evaluated.
fn repl() -> ExitCode {
    let mut defs: Vec<Item> = Vec::new();
    let stdin = io::stdin();
    print!("stitch> ");
    let _ = io::stdout().flush();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        print!("{}", run_repl_line(&mut defs, &line));
        print!("stitch> ");
        let _ = io::stdout().flush();
    }
    println!();
    ExitCode::SUCCESS
}
