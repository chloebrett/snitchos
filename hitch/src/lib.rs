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
    /// The self-describing bytes were not a valid encoding (truncated, malformed).
    Codec(postcard::Error),
    /// A value or its bytes did not conform to the [`TypeSchema`] it was packed or
    /// unpacked against — a wrong kind, field name, arity, unknown variant, an
    /// out-of-range length, an invalid `bool`, or invalid UTF-8.
    SchemaMismatch,
    /// A packed hitch ended before the schema was satisfied.
    UnexpectedEnd,
    /// A packed hitch decoded successfully but left bytes unconsumed.
    TrailingBytes,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Codec(e) => write!(f, "invalid hitch encoding: {e}"),
            Error::SchemaMismatch => f.write_str("hitch value does not conform to its schema"),
            Error::UnexpectedEnd => f.write_str("packed hitch ended unexpectedly"),
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

/// Serialize a [`Value`] to its **packed** encoding against `schema`: positional,
/// fixed-width little-endian bytes carrying only data — no names, variants, or
/// tags, since `schema` supplies them. The result is byte-identical to the
/// `repr(C)` in-memory image of the equivalent Rust type, so a Rust ELF stage can
/// read it as a struct (and a no-padding POD type can be transmuted, not
/// serialized). Fails with [`Error::SchemaMismatch`] if `value` does not conform.
pub fn hitch_packed(value: &Value, schema: &TypeSchema) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::new();
    pack(value, schema, &mut buf)?;
    Ok(buf)
}

/// Deserialize a packed hitch back into a [`Value`], using `schema` to drive the
/// positional decode and to restore the names and variant labels the bytes omit.
/// Fails on malformed bytes, a schema mismatch, or trailing bytes.
pub fn unhitch_packed(bytes: &[u8], schema: &TypeSchema) -> Result<Value, Error> {
    let mut cur = Cursor { bytes };
    let value = unpack(schema, &mut cur)?;
    if cur.bytes.is_empty() {
        Ok(value)
    } else {
        Err(Error::TrailingBytes)
    }
}

/// Append a length, count, or variant index as a fixed 4-byte little-endian
/// `u32`. `u32` because no realistic record holds four billion of anything; an
/// overflow is a schema mismatch, never a silent truncation.
fn push_u32(n: usize, buf: &mut Vec<u8>) -> Result<(), Error> {
    let n = u32::try_from(n).map_err(|_| Error::SchemaMismatch)?;
    buf.extend_from_slice(&n.to_le_bytes());
    Ok(())
}

fn pack(value: &Value, schema: &TypeSchema, buf: &mut Vec<u8>) -> Result<(), Error> {
    match (schema, value) {
        (TypeSchema::Bool, Value::Bool(b)) => {
            buf.push(u8::from(*b));
            Ok(())
        }
        (TypeSchema::I64, Value::I64(n)) => {
            buf.extend_from_slice(&n.to_le_bytes());
            Ok(())
        }
        (TypeSchema::U64, Value::U64(n)) => {
            buf.extend_from_slice(&n.to_le_bytes());
            Ok(())
        }
        (TypeSchema::F64, Value::F64(n)) => {
            buf.extend_from_slice(&n.to_le_bytes());
            Ok(())
        }
        (TypeSchema::Str, Value::Str(s)) => {
            push_u32(s.len(), buf)?;
            buf.extend_from_slice(s.as_bytes());
            Ok(())
        }
        (TypeSchema::Bytes, Value::Bytes(b)) => {
            push_u32(b.len(), buf)?;
            buf.extend_from_slice(b);
            Ok(())
        }
        (TypeSchema::Seq(elem), Value::Seq(items)) => {
            push_u32(items.len(), buf)?;
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
            push_u32(idx, buf)?;
            pack(payload, &variants[idx].1, buf)
        }
        _ => Err(Error::SchemaMismatch),
    }
}

/// A forward byte cursor for the positional decode. Each read advances past the
/// bytes consumed and errors with [`Error::UnexpectedEnd`] if the input runs out.
struct Cursor<'a> {
    bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.bytes.len() < n {
            return Err(Error::UnexpectedEnd);
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        // `take(N)` returns exactly `N` bytes or errors, so the slice-to-array
        // conversion below cannot fail.
        Ok(self.take(N)?.try_into().expect("take(N) yields N bytes"))
    }

    fn u32(&mut self) -> Result<usize, Error> {
        Ok(u32::from_le_bytes(self.array::<4>()?) as usize)
    }
}

fn unpack(schema: &TypeSchema, cur: &mut Cursor) -> Result<Value, Error> {
    match schema {
        TypeSchema::Bool => match cur.array::<1>()?[0] {
            0 => Ok(Value::Bool(false)),
            1 => Ok(Value::Bool(true)),
            _ => Err(Error::SchemaMismatch),
        },
        TypeSchema::I64 => Ok(Value::I64(i64::from_le_bytes(cur.array::<8>()?))),
        TypeSchema::U64 => Ok(Value::U64(u64::from_le_bytes(cur.array::<8>()?))),
        TypeSchema::F64 => Ok(Value::F64(f64::from_le_bytes(cur.array::<8>()?))),
        TypeSchema::Str => {
            let len = cur.u32()?;
            let bytes = cur.take(len)?;
            let text = core::str::from_utf8(bytes).map_err(|_| Error::SchemaMismatch)?;
            Ok(Value::Str(text.into()))
        }
        TypeSchema::Bytes => {
            let len = cur.u32()?;
            Ok(Value::Bytes(cur.take(len)?.to_vec()))
        }
        TypeSchema::Seq(elem) => {
            let count = cur.u32()?;
            // Grow as elements are actually read — never pre-allocate against an
            // untrusted count, which would let a bogus length OOM the decoder.
            let mut items = Vec::new();
            for _ in 0..count {
                items.push(unpack(elem, cur)?);
            }
            Ok(Value::Seq(items))
        }
        TypeSchema::Product { type_name, fields } => {
            let mut out = Vec::with_capacity(fields.len());
            for (name, field_schema) in fields {
                out.push((name.clone(), unpack(field_schema, cur)?));
            }
            Ok(Value::Product {
                type_name: type_name.clone(),
                fields: out,
            })
        }
        TypeSchema::Sum { type_name, variants } => {
            let idx = cur.u32()?;
            let (name, variant_schema) = variants.get(idx).ok_or(Error::SchemaMismatch)?;
            let payload = unpack(variant_schema, cur)?;
            Ok(Value::Sum {
                type_name: type_name.clone(),
                variant: name.clone(),
                payload: Box::new(payload),
            })
        }
    }
}
