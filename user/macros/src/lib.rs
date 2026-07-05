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
use syn::{bracketed, parenthesized, parse_quote, Expr, Ident, ItemFn, LitStr, Token, Type};

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
/// `<T as hitch::Schema>::SCHEMA`; `needs` are typed authority slots. `output` is
/// present only for a `~>` **stage** (`in`→`out`); a program that just declares
/// `needs` has neither, and emits no stage-interface note.
struct ManifestArgs {
    input: Option<Type>,
    output: Option<Type>,
    needs: Vec<SlotArg>,
}

/// One `needs` entry: `("role", ObjectKind, rights_expr)` — e.g.
/// `("fs", ENDPOINT, SEND)`. `object` is an `abi::object_kind` constant name and
/// `rights` an expression over `abi::rights` constants; both are reached through the
/// runtime's re-exports at emit time (`::snitchos_user::object_kind` / `::rights`).
struct SlotArg {
    name: LitStr,
    object: Ident,
    rights: Expr,
}

impl Parse for SlotArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let name: LitStr = content.parse()?;
        content.parse::<Token![,]>()?;
        let object: Ident = content.parse()?;
        content.parse::<Token![,]>()?;
        let rights: Expr = content.parse()?;
        Ok(SlotArg { name, object, rights })
    }
}

