//! `ConstSchema` is the const-constructible twin of `TypeSchema`: it uses
//! `&'static str` / `&'static [..]` instead of `String`/`Vec`, so a type's shape
//! is available as an associated `const SCHEMA` — the foundation for emitting a
//! manifest into a `#[link_section]` static (which must be const). The runtime
//! `schema()` projects it to the alloc `TypeSchema`, so the const is the single
//! source of truth.

use hitch::{ConstSchema, Schema, TypeSchema};

#[test]
fn primitives_have_a_const_schema() {
    assert_eq!(<bool as Schema>::SCHEMA, ConstSchema::Bool);
    assert_eq!(<i64 as Schema>::SCHEMA, ConstSchema::I64);
    assert_eq!(<u32 as Schema>::SCHEMA, ConstSchema::U32);
    assert_eq!(<f32 as Schema>::SCHEMA, ConstSchema::F32);
}

#[test]
fn every_primitive_projects_to_its_runtime_schema() {
    // `schema()` is a default that converts `SCHEMA` to the alloc `TypeSchema`
    // (exercising every scalar arm of `to_type_schema`), so existing runtime
    // callers keep working with no per-type code.
    assert_eq!(<bool as Schema>::schema(), TypeSchema::Bool);
    assert_eq!(<i8 as Schema>::schema(), TypeSchema::I8);
    assert_eq!(<i16 as Schema>::schema(), TypeSchema::I16);
    assert_eq!(<i32 as Schema>::schema(), TypeSchema::I32);
    assert_eq!(<i64 as Schema>::schema(), TypeSchema::I64);
    assert_eq!(<u8 as Schema>::schema(), TypeSchema::U8);
    assert_eq!(<u16 as Schema>::schema(), TypeSchema::U16);
    assert_eq!(<u32 as Schema>::schema(), TypeSchema::U32);
    assert_eq!(<u64 as Schema>::schema(), TypeSchema::U64);
    assert_eq!(<f32 as Schema>::schema(), TypeSchema::F32);
    assert_eq!(<f64 as Schema>::schema(), TypeSchema::F64);
    assert_eq!(<String as Schema>::schema(), TypeSchema::Str);
}

#[test]
fn const_schema_projects_bytes_and_seq() {
    // `Bytes` and `Seq` have no primitive `Schema` impl, so cover their
    // `to_type_schema` arms directly.
    assert_eq!(ConstSchema::Bytes.to_type_schema(), TypeSchema::Bytes);
    assert_eq!(
        ConstSchema::Seq(&ConstSchema::U64).to_type_schema(),
        TypeSchema::Seq(Box::new(TypeSchema::U64))
    );
}

#[cfg(feature = "derive")]
#[test]
fn a_derived_struct_has_a_const_schema() {
    #[derive(Schema)]
    #[allow(dead_code, reason = "reflected via SCHEMA, never instantiated")]
    struct Point {
        x: i64,
        y: u32,
    }

    assert_eq!(
        Point::SCHEMA,
        ConstSchema::Product {
            type_name: "Point",
            fields: &[(Some("x"), ConstSchema::I64), (Some("y"), ConstSchema::U32)],
        }
    );
}

#[cfg(feature = "derive")]
#[test]
fn a_derived_enum_has_a_const_sum_schema() {
    #[derive(Schema)]
    #[allow(dead_code, reason = "reflected via SCHEMA, never instantiated")]
    enum Status {
        Ok(u64),
        Pending,
    }

    assert_eq!(
        Status::SCHEMA,
        ConstSchema::Sum {
            type_name: "Status",
            variants: &[
                (
                    "Ok",
                    ConstSchema::Product {
                        type_name: "Status",
                        fields: &[(None, ConstSchema::U64)],
                    }
                ),
                (
                    "Pending",
                    ConstSchema::Product { type_name: "Status", fields: &[] }
                ),
            ],
        }
    );
}
