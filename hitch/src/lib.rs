//! **Hitch** — the self-describing algebraic value model for `SnitchOS`.
//!
//! Hitch is the lingua franca that data takes when it crosses a boundary: an
//! IPC endpoint, the telemetry channel, a file, or the cross-process `~>` pipe.
//! A serialized value is *a hitch*; the verbs are [`hitch`] (serialize) and
//! [`unhitch`] (deserialize). See `README.md` for the why, and
//! `docs/typed-processes-and-the-data-model-design.md` for the full design.
//!
//! Hitch knows nothing about Stitch (or any other crate). It is a leaf: every
//! consumer depends on Hitch, never the reverse — so the interpreter never
//! leaks into `protocol`/`kernel`. `stitch` owns the `DataValue` ↔ [`Value`]
//! bridge; Rust types reflect in via `#[derive(Schema)]`.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

/// One algebraic shape covering everything `SnitchOS` moves around: `protocol`'s
/// `Frame` is a [`Value::Sum`], a Rust `struct` is a [`Value::Product`], and
/// Stitch's `DataValue` is already this shape. Products keep their field names
/// (optional, for tuple-like data), so a `Value` is *self-describing* — encoding
/// one carries enough to reconstruct a named record with no prior schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Bytes(Vec<u8>),
    Seq(Vec<Value>),
    /// A struct / record / `prod` — fields in declaration order, each optionally
    /// named (`None` for positional/tuple fields).
    Product {
        type_name: String,
        fields: Vec<(Option<String>, Value)>,
    },
    /// An enum / variant / `sum` — the chosen variant and its payload.
    Sum {
        type_name: String,
        variant: String,
        payload: Box<Value>,
    },
}

/// The *shape* of a [`Value`] — what `#[derive(Schema)]` emits for a Rust type
/// and what the packed encoding decodes against. It mirrors [`Value`] at the
/// type level, with one difference that matters: a [`Value::Sum`] instance names
/// a *single* variant, whereas [`TypeSchema::Sum`] lists *every* variant the type
/// admits.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeSchema {
    Bool,
    I64,
    U64,
    F64,
    Str,
    Bytes,
    /// A homogeneous sequence of the inner shape.
    Seq(Box<TypeSchema>),
    /// A struct / record / `prod` — fields in declaration order, each optionally
    /// named (`None` for positional fields).
    Product {
        type_name: String,
        fields: Vec<(Option<String>, TypeSchema)>,
    },
    /// An enum / variant / `sum` — every variant the type can take.
    Sum {
        type_name: String,
        variants: Vec<(String, TypeSchema)>,
    },
}

impl TypeSchema {
    /// Does `value` structurally conform to this shape?
    ///
    /// Conformance is **structural**: `type_name`s are display labels and play no
    /// part, so a Rust `Table` and a Stitch `Table` of the same shape accept each
    /// other — what cross-language `~>` needs. Field *names* and order, and
    /// variant *names*, are part of the structure and must match. An empty
    /// sequence conforms to any [`TypeSchema::Seq`].
    #[must_use]
    pub fn accepts(&self, value: &Value) -> bool {
        match (self, value) {
            (TypeSchema::Bool, Value::Bool(_))
            | (TypeSchema::I64, Value::I64(_))
            | (TypeSchema::U64, Value::U64(_))
            | (TypeSchema::F64, Value::F64(_))
            | (TypeSchema::Str, Value::Str(_))
            | (TypeSchema::Bytes, Value::Bytes(_)) => true,
            (TypeSchema::Seq(elem), Value::Seq(items)) => {
                items.iter().all(|item| elem.accepts(item))
            }
            (
                TypeSchema::Product { fields: schema, .. },
                Value::Product { fields: actual, .. },
            ) => {
                schema.len() == actual.len()
                    && schema.iter().zip(actual).all(|((sname, sshape), (vname, vvalue))| {
                        sname == vname && sshape.accepts(vvalue)
                    })
            }
            (
                TypeSchema::Sum { variants, .. },
                Value::Sum { variant, payload, .. },
            ) => variants
                .iter()
                .any(|(name, shape)| name == variant && shape.accepts(payload)),
            _ => false,
        }
    }
}

