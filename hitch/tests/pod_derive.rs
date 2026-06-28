//! `#[derive(Pod)]` generates the `unsafe impl hitch::Pod` for a valid C-ABI
//! struct (and, via the const checks it emits, refuses to for one with padding, a
//! non-`Pod` field, or no `#[repr(C)]` — those are compile errors, exercised
//! manually / by future trybuild fixtures). Here we prove the *positive* path:
//! a derived `Pod` casts to bytes exactly like a hand-written impl.

#![cfg(feature = "derive")]

use hitch::{pod_bytes, Pod, Schema, TypeSchema, Value};

#[derive(Schema, Pod, Clone, Copy)]
#[repr(C)]
struct CapDesc {
    handle: u32,
    kind: u32,
    rights: u32,
    reserved: u32,
    badge: u64,
}

#[test]
fn a_derived_pod_casts_to_its_repr_c_image() {
    let descs = [
        CapDesc { handle: 1, kind: 2, rights: 3, reserved: 0, badge: 4 },
        CapDesc { handle: 5, kind: 6, rights: 7, reserved: 0, badge: 8 },
    ];
    assert_eq!(pod_bytes(&descs).len(), 48);
}

#[test]
fn the_derived_pod_cast_matches_the_derived_schema_pack() {
    // The two derives — Schema and Pod — agree: the zero-copy cast equals
    // hitch_packed against the derived schema. Same struct, same bytes, two paths.
    let desc = CapDesc { handle: 7, kind: 2, rights: 5, reserved: 0, badge: 99 };

    let value = Value::Product {
        type_name: "CapDesc".into(),
        fields: vec![
            (Some("handle".into()), Value::U64(7)),
            (Some("kind".into()), Value::U64(2)),
            (Some("rights".into()), Value::U64(5)),
            (Some("reserved".into()), Value::U64(0)),
            (Some("badge".into()), Value::U64(99)),
        ],
    };
    let TypeSchema::Product { .. } = CapDesc::schema() else {
        panic!("a struct derives a Product schema");
    };

    assert_eq!(
        pod_bytes(core::slice::from_ref(&desc)),
        hitch::hitch_packed(&value, &CapDesc::schema())
            .expect("packs")
            .as_slice()
    );
}
