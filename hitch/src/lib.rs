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
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
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
            | (TypeSchema::F32 | TypeSchema::F64, Value::F64(_))
            | (TypeSchema::U64, Value::U64(_))
            | (TypeSchema::I64, Value::I64(_))
            | (TypeSchema::Str, Value::Str(_))
            | (TypeSchema::Bytes, Value::Bytes(_)) => true,
            (TypeSchema::U8, Value::U64(n)) => uint_fits(*n, 1),
            (TypeSchema::U16, Value::U64(n)) => uint_fits(*n, 2),
            (TypeSchema::U32, Value::U64(n)) => uint_fits(*n, 4),
            (TypeSchema::I8, Value::I64(n)) => int_fits(*n, 1),
            (TypeSchema::I16, Value::I64(n)) => int_fits(*n, 2),
            (TypeSchema::I32, Value::I64(n)) => int_fits(*n, 4),
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

    /// Is a value produced against `self` (a stage's `out`) consumable against
    /// `other` (the next stage's `in`)? The structural typecheck behind `~>`. v1 is
    /// **structural equality modulo `type_name`**: like [`accepts`](Self::accepts),
    /// field names/order and variant names/order are part of the shape, but the
    /// `type_name` label is display-only — so a Rust `Table` and a Stitch `Table` of
    /// the same shape are compatible (cross-language `~>`). Numeric widths are
    /// distinct shapes: `U32` is not `U64`.
    #[must_use]
    pub fn compatible(&self, other: &TypeSchema) -> bool {
        match (self, other) {
            (TypeSchema::Seq(a), TypeSchema::Seq(b)) => a.compatible(b),
            (TypeSchema::Product { fields: a, .. }, TypeSchema::Product { fields: b, .. }) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|((an, at), (bn, bt))| an == bn && at.compatible(bt))
            }
            (TypeSchema::Sum { variants: a, .. }, TypeSchema::Sum { variants: b, .. }) => {
                a.len() == b.len()
                    && a.iter().zip(b).all(|((an, at), (bn, bt))| an == bn && at.compatible(bt))
            }
            // Leaves (and mismatched kinds) carry no `type_name` and no nested
            // schema, so structural equality is just value equality.
            (a, b) => a == b,
        }
    }
}

/// A **typed-process interface**: the input/output shapes a program (a `~>` stage)
/// consumes and produces, plus the capabilities it declares (`uses`). It is a
/// program's `main` signature externalized — `main(x: T) -> U uses C`. The artifact
/// `~>` typechecks against (structural [`TypeSchema`] compatibility of `out` → next
/// `in`) and the shell reads to decide which caps to grant a spawned stage.
///
/// `input` is `None` for a **source** (a stage with no upstream input, a zero-param
/// `main`). `uses` are the declared effect names (the soft authority layer; see the
/// userland doc §5).
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub input: Option<TypeSchema>,
    pub output: TypeSchema,
    pub uses: Vec<String>,
}

/// The const-constructible twin of [`TypeSchema`]: the same shape, but built from
/// `&'static str` / `&'static [..]` so a type's shape can be an associated
/// `const`. This is what `#[derive(Schema)]` emits, and what a manifest in a
/// `#[link_section]` static (which must be const) is built from. [`TypeSchema`] is
/// its runtime, allocating projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstSchema {
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Str,
    Bytes,
    Seq(&'static ConstSchema),
    Product {
        type_name: &'static str,
        fields: &'static [(Option<&'static str>, ConstSchema)],
    },
    Sum {
        type_name: &'static str,
        variants: &'static [(&'static str, ConstSchema)],
    },
}

