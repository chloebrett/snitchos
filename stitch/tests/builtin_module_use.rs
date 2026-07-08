//! `use` of a built-in stdlib module (`Str`, `List`, `Seq`) must resolve in the
//! single-program / REPL path, not only the multi-module path. Without this, a
//! `:load`ed program that opens with `use Str` loads its defs but faults with
//! "unbound variable `Str`" the moment it runs — because `build_env_in` (the
//! REPL/`eval_program` path) never linked built-in imports. Regression test.

use stitch::testing::{run_program, run_program_err};
use stitch::value::Value;

#[test]
fn use_str_resolves_in_a_single_program() {
    assert_eq!(
        run_program(r#"use Str  main() = Str.upper("hi")"#),
        Value::Str("HI".into())
    );
}

#[test]
fn use_list_resolves_in_a_single_program() {
    // The `List` module (from stim's Group-1 primitives) must be reachable too.
    assert_eq!(
        run_program("use List  main() = unwrapOr(List.at([10, 20], 1), 0)"),
        Value::Int(20)
    );
}

#[test]
fn an_unknown_use_stays_a_silent_no_op_in_the_single_program_path() {
    // There are no user modules here, so `use Whatever` can only name a built-in.
    // An unknown one must not become a hard error at env-build time — it stays a
    // no-op (as it was before built-in linking), and only an actual *use* of the
    // unbound name faults. Here nothing references it, so the program runs.
    assert_eq!(run_program("use Nope  main() = 1 + 1"), Value::Int(2));
}

#[test]
fn the_repl_load_path_resolves_a_builtin_use() {
    // The exact shell `:load` path: `load_source` accumulates defs (with
    // `use Str`), then `eval_line` runs an expression that calls into the module.
    // This is what a `:load`ed stim FSM needs — proof the shell, not just
    // `eval_program`, resolves built-in `use`.
    let mut repl = stitch::runner::Repl::new();
    repl.load_source("use Str");
    let out = repl.eval_line(r#"Str.upper("hi")"#);
    assert!(out.contains("HI"), "REPL should resolve `use Str`; got {out:?}");
}

#[test]
fn a_selection_import_of_a_builtin_is_a_safe_no_op_in_the_single_program_path() {
    // Whole-module `use Str` is linked here; a *selection* import
    // (`use Str.{upper}`) is left to the multi-module path. It must not be linked
    // eagerly (which could panic on a missing member) — it stays a no-op, so an
    // unused selection runs fine and an actual use of the name is a graceful
    // "unbound variable" fault, never a crash.
    assert_eq!(run_program("use Str.{upper}  main() = 7"), Value::Int(7));
    assert!(
        run_program_err(r#"use Str.{upper}  main() = upper("hi")"#).contains("upper"),
        "selection-imported member should be unbound, not crash"
    );
}