/// Something went wrong hitching or unhitching a value.
#[derive(Debug)]
pub enum Error {
    /// The bytes were not a valid encoding (truncated, malformed).
    Codec(postcard::Error),
    /// A value did not conform to the [`TypeSchema`] it was packed or unpacked
    /// against — a wrong kind, field name, arity, or unknown variant.
    SchemaMismatch,
    /// A packed hitch decoded successfully but left bytes unconsumed.
    TrailingBytes,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Codec(e) => write!(f, "invalid hitch encoding: {e}"),
            Error::SchemaMismatch => f.write_str("hitch value does not conform to its schema"),
            Error::TrailingBytes => f.write_str("trailing bytes after a packed hitch"),
        }
    }
}

impl core::error::Error for Error {}

/// Serialize a [`Value`] to its **self-describing** encoding: postcard the
/// `Value` itself, so the bytes carry field names and variants and any consumer
/// can [`unhitch`] them with no prior schema.
#[must_use]
pub fn hitch(value: &Value) -> Vec<u8> {
    // Serializing a finite in-memory `Value` to a growable buffer has no failure
    // mode: there is no I/O, and our closed model contains no construct postcard
    // can reject. `to_allocvec` only returns `Err` for serializer faults this
    // type cannot produce.
    postcard::to_allocvec(value).expect("hitching a Value is infallible")
}

/// Deserialize a self-describing hitch back into a [`Value`]. Fails with
/// [`Error`] if `bytes` is not a valid encoding.
pub fn unhitch(bytes: &[u8]) -> Result<Value, Error> {
    postcard::from_bytes(bytes).map_err(Error::Codec)
}

/// Serialize a [`Value`] to its **packed** encoding against `schema`: positional
/// bytes carrying only data — no names, variants, or tags, since `schema`
/// supplies them. The result is byte-identical to postcard of the equivalent
/// Rust type, so a Rust ELF stage and a Hitch value interoperate. Fails with
/// [`Error::SchemaMismatch`] if `value` does not conform to `schema`.
pub fn hitch_packed(value: &Value, schema: &TypeSchema) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::new();
    pack(value, schema, &mut buf)?;
    Ok(buf)
}

/// Deserialize a packed hitch back into a [`Value`], using `schema` to drive the
/// positional decode and to restore the names and variant labels the bytes omit.
/// Fails on malformed bytes, a schema mismatch, or trailing bytes.
pub fn unhitch_packed(bytes: &[u8], schema: &TypeSchema) -> Result<Value, Error> {
    let (value, rest) = unpack(schema, bytes)?;
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(Error::TrailingBytes)
    }
}

/// Append the postcard encoding of one leaf scalar to `buf`. postcard's per-type
/// layout (varint ints, length-prefixed strings/bytes) is exactly what makes a
/// packed product match postcard of the equivalent struct.
fn push<T: Serialize>(value: &T, buf: &mut Vec<u8>) -> Result<(), Error> {
    let bytes = postcard::to_allocvec(value).map_err(Error::Codec)?;
    buf.extend_from_slice(&bytes);
    Ok(())
}

/// Read one value of type `T` off the front of `bytes`, returning it and the
/// remaining bytes — the positional decode primitive.
fn take<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<(T, &'a [u8]), Error> {
    postcard::take_from_bytes(bytes).map_err(Error::Codec)
}

