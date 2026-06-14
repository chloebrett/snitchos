//! Abstract syntax tree. Grows one node per parser increment.

/// An expression.
#[derive(Debug, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A name in expression position — a variable reference.
    Var(String),
    /// An infix operator application.
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
}

/// Infix binary operators. Grows as more of the §2 precedence table lands.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}
