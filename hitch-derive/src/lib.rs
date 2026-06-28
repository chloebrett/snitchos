//! `#[derive(Schema)]` for [`hitch::Schema`]: reflect a Rust struct or enum into
//! its `hitch::TypeSchema` at compile time, recursing into field types.
//!
//! - a **struct** becomes a `Product` of its fields (named → `Some(name)`,
//!   tuple → `None`, unit → empty);
//! - an **enum** becomes a `Sum`, each variant carrying a `Product` of *its*
//!   fields — matching how the Stitch bridge represents a sum variant.
//!
//! Generated code names everything by absolute path (`hitch::…`,
//! `hitch::__private::…`), so the consumer needs nothing in scope. v1 reflects
//! non-generic types over `Schema` field types (the 64-bit scalar set + nested
//! derived types); generics and `Vec`/collection fields are not yet handled.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

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
                quote! { (#variant_name.into(), #payload) }
            });
            let variants = collect_vec(variants);
            quote! {
                hitch::TypeSchema::Sum {
                    type_name: #name_str.into(),
                    variants: #variants,
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
            fn schema() -> hitch::TypeSchema {
                #body
            }
        }
    }
    .into()
}

/// `hitch::TypeSchema::Product { type_name, fields }` for a set of fields.
fn product_expr(type_name: &str, fields: &Fields) -> TokenStream2 {
    let entries = match fields {
        Fields::Named(named) => named
            .named
            .iter()
            .map(|field| {
                let label = field.ident.as_ref().expect("named field has an ident").to_string();
                let ty = &field.ty;
                quote! { (Some(#label.into()), <#ty as hitch::Schema>::schema()) }
            })
            .collect::<Vec<_>>(),
        Fields::Unnamed(unnamed) => unnamed
            .unnamed
            .iter()
            .map(|field| {
                let ty = &field.ty;
                quote! { (None, <#ty as hitch::Schema>::schema()) }
            })
            .collect::<Vec<_>>(),
        Fields::Unit => Vec::new(),
    };
    let fields = collect_vec(entries.into_iter());
    quote! {
        hitch::TypeSchema::Product {
            type_name: #type_name.into(),
            fields: #fields,
        }
    }
}

/// Build a `Vec` from the entry expressions. An empty set must use `Vec::new()`
/// (an empty `Vec::from([])` can't infer its element type).
fn collect_vec(entries: impl Iterator<Item = TokenStream2>) -> TokenStream2 {
    let entries: Vec<_> = entries.collect();
    if entries.is_empty() {
        quote! { hitch::__private::Vec::new() }
    } else {
        quote! { hitch::__private::Vec::from([ #(#entries),* ]) }
    }
}
