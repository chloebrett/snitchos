//! Procedural macros for the SnitchOS userspace runtime.
//!
//! Currently one attribute: [`macro@entry`], which marks a program's entry
//! function. It hides the no_std entry tax — the `#[unsafe(no_mangle)]
//! extern "C"` decoration the runtime's `__snitchos_start` needs to find and
//! call — so a program writes a plain `fn main()`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{ItemFn, parse_quote};

/// Mark the entry function of a SnitchOS userspace program.
///
/// ```ignore
/// #[snitchos_user::entry]
/// fn main() {
///     // ...
/// }
/// ```
///
/// expands to the `#[unsafe(no_mangle)] extern "C" fn main()` that the runtime
/// crt0 (`start.S` → `__snitchos_start`) links against and calls. The program
/// keeps writing a normal `fn main`; the ABI plumbing is the macro's job.
#[proc_macro_attribute]
pub fn entry(_attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_entry(item.into()).into()
}

/// Token transform behind [`macro@entry`], typed over `proc_macro2` so it is
/// unit-testable (the `#[proc_macro_attribute]` entry point above cannot be
/// called outside a real macro expansion).
fn expand_entry(item: TokenStream2) -> TokenStream2 {
    let mut func: ItemFn = match syn::parse2(item) {
        Ok(func) => func,
        Err(err) => return err.to_compile_error(),
    };
    func.sig.abi = Some(parse_quote!(extern "C"));
    quote! {
        #[unsafe(no_mangle)]
        #func
    }
}

#[cfg(test)]
mod tests {
    use super::expand_entry;
    use quote::quote;

    #[test]
    fn gives_main_a_no_mangle_extern_c_abi_and_keeps_the_body() {
        let out = expand_entry(quote! {
            fn main() {
                let marker = 42;
            }
        })
        .to_string();

        assert!(out.contains("no_mangle"), "must export an unmangled symbol: {out}");
        assert!(out.contains("extern \"C\""), "entry must use the C ABI: {out}");
        assert!(out.contains("fn main"), "symbol must be named `main`: {out}");
        assert!(out.contains("let marker = 42"), "original body must survive: {out}");
    }
}
