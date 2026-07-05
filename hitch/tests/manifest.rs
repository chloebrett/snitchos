//! A `ConstManifest` (input/output `ConstSchema` + `uses`) encodes to a fixed-size
//! byte note **in a `const`** — that's the whole point: the bytes can live in a
//! `#[link_section]` static. The host seed step then `decode_manifest`s them back
//! into a runtime `Manifest`. Round-trip through that encoding is the contract.

use hitch::{
    decode_manifest, encode_manifest, ConstManifest, ConstSchema, ConstSlot, Manifest, Slot,
    TypeSchema,
};
use snitchos_abi::{object_kind, rights};

#[test]
fn a_needs_slot_round_trips_with_object_and_rights() {
    // A typed authority slot (name + object kind + rights) survives the note
    // encoding — the v2 shape that replaces the bare `uses` strings. The
    // object/rights values are sourced from the ABI, so this also pins that
    // `Slot` mirrors the ABI discriminants (no forked numbering).
    const M: ConstManifest = ConstManifest {
        input: None,
        output: ConstSchema::Bool,
        needs: &[ConstSlot {
            name: "fs",
            object: object_kind::ENDPOINT as u8,
            rights: rights::SEND,
        }],
    };
    let decoded = decode_manifest(&encode_manifest(&M)).expect("decodes");
    assert_eq!(
        decoded.needs,
        vec![Slot {
            name: "fs".into(),
            object: object_kind::ENDPOINT as u8,
            rights: rights::SEND,
        }],
    );
}

#[test]
fn decode_rejects_an_unknown_version() {
    // The version byte (payload[0], at offset 4) exists so a future format change
    // is rejected loudly instead of misparsed. Corrupt it and decoding must fail.
    const M: ConstManifest =
        ConstManifest { input: None, output: ConstSchema::Bool, needs: &[] };
    let mut bytes = encode_manifest(&M);
    bytes[4] = 0xFF;
    assert!(
        decode_manifest(&bytes).is_err(),
        "an unknown manifest version must be rejected, not misparsed",
    );
}

#[test]
fn a_manifest_round_trips_through_the_note_encoding() {
    const M: ConstManifest = ConstManifest {
        input: Some(ConstSchema::Product {
            type_name: "Row",
            fields: &[(Some("name"), ConstSchema::Str), (Some("n"), ConstSchema::U32)],
        }),
        output: ConstSchema::Seq(&ConstSchema::U64),
        // object 2 = Endpoint, rights 0b0010 = SEND (raw here; the ABI-agreement
        // pin is `a_needs_slot_round_trips_with_object_and_rights`).
        needs: &[
            ConstSlot { name: "fs", object: 2, rights: 0b0010 },
            ConstSlot { name: "log", object: 2, rights: 0b0010 },
        ],
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
            needs: vec![
                Slot { name: "fs".into(), object: 2, rights: 0b0010 },
                Slot { name: "log".into(), object: 2, rights: 0b0010 },
            ],
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
        needs: &[],
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
    // lengths, the version byte, the payload-length prefix, the slot fields)
    // changes the bytes and is caught. This is the golden-bytes guard — a diff
    // here must be a deliberate, reviewed format change.
    const M: ConstManifest = ConstManifest {
        input: None,
        output: ConstSchema::U8,
        needs: &[ConstSlot { name: "X", object: 2, rights: 0b0010 }],
    };
    let bytes = encode_manifest(&M);

    let payload: &[u8] = &[
        0x01, // format version = 1
        0x00, // input: None
        0x05, // output: U8 (tag 5)
        0x01, 0x00, 0x00, 0x00, // needs count = 1
        0x01, 0x00, 0x00, 0x00, // slot[0].name length = 1
        b'X', // "X"
        0x02, // slot[0].object = 2 (Endpoint)
        0x02, 0x00, 0x00, 0x00, // slot[0].rights = 0b0010 (SEND)
    ];
    assert_eq!(&bytes[0..4], &(payload.len() as u32).to_le_bytes(), "length prefix");
    assert_eq!(&bytes[4..4 + payload.len()], payload, "payload");
}

#[test]
fn a_source_manifest_has_no_input() {
    const M: ConstManifest = ConstManifest {
        input: None,
        output: ConstSchema::Bool,
        needs: &[],
    };
    let decoded = decode_manifest(&encode_manifest(&M)).expect("decodes");
    assert_eq!(decoded.input, None);
    assert_eq!(decoded.output, TypeSchema::Bool);
    assert!(decoded.needs.is_empty());
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
        needs: &[],
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
