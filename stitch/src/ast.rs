//! Abstract syntax tree. Grows one node per parser increment.

/// A top-level declaration in a program.
#[derive(Debug, PartialEq)]
pub enum Item {
    /// `prod Name<generics>(fields)` — a product type.
    Prod {
        name: String,
        generics: Vec<String>,
        fields: Vec<Field>,
    },
    /// `sum Name<generics> = variant | …` — a sum type.
    Sum {
        name: String,
        generics: Vec<String>,
        variants: Vec<Variant>,
    },
    /// A function: `name(params) -> Ret? body`. (No `fn` keyword; the `uses`
    /// effects clause is deferred.)
    Func {
        name: String,
        params: Vec<Param>,
        ret: Option<Type>,
        body: Expr,
    },
    /// `contract Name<generics> { method-signatures }` — a behavior contract.
    Contract {
        name: String,
        generics: Vec<String>,
        methods: Vec<Method>,
    },
    /// `on Type { … }` or `on Type : Contract { … }` — methods on a type,
    /// optionally declaring conformance to a contract.
    On {
        target: Type,
        contract: Option<Type>,
        methods: Vec<Method>,
    },
}

/// A method, shared by `contract` signatures and (later) `on` blocks.
/// `body` is `None` for an abstract contract signature, `Some` for a default
/// or concrete method.
#[derive(Debug, PartialEq)]
pub struct Method {
    pub name: String,
    pub modifier: MethodModifier,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub body: Option<Expr>,
}

/// A method's relationship to the receiver `@`.
#[derive(Debug, PartialEq)]
pub enum MethodModifier {
    /// Bare: an instance method with an immutable `@`.
    Instance,
    /// `mut`: may mutate `@`.
    Mut,
    /// `free`: no receiver (associated function).
    Free,
}

/// A function parameter: `name` or `name: Type` (type optional in v0).
#[derive(Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
}

/// A call argument: a value, optionally with a `label:` (Swift-style).
#[derive(Debug, PartialEq)]
pub struct Arg {
    pub label: Option<String>,
    pub value: Expr,
}

/// A field of a product or a variant. `name` is `None` for positional fields
/// (`Celsius(Int)`, `Some(T)`); `Some` for named fields (`Point(x: Int)`).
#[derive(Debug, PartialEq)]
pub struct Field {
    pub name: Option<String>,
    pub mutable: bool,
    pub ty: Type,
}

/// A sum variant: a name and zero or more fields.
#[derive(Debug, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
}

/// A type annotation. Parsed but not checked in v0.
#[derive(Debug, PartialEq)]
pub enum Type {
    /// `Int`, `List<Int>`, `Maybe<T>`, `Result<T, E>`.
    Name { name: String, args: Vec<Type> },
    /// `A -> B` (right-associative). Multi-param/tuple types are deferred.
    Func { param: Box<Type>, ret: Box<Type> },
}

/// An expression.
#[derive(Debug, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A name in expression position — a variable reference.
    Var(String),
    /// The method receiver `@` (self).
    SelfRef,
    /// A spread `..base` — only valid as a construction/call argument (the
    /// functional-update base in `Point(..p, x: 10)`).
    Spread(Box<Expr>),
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
        args: Vec<Arg>,
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
    /// A tuple — the anonymous product: `(a, b, …)`. `()` is the unit tuple.
    Tuple(Vec<Expr>),
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
    /// An assignment `target = value` — `target` is an lvalue (`@x`, a `mut`
    /// local); validity is checked at eval time, not parse time.
    Assign { target: Expr, value: Expr },
    /// `use binding? <- call` — Gleam-style callback sugar. The rest of the
    /// enclosing block becomes the callback (desugared at eval time).
    Use { binding: Option<String>, call: Expr },
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
