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
    /// A string literal, as a sequence of literal text and `{expr}`
    /// interpolations. A plain string is a single `Lit` segment.
    Str(Vec<StrSegment>),
    /// A block: zero or more statements then an optional result expression.
    /// The block evaluates to `result` (or unit if absent).
    Block {
        stmts: Vec<Stmt>,
        result: Option<Box<Expr>>,
    },
    /// `match subject { arm* }` (subject form; subjectless is a later increment).
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
    },
}

/// One arm of a `match`: `pattern (if guard)? => body`.
#[derive(Debug, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

/// A match pattern.
#[derive(Debug, PartialEq)]
pub enum Pattern {
    /// `_` — matches anything, binds nothing.
    Wildcard,
    Int(i64),
    Bool(bool),
    /// A lowercase name — matches anything and binds it.
    Binding(String),
    /// `Name` or `Name(sub, …)` — a sum variant / product destructure.
    Constructor { name: String, args: Vec<Pattern> },
    /// `(a, b, …)` — a tuple destructure.
    Tuple(Vec<Pattern>),
    /// `a | b | …` — matches if any alternative matches.
    Or(Vec<Pattern>),
}

/// A statement inside a block.
#[derive(Debug, PartialEq)]
pub enum Stmt {
    /// `let name = value` / `let mut name = value` (type annotations later).
    Let {
        name: String,
        mutable: bool,
        value: Expr,
    },
    /// An expression evaluated for its effect.
    Expr(Expr),
}

/// One piece of a string literal in the AST.
#[derive(Debug, PartialEq)]
pub enum StrSegment {
    Lit(String),
    Interp(Box<Expr>),
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
