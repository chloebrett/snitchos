//! That the canon libraries are *importable* — their `ext` surface reachable
//! from another module by qualified name.
//!
//! This file used to hold the libraries' behavioural assertions, written in Rust
//! against `format!`-assembled driver modules. Those now live in the `.st` files
//! themselves as native `test` declarations (see
//! `docs/stitch-testing-design.md`), where they get diagnostics, type checking,
//! and a printer — and where they run on the metal.
//!
//! What did *not* move is the axis this file is now reduced to. A native test
//! sits *inside* its module and calls `summarise(…)` unqualified, so it says
//! nothing about whether `ext` actually exported anything, nor whether
//! `stats.summarise` resolves from a *different* module. Nothing else covers
//! that: `builtin_module_use.rs` tests the built-in `Str`/`List` modules, and no
//! canon program imports these two. Deleting the file wholesale would have
//! retired real coverage of the export boundary while every remaining test
//! stayed green.
//!
//! So: one import smoke test per library, asserting the boundary and nothing
//! about the behaviour behind it.

use stitch::testing::run_modules;
use stitch::value::Value;

/// `ext` on `summarise`/`Summary` means another module can reach them by path,
/// and that the product's `ext` fields are readable from outside.
#[test]
fn stats_exports_are_reachable_from_another_module() {
    let stats = read("fs-image/lib/stats.st");
    let driver = "use stats\n\nmain() = stats.summarise([3, 1, 4]).count";

    assert_eq!(run_modules(&[("stats", &stats), ("driver", driver)], "driver"), Value::Int(3));
}

/// Same boundary for `text`, whose exports are plain functions over `Str`.
#[test]
fn text_exports_are_reachable_from_another_module() {
    let text = read("fs-image/lib/text.st");
    let driver = "use text\n\nmain() = text.pad(\"ok\", 4)";

    assert_eq!(
        run_modules(&[("text", &text), ("driver", driver)], "driver"),
        Value::Str("ok  ".into())
    );
}

fn read(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stitch/ has a parent")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()))
}
