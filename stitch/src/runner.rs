//! The `.st` program runner: parse a source program, run it, and render the
//! result and telemetry into stdout/stderr text + an exit code. The logic lives
//! here (host-testable); `main.rs` is a thin wiring layer over it.

use std::fmt::Write as _;

use crate::ast::Item;
use crate::interp::eval_program_with_telemetry;
use crate::parser::{parse, parse_program};
use crate::value::{TelemetryEvent, Value};

/// Writing to a `String` never fails; this names that contract at each `write!`.
const INFALLIBLE: &str = "writing to a String is infallible";

/// The outcome of running a program: what to print where, and the exit code.
#[derive(Debug)]
pub struct RunResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Parse and run a Stitch program. Telemetry (and `main`'s non-unit result) go
/// to stdout; a parse error (exit 2) or runtime error (exit 1) goes to stderr.
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
    let (result, events) = eval_program_with_telemetry(&items);
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
    use crate::runner::run_program_source;

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
