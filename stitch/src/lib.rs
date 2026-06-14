//! Stitch — a small, immutable-by-default managed language for `SnitchOS`.
//!
//! Host-side implementation. v0 is a tree-walk interpreter over a dynamically
//! typed core (type annotations are parsed but not checked). Surface grammar:
//! `plans/lang/01-grammar-and-precedence.md`; build order:
//! `plans/lang/02-walking-skeleton.md`.

pub mod ast;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod value;
