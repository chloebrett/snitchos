//! The **packed** encoding: positional, fixed-width little-endian bytes driven by
//! a `TypeSchema`, carrying only data — no field names, no variant names, no type
//! tags (the schema supplies all of those). It must round-trip against its schema,
//! be smaller than the self-describing form, reject values that don't conform,
//! and — the interop guarantee — be byte-identical to the `repr(C)` in-memory
//! image of the equivalent Rust type, which is what lets the POD fast-path
//! transmute it.

use hitch::{hitch, hitch_packed, unhitch_packed, TypeSchema, Value};

fn sample() -> (Value, TypeSchema) {
    let schema = TypeSchema::Product {
        type_name: "Reading".into(),
        fields: vec![
            (Some("name".into()), TypeSchema::Str),
            (
                Some("samples".into()),
                TypeSchema::Seq(Box::new(TypeSchema::I64)),
            ),
            (
                Some("status".into()),
                TypeSchema::Sum {
                    type_name: "Status".into(),
                    variants: vec![
                        ("Ok".into(), TypeSchema::U64),
                        ("Err".into(), TypeSchema::Str),
                    ],
                },
            ),
        ],
    };
    let value = Value::Product {
        type_name: "Reading".into(),
        fields: vec![
            (Some("name".into()), Value::Str("hot.avg".into())),
            (
                Some("samples".into()),
                Value::Seq(vec![Value::I64(1), Value::I64(-2)]),
            ),
            (
                Some("status".into()),
                Value::Sum {
                    type_name: "Status".into(),
                    variant: "Err".into(),
                    payload: Box::new(Value::Str("boom".into())),
                },
            ),
        ],
    };
    (value, schema)
}

#[test]
fn packed_round_trips_against_its_schema() {
    let (value, schema) = sample();
    let bytes = hitch_packed(&value, &schema).expect("a conforming value packs");
    let back = unhitch_packed(&bytes, &schema).expect("packed bytes unpack");
    assert_eq!(back, value);
}

#[test]
fn packed_is_smaller_than_self_describing() {
    let (value, schema) = sample();
    let packed = hitch_packed(&value, &schema).expect("packs");
    assert!(
        packed.len() < hitch(&value).len(),
        "packed drops names and tags, so it must be smaller"
    );
}

#[test]
fn each_scalar_packs_and_round_trips() {
    let cases = [
        (TypeSchema::Bool, Value::Bool(true)),
        (TypeSchema::Bool, Value::Bool(false)),
        (TypeSchema::I64, Value::I64(-3)),
        (TypeSchema::U64, Value::U64(42)),
        (TypeSchema::F64, Value::F64(1.5)),
        (TypeSchema::Str, Value::Str("x".into())),
        (TypeSchema::Bytes, Value::Bytes(vec![0xde, 0xad])),
    ];
    for (schema, value) in &cases {
        let bytes = hitch_packed(value, schema).expect("scalar packs");
        assert_eq!(&unhitch_packed(&bytes, schema).expect("unpacks"), value);
    }
}

#[test]
fn packing_a_non_conforming_value_errors() {
    assert!(hitch_packed(&Value::Str("x".into()), &TypeSchema::I64).is_err());
}

#[test]
fn unpacking_truncated_bytes_errors() {
    let (_, schema) = sample();
    assert!(unhitch_packed(&[0x01], &schema).is_err());
}

#[test]
fn unpacking_rejects_trailing_bytes() {
    let bytes = hitch_packed(&Value::U64(7), &TypeSchema::U64).expect("packs");
    let mut with_garbage = bytes.clone();
    with_garbage.push(0xff);
    assert_eq!(unhitch_packed(&bytes, &TypeSchema::U64).expect("clean"), Value::U64(7));
    assert!(unhitch_packed(&with_garbage, &TypeSchema::U64).is_err());
}

#[test]
fn packed_is_the_fixed_width_c_abi_image() {
    // An all-scalar, no-padding `repr(C)` struct's packed form is exactly its
    // in-memory byte image: fields contiguous, each little-endian, no varint and
    // no names. That equality is what makes the POD transmute fast-path sound.
    let schema = TypeSchema::Product {
        type_name: "Point".into(),
        fields: vec![
            (Some("x".into()), TypeSchema::I64),
            (Some("y".into()), TypeSchema::U64),
        ],
    };
    let value = Value::Product {
        type_name: "Point".into(),
        fields: vec![
            (Some("x".into()), Value::I64(-3)),
            (Some("y".into()), Value::U64(7)),
        ],
    };

    let mut image = Vec::new();
    image.extend_from_slice(&(-3i64).to_le_bytes());
    image.extend_from_slice(&7u64.to_le_bytes());

    assert_eq!(hitch_packed(&value, &schema).expect("packs"), image);
}
