//! The `.st` program runner: parse a source program, run it, and render the
//! result and telemetry into stdout/stderr text + an exit code. The logic lives
//! here (host-testable); `main.rs` is a thin wiring layer over it.

use core::fmt::Write as _;

use alloc::collections::BTreeSet;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::Item;
use crate::env::Env;
use crate::interp::{
    Module, build_env_with_telemetry, eval, eval_modules_with_telemetry,
    eval_program_with_telemetry, is_builtin_module, prelude_items,
};
use crate::parser::{parse, parse_program};
use crate::telemetry::{RecordingTelemetry, Telemetry};
use crate::value::{RuntimeError, TelemetryEvent, Value};

/// Writing to a `String` never fails; this names that contract at each `write!`.
const INFALLIBLE: &str = "writing to a String is infallible";

/// The outcome of running a program: what to print where, and the exit code.
#[derive(Debug)]
pub struct RunResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Parse and run a single-module Stitch program (no imports — the REPL and
/// `eval_program` path). Telemetry (and `main`'s non-unit result) go to stdout; a
/// parse error (exit 2) or runtime error (exit 1) goes to stderr.
pub fn run_program_source(src: &str) -> RunResult {
    let items = match parse_program(src) {
        Ok(items) => items,
        Err(error) => {
            return RunResult {
                stdout: String::new(),
                stderr: format!("parse error: {}\n", error.message),
                exit_code: 2,
            };
        }
    };
    finish(eval_program_with_telemetry(&items))
}

/// Discover, load, and run a multi-file program rooted at the `entry` module,
/// fetching each imported module's source by name via `fetch` (the filesystem in
/// the CLI, an in-memory map in tests). A load error (missing module / parse
/// error in any module) goes to stderr with exit 2.
pub fn run_module_files(
    entry: &str,
    fetch: impl Fn(&str) -> Result<String, String>,
) -> RunResult {
    let modules = match discover_modules(entry, fetch) {
        Ok(modules) => modules,
        Err(message) => {
            return RunResult {
                stdout: String::new(),
                stderr: format!("load error: {message}\n"),
                exit_code: 2,
            };
        }
    };
    finish(eval_modules_with_telemetry(&modules, entry))
}

/// Walk a program's `use` imports from `entry`, fetching and parsing each
/// reachable module exactly once, into the module set `eval_modules` consumes.
/// Built-in stdlib modules (`Seq`/`Str`) are runtime-provided, so they're skipped
/// (never fetched). Import cycles terminate — a module is loaded at most once.
///
/// # Errors
/// A module that can't be fetched, or whose source fails to parse, aborts the
/// load with a message naming the module.
pub fn discover_modules(
    entry: &str,
    fetch: impl Fn(&str) -> Result<String, String>,
) -> Result<Vec<Module>, String> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![entry.to_string()];
    let mut modules = Vec::new();
    while let Some(name) = pending.pop() {
        if is_builtin_module(&name) || !seen.insert(name.clone()) {
            continue;
        }
        let source = fetch(&name)?;
        let items = parse_program(&source)
            .map_err(|error| format!("in module `{name}`: {}", error.message))?;
        for item in &items {
            if let Item::Use { module, .. } = item {
                pending.push(module.clone());
            }
        }
        modules.push(Module { name, items });
    }
    Ok(modules)
}

/// Render an evaluation outcome (result + telemetry) into stdout/stderr + an exit
/// code — shared by the single-module and multi-file paths.
fn finish((result, events): (Result<Value, RuntimeError>, Vec<TelemetryEvent>)) -> RunResult {
    let mut stdout = render_telemetry(&events);
    match result {
        Ok(value) => {
            if value != Value::Unit {
                writeln!(stdout, "=> {}", value.display()).expect(INFALLIBLE);
            }
            RunResult { stdout, stderr: String::new(), exit_code: 0 }
        }
        Err(error) => RunResult {
            stdout,
            stderr: format!("runtime error: {}\n", error.message()),
            exit_code: 1,
        },
    }
}

