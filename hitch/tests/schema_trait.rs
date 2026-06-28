//! `Schema` is "what shape does this Rust type have?" — the static counterpart of
//! a runtime `Value`'s shape. The primitives anchor the recursion that
//! `#[derive(Schema)]` builds on: a derived struct's schema is its fields'
//! schemas, which bottom out here.

use hitch::{Schema, TypeSchema};

#[test]
fn primitive_types_report_their_scalar_schema() {
    assert_eq!(bool::schema(), TypeSchema::Bool);
    assert_eq!(i64::schema(), TypeSchema::I64);
    assert_eq!(u64::schema(), TypeSchema::U64);
    assert_eq!(f64::schema(), TypeSchema::F64);
    assert_eq!(String::schema(), TypeSchema::Str);
}
