//! Abstract syntax tree. Grows one node per parser increment.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

/// A top-level declaration in a program.
#[derive(Debug, PartialEq, Clone)]
pub enum Item {
    /// `prod Name<generics>(fields)` — a product type. `public` is the `pub`
    /// marker: items are private to their module unless exported.
    Prod {
        name: String,
        generics: Vec<String>,
        fields: Vec<Field>,
        public: bool,
    },
    /// `sum Name<generics> = variant | …` — a sum type.
    Sum {
        name: String,
        generics: Vec<String>,
        variants: Vec<Variant>,
        public: bool,
    },
    /// A function: `name(params) -> Ret? (uses Cap, …)? body`. `uses` is the
    /// capability/effects clause — the authority the body is permitted to
    /// exercise (e.g. `uses Telemetry` to call `emit`/`span`). Empty when
    /// omitted.
    Func {
        name: String,
        params: Vec<Param>,
        ret: Option<Type>,
        uses: Vec<String>,
        body: Expr,
        public: bool,
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
    /// `let name = value` / `let mut name = value` at module scope — a top-level
    /// constant. (Type annotations on bindings are deferred, as for `Stmt::Let`.)
    Const {
        name: String,
        mutable: bool,
        value: Expr,
        public: bool,
    },
    /// `use M` brings module `M` into scope (reach members by path, `M.member`);
    /// `use M.{a, b}` binds the named exported members unqualified. `names` is
    /// `None` for a whole-module import, `Some` for a selection.
    Use {
        module: String,
        names: Option<Vec<String>>,
    },
}

/// A method, shared by `contract` signatures and (later) `on` blocks.
/// `body` is `None` for an abstract contract signature, `Some` for a default
/// or concrete method.
#[derive(Debug, PartialEq, Clone)]
pub struct Method {
    pub name: String,
    pub modifier: MethodModifier,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// The capability/effects clause (`uses Telemetry`, …) — the authority the
    /// method body may exercise. Empty when omitted; same gate as a function.
    pub uses: Vec<String>,
    pub body: Option<Expr>,
}

/// A method's relationship to the receiver `@`.
#[derive(Debug, PartialEq, Clone)]
pub enum MethodModifier {
    /// Bare: an instance method with an immutable `@`.
    Instance,
    /// `mut`: may mutate `@`.
    Mut,
    /// `free`: no receiver (associated function).
    Free,
}

/// A function parameter: `name` or `name: Type` (type optional in v0).
#[derive(Debug, PartialEq, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
}

/// A call argument: a value, optionally with a `label:` (Swift-style).
#[derive(Debug, PartialEq, Clone)]
pub struct Arg {
    pub label: Option<String>,
    pub value: Expr,
}

/// A field of a product or a variant. `name` is `None` for positional fields
/// (`Celsius(Int)`, `Some(T)`); `Some` for named fields (`Point(x: Int)`).
/// `public` is the per-field `ext` mark: on an exported type, fields are private
/// (the representation is hidden) unless marked — a fully transparent type marks
/// every field, an opaque one marks none.
#[derive(Debug, PartialEq, Clone)]
pub struct Field {
    pub name: Option<String>,
    pub mutable: bool,
    pub ty: Type,
    pub public: bool,
}

/// A sum variant: a name and zero or more fields.
#[derive(Debug, PartialEq, Clone)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
}

/// A type annotation. Parsed but not checked in v0.
#[derive(Debug, PartialEq, Clone)]
pub enum Type {
    /// `Int`, `List<Int>`, `Maybe<T>`, `Result<T, E>`.
    Name { name: String, args: Vec<Type> },
    /// `A -> B` (right-associative). A multi-param function type `(A, B) -> C`
    /// is `Func { param: Tuple([A, B]), ret: C }`.
    Func { param: Box<Type>, ret: Box<Type> },
    /// `(A, B, …)` — a tuple type. `()` is the unit type (empty tuple).
    Tuple(Vec<Type>),
    /// `@` — the self-type, the receiver's own type. Only meaningful inside an
    /// `on`/`contract` method signature (`unwrap() -> @`). Parsed but not checked
    /// in v0; gating + meaning arrive with the type system.
    SelfType,
}

/// An expression.
#[derive(Debug, PartialEq, Clone)]
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
    /// A range `start..end` / `start..=end`, lazy (`Seq<Int>`). Either end may
    /// be absent: `n..` (open from), `..n` / `..=n` (open to), `..` (full).
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
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
    /// `match subject { arm* }` (subject form).
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `match { cond => body … _ => default }` — a condition table. Each arm
    /// is `(condition, body)`; `default` is the mandatory catch-all body.
    /// Lowered to nested `Expr::If` chains before evaluation.
    SubjectlessMatch {
        arms: Vec<(Expr, Expr)>,
        default: Box<Expr>,
    },
}

/// One arm of a `match`: `pattern (if guard)? => body`.
#[derive(Debug, PartialEq, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

/// A match pattern.
#[derive(Debug, PartialEq, Clone)]
pub enum Pattern {
    /// `_` — matches anything, binds nothing.
    Wildcard,
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A plain string literal pattern. Interpolation is rejected at parse time
    /// — a pattern matches a fixed value, so `"{x}"` has no meaning here.
    Str(String),
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
#[derive(Debug, PartialEq, Clone)]
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
#[derive(Debug, PartialEq, Clone)]
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
    CrossPipe, // ~> (cross-process pipe; same precedence as `|>`)
    Range,     // ..
    RangeIncl, // ..=
}