/// A read-eval-print loop that **builds its environment once and reuses it**.
/// The prelude (and accumulated definitions) are registered into one shared env;
/// every expression line evaluates against that cached env instead of rebuilding
/// the world — the costly per-line prelude re-registration is gone. A new
/// declaration invalidates the cache so the next eval picks it up.
///
/// (Expressions evaluate directly, not wrapped in `main()`, so a bare top-level
/// `?` has no enclosing function — a non-issue at a prompt.)
pub struct Repl {
    /// The prelude AST, parsed once.
    prelude: Vec<Item>,
    /// Accumulated declarations (functions, types, consts) from prior lines.
    defs: Vec<Item>,
    /// The built environment, rebuilt lazily after a new declaration.
    env: Option<Env>,
    /// The telemetry backend every rebuilt env shares — the in-memory recorder
    /// by default, or the on-target capability-backed one via
    /// [`Repl::with_telemetry`]. Held here so it survives env rebuilds (so e.g.
    /// the on-target metric registrations persist across new declarations).
    telemetry: Rc<dyn Telemetry>,
}

impl Default for Repl {
    fn default() -> Self {
        Self::new()
    }
}

impl Repl {
    /// A fresh REPL with the prelude parsed (once) and no user definitions yet.
    /// Telemetry is recorded in memory (rendered per line).
    #[must_use]
    pub fn new() -> Self {
        Repl::with_telemetry(Rc::new(RecordingTelemetry::default()))
    }

    /// A fresh REPL whose evaluated lines route `emit`/`span` through
    /// `telemetry`. The on-target REPL passes a `RuntimeTelemetry` so a Stitch
    /// program's spans and metrics become real frames on the wire.
    #[must_use]
    pub fn with_telemetry(telemetry: Rc<dyn Telemetry>) -> Self {
        Repl { prelude: prelude_items(), defs: Vec::new(), env: None, telemetry }
    }

    /// Load a whole `.st` source: parse it and accumulate its declarations into
    /// the session (invalidating the cached env), so its functions and types are
    /// callable at the prompt afterward. The fetching — disk on the host, the
    /// filesystem endpoint on the metal — is the caller's job; this is the
    /// backend-agnostic core. Returns a one-line summary, or a `load error:`
    /// message that leaves the session untouched.
    pub fn load_source(&mut self, src: &str) -> String {
        match parse_program(src) {
            Ok(items) if items.is_empty() => "loaded nothing (empty source)\n".to_string(),
            Ok(items) => {
                let count = items.len();
                self.defs.extend(items);
                self.env = None;
                format!("loaded {count} definition(s)\n")
            }
            Err(error) => format!("load error: {}\n", error.message),
        }
    }

    /// Evaluate one line. A line that parses as declarations is accumulated
    /// (invalidating the cached env) and produces no output; otherwise it's run as
    /// an expression against the cached env, and its telemetry + result (or error)
    /// is returned as text.
    pub fn eval_line(&mut self, line: &str) -> String {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        // Declarations (`f(x) = …`, `prod …`) accumulate silently and drop the
        // cached env so the next expression rebuilds with them in scope.
        if let Ok(items) = parse_program(trimmed)
            && !items.is_empty()
        {
            self.defs.extend(items);
            self.env = None;
            return String::new();
        }
        let expr = match parse(trimmed) {
            Ok(expr) => expr,
            Err(error) => return format!("parse error: {}\n", error.message),
        };
        // Build the env once (prelude + defs), then reuse it for every expression.
        if self.env.is_none() {
            let mut all = self.prelude.clone();
            all.extend_from_slice(&self.defs);
            self.env = Some(build_env_with_telemetry(Rc::clone(&self.telemetry), &all));
        }
        let env = self.env.as_ref().expect("env was just built above");
        let result = eval(&expr, env);
        // Drain only *this* line's telemetry from the long-lived sink.
        let mut out = render_telemetry(&env.take_telemetry());
        match result {
            Ok(value) if value != Value::Unit => {
                writeln!(out, "=> {}", value.display()).expect(INFALLIBLE);
            }
            Ok(_) => {}
            Err(error) => writeln!(out, "runtime error: {}", error.message()).expect(INFALLIBLE),
        }
        out
    }
}

