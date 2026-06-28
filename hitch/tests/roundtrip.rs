//! The self-describing encoding must survive a `hitch` → `unhitch` round-trip
//! with the value unchanged — the foundational guarantee the whole format rests
//! on. One value spans the scalar set, a nested `Seq`, a named `Product`, and a
//! `Sum`, so the round-trip exercises every shape in the v1 model at once.

use hitch::{hitch, unhitch, Value};

#[test]
fn self_describing_roundtrip_preserves_the_value() {
    let value = Value::Product {
        type_name: "Reading".into(),
        fields: vec![
            (Some("name".into()), Value::Str("hot.avg".into())),
            (Some("live".into()), Value::Bool(true)),
            (Some("count".into()), Value::I64(-3)),
            (Some("total".into()), Value::U64(42)),
            (Some("avg".into()), Value::F64(1.5)),
            (Some("raw".into()), Value::Bytes(vec![0xde, 0xad])),
            (
                Some("samples".into()),
                Value::Seq(vec![Value::I64(1), Value::I64(2)]),
            ),
            (
                Some("status".into()),
                Value::Sum {
                    type_name: "Status".into(),
                    variant: "Ok".into(),
                    payload: Box::new(Value::U64(0)),
                },
            ),
        ],
    };

    let bytes = hitch(&value);
    let back = unhitch(&bytes).expect("a freshly hitched value unhitches");

    assert_eq!(back, value);
}