impl Parse for ManifestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut in_ty: Option<Type> = None;
        let mut out_ty: Option<Type> = None;
        let mut needs: Vec<SlotArg> = Vec::new();
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
                    "needs" => {
                        let content;
                        bracketed!(content in input);
                        needs = Punctuated::<SlotArg, Token![,]>::parse_terminated(&content)?
                            .into_iter()
                            .collect();
                    }
                    other => {
                        return Err(syn::Error::new(
                            key.span(),
                            format!("unknown `entry` key `{other}` (expected `in`, `out`, `needs`)"),
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
        // A stage with a typed input must declare its output; `needs`-only (no
        // stage interface) is fine and emits no note.
        if in_ty.is_some() && out_ty.is_none() {
            return Err(input.error("`#[entry(in = T, ..)]` (a stage) requires `out = U`"));
        }
        Ok(ManifestArgs { input: in_ty, output: out_ty, needs })
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

    // Publish the program's `#[entry(needs)]` slot table (the `__SNITCH_SLOTS` const
    // emitted below, for every program) so `bootstrap().get(name)` can resolve role
    // names → delegated handles.
    let register_slots: syn::Stmt = parse_quote! {
        ::snitchos_user::__register_slots(__SNITCH_SLOTS);
    };
    func.block.stmts.insert(0, register_slots);

    // Parse the manifest clause once (empty attr = a bare `#[entry]`, no needs).
    let args = if attr.is_empty() {
        None
    } else {
        match syn::parse2::<ManifestArgs>(attr) {
            Ok(args) => Some(args),
            Err(err) => return err.to_compile_error(),
        }
    };

    // The `.snitch.iface` note (satisfier-facing) is emitted for any manifest
    // clause — its `needs` are what a satisfier reads to grant authority. A bare
    // `#[entry]` (no clause) has no note. The `in`/`out` stage interface is the
    // optional part (a `needs`-only program declares neither).
    let manifest = args.as_ref().map_or_else(TokenStream2::new, manifest_items);
    // The `__SNITCH_SLOTS` name→object table (program-facing) is emitted for *every*
    // program — empty when there are no needs — so the runtime can resolve
    // `bootstrap().get(name)` unconditionally.
    let needs: &[SlotArg] = args.as_ref().map_or(&[], |a| a.needs.as_slice());
    let slots = slots_table(needs);

    quote! {
        #[unsafe(no_mangle)]
        #func

        #manifest
        #slots
    }
}

/// The `__SNITCH_SLOTS` name→object table, in declaration order — the runtime
/// resolves `bootstrap().get(name)` against this (a role name → its handle index,
/// `delegated_handle(index)`) without parsing the ELF note. Emitted for every
/// program (empty when there are no needs); consumed by the runtime bootstrap
/// accessor in a later increment.
fn slots_table(needs: &[SlotArg]) -> TokenStream2 {
    let entries = needs.iter().map(|s| {
        let name = &s.name;
        let object = &s.object;
        quote! { (#name, ::snitchos_user::object_kind::#object as u8) }
    });
    quote! {
        const __SNITCH_SLOTS: &[(&str, u8)] = &[ #(#entries),* ];
    }
}

/// The `const ConstManifest` + the `.snitch.iface` static for a clause.
fn manifest_items(args: &ManifestArgs) -> TokenStream2 {
    let input = match &args.input {
        Some(ty) => quote! { ::core::option::Option::Some(<#ty as hitch::Schema>::SCHEMA) },
        None => quote! { ::core::option::Option::None },
    };
    let output = match &args.output {
        Some(ty) => quote! { ::core::option::Option::Some(<#ty as hitch::Schema>::SCHEMA) },
        None => quote! { ::core::option::Option::None },
    };
    let slots = args.needs.iter().map(|s| {
        let name = &s.name;
        let object = &s.object;
        let rights = &s.rights;
        quote! {
            hitch::ConstSlot {
                name: #name,
                object: ::snitchos_user::object_kind::#object as u8,
                rights: { use ::snitchos_user::rights::*; #rights },
            }
        }
    });
    quote! {
        const __SNITCH_MANIFEST: hitch::ConstManifest = hitch::ConstManifest {
            input: #input,
            output: #output,
            needs: &[ #(#slots),* ],
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
            quote! { in = Row, out = Table, needs = [("fs", ENDPOINT, SEND)] },
            quote! { fn main() {} },
        )
        .to_string();

        assert!(out.contains("__SNITCH_IFACE"), "emits the note static: {out}");
        assert!(out.contains("snitch.iface"), "into the right section: {out}");
        assert!(out.contains("encode_manifest"), "const-encodes the manifest: {out}");
        assert!(out.contains("Row"), "input type referenced: {out}");
        assert!(out.contains("Table"), "output type referenced: {out}");
        // The typed slot: a `ConstSlot` naming the role, its object kind, and rights.
        assert!(out.contains("ConstSlot"), "emits a typed slot: {out}");
        assert!(out.contains("\"fs\""), "slot role name listed: {out}");
        assert!(out.contains("ENDPOINT"), "slot object kind listed: {out}");
        assert!(out.contains("SEND"), "slot rights listed: {out}");
    }

    #[test]
    fn a_source_clause_has_no_input() {
        let out = expand_entry(quote! { out = Table }, quote! { fn main() {} }).to_string();
        assert!(out.contains("Option :: None"), "source stage has no input: {out}");
        assert!(out.contains("__SNITCH_IFACE"), "still emits the note: {out}");
    }

    #[test]
    fn emits_a_slots_table_in_declaration_order() {
        // The runtime-facing half of manifest-as-index: a compile-time table mapping
        // each declared role name to its handle index (declaration order), so
        // `bootstrap().get("fs")` resolves a name without parsing the ELF note.
        let out = expand_entry(
            quote! { out = Table, needs = [("fs", ENDPOINT, SEND), ("log", ENDPOINT, RECV)] },
            quote! { fn main() {} },
        )
        .to_string();

        assert!(out.contains("__SNITCH_SLOTS"), "emits the slot table: {out}");
        let fs_at = out.find("\"fs\"").expect("fs role in the table");
        let log_at = out.find("\"log\"").expect("log role in the table");
        assert!(fs_at < log_at, "roles listed in declaration order (fs before log): {out}");
    }

    #[test]
    fn no_needs_clause_emits_an_empty_slots_table() {
        // Every program emits the table — empty for a bare `#[entry]` — so the
        // runtime can reference `__SNITCH_SLOTS` unconditionally (Increment 3 relies
        // on the symbol always existing).
        let out = expand_entry(quote! {}, quote! { fn main() {} }).to_string();
        assert!(out.contains("__SNITCH_SLOTS"), "still emits an (empty) slot table: {out}");
    }

    #[test]
    fn needs_without_out_still_emits_a_note_carrying_the_needs() {
        // A needs-only program (no `~>` stage interface) still emits the note — so a
        // satisfier can read its required authorities off the `user.iface` xattr —
        // it just has no `out` type. The runtime-facing slot table is emitted too.
        let out = expand_entry(
            quote! { needs = [("fs", ENDPOINT, SEND)] },
            quote! { fn main() {} },
        )
        .to_string();
        assert!(out.contains("__SNITCH_IFACE"), "emits the note (needs are satisfier-facing): {out}");
        assert!(out.contains("__SNITCH_SLOTS"), "emits the slot table: {out}");
        assert!(out.contains("\"fs\""), "lists the fs role: {out}");
    }
}