/// Render telemetry events as an indented tree: spans bracket their contents.
fn render_telemetry(events: &[TelemetryEvent]) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for event in events {
        match event {
            TelemetryEvent::SpanOpen { name } => {
                writeln!(out, "{}span {name} {{", "  ".repeat(depth)).expect(INFALLIBLE);
                depth += 1;
            }
            TelemetryEvent::SpanClose { .. } => {
                depth = depth.saturating_sub(1);
                writeln!(out, "{}}}", "  ".repeat(depth)).expect(INFALLIBLE);
            }
            TelemetryEvent::Emit { name, value } => {
                writeln!(out, "{}emit {name} = {}", "  ".repeat(depth), value.display())
                    .expect(INFALLIBLE);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::runner::{discover_modules, run_module_files, run_program_source};

    #[test]
    fn emitting_from_a_function_without_uses_telemetry_is_refused() {
        // `speak` calls emit but declares no `uses` — even though `main` holds
        // Telemetry, authority does not inherit across the named-function boundary.
        let r = run_program_source("speak() = emit(\"x\", 1)  main() uses Telemetry = speak()");
        assert_eq!(r.exit_code, 1, "stdout={} stderr={}", r.stdout, r.stderr);
        assert!(r.stderr.contains("uses Telemetry"), "{}", r.stderr);
    }

    #[test]
    fn emitting_from_a_function_that_declares_uses_telemetry_is_allowed() {
        let r = run_program_source("main() uses Telemetry = emit(\"hits\", 7)");
        assert_eq!(r.exit_code, 0, "stderr={}", r.stderr);
        assert!(r.stdout.contains("emit hits = 7"), "{}", r.stdout);
    }

    #[test]
    fn loading_source_registers_definitions_for_later_calls() {
        let mut repl = super::Repl::new();
        let summary = repl.load_source("double(x) = x * 2\ntriple(x) = x * 3");
        assert!(summary.contains('2'), "summary should report 2 defs: {summary}");
        assert_eq!(repl.eval_line("double(21)").trim(), "=> 42");
        assert_eq!(repl.eval_line("triple(14)").trim(), "=> 42");
    }

    #[test]
    fn loading_invalid_source_reports_an_error_and_keeps_the_session() {
        let mut repl = super::Repl::new();
        let summary = repl.load_source("prod (((");
        assert!(summary.to_lowercase().contains("error"), "expected an error: {summary}");
        // The bad load must not poison the session — the REPL stays usable.
        assert_eq!(repl.eval_line("1 + 1").trim(), "=> 2");
    }

    /// A fake filesystem: fetch a module's source by name from an in-memory map
    /// the closure owns (so it borrows nothing).
    fn fake_fs(sources: &[(&str, &str)]) -> impl Fn(&str) -> Result<String, String> {
        let map: HashMap<String, String> = sources
            .iter()
            .map(|(name, src)| ((*name).to_string(), (*src).to_string()))
            .collect();
        move |name: &str| {
            map.get(name)
                .cloned()
                .ok_or_else(|| format!("cannot find module `{name}`"))
        }
    }

    #[test]
    fn discovers_imported_modules_transitively() {
        let modules = discover_modules(
            "main",
            fake_fs(&[
                ("main", "use a  main() = a.f()"),
                ("a", "use b  ext f() = b.g()"),
                ("b", "ext g() = 1"),
            ]),
        )
        .expect("should discover the transitive set");
        let mut names = modules.iter().map(|m| m.name.clone()).collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["a", "b", "main"]);
    }

    #[test]
    fn the_loader_skips_builtin_modules() {
        // `Seq` is runtime-provided, not a file — the loader must not try to read it.
        let modules = discover_modules("main", fake_fs(&[("main", "use Seq  main() = 1")]))
            .expect("Seq is built-in, not loaded from disk");
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "main");
    }

    #[test]
    fn a_missing_imported_module_is_a_load_error() {
        let error = discover_modules("main", fake_fs(&[("main", "use gone  main() = 1")]))
            .expect_err("the missing import should fail discovery");
        assert!(error.contains("gone"), "{error}");
    }

    #[test]
    fn import_cycles_terminate_during_discovery() {
        let modules = discover_modules(
            "main",
            fake_fs(&[("main", "use a  main() = 1"), ("a", "use main  ext f() = 1")]),
        )
        .expect("a cycle must not hang the loader");
        assert_eq!(modules.len(), 2);
    }

    #[test]
    fn run_module_files_runs_a_multi_file_program() {
        let result = run_module_files(
            "main",
            fake_fs(&[
                ("main", "use math  main() = math.double(21)"),
                ("math", "ext double(x) = x * 2"),
            ]),
        );
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "=> 42\n");
    }

    #[test]
    fn run_module_files_reports_a_load_error_on_stderr() {
        let result = run_module_files("main", fake_fs(&[("main", "use gone  main() = 1")]));
        assert_eq!(result.exit_code, 2);
        assert!(result.stderr.contains("gone"), "{}", result.stderr);
    }

    #[test]
    fn runs_a_program_and_prints_its_result() {
        let result = run_program_source("main() = 1 + 2");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stderr, "");
        assert_eq!(result.stdout, "=> 3\n");
    }

    #[test]
    fn a_unit_result_prints_no_result_line() {
        let result = run_program_source(r#"main() uses Telemetry = emit("x", 1)"#);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "emit x = 1\n");
    }

    #[test]
    fn renders_telemetry_with_span_nesting() {
        let src = r#"main() uses Telemetry = span("report", () -> emit("hot.count", 2))"#;
        let result = run_program_source(src);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "span report {\n  emit hot.count = 2\n}\n");
    }

    #[test]
    fn a_parse_error_goes_to_stderr_with_exit_2() {
        let result = run_program_source("main() = 1 +");
        assert_eq!(result.exit_code, 2);
        assert_eq!(result.stdout, "");
        assert!(result.stderr.starts_with("parse error:"), "{}", result.stderr);
    }

    #[test]
    fn a_runtime_error_goes_to_stderr_with_exit_1() {
        let result = run_program_source("main() = 1 / 0");
        assert_eq!(result.exit_code, 1);
        assert!(
            result.stderr.contains("division by zero"),
            "{}",
            result.stderr
        );
    }

    #[test]
    fn telemetry_emitted_before_a_runtime_error_is_still_shown() {
        let src = r#"
            boom() = 1 / 0
            main() uses Telemetry = { emit("before", 1)  boom() }
        "#;
        let result = run_program_source(src);
        assert_eq!(result.exit_code, 1);
        assert!(result.stdout.contains("emit before = 1"), "{}", result.stdout);
        assert!(result.stderr.contains("division by zero"));
    }

    use crate::runner::Repl;

    #[test]
    fn the_repl_evaluates_a_bare_expression() {
        assert_eq!(Repl::new().eval_line("1 + 2"), "=> 3\n");
    }

    #[test]
    fn the_repl_accumulates_definitions_then_uses_them() {
        let mut repl = Repl::new();
        assert_eq!(repl.eval_line("double(x) = x * 2"), "");
        assert_eq!(repl.eval_line("double(21)"), "=> 42\n");
    }

    #[test]
    fn the_repl_reuses_the_cached_env_across_many_expressions() {
        // The whole point of the cache: a definition is built once, then many
        // expressions evaluate against the same env (no per-line prelude rebuild).
        let mut repl = Repl::new();
        assert_eq!(repl.eval_line("sq(x) = x * x"), "");
        assert_eq!(repl.eval_line("sq(3)"), "=> 9\n");
        assert_eq!(repl.eval_line("sq(4)"), "=> 16\n");
        assert_eq!(repl.eval_line("sq(5)"), "=> 25\n");
    }

    #[test]
    fn the_repl_renders_only_this_lines_telemetry() {
        // The env is long-lived, so its sink must be drained per line — line two's
        // output must not carry line one's event.
        let mut repl = Repl::new();
        assert!(repl.eval_line(r#"emit("a", 1)"#).contains("emit a = 1"));
        let second = repl.eval_line(r#"emit("b", 2)"#);
        assert!(second.contains("emit b = 2"), "{second}");
        assert!(!second.contains("emit a = 1"), "{second}");
    }

    #[test]
    fn the_repl_reports_a_runtime_error_inline() {
        assert!(Repl::new().eval_line("1 / 0").contains("division by zero"));
    }

    #[test]
    fn a_blank_repl_line_produces_nothing() {
        assert_eq!(Repl::new().eval_line("   "), "");
    }
}
