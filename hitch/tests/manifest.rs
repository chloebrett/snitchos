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
