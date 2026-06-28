//! `#[derive(Schema)]` for [`hitch::Schema`]: reflect a Rust struct or enum into
//! its const `hitch::ConstSchema` at compile time (an associated `const SCHEMA`),
//! recursing into field types.
//!
//! - a **struct** becomes a `Product` of its fields (named → `Some(name)`,
//!   tuple → `None`, unit → empty);
//! - an **enum** becomes a `Sum`, each variant carrying a `Product` of *its*
//!   fields — matching how the Stitch bridge represents a sum variant.
//!
//! The schema is a `const` (built from `&'static str` / slice literals), so it can
//! live in a `#[link_section]` static. Generated code names everything by absolute
//! path (`hitch::…`), so the consumer needs nothing in scope. v1 reflects
//! non-generic types over `Schema` field types (the scalar set + nested derived
//! types); generics and `Vec`/collection fields are not yet handled.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Attribute, Data, DeriveInput, Fields};

#[proc_macro_derive(Schema)]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    if let Some(param) = input.generics.params.first() {
        return syn::Error::new_spanned(param, "#[derive(Schema)] does not support generics yet")
            .to_compile_error()
            .into();
    }

    let name = &input.ident;
    let name_str = name.to_string();

    let body = match &input.data {
        Data::Struct(data) => product_expr(&name_str, &data.fields),
        Data::Enum(data) => {
            let variants = data.variants.iter().map(|variant| {
                let variant_name = variant.ident.to_string();
                // The variant's payload is the Product of its fields, labelled
                // with the enum's type name (the bridge does the same).
                let payload = product_expr(&name_str, &variant.fields);
                quote! { (#variant_name, #payload) }
            });
            quote! {
                hitch::ConstSchema::Sum {
                    type_name: #name_str,
                    variants: &[ #(#variants),* ],
                }
            }
        }
        Data::Union(_) => {
            return syn::Error::new_spanned(name, "#[derive(Schema)] does not support unions")
                .to_compile_error()
                .into();
        }
    };

    quote! {
        impl hitch::Schema for #name {
            const SCHEMA: hitch::ConstSchema = #body;
        }
    }
    .into()
}

/// `#[derive(Pod)]` for `hitch_pod::Pod`: generate the `unsafe impl` only when the
/// type is provably safe to reinterpret as bytes. The generated code compile-time
/// checks all three obligations, so the `unsafe` is gated, not trusted:
///
/// - **`#[repr(C)]`** (and not `packed`) — checked by reading the attribute;
/// - **every field is `Pod`** — `__assert_pod::<FieldTy>()` fails to resolve for a
///   pointer, reference, `String`, `bool`, etc.;
/// - **no padding** — a `const` assertion that `size_of::<T>()` equals the sum of
///   the field sizes (padding would make the whole larger).
///
/// Only non-generic structs qualify; enums and unions are rejected.
#[proc_macro_derive(Pod)]
pub fn derive_pod(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    if let Some(param) = input.generics.params.first() {
        return syn::Error::new_spanned(param, "#[derive(Pod)] does not support generics")
            .to_compile_error()
            .into();
    }
    let Data::Struct(data) = &input.data else {
        return syn::Error::new_spanned(name, "#[derive(Pod)] supports only structs")
            .to_compile_error()
            .into();
    };
    if !is_repr_c(&input.attrs) {
        return syn::Error::new_spanned(
            name,
            "#[derive(Pod)] requires #[repr(C)] (and not `packed`)",
        )
        .to_compile_error()
        .into();
    }

    let field_types = data.fields.iter().map(|field| &field.ty).collect::<Vec<_>>();
    quote! {
        // SAFETY: the `const` block below proves repr(C), all-fields-Pod, and
        // no-padding at compile time; if any fails, this item does not compile.
        unsafe impl hitch_pod::Pod for #name {}
        const _: () = {
            const fn __assert_pod<__T: hitch_pod::Pod>() {}
            #( __assert_pod::<#field_types>(); )*
            ::core::assert!(
                ::core::mem::size_of::<#name>()
                    == 0 #( + ::core::mem::size_of::<#field_types>() )*,
                "#[derive(Pod)] requires a type with no padding",
            );
        };
    }
    .into()
}

/// Is `#[repr(C)]` present and `packed` absent? (`packed` would make field
/// references unaligned, unsound to expose.)
fn is_repr_c(attrs: &[Attribute]) -> bool {
    let mut has_c = false;
    let mut packed = false;
    for attr in attrs {
        if attr.path().is_ident("repr") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("C") {
                    has_c = true;
                }
                if meta.path.is_ident("packed") {
                    packed = true;
                }
                Ok(())
            });
        }
    }
    has_c && !packed
}

/// A const `hitch::ConstSchema::Product { type_name, fields }` for a set of
/// fields. Fields are a `&'static [..]` slice literal — const-constructible, so
/// the whole schema is a `const` (unlike the runtime `Vec`-based `TypeSchema`).
fn product_expr(type_name: &str, fields: &Fields) -> TokenStream2 {
    let entries = match fields {
        Fields::Named(named) => named
            .named
            .iter()
            .map(|field| {
                let label = field.ident.as_ref().expect("named field has an ident").to_string();
                let ty = &field.ty;
                quote! { (Some(#label), <#ty as hitch::Schema>::SCHEMA) }
            })
            .collect::<Vec<_>>(),
        Fields::Unnamed(unnamed) => unnamed
            .unnamed
            .iter()
            .map(|field| {
                let ty = &field.ty;
                quote! { (None, <#ty as hitch::Schema>::SCHEMA) }
            })
            .collect::<Vec<_>>(),
        Fields::Unit => Vec::new(),
    };
    quote! {
        hitch::ConstSchema::Product {
            type_name: #type_name,
            fields: &[ #(#entries),* ],
        }
    }
}