impl ConstSchema {
    /// Project to the runtime, allocating [`TypeSchema`].
    #[must_use]
    pub fn to_type_schema(&self) -> TypeSchema {
        match self {
            ConstSchema::Bool => TypeSchema::Bool,
            ConstSchema::I8 => TypeSchema::I8,
            ConstSchema::I16 => TypeSchema::I16,
            ConstSchema::I32 => TypeSchema::I32,
            ConstSchema::I64 => TypeSchema::I64,
            ConstSchema::U8 => TypeSchema::U8,
            ConstSchema::U16 => TypeSchema::U16,
            ConstSchema::U32 => TypeSchema::U32,
            ConstSchema::U64 => TypeSchema::U64,
            ConstSchema::F32 => TypeSchema::F32,
            ConstSchema::F64 => TypeSchema::F64,
            ConstSchema::Str => TypeSchema::Str,
            ConstSchema::Bytes => TypeSchema::Bytes,
            ConstSchema::Seq(inner) => TypeSchema::Seq(Box::new(inner.to_type_schema())),
            ConstSchema::Product { type_name, fields } => TypeSchema::Product {
                type_name: (*type_name).into(),
                fields: fields
                    .iter()
                    .map(|(name, schema)| (name.map(Into::into), schema.to_type_schema()))
                    .collect(),
            },
            ConstSchema::Sum { type_name, variants } => TypeSchema::Sum {
                type_name: (*type_name).into(),
                variants: variants
                    .iter()
                    .map(|(name, schema)| ((*name).into(), schema.to_type_schema()))
                    .collect(),
            },
        }
    }
}

/// What shape does a Rust type take in the Hitch model? `<T as Schema>::SCHEMA` is
/// the const [`ConstSchema`] a value of `T` conforms to; `T::schema()` is its
/// runtime [`TypeSchema`] projection. `#[derive(Schema)]` supplies the const by
/// recursing into a struct's/enum's fields, which bottom out at the primitive
/// impls below.
pub trait Schema {
    /// This type's shape, as a `const`.
    const SCHEMA: ConstSchema;

    /// This type's shape as a runtime [`TypeSchema`] — the default projection of
    /// [`SCHEMA`](Self::SCHEMA), so a derive need only supply the const.
    #[must_use]
    fn schema() -> TypeSchema {
        Self::SCHEMA.to_type_schema()
    }
}

impl Schema for bool {
    const SCHEMA: ConstSchema = ConstSchema::Bool;
}
impl Schema for i8 {
    const SCHEMA: ConstSchema = ConstSchema::I8;
}
impl Schema for i16 {
    const SCHEMA: ConstSchema = ConstSchema::I16;
}
impl Schema for i32 {
    const SCHEMA: ConstSchema = ConstSchema::I32;
}
impl Schema for i64 {
    const SCHEMA: ConstSchema = ConstSchema::I64;
}
impl Schema for u8 {
    const SCHEMA: ConstSchema = ConstSchema::U8;
}
impl Schema for u16 {
    const SCHEMA: ConstSchema = ConstSchema::U16;
}
impl Schema for u32 {
    const SCHEMA: ConstSchema = ConstSchema::U32;
}
impl Schema for u64 {
    const SCHEMA: ConstSchema = ConstSchema::U64;
}
impl Schema for f32 {
    const SCHEMA: ConstSchema = ConstSchema::F32;
}
impl Schema for f64 {
    const SCHEMA: ConstSchema = ConstSchema::F64;
}
impl Schema for String {
    const SCHEMA: ConstSchema = ConstSchema::Str;
}

/// Fixed byte size of the `.note.snitch.iface` manifest note. A manifest larger
/// than this is a **compile error** (the const encoder indexes past the buffer) —
/// the right failure; bump this if a real interface needs more.
pub const MANIFEST_BYTES: usize = 1024;

/// The const-constructible form of a [`Manifest`]: built from [`ConstSchema`] +
/// `&'static` data so it can be `const`-encoded into a `#[link_section]` static by
/// [`encode_manifest`]. `#[entry(in, out, uses)]` builds one of these.
pub struct ConstManifest {
    pub input: Option<ConstSchema>,
    pub output: ConstSchema,
    pub uses: &'static [&'static str],
}

const fn put_tag(tag: u8, buf: &mut [u8], pos: usize) -> usize {
    buf[pos] = tag;
    pos + 1
}

const fn put_u32(n: u32, buf: &mut [u8], pos: usize) -> usize {
    let b = n.to_le_bytes();
    buf[pos] = b[0];
    buf[pos + 1] = b[1];
    buf[pos + 2] = b[2];
    buf[pos + 3] = b[3];
    pos + 4
}

const fn put_str(s: &str, buf: &mut [u8], pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut pos = put_u32(bytes.len() as u32, buf, pos);
    let mut i = 0;
    while i < bytes.len() {
        buf[pos] = bytes[i];
        pos += 1;
        i += 1;
    }
    pos
}

