//! Round-trip guarantees for the corners of the model: each scalar on its own,
//! empty collections, positional (unnamed) fields and their order, and deep
//! nesting. The foundational `roundtrip.rs` proves one big mixed value survives;
//! these pin the edges that a single example glosses over.

use hitch::{hitch, unhitch, Value};

fn assert_roundtrips(value: &Value) {
    let back = unhitch(&hitch(value)).expect("a freshly hitched value unhitches");
    assert_eq!(&back, value);
}

#[test]
fn each_scalar_round_trips_at_the_top_level() {
    for value in [
        Value::Bool(true),
        Value::Bool(false),
        Value::I64(-3),
        Value::U64(42),
        Value::F64(1.5),
        Value::Str("hot.avg".into()),
        Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
    ] {
        assert_roundtrips(&value);
    }
}

#[test]
fn empty_seq_and_empty_product_round_trip() {
    assert_roundtrips(&Value::Seq(vec![]));
    assert_roundtrips(&Value::Product {
        type_name: "Empty".into(),
        fields: vec![],
    });
}

#[test]
fn positional_fields_and_their_order_survive() {
    // `None`-named (tuple-style) fields, in a deliberate order: both the absence
    // of names and the ordering must come back unchanged.
    let value = Value::Product {
        type_name: "Pair".into(),
        fields: vec![(None, Value::I64(1)), (None, Value::Str("two".into()))],
    };
    assert_roundtrips(&value);
}

#[test]
fn deeply_nested_value_round_trips() {
    let value = Value::Seq(vec![Value::Sum {
        type_name: "Tree".into(),
        variant: "Node".into(),
        payload: Box::new(Value::Product {
            type_name: "N".into(),
            fields: vec![(
                Some("kids".into()),
                Value::Seq(vec![Value::Sum {
                    type_name: "Tree".into(),
                    variant: "Leaf".into(),
                    payload: Box::new(Value::I64(7)),
                }]),
            )],
        }),
    }]);
    assert_roundtrips(&value);
}
