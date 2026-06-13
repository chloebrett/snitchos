//! Proc-macros for the SnitchOS userspace runtime.
//!
//! Currently just `#[snitchos_user::main]`, which lets a program write a plain
//! `fn main()` instead of the `#[no_mangle] rust_main(Startup)` entry shim.

use proc_macro::{TokenStream, TokenTree};

/// Mark the program entry point. Apply to a `fn main()` returning `()`:
///
/// ```ignore
/// #[snitchos_user::main]
/// fn main() {
///     let _span = snitchos_user::tracer().span("hello");
/// }
/// ```
///
/// Generates the `#[no_mangle] rust_main(Startup)` shim the runtime calls: it
/// stashes the startup capabilities (so the free accessors
/// `snitchos_user::tracer()` / `telemetry()` can read them), then calls your
/// function. The runtime calls `exit()` after it returns, so guards (e.g. a
/// span) drop first.
#[proc_macro_attribute]
pub fn main(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // The entry's name is the ident immediately after `fn` (skipping any
    // attributes / `pub` / `async` etc. before it).
    let mut iter = item.clone().into_iter();
    let mut name = None;
    while let Some(tt) = iter.next() {
        if matches!(&tt, TokenTree::Ident(id) if id.to_string() == "fn") {
            if let Some(TokenTree::Ident(n)) = iter.next() {
                name = Some(n.to_string());
            }
            break;
        }
    }
    let name = name.expect("#[snitchos_user::main] must be applied to a function");

    let shim = format!(
        "#[unsafe(no_mangle)] pub extern \"C\" fn rust_main(__startup: ::snitchos_user::Startup) {{ \
            ::snitchos_user::__set_startup(__startup); {name}(); }}"
    );

    let mut out = item;
    out.extend(shim.parse::<TokenStream>().expect("generated entry shim parses"));
    out
}
