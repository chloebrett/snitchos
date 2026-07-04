//! Procedural macros for the SnitchOS userspace runtime.
//!
//! One attribute: [`macro@entry`], which marks a program's entry function. It
//! hides the no_std entry tax — the `#[unsafe(no_mangle)] extern "C"` decoration
//! the runtime's `__snitchos_start` needs — so a program writes a plain
//! `fn main()`. With a manifest clause (`#[entry(in = T, out = U, uses = [..])]`)
//! it *also* emits the program's typed interface, `hitch`-encoded, into a
//! `.snitch.iface` ELF section for the seed step to lift into the
//! `user.iface` xattr.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{bracketed, parse_quote, Ident, ItemFn, Token, Type};

/// Mark the entry function of a SnitchOS userspace program.
///
/// ```ignore
/// #[snitchos_user::entry]
/// fn main() { /* ... */ }
/// ```
///
/// expands to the `#[unsafe(no_mangle)] extern "C" fn main()` the runtime crt0
/// links against. A **manifest clause** additionally externalizes the program's
/// typed `~>`-stage interface into a `.snitch.iface` ELF note:
///
/// ```ignore
/// #[snitchos_user::entry(in = Row, out = Table, uses = [FsRead, ConsoleOut])]
/// fn main() { /* ... */ }
/// ```
///
/// `in` is omitted for a source stage; `out` is required when any clause is given.
#[proc_macro_attribute]
pub fn entry(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_entry(attr.into(), item.into()).into()
}

/// The interface declared by an `#[entry(..)]` clause. Types feed
/// `<T as hitch::Schema>::SCHEMA`; `uses` are bare effect names, stringified.
struct ManifestArgs {
    input: Option<Type>,
    output: Type,
    uses: Vec<Ident>,
}

impl Parse for ManifestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut in_ty: Option<Type> = None;
        let mut out_ty: Option<Type> = None;
        let mut uses: Vec<Ident> = Vec::new();
        while !input.is_empty() {
            // `in` is a keyword, so it is matched as a token rather than an ident.
            if input.peek(Token![in]) {
                input.parse::<Token![in]>()?;
                input.parse::<Token![=]>()?;
                in_ty = Some(input.parse()?);
            } else {
                let key: Ident = input.parse()?;
                input.parse::<Token![=]>()?;
                match key.to_string().as_str() {
                    "out" => out_ty = Some(input.parse()?),
                    "uses" => {
                        let content;
                        bracketed!(content in input);
                        uses = Punctuated::<Ident, Token![,]>::parse_terminated(&content)?
                            .into_iter()
                            .collect();
                    }
                    other => {
                        return Err(syn::Error::new(
                            key.span(),
                            format!("unknown `entry` key `{other}` (expected `in`, `out`, `uses`)"),
                        ));
                    }
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else {
                break;
            }
        }
        let output =
            out_ty.ok_or_else(|| input.error("`#[entry(..)]` manifest requires `out = T`"))?;
        Ok(ManifestArgs { input: in_ty, output, uses })
    }
}

/// Token transform behind [`macro@entry`], typed over `proc_macro2` so it is
/// unit-testable (the `#[proc_macro_attribute]` entry point cannot be called
/// outside a real macro expansion).
fn expand_entry(attr: TokenStream2, item: TokenStream2) -> TokenStream2 {
    let mut func: ItemFn = match syn::parse2(item) {
        Ok(func) => func,
        Err(err) => return err.to_compile_error(),
    };
    func.sig.abi = Some(parse_quote!(extern "C"));

    // Auto-instrumentation: open a process-lifetime root span named after the
    // binary, held for the whole body. Every program is observable birth-to-death
    // on the wire even if it opens no span itself, and any span it *does* open
    // nests under this root — observability by construction, not opt-in-per-call.
    // The guard drops when `main` returns (emitting `SpanEnd`); a program that
    // `exit()`s mid-body simply never emits the close, and the kernel reclaims it.
    let root_span: syn::Stmt = parse_quote! {
        let __snitch_root_span =
            ::snitchos_user::tracer().span(::core::env!("CARGO_BIN_NAME"));
    };
    func.block.stmts.insert(0, root_span);

    let manifest = if attr.is_empty() {
        TokenStream2::new()
    } else {
        match syn::parse2::<ManifestArgs>(attr) {
            Ok(args) => manifest_items(&args),
            Err(err) => return err.to_compile_error(),
        }
    };

    quote! {
        #[unsafe(no_mangle)]
        #func

        #manifest
    }
}

