//! The POD fast path: for a `#[repr(C)]`, padding-free, all-scalar type, the
//! packed encoding *is* the in-memory image, so a `&[T]` casts to `&[u8]`
//! zero-copy instead of serializing field-by-field. This is where `CapList`'s
//! hand-rolled `from_raw_parts` moves — one audited place, gated by `T: Pod`.
//!
//! The cast must agree byte-for-byte with the slow (value-based) `hitch_packed`,
//! so the fast path is a transparent optimization, not a second format.

use hitch::{from_pod_bytes, hitch_packed, pod_bytes, Pod, TypeSchema, Value};

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
struct CapDesc {
    handle: u32,
    kind: u32,
    rights: u32,
    reserved: u32,
    badge: u64,
}

// SAFETY: `#[repr(C)]`; 4×u32 + u64 = 24 bytes laid out with no padding (`reserved`
// aligns `badge` to 8); every field is an integer, so all bit patterns are valid.
// 5b's `#[derive(Pod)]` checks exactly this instead of trusting the comment.
unsafe impl Pod for CapDesc {}

fn descs() -> [CapDesc; 2] {
    [
        CapDesc { handle: 7, kind: 2, rights: 0b101, reserved: 0, badge: 0xdead_beef },
        CapDesc { handle: 8, kind: 1, rights: 0b010, reserved: 0, badge: 0x1234 },
    ]
}

#[test]
fn pod_bytes_is_the_repr_c_image_and_round_trips() {
    let descs = descs();
    let bytes = pod_bytes(&descs);
    assert_eq!(bytes.len(), 48, "2 × 24 bytes, zero-copy");

    let back = from_pod_bytes::<CapDesc>(bytes).expect("round-trips");
    assert_eq!(back, descs);
}

#[test]
fn from_pod_bytes_rejects_a_partial_element() {
    assert!(from_pod_bytes::<CapDesc>(&[0u8; 23]).is_err());
}

#[test]
fn the_fast_path_agrees_with_field_by_field_hitch_packed() {
    let desc = CapDesc { handle: 7, kind: 2, rights: 5, reserved: 0, badge: 99 };

    let schema = TypeSchema::Product {
        type_name: "CapDesc".into(),
        fields: vec![
            (Some("handle".into()), TypeSchema::U32),
            (Some("kind".into()), TypeSchema::U32),
            (Some("rights".into()), TypeSchema::U32),
            (Some("reserved".into()), TypeSchema::U32),
            (Some("badge".into()), TypeSchema::U64),
        ],
    };
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

    assert_eq!(
        pod_bytes(core::slice::from_ref(&desc)),
        hitch_packed(&value, &schema).expect("packs").as_slice(),
        "the transmute and the serialize produce identical bytes"
    );
}
