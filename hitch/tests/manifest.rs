//! A `ConstManifest` (input/output `ConstSchema` + `uses`) encodes to a fixed-size
//! byte note **in a `const`** — that's the whole point: the bytes can live in a
//! `#[link_section]` static. The host seed step then `decode_manifest`s them back
//! into a runtime `Manifest`. Round-trip through that encoding is the contract.

use hitch::{decode_manifest, encode_manifest, ConstManifest, ConstSchema, Manifest, TypeSchema};

#[test]
fn a_manifest_round_trips_through_the_note_encoding() {
    const M: ConstManifest = ConstManifest {
        input: Some(ConstSchema::Product {
            type_name: "Row",
            fields: &[(Some("name"), ConstSchema::Str), (Some("n"), ConstSchema::U32)],
        }),
        output: ConstSchema::Seq(&ConstSchema::U64),
        uses: &["FsRead", "ConsoleOut"],
    };
    // The headline: encoding happens in a `const`, so these bytes can be a static.
    const BYTES: [u8; hitch::MANIFEST_BYTES] = encode_manifest(&M);

    assert_eq!(
        decode_manifest(&BYTES).expect("decodes"),
        Manifest {
            input: Some(TypeSchema::Product {
                type_name: "Row".into(),
                fields: vec![
                    (Some("name".into()), TypeSchema::Str),
                    (Some("n".into()), TypeSchema::U32),
                ],
            }),
            output: TypeSchema::Seq(Box::new(TypeSchema::U64)),
            uses: vec!["FsRead".into(), "ConsoleOut".into()],
        }
    );
}

#[test]
fn every_scalar_kind_round_trips_in_a_manifest() {
    const M: ConstManifest = ConstManifest {
        input: None,
        output: ConstSchema::Product {
            type_name: "All",
            fields: &[
                (Some("bool"), ConstSchema::Bool),
                (Some("i8"), ConstSchema::I8),
                (Some("i16"), ConstSchema::I16),
                (Some("i32"), ConstSchema::I32),
                (Some("i64"), ConstSchema::I64),
                (Some("u8"), ConstSchema::U8),
                (Some("u16"), ConstSchema::U16),
                (Some("u32"), ConstSchema::U32),
                (Some("u64"), ConstSchema::U64),
                (Some("f32"), ConstSchema::F32),
                (Some("f64"), ConstSchema::F64),
                (Some("str"), ConstSchema::Str),
                (Some("bytes"), ConstSchema::Bytes),
            ],
        },
        uses: &[],
    };
    let decoded = decode_manifest(&encode_manifest(&M)).expect("decodes");
    let TypeSchema::Product { fields, .. } = decoded.output else {
        panic!("an all-scalar product");
    };
    let kinds: Vec<TypeSchema> = fields.into_iter().map(|(_, s)| s).collect();
    assert_eq!(
        kinds,
        vec![
            TypeSchema::Bool,
            TypeSchema::I8,
            TypeSchema::I16,
            TypeSchema::I32,
            TypeSchema::I64,
            TypeSchema::U8,
            TypeSchema::U16,
            TypeSchema::U32,
            TypeSchema::U64,
            TypeSchema::F32,
            TypeSchema::F64,
            TypeSchema::Str,
            TypeSchema::Bytes,
        ]
    );
}

#[test]
fn the_note_byte_layout_is_exact() {
    // Pin the encoding so any arithmetic slip in the const writers (offsets,
    // lengths, the payload-length prefix) changes the bytes and is caught.
    const M: ConstManifest = ConstManifest {
        input: None,
        output: ConstSchema::U8,
        uses: &["X"],
    };
    let bytes = encode_manifest(&M);

    let payload: &[u8] = &[
        0x00, // input: None
        0x05, // output: U8 (tag 5)
        0x01, 0x00, 0x00, 0x00, // uses count = 1
        0x01, 0x00, 0x00, 0x00, // use[0] length = 1
        b'X', // "X"
    ];
    assert_eq!(&bytes[0..4], &(payload.len() as u32).to_le_bytes(), "length prefix");
    assert_eq!(&bytes[4..4 + payload.len()], payload, "payload");
}

#[test]
fn a_source_manifest_has_no_input() {
    const M: ConstManifest = ConstManifest {
        input: None,
        output: ConstSchema::Bool,
        uses: &[],
    };
    let decoded = decode_manifest(&encode_manifest(&M)).expect("decodes");
    assert_eq!(decoded.input, None);
    assert_eq!(decoded.output, TypeSchema::Bool);
    assert!(decoded.uses.is_empty());
}

#[test]
fn an_enum_output_round_trips() {
    const M: ConstManifest = ConstManifest {
        input: None,
        output: ConstSchema::Sum {
            type_name: "Status",
            variants: &[
                ("Ok", ConstSchema::Product { type_name: "Status", fields: &[(None, ConstSchema::U64)] }),
                ("Pending", ConstSchema::Product { type_name: "Status", fields: &[] }),
            ],
        },
        uses: &[],
    };
    let decoded = decode_manifest(&encode_manifest(&M)).expect("decodes");
    assert_eq!(
        decoded.output,
        TypeSchema::Sum {
            type_name: "Status".into(),
            variants: vec![
                (
                    "Ok".into(),
                    TypeSchema::Product {
                        type_name: "Status".into(),
                        fields: vec![(None, TypeSchema::U64)],
                    }
                ),
                (
                    "Pending".into(),
                    TypeSchema::Product { type_name: "Status".into(), fields: vec![] }
                ),
            ],
        }
    );
}