/// The `const ConstManifest` + the `.snitch.iface` static for a clause.
fn manifest_items(args: &ManifestArgs) -> TokenStream2 {
    let input = match &args.input {
        Some(ty) => quote! { ::core::option::Option::Some(<#ty as hitch::Schema>::SCHEMA) },
        None => quote! { ::core::option::Option::None },
    };
    let output = &args.output;
    let uses = args.uses.iter().map(ToString::to_string);
    quote! {
        const __SNITCH_MANIFEST: hitch::ConstManifest = hitch::ConstManifest {
            input: #input,
            output: <#output as hitch::Schema>::SCHEMA,
            uses: &[ #(#uses),* ],
        };
        #[unsafe(link_section = ".snitch.iface")]
        #[used]
        static __SNITCH_IFACE: [u8; hitch::MANIFEST_BYTES] =
            hitch::encode_manifest(&__SNITCH_MANIFEST);
    }
}

#[cfg(test)]
mod tests {
    use super::expand_entry;
    use quote::quote;

    #[test]
    fn no_clause_just_decorates_main() {
        let out = expand_entry(
            quote! {},
            quote! {
                fn main() {
                    let marker = 42;
                }
            },
        )
        .to_string();

        assert!(out.contains("no_mangle"), "must export an unmangled symbol: {out}");
        assert!(out.contains("extern \"C\""), "entry must use the C ABI: {out}");
        assert!(out.contains("fn main"), "symbol must be named `main`: {out}");
        assert!(out.contains("let marker = 42"), "original body must survive: {out}");
        assert!(!out.contains("__SNITCH_IFACE"), "no manifest without a clause: {out}");
    }

    #[test]
    fn injects_a_lifetime_root_span_named_by_bin() {
        let out = expand_entry(
            quote! {},
            quote! {
                fn main() {
                    let marker = 42;
                }
            },
        )
        .to_string();

        // Every program opens a process-lifetime root span so it is observable
        // birth-to-death even if it never opens one itself — auto-instrumentation
        // by construction, not opt-in-per-call.
        assert!(out.contains("tracer"), "root span opens through the tracer: {out}");
        assert!(
            out.contains("CARGO_BIN_NAME"),
            "root span is named after the binary at compile time: {out}"
        );
        assert!(out.contains("let marker = 42"), "original body still runs: {out}");
    }

    #[test]
    fn a_manifest_clause_emits_the_note_static() {
        let out = expand_entry(
            quote! { in = Row, out = Table, uses = [FsRead, ConsoleOut] },
            quote! { fn main() {} },
        )
        .to_string();

        assert!(out.contains("__SNITCH_IFACE"), "emits the note static: {out}");
        assert!(out.contains("snitch.iface"), "into the right section: {out}");
        assert!(out.contains("encode_manifest"), "const-encodes the manifest: {out}");
        assert!(out.contains("Row"), "input type referenced: {out}");
        assert!(out.contains("Table"), "output type referenced: {out}");
        assert!(
            out.contains("FsRead") && out.contains("ConsoleOut"),
            "uses listed: {out}"
        );
    }

    #[test]
    fn a_source_clause_has_no_input() {
        let out = expand_entry(quote! { out = Table }, quote! { fn main() {} }).to_string();
        assert!(out.contains("Option :: None"), "source stage has no input: {out}");
        assert!(out.contains("__SNITCH_IFACE"), "still emits the note: {out}");
    }
}