const fn put_opt_str(name: Option<&str>, buf: &mut [u8], pos: usize) -> usize {
    match name {
        Some(s) => {
            let pos = put_tag(1, buf, pos);
            put_str(s, buf, pos)
        }
        None => put_tag(0, buf, pos),
    }
}

const fn put_schema(schema: &ConstSchema, buf: &mut [u8], pos: usize) -> usize {
    match schema {
        ConstSchema::Bool => put_tag(0, buf, pos),
        ConstSchema::I8 => put_tag(1, buf, pos),
        ConstSchema::I16 => put_tag(2, buf, pos),
        ConstSchema::I32 => put_tag(3, buf, pos),
        ConstSchema::I64 => put_tag(4, buf, pos),
        ConstSchema::U8 => put_tag(5, buf, pos),
        ConstSchema::U16 => put_tag(6, buf, pos),
        ConstSchema::U32 => put_tag(7, buf, pos),
        ConstSchema::U64 => put_tag(8, buf, pos),
        ConstSchema::F32 => put_tag(9, buf, pos),
        ConstSchema::F64 => put_tag(10, buf, pos),
        ConstSchema::Str => put_tag(11, buf, pos),
        ConstSchema::Bytes => put_tag(12, buf, pos),
        ConstSchema::Seq(inner) => {
            let pos = put_tag(13, buf, pos);
            put_schema(inner, buf, pos)
        }
        ConstSchema::Product { type_name, fields } => {
            let pos = put_tag(14, buf, pos);
            let mut pos = put_str(type_name, buf, pos);
            pos = put_u32(fields.len() as u32, buf, pos);
            let mut i = 0;
            while i < fields.len() {
                pos = put_opt_str(fields[i].0, buf, pos);
                pos = put_schema(&fields[i].1, buf, pos);
                i += 1;
            }
            pos
        }
        ConstSchema::Sum { type_name, variants } => {
            let pos = put_tag(15, buf, pos);
            let mut pos = put_str(type_name, buf, pos);
            pos = put_u32(variants.len() as u32, buf, pos);
            let mut i = 0;
            while i < variants.len() {
                pos = put_str(variants[i].0, buf, pos);
                pos = put_schema(&variants[i].1, buf, pos);
                i += 1;
            }
            pos
        }
    }
}

/// Encode a [`ConstManifest`] into the fixed-size note: a 4-byte little-endian
/// payload length, the payload, then zero padding to [`MANIFEST_BYTES`]. `const`,
/// so it can initialize a `#[link_section]` static directly.
#[must_use]
pub const fn encode_manifest(manifest: &ConstManifest) -> [u8; MANIFEST_BYTES] {
    let mut buf = [0u8; MANIFEST_BYTES];
    let mut pos = 4;
    pos = match &manifest.input {
        Some(schema) => {
            let pos = put_tag(1, &mut buf, pos);
            put_schema(schema, &mut buf, pos)
        }
        None => put_tag(0, &mut buf, pos),
    };
    pos = put_schema(&manifest.output, &mut buf, pos);
    pos = put_u32(manifest.uses.len() as u32, &mut buf, pos);
    let mut i = 0;
    while i < manifest.uses.len() {
        pos = put_str(manifest.uses[i], &mut buf, pos);
        i += 1;
    }
    let prefix = ((pos - 4) as u32).to_le_bytes();
    buf[0] = prefix[0];
    buf[1] = prefix[1];
    buf[2] = prefix[2];
    buf[3] = prefix[3];
    buf
}

fn read_manifest_str(cur: &mut Cursor) -> Result<String, Error> {
    let len = cur.u32()?;
    let bytes = cur.take(len)?;
    core::str::from_utf8(bytes)
        .map(Into::into)
        .map_err(|_| Error::SchemaMismatch)
}

fn read_manifest_opt_str(cur: &mut Cursor) -> Result<Option<String>, Error> {
    match cur.array::<1>()?[0] {
        0 => Ok(None),
        1 => Ok(Some(read_manifest_str(cur)?)),
        _ => Err(Error::SchemaMismatch),
    }
}

