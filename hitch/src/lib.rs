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

/// Decoding a hitch failed — the bytes were not a valid encoding of a [`Value`].
/// Wraps the underlying codec error without exposing it in the public API.
#[derive(Debug)]
pub struct Error(postcard::Error);

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid hitch encoding: {}", self.0)
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
    postcard::from_bytes(bytes).map_err(Error)
}
