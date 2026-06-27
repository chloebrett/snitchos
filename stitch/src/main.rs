//! The `stitch` CLI: `stitch <file.st>` runs a program; `stitch` with no args
//! starts a REPL. All logic lives in `stitch::runner`; this is just wiring.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use stitch::runner::{Repl, run_module_files};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next() {
        Some(path) => run_file(&path),
        None => repl(),
    }
}

/// Run a `.st` file as the entry module of a (possibly multi-file) program: its
/// `use` imports resolve to sibling `<name>.st` files in the same directory.
/// Telemetry/result to stdout, errors to stderr, exit code.
fn run_file(path: &str) -> ExitCode {
    let path = Path::new(path);
    let Some(entry) = path.file_stem().and_then(|stem| stem.to_str()) else {
        eprintln!("error: invalid file name {}", path.display());
        return ExitCode::from(2);
    };
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let fetch = |name: &str| {
        let module_path = dir.join(format!("{name}.st"));
        std::fs::read_to_string(&module_path)
            .map_err(|error| format!("cannot read `{}`: {error}", module_path.display()))
    };
    let result = run_module_files(entry, fetch);
    print!("{}", result.stdout);
    eprint!("{}", result.stderr);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation, reason = "exit codes are 0-2")]
    ExitCode::from(result.exit_code as u8)
}

/// A line-at-a-time REPL: definitions accumulate, expressions are evaluated, and
/// `:load <path>` reads a `.st` file from disk and registers its definitions.
fn repl() -> ExitCode {
    let mut repl = Repl::new();
    let stdin = io::stdin();
    print!("stitch> ");
    let _ = io::stdout().flush();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        match line.trim().strip_prefix(":load ") {
            Some(path) => print!("{}", load_file(&mut repl, path.trim())),
            None => print!("{}", repl.eval_line(&line)),
        }
        print!("stitch> ");
        let _ = io::stdout().flush();
    }
    println!();
    ExitCode::SUCCESS
}

/// Read a `.st` file from disk and register its definitions into the REPL
/// session. The host filesystem here stands in for the on-target fs endpoint.
fn load_file(repl: &mut Repl, path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(src) => repl.load_source(&src),
        Err(error) => format!("load error: cannot read `{path}`: {error}\n"),
    }
}