fn read_manifest_schema(cur: &mut Cursor) -> Result<TypeSchema, Error> {
    let schema = match cur.array::<1>()?[0] {
        0 => TypeSchema::Bool,
        1 => TypeSchema::I8,
        2 => TypeSchema::I16,
        3 => TypeSchema::I32,
        4 => TypeSchema::I64,
        5 => TypeSchema::U8,
        6 => TypeSchema::U16,
        7 => TypeSchema::U32,
        8 => TypeSchema::U64,
        9 => TypeSchema::F32,
        10 => TypeSchema::F64,
        11 => TypeSchema::Str,
        12 => TypeSchema::Bytes,
        13 => TypeSchema::Seq(Box::new(read_manifest_schema(cur)?)),
        14 => {
            let type_name = read_manifest_str(cur)?;
            let count = cur.u32()?;
            let mut fields = Vec::new();
            for _ in 0..count {
                let name = read_manifest_opt_str(cur)?;
                fields.push((name, read_manifest_schema(cur)?));
            }
            TypeSchema::Product { type_name, fields }
        }
        15 => {
            let type_name = read_manifest_str(cur)?;
            let count = cur.u32()?;
            let mut variants = Vec::new();
            for _ in 0..count {
                let name = read_manifest_str(cur)?;
                variants.push((name, read_manifest_schema(cur)?));
            }
            TypeSchema::Sum { type_name, variants }
        }
        _ => return Err(Error::SchemaMismatch),
    };
    Ok(schema)
}

/// Decode a `.note.snitch.iface` note (as written by [`encode_manifest`]) back
/// into a runtime [`Manifest`] — what the host seed step does to populate the
/// `user.iface` xattr. The trailing zero padding is ignored.
pub fn decode_manifest(bytes: &[u8]) -> Result<Manifest, Error> {
    let mut cur = Cursor { bytes };
    cur.u32()?; // payload length: framing only — the body below is self-delimiting
    let input = match cur.array::<1>()?[0] {
        0 => None,
        1 => Some(read_manifest_schema(&mut cur)?),
        _ => return Err(Error::SchemaMismatch),
    };
    let output = read_manifest_schema(&mut cur)?;
    let uses_count = cur.u32()?;
    let mut uses = Vec::new();
    for _ in 0..uses_count {
        uses.push(read_manifest_str(&mut cur)?);
    }
    Ok(Manifest { input, output, uses })
}

/// The Plain-Old-Data primitive: [`Pod`], the zero-copy [`pod_bytes`], and
/// `#[derive(Pod)]` all live in the alloc-free [`hitch_pod`] leaf (so the `abi`
/// crate can use them without the value model). Re-exported here so `hitch` users
/// see them unchanged. The allocating inverse [`from_pod_bytes`] stays below.
pub use hitch_pod::{pod_bytes, Pod};

/// Copy `bytes` into a `Vec<T>` — the inverse of [`pod_bytes`]. Copies (so `bytes`
/// needs no particular alignment). Fails if `bytes` is not a whole number of `T`s.
pub fn from_pod_bytes<T: Pod>(bytes: &[u8]) -> Result<Vec<T>, Error> {
    let size = core::mem::size_of::<T>();
    if size == 0 || !bytes.len().is_multiple_of(size) {
        return Err(Error::SchemaMismatch);
    }
    let mut out = Vec::with_capacity(bytes.len() / size);
    for chunk in bytes.chunks_exact(size) {
        // SAFETY: `chunk` is exactly `size_of::<T>()` bytes and `T: Pod` accepts
        // any bit pattern; `read_unaligned` tolerates `bytes`' arbitrary alignment.
        out.push(unsafe { chunk.as_ptr().cast::<T>().read_unaligned() });
    }
    Ok(out)
}

/// `#[derive(Schema)]` from the `hitch-derive` crate, re-exported so a consumer
/// writes `#[derive(hitch::Schema)]` against this one dependency (the serde
/// convention). Behind the default `derive` feature.
#[cfg(feature = "derive")]
pub use hitch_derive::Schema;

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

/// Does `n` fit in `bytes` unsigned little-endian bytes? (`bytes` is 1/2/4/8.)
fn uint_fits(n: u64, bytes: usize) -> bool {
    bytes >= 8 || n < (1u64 << (bytes * 8))
}

