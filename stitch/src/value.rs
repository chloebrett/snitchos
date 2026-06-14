//! Runtime values and errors for the tree-walk interpreter. v0 is dynamically
//! typed: a `Value` carries its own kind, and operations check kinds at runtime
//! (no implicit Int/Float coercion — that previews the eventual static types).

use std::fmt;
use std::rc::Rc;

use crate::ast::Expr;
use crate::env::Env;

/// A value produced by evaluating an expression.
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// The unit value `()` — what a block with no trailing expression, and an
    /// expression evaluated only for effect, produce.
    Unit,
    /// A function: its parameters, body, and the environment it closed over,
    /// bundled into one shared heap object (cheap to pass around — cloning a
    /// `Value::Closure` just bumps the `Rc`).
    Closure(Rc<ClosureData>),
}

/// The captured contents of a closure: parameter names, the body to evaluate on
/// each call, and the lexical environment in effect where the lambda appeared.
pub struct ClosureData {
    pub params: Vec<String>,
    pub body: Expr,
    pub env: Env,
}

impl Value {
    /// The kind name, for error messages (`"Int"`, `"Function"`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::Unit => "Unit",
            Value::Closure(_) => "Function",
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "Int({n})"),
            Value::Float(x) => write!(f, "Float({x})"),
            Value::Bool(b) => write!(f, "Bool({b})"),
            Value::Unit => write!(f, "Unit"),
            Value::Closure(c) => write!(f, "Closure/{}", c.params.len()),
        }
    }
}

impl PartialEq for Value {
    /// Primitives compare by value; functions compare by identity (two closures
    /// are equal only if they are the same object) — functions have no
    /// structural equality.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            _ => false,
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
