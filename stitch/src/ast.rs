//! Abstract syntax tree. Grows one node per parser increment.

/// An expression.
#[derive(Debug, PartialEq)]
pub enum Expr {
    Int(i64),
}