/// Does `n` fit in `bytes` signed (two's-complement) bytes?
fn int_fits(n: i64, bytes: usize) -> bool {
    if bytes >= 8 {
        return true;
    }
    let limit = 1i64 << (bytes * 8 - 1);
    n >= -limit && n < limit
}

/// Write the low `bytes` little-endian bytes of `n`, erroring if it doesn't fit.
/// A fitting unsigned value's low bytes are its correct narrow representation.
fn push_uint(n: u64, bytes: usize, buf: &mut Vec<u8>) -> Result<(), Error> {
    if !uint_fits(n, bytes) {
        return Err(Error::SchemaMismatch);
    }
    buf.extend_from_slice(&n.to_le_bytes()[..bytes]);
    Ok(())
}

/// Write the low `bytes` little-endian bytes of `n` as a narrow signed integer,
/// erroring if it doesn't fit. The upper bytes a fitting value sheds are just sign
/// extension, so the low bytes are its correct two's-complement.
fn push_int(n: i64, bytes: usize, buf: &mut Vec<u8>) -> Result<(), Error> {
    if !int_fits(n, bytes) {
        return Err(Error::SchemaMismatch);
    }
    buf.extend_from_slice(&n.to_le_bytes()[..bytes]);
    Ok(())
}

/// Read `bytes` little-endian bytes as an unsigned value, zero-extended to `u64`.
fn read_uint(cur: &mut Cursor, bytes: usize) -> Result<u64, Error> {
    let raw = cur.take(bytes)?;
    let mut wide = [0u8; 8];
    wide[..bytes].copy_from_slice(raw);
    Ok(u64::from_le_bytes(wide))
}

/// Read `bytes` little-endian bytes as a signed value, sign-extended to `i64`.
fn read_int(cur: &mut Cursor, bytes: usize) -> Result<i64, Error> {
    let raw = cur.take(bytes)?;
    let fill = if raw[bytes - 1] & 0x80 != 0 { 0xFF } else { 0x00 };
    let mut wide = [fill; 8];
    wide[..bytes].copy_from_slice(raw);
    Ok(i64::from_le_bytes(wide))
}

fn pack(value: &Value, schema: &TypeSchema, buf: &mut Vec<u8>) -> Result<(), Error> {
    match (schema, value) {
        (TypeSchema::Bool, Value::Bool(b)) => {
            buf.push(u8::from(*b));
            Ok(())
        }
        (TypeSchema::U8, Value::U64(n)) => push_uint(*n, 1, buf),
        (TypeSchema::U16, Value::U64(n)) => push_uint(*n, 2, buf),
        (TypeSchema::U32, Value::U64(n)) => push_uint(*n, 4, buf),
        (TypeSchema::U64, Value::U64(n)) => push_uint(*n, 8, buf),
        (TypeSchema::I8, Value::I64(n)) => push_int(*n, 1, buf),
        (TypeSchema::I16, Value::I64(n)) => push_int(*n, 2, buf),
        (TypeSchema::I32, Value::I64(n)) => push_int(*n, 4, buf),
        (TypeSchema::I64, Value::I64(n)) => push_int(*n, 8, buf),
        (TypeSchema::F32, Value::F64(n)) => {
            buf.extend_from_slice(&(*n as f32).to_le_bytes());
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
        TypeSchema::U8 => Ok(Value::U64(read_uint(cur, 1)?)),
        TypeSchema::U16 => Ok(Value::U64(read_uint(cur, 2)?)),
        TypeSchema::U32 => Ok(Value::U64(read_uint(cur, 4)?)),
        TypeSchema::U64 => Ok(Value::U64(read_uint(cur, 8)?)),
        TypeSchema::I8 => Ok(Value::I64(read_int(cur, 1)?)),
        TypeSchema::I16 => Ok(Value::I64(read_int(cur, 2)?)),
        TypeSchema::I32 => Ok(Value::I64(read_int(cur, 4)?)),
        TypeSchema::I64 => Ok(Value::I64(read_int(cur, 8)?)),
        TypeSchema::F32 => Ok(Value::F64(f64::from(f32::from_le_bytes(cur.array::<4>()?)))),
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
