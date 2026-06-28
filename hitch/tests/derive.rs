//! `#[derive(Schema)]` reflects a Rust struct or enum into its `TypeSchema`,
//! recursing into field types. A struct becomes a `Product`; an enum becomes a
//! `Sum` whose every variant carries a `Product` of its fields (matching how the
//! Stitch bridge represents a sum variant). Field names and order are preserved;
//! a tuple field is positional (`None`).

#![cfg(feature = "derive")]
#![allow(
    dead_code,
    reason = "fixtures exist only to be reflected by #[derive(Schema)] via ::schema(); they are never instantiated"
)]

use hitch::{Schema, TypeSchema};

#[derive(Schema)]
struct Point {
    x: i64,
    y: u64,
}

#[derive(Schema)]
struct Pair(i64, bool);

#[derive(Schema)]
struct Nothing;

#[derive(Schema)]
struct Nested {
    point: Point,
    flag: bool,
}

#[derive(Schema)]
enum Status {
    Pending,
    Ok(u64),
    Err { code: i64, msg: String },
}

#[test]
fn struct_derives_a_product_of_its_fields() {
    assert_eq!(
        Point::schema(),
        TypeSchema::Product {
            type_name: "Point".into(),
            fields: vec![
                (Some("x".into()), TypeSchema::I64),
                (Some("y".into()), TypeSchema::U64),
            ],
        }
    );
}

#[test]
fn tuple_struct_fields_are_positional() {
    assert_eq!(
        Pair::schema(),
        TypeSchema::Product {
            type_name: "Pair".into(),
            fields: vec![(None, TypeSchema::I64), (None, TypeSchema::Bool)],
        }
    );
}

#[test]
fn unit_struct_is_an_empty_product() {
    assert_eq!(
        Nothing::schema(),
        TypeSchema::Product {
            type_name: "Nothing".into(),
            fields: vec![],
        }
    );
}

#[test]
fn nested_types_recurse() {
    let TypeSchema::Product { fields, .. } = Nested::schema() else {
        panic!("a struct derives a Product");
    };
    assert_eq!(fields[0].1, Point::schema());
    assert_eq!(fields[1].1, TypeSchema::Bool);
}

#[test]
fn enum_derives_a_sum_with_product_payloads() {
    let product = |fields| TypeSchema::Product {
        type_name: "Status".into(),
        fields,
    };
    assert_eq!(
        Status::schema(),
        TypeSchema::Sum {
            type_name: "Status".into(),
            variants: vec![
                ("Pending".into(), product(vec![])),
                ("Ok".into(), product(vec![(None, TypeSchema::U64)])),
                (
                    "Err".into(),
                    product(vec![
                        (Some("code".into()), TypeSchema::I64),
                        (Some("msg".into()), TypeSchema::Str),
                    ])
                ),
            ],
        }
    );
}
