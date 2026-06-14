//! Abstract syntax tree. Grows one node per parser increment.

/// An expression.
#[derive(Debug, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A name in expression position — a variable reference.
    Var(String),
    /// A lambda placeholder before desugaring: `None` is bare `$`, `Some("a")`
    /// is `$a`. The parser rewrites these into a `Lambda` at the call argument
    /// that encloses them; a `Placeholder` surviving into a final AST is a bug.
    Placeholder(Option<String>),
    /// An infix operator application.
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// A prefix unary operator application.
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },
    /// A function/constructor call: `callee(args…)`.
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// Field access: `object.name`.
    Field {
        object: Box<Expr>,
        name: String,
    },
    /// Safe-navigation field access: `object?.name`.
    SafeField {
        object: Box<Expr>,
        name: String,
    },
    /// The try operator: `expr?`.
    Try(Box<Expr>),
    /// Indexing: `object[index]`.
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    /// A lambda: `x -> body`, `(a, b) -> body`, `() -> body`. Params are bare
    /// names for now (`_` to ignore); type annotations arrive with declarations.
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
    },
    /// The inline conditional `cond => then | els`.
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// An eager list literal: `[a, b, c]`.
    List(Vec<Expr>),
    /// An eager map literal: `[k: v, …]` (empty is `[:]`).
    Map(Vec<(Expr, Expr)>),
}

/// Prefix unary operators.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum UnOp {
    Neg, // -
    Not, // not
}

/// Infix binary operators.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Pipe,      // |>
    Range,     // ..
    RangeIncl, // ..=
}
