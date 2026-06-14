//! Runtime values and errors for the tree-walk interpreter. v0 is dynamically
//! typed: a `Value` carries its own kind, and operations check kinds at runtime
//! (no implicit Int/Float coercion — that previews the eventual static types).

/// A value produced by evaluating an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Value {
    /// The kind name, for error messages (`"Int"`, `"Float"`, `"Bool"`).
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
        }
    }
}

/// A runtime (evaluation) error. Carries a human-readable message, mirroring
/// `ParseError`; structured variants can come later if call sites need them.
#[derive(Debug, PartialEq)]
pub struct RuntimeError {
    pub message: String,
}

impl RuntimeError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
