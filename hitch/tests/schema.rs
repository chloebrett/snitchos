//! A `TypeSchema` is the *shape* of a value — what `#[derive(Schema)]` will emit
//! for a Rust type and what the packed encoding decodes against. Its foundational
//! operation is **conformance**: does a `Value` match this shape? Conformance is
//! *structural* — `type_name`s are display labels, not part of the check, so a
//! Rust `Table` and a Stitch `Table` of the same shape conform to each other
//! (what cross-language `~>` needs). Field names/order and variant names *are*
//! structural.

use hitch::{TypeSchema, Value};

#[test]
fn scalar_schemas_accept_their_kind_and_reject_others() {
    let cases = [
        (TypeSchema::Bool, Value::Bool(true)),
        (TypeSchema::I64, Value::I64(-3)),
        (TypeSchema::U64, Value::U64(3)),
        (TypeSchema::F64, Value::F64(1.5)),
        (TypeSchema::Str, Value::Str("x".into())),
        (TypeSchema::Bytes, Value::Bytes(vec![1])),
    ];
    // Only the diagonal conforms: each schema accepts its own kind and rejects
    // every other scalar.
    for (i, (schema, _)) in cases.iter().enumerate() {
        for (j, (_, value)) in cases.iter().enumerate() {
            assert_eq!(
                schema.accepts(value),
                i == j,
                "{schema:?} vs {value:?}"
            );
        }
    }
}

#[test]
fn seq_schema_checks_every_element() {
    let schema = TypeSchema::Seq(Box::new(TypeSchema::I64));
    assert!(schema.accepts(&Value::Seq(vec![Value::I64(1), Value::I64(2)])));
    assert!(schema.accepts(&Value::Seq(vec![])), "an empty seq conforms");
    assert!(
        !schema.accepts(&Value::Seq(vec![Value::I64(1), Value::Str("x".into())])),
        "one off-type element fails the whole seq"
    );
    assert!(!schema.accepts(&Value::I64(1)), "a scalar is not a seq");
}

#[test]
fn product_schema_checks_arity_names_order_and_field_types() {
    let schema = TypeSchema::Product {
        type_name: "Reading".into(),
        fields: vec![
            (Some("name".into()), TypeSchema::Str),
            (Some("count".into()), TypeSchema::I64),
        ],
    };
    let product = |type_name: &str, fields| Value::Product {
        type_name: type_name.into(),
        fields,
    };

    assert!(schema.accepts(&product(
        "Reading",
        vec![
            (Some("name".into()), Value::Str("hot".into())),
            (Some("count".into()), Value::I64(2)),
        ],
    )));
    assert!(
        schema.accepts(&product(
            "Renamed",
            vec![
                (Some("name".into()), Value::Str("hot".into())),
                (Some("count".into()), Value::I64(2)),
            ],
        )),
        "type_name is a label, not part of structural conformance"
    );
    assert!(
        !schema.accepts(&product(
            "Reading",
            vec![
                (Some("name".into()), Value::I64(0)),
                (Some("count".into()), Value::I64(2)),
            ],
        )),
        "wrong field type"
    );
    assert!(
        !schema.accepts(&product(
            "Reading",
            vec![
                (Some("label".into()), Value::Str("hot".into())),
                (Some("count".into()), Value::I64(2)),
            ],
        )),
        "wrong field name"
    );
    assert!(
        !schema.accepts(&product(
            "Reading",
            vec![(Some("name".into()), Value::Str("hot".into()))],
        )),
        "wrong arity"
    );
}

#[test]
fn sum_schema_accepts_a_known_variant_with_a_conforming_payload() {
    let schema = TypeSchema::Sum {
        type_name: "Status".into(),
        variants: vec![
            ("Ok".into(), TypeSchema::U64),
            ("Err".into(), TypeSchema::Str),
        ],
    };
    let sum = |variant: &str, payload| Value::Sum {
        type_name: "Status".into(),
        variant: variant.into(),
        payload: Box::new(payload),
    };

    assert!(schema.accepts(&sum("Ok", Value::U64(0))));
    assert!(schema.accepts(&sum("Err", Value::Str("boom".into()))));
    assert!(
        !schema.accepts(&sum("Pending", Value::U64(0))),
        "unknown variant"
    );
    assert!(
        !schema.accepts(&sum("Ok", Value::Str("no".into()))),
        "known variant, wrong payload type"
    );
    assert!(!schema.accepts(&Value::U64(0)), "a scalar is not a sum");
}
