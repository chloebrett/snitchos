//! The `.st` program runner: parse a source program, run it, and render the
//! result and telemetry into stdout/stderr text + an exit code. The logic lives
//! here (host-testable); `main.rs` is a thin wiring layer over it.

use core::fmt::Write as _;

use alloc::collections::BTreeSet;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::Item;
use crate::interp::{
    Module, eval_modules_with_telemetry, eval_program_with_telemetry, is_builtin_module,
};
use crate::parser::{parse, parse_program};
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

/// Evaluate one REPL line against the accumulated definitions `defs`. A line
/// that parses as declarations is appended to `defs` (and produces no output);
/// otherwise it's run as an expression — `main() = <expr>` against the defs —
/// and its telemetry, result, or error is returned as text.
pub fn run_repl_line(defs: &mut Vec<Item>, line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Declarations (`f(x) = …`, `prod …`) accumulate silently.
    if let Ok(items) = parse_program(trimmed)
        && !items.is_empty()
    {
        defs.extend(items);
        return String::new();
    }
    let expr = match parse(trimmed) {
        Ok(expr) => expr,
        Err(error) => return format!("parse error: {}\n", error.message),
    };
    let mut program = defs.clone();
    program.push(Item::Func {
        name: "main".to_string(),
        params: Vec::new(),
        ret: None,
        body: expr,
        public: false,
    });
    let (result, events) = eval_program_with_telemetry(&program);
    let mut out = render_telemetry(&events);
    match result {
        Ok(value) if value != Value::Unit => {
            writeln!(out, "=> {}", value.display()).expect(INFALLIBLE);
        }
        Ok(_) => {}
        Err(error) => writeln!(out, "runtime error: {}", error.message()).expect(INFALLIBLE),
    }
    out
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
        let result = run_program_source(r#"main() = emit("x", 1)"#);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "emit x = 1\n");
    }

    #[test]
    fn renders_telemetry_with_span_nesting() {
        let src = r#"main() = span("report", () -> emit("hot.count", 2))"#;
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
            main() = { emit("before", 1)  boom() }
        "#;
        let result = run_program_source(src);
        assert_eq!(result.exit_code, 1);
        assert!(result.stdout.contains("emit before = 1"), "{}", result.stdout);
        assert!(result.stderr.contains("division by zero"));
    }

    use crate::ast::Item;
    use crate::runner::run_repl_line;

    #[test]
    fn the_repl_evaluates_a_bare_expression() {
        let mut defs: Vec<Item> = Vec::new();
        assert_eq!(run_repl_line(&mut defs, "1 + 2"), "=> 3\n");
    }

    #[test]
    fn the_repl_accumulates_definitions_then_uses_them() {
        let mut defs: Vec<Item> = Vec::new();
        assert_eq!(run_repl_line(&mut defs, "double(x) = x * 2"), "");
        assert_eq!(run_repl_line(&mut defs, "double(21)"), "=> 42\n");
    }

    #[test]
    fn the_repl_reports_a_runtime_error_inline() {
        let mut defs: Vec<Item> = Vec::new();
        assert!(run_repl_line(&mut defs, "1 / 0").contains("division by zero"));
    }

    #[test]
    fn a_blank_repl_line_produces_nothing() {
        let mut defs: Vec<Item> = Vec::new();
        assert_eq!(run_repl_line(&mut defs, "   "), "");
    }
}
