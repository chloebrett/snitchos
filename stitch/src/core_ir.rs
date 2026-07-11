//! Core IR: the desugared, evaluator-facing tree the lowering pass produces from
//! the surface AST (`ast::Expr`). It is a strict subset of the surface shape —
//! the surface-only nodes (`Placeholder`, `OperatorRef`, `SubjectlessMatch`, and
//! `Stmt::Use`) are gone, folded into ordinary core nodes by lowering. Every node
//! carries the `Span` of the surface node it came from, so runtime faults can cite
//! `line:col`.
//!
//! Two deliberate shape changes from the surface AST:
//! - Lambda / function bodies are `Rc<CoreExpr>`, not `Box<Expr>` — a closure
//!   captures a shared code-ref rather than deep-cloning its body.
//! - `Pattern`, `Field`, `Variant`, `Param`, `Type`, and `MethodModifier` are
//!   reused from `ast` unchanged (they carry no surface-only expression nodes).

use core::fmt;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::{BinOp, Field, MethodModifier, Param, Pattern, Type, UnOp, Variant};
use crate::lexer::Span;

/// A core expression node: its shape plus the source span it lowered from. Like
/// `ast::Expr`, `PartialEq` compares only `kind` and `Debug` forwards to `kind`,
/// so spans are metadata that never leak into structural comparisons or snapshots.
#[derive(Clone)]
pub struct CoreExpr {
    pub kind: CoreExprKind,
    pub span: Span,
}

impl CoreExpr {
    #[must_use]
    pub fn new(kind: CoreExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

impl PartialEq for CoreExpr {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl fmt::Debug for CoreExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}

/// The shape of a core expression — `ast::ExprKind` minus the surface-only nodes.
#[derive(Debug, PartialEq, Clone)]
pub enum CoreExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Var(String),
    SelfRef,
    /// A spread `..base` — retained (construction needs it); not surface-only.
    Spread(Box<CoreExpr>),
    Binary {
        op: BinOp,
        left: Box<CoreExpr>,
        right: Box<CoreExpr>,
    },
    Unary {
        op: UnOp,
        operand: Box<CoreExpr>,
    },
    Call {
        callee: Box<CoreExpr>,
        args: Vec<CoreArg>,
    },
    Field {
        object: Box<CoreExpr>,
        name: String,
    },
    SafeField {
        object: Box<CoreExpr>,
        name: String,
    },
    Try(Box<CoreExpr>),
    Index {
        object: Box<CoreExpr>,
        index: Box<CoreExpr>,
    },
    /// A lambda — `body` is `Rc` so a closure shares the code rather than cloning.
    Lambda {
        params: Vec<String>,
        body: Rc<CoreExpr>,
    },
    Range {
        start: Option<Box<CoreExpr>>,
        end: Option<Box<CoreExpr>>,
        inclusive: bool,
    },
    If {
        cond: Box<CoreExpr>,
        then: Box<CoreExpr>,
        els: Box<CoreExpr>,
    },
    Tuple(Vec<CoreExpr>),
    List(Vec<CoreExpr>),
    Map(Vec<(CoreExpr, CoreExpr)>),
    Str(Vec<CoreStrSegment>),
    Block {
        stmts: Vec<CoreStmt>,
        result: Option<Box<CoreExpr>>,
    },
    Match {
        subject: Box<CoreExpr>,
        arms: Vec<CoreMatchArm>,
    },
    /// `handle <op> with <handler> { body }` — install a dynamically-scoped
    /// effect handler for `op` over the body's extent (see the surface node).
    Handle {
        op: String,
        handler: Box<CoreExpr>,
        body: Box<CoreExpr>,
    },
    /// `without <cap> { body }` — drop capability `cap` for the body's extent
    /// (attenuation; see the surface node).
    Without {
        cap: String,
        body: Box<CoreExpr>,
    },
}

/// A call argument in the core IR.
#[derive(Debug, PartialEq, Clone)]
pub struct CoreArg {
    pub label: Option<String>,
    pub value: CoreExpr,
}

/// One piece of a string literal: literal text or an interpolated expression.
#[derive(Debug, PartialEq, Clone)]
pub enum CoreStrSegment {
    Lit(String),
    Interp(Box<CoreExpr>),
}

/// A statement inside a core block — `Stmt::Use` is gone (desugared to a `Call`).
#[derive(Debug, PartialEq, Clone)]
pub enum CoreStmt {
    Let {
        name: String,
        mutable: bool,
        value: CoreExpr,
    },
    Assign {
        target: CoreExpr,
        value: CoreExpr,
    },
    Expr(CoreExpr),
}

/// One arm of a core `match`. `Pattern` is reused from the surface AST.
#[derive(Debug, PartialEq, Clone)]
pub struct CoreMatchArm {
    pub pattern: Pattern,
    pub guard: Option<CoreExpr>,
    pub body: CoreExpr,
}

/// A top-level declaration in the core IR — mirrors `ast::Item` with executable
/// bodies lowered to `CoreExpr`. Type metadata (`Field`, `Variant`, `Param`,
/// `Type`, generics) is reused from the surface AST unchanged.
#[derive(Debug, PartialEq, Clone)]
pub enum CoreItem {
    Prod {
        name: String,
        generics: Vec<String>,
        fields: Vec<Field>,
        public: bool,
    },
    Sum {
        name: String,
        generics: Vec<String>,
        variants: Vec<Variant>,
        public: bool,
    },
    Func {
        name: String,
        params: Vec<Param>,
        ret: Option<Type>,
        uses: Vec<String>,
        body: Rc<CoreExpr>,
        public: bool,
    },
    Contract {
        name: String,
        generics: Vec<String>,
        methods: Vec<CoreMethod>,
    },
    On {
        target: Type,
        contract: Option<Type>,
        methods: Vec<CoreMethod>,
    },
    Const {
        name: String,
        mutable: bool,
        value: CoreExpr,
        public: bool,
    },
    Use {
        module: String,
        names: Option<Vec<String>>,
    },
}

/// A method in the core IR — `body` lowered to `CoreExpr` (`None` for an abstract
/// contract signature).
#[derive(Debug, PartialEq, Clone)]
pub struct CoreMethod {
    pub name: String,
    pub modifier: MethodModifier,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    pub uses: Vec<String>,
    pub body: Option<CoreExpr>,
}
