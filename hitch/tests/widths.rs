//! Fixed-width integer and `f32` schemas. `Value` stays 64-bit (the dynamic
//! carrier, like Stitch's `Int`/`Float`); the *schema* decides the wire width, so
//! a `U64` value packs to 1/2/4/8 bytes depending on whether its schema is
//! `U8`/`U16`/`U32`/`U64`. That width is what makes a packed Hitch value
//! byte-match a real C-ABI struct.

use hitch::{hitch_packed, unhitch_packed, Schema, TypeSchema, Value};

#[test]
fn narrow_schemas_accept_in_range_and_reject_out_of_range_or_wrong_sign() {
    assert!(TypeSchema::U8.accepts(&Value::U64(255)));
    assert!(!TypeSchema::U8.accepts(&Value::U64(256)));
    assert!(TypeSchema::U16.accepts(&Value::U64(65_535)));
    assert!(!TypeSchema::U16.accepts(&Value::U64(65_536)));
    assert!(TypeSchema::U32.accepts(&Value::U64(4_294_967_295)));
    assert!(!TypeSchema::U32.accepts(&Value::U64(4_294_967_296)));

    assert!(TypeSchema::I8.accepts(&Value::I64(-128)));
    assert!(TypeSchema::I8.accepts(&Value::I64(127)));
    assert!(!TypeSchema::I8.accepts(&Value::I64(128)));
    assert!(!TypeSchema::I8.accepts(&Value::I64(-129)));
    assert!(TypeSchema::I16.accepts(&Value::I64(32_767)));
    assert!(!TypeSchema::I16.accepts(&Value::I64(32_768)));
    assert!(TypeSchema::I32.accepts(&Value::I64(2_147_483_647)));
    assert!(!TypeSchema::I32.accepts(&Value::I64(2_147_483_648)));

    // Sign is part of the kind: an unsigned schema rejects a signed value and
    // vice versa.
    assert!(!TypeSchema::U8.accepts(&Value::I64(1)));
    assert!(!TypeSchema::I8.accepts(&Value::U64(1)));
}

#[test]
fn a_narrow_value_packs_to_its_width_and_round_trips() {
    let cases = [
        (TypeSchema::U8, Value::U64(200), 1usize),
        (TypeSchema::U16, Value::U64(40_000), 2),
        (TypeSchema::U32, Value::U64(3_000_000_000), 4),
        (TypeSchema::I8, Value::I64(-100), 1),
        (TypeSchema::I8, Value::I64(100), 1),
        (TypeSchema::I16, Value::I64(-30_000), 2),
        (TypeSchema::I16, Value::I64(30_000), 2),
        (TypeSchema::I32, Value::I64(-2_000_000_000), 4),
        (TypeSchema::I32, Value::I64(2_000_000_000), 4),
        (TypeSchema::F32, Value::F64(1.5), 4),
    ];
    for (schema, value, width) in &cases {
        let bytes = hitch_packed(value, schema).expect("packs");
        assert_eq!(bytes.len(), *width, "{schema:?} packs to {width} bytes");
        assert_eq!(&unhitch_packed(&bytes, schema).expect("unpacks"), value);
    }
}

#[test]
fn packing_an_out_of_range_value_errors() {
    assert!(hitch_packed(&Value::U64(256), &TypeSchema::U8).is_err());
    assert!(hitch_packed(&Value::I64(128), &TypeSchema::I8).is_err());
}

#[test]
fn primitive_width_types_report_their_schema() {
    assert_eq!(u8::schema(), TypeSchema::U8);
    assert_eq!(u16::schema(), TypeSchema::U16);
    assert_eq!(u32::schema(), TypeSchema::U32);
    assert_eq!(i8::schema(), TypeSchema::I8);
    assert_eq!(i16::schema(), TypeSchema::I16);
    assert_eq!(i32::schema(), TypeSchema::I32);
    assert_eq!(f32::schema(), TypeSchema::F32);
}

#[cfg(feature = "derive")]
#[test]
fn a_c_abi_struct_packs_to_its_repr_c_image() {
    // The payoff: a `CapDesc`-shaped struct derives a schema whose packed encoding
    // is byte-for-byte the struct's `repr(C)` image — the precondition that makes
    // the (future) POD transmute fast-path sound.
    #[derive(Schema)]
    #[allow(dead_code, reason = "reflected via ::schema(), never instantiated")]
    struct CapDesc {
        handle: u32,
        kind: u32,
        rights: u32,
        reserved: u32,
        badge: u64,
    }

    let value = Value::Product {
        type_name: "CapDesc".into(),
        fields: vec![
            (Some("handle".into()), Value::U64(7)),
            (Some("kind".into()), Value::U64(2)),
            (Some("rights".into()), Value::U64(0b101)),
            (Some("reserved".into()), Value::U64(0)),
            (Some("badge".into()), Value::U64(0xdead_beef)),
        ],
    };
    let bytes = hitch_packed(&value, &CapDesc::schema()).expect("packs");

    let mut image = Vec::new();
    for v in [7u32, 2, 0b101, 0] {
        image.extend_from_slice(&v.to_le_bytes());
    }
    image.extend_from_slice(&0xdead_beef_u64.to_le_bytes());

    assert_eq!(bytes, image, "packed == the repr(C) image");
    assert_eq!(bytes.len(), 24);
}
