//! The `stitch` CLI: `stitch <file.st>` runs a program; `stitch` with no args
//! starts a REPL. All logic lives in `stitch::runner`; this is just wiring.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use stitch::platform::Platform;
use stitch::runner::{Repl, run_module_files};
use stitch::telemetry::RecordingTelemetry;

/// The host CLI's console backend: real stdout/stdin, so `print`/`readLine` work
/// at the prompt. The metal uses `RuntimePlatform`; tests use `FakePlatform`.
struct StdPlatform;

impl Platform for StdPlatform {
    fn write(&self, text: &str) {
        print!("{text}");
        let _ = io::stdout().flush();
    }

    fn read_line(&self) -> Option<String> {
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None, // EOF or error ends the session
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
        }
    }
}

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
/// Both the REPL's own line reading and a Stitch `readLine()` go through the one
/// [`StdPlatform`], so they draw from a single stdin stream.
fn repl() -> ExitCode {
    let platform = Rc::new(StdPlatform);
    let mut repl = Repl::with_backends(Rc::new(RecordingTelemetry::default()), platform.clone());
    platform.write("stitch> ");
    while let Some(line) = platform.read_line() {
        let out = match line.trim().strip_prefix(":load ") {
            Some(path) => load_file(&mut repl, path.trim()),
            None => repl.eval_line(&line),
        };
        platform.write(&out);
        platform.write("stitch> ");
    }
    platform.write("\n");
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
