//! `TypeSchema::compatible` — the `~>` gate: can a value produced against `self`
//! (a stage's `out`) be consumed against `other` (the next stage's `in`)? v1 is
//! **structural equality modulo `type_name`** (display-only) — the same structural
//! rule as `accepts`: field names/order and variant names/order matter; the type
//! label does not. So a Rust `Table` and a Stitch `Table` of the same shape are
//! compatible, which is what cross-language `~>` needs.

use hitch::TypeSchema;

fn product(type_name: &str, fields: Vec<(Option<String>, TypeSchema)>) -> TypeSchema {
    TypeSchema::Product { type_name: type_name.into(), fields }
}

#[test]
fn identical_scalars_are_compatible_distinct_ones_are_not() {
    assert!(TypeSchema::I64.compatible(&TypeSchema::I64));
    assert!(TypeSchema::Str.compatible(&TypeSchema::Str));
    assert!(!TypeSchema::I64.compatible(&TypeSchema::U64));
    // Narrow widths are distinct shapes — a U32 packing is not a U64 packing.
    assert!(!TypeSchema::U32.compatible(&TypeSchema::U64));
}

#[test]
fn a_seq_is_compatible_when_its_element_is() {
    let ints = TypeSchema::Seq(Box::new(TypeSchema::I64));
    let more_ints = TypeSchema::Seq(Box::new(TypeSchema::I64));
    let strs = TypeSchema::Seq(Box::new(TypeSchema::Str));
    assert!(ints.compatible(&more_ints));
    assert!(!ints.compatible(&strs));
}

#[test]
fn products_are_compatible_ignoring_type_name() {
    // Same shape, different type label → compatible (the cross-language property).
    let table = product("Table", vec![(Some("n".into()), TypeSchema::I64)]);
    let grid = product("Grid", vec![(Some("n".into()), TypeSchema::I64)]);
    assert!(table.compatible(&grid));
}

#[test]
fn products_differing_in_field_name_type_or_arity_are_not_compatible() {
    let base = product("T", vec![(Some("n".into()), TypeSchema::I64)]);
    assert!(!base.compatible(&product("T", vec![(Some("m".into()), TypeSchema::I64)]))); // name
    assert!(!base.compatible(&product("T", vec![(Some("n".into()), TypeSchema::Str)]))); // type
    assert!(!base.compatible(&product("T", vec![]))); // arity
}

#[test]
fn product_field_order_matters() {
    let ab = product(
        "T",
        vec![(Some("a".into()), TypeSchema::I64), (Some("b".into()), TypeSchema::Str)],
    );
    let ba = product(
        "T",
        vec![(Some("b".into()), TypeSchema::Str), (Some("a".into()), TypeSchema::I64)],
    );
    assert!(!ab.compatible(&ba));
}

#[test]
fn sums_ignore_type_name_but_not_variant_names() {
    let sum = |type_name: &str, variant: &str| TypeSchema::Sum {
        type_name: type_name.into(),
        variants: vec![(variant.into(), product(type_name, vec![]))],
    };
    assert!(sum("Shape", "Circle").compatible(&sum("Form", "Circle"))); // type_name ignored
    assert!(!sum("Shape", "Circle").compatible(&sum("Shape", "Square"))); // variant name matters
}

#[test]
fn different_kinds_are_not_compatible() {
    assert!(!TypeSchema::I64.compatible(&TypeSchema::Seq(Box::new(TypeSchema::I64))));
    assert!(
        !product("T", vec![])
            .compatible(&TypeSchema::Sum { type_name: "T".into(), variants: vec![] })
    );
}