fn pack(value: &Value, schema: &TypeSchema, buf: &mut Vec<u8>) -> Result<(), Error> {
    match (schema, value) {
        (TypeSchema::Bool, Value::Bool(b)) => push(b, buf),
        (TypeSchema::I64, Value::I64(n)) => push(n, buf),
        (TypeSchema::U64, Value::U64(n)) => push(n, buf),
        (TypeSchema::F64, Value::F64(n)) => push(n, buf),
        (TypeSchema::Str, Value::Str(s)) => push(s, buf),
        (TypeSchema::Bytes, Value::Bytes(b)) => push(b, buf),
        (TypeSchema::Seq(elem), Value::Seq(items)) => {
            // Length first (postcard encodes a seq as varint(len) ++ elements),
            // then each element against the element schema.
            push(&(items.len() as u64), buf)?;
            items.iter().try_for_each(|item| pack(item, elem, buf))
        }
        (
            TypeSchema::Product { fields: schema_fields, .. },
            Value::Product { fields: value_fields, .. },
        ) => {
            if schema_fields.len() != value_fields.len() {
                return Err(Error::SchemaMismatch);
            }
            schema_fields.iter().zip(value_fields).try_for_each(
                |((sname, sshape), (vname, vvalue))| {
                    if sname == vname {
                        pack(vvalue, sshape, buf)
                    } else {
                        Err(Error::SchemaMismatch)
                    }
                },
            )
        }
        (
            TypeSchema::Sum { variants, .. },
            Value::Sum { variant, payload, .. },
        ) => {
            let idx = variants
                .iter()
                .position(|(name, _)| name == variant)
                .ok_or(Error::SchemaMismatch)?;
            // The variant index stands in for the name (postcard encodes an enum
            // as varint(discriminant) ++ payload).
            push(&(idx as u64), buf)?;
            pack(payload, &variants[idx].1, buf)
        }
        _ => Err(Error::SchemaMismatch),
    }
}

fn unpack<'a>(schema: &TypeSchema, bytes: &'a [u8]) -> Result<(Value, &'a [u8]), Error> {
    match schema {
        TypeSchema::Bool => take::<bool>(bytes).map(|(v, r)| (Value::Bool(v), r)),
        TypeSchema::I64 => take::<i64>(bytes).map(|(v, r)| (Value::I64(v), r)),
        TypeSchema::U64 => take::<u64>(bytes).map(|(v, r)| (Value::U64(v), r)),
        TypeSchema::F64 => take::<f64>(bytes).map(|(v, r)| (Value::F64(v), r)),
        TypeSchema::Str => take::<String>(bytes).map(|(v, r)| (Value::Str(v), r)),
        TypeSchema::Bytes => take::<Vec<u8>>(bytes).map(|(v, r)| (Value::Bytes(v), r)),
        TypeSchema::Seq(elem) => {
            let (count, mut rest) = take::<u64>(bytes)?;
            let mut items = Vec::new();
            for _ in 0..count {
                let (item, next) = unpack(elem, rest)?;
                items.push(item);
                rest = next;
            }
            Ok((Value::Seq(items), rest))
        }
        TypeSchema::Product { type_name, fields } => {
            let mut rest = bytes;
            let mut out = Vec::with_capacity(fields.len());
            for (name, field_schema) in fields {
                let (value, next) = unpack(field_schema, rest)?;
                out.push((name.clone(), value));
                rest = next;
            }
            Ok((
                Value::Product {
                    type_name: type_name.clone(),
                    fields: out,
                },
                rest,
            ))
        }
        TypeSchema::Sum { type_name, variants } => {
            let (idx, rest) = take::<u64>(bytes)?;
            let idx = usize::try_from(idx).map_err(|_| Error::SchemaMismatch)?;
            let (name, variant_schema) = variants.get(idx).ok_or(Error::SchemaMismatch)?;
            let (payload, rest) = unpack(variant_schema, rest)?;
            Ok((
                Value::Sum {
                    type_name: type_name.clone(),
                    variant: name.clone(),
                    payload: Box::new(payload),
                },
                rest,
            ))
        }
    }
}
