//! Stitch — a small, immutable-by-default managed language for `SnitchOS`.
//!
//! The library is `no_std` + `alloc` so the tree-walk interpreter can run on the
//! `SnitchOS` target (a userspace process with a heap), not just the dev host.
//! Under `cargo test` it builds with `std` (so the harness + `insta` work); real
//! builds and `riscv64gc-unknown-none-elf` are `no_std`. The host CLI (`main.rs`)
//! is a separate `std` binary depending on this lib.
//!
//! v0 is a tree-walk interpreter over a dynamically typed core (type annotations
//! are parsed but not checked). Surface grammar:
//! `plans/lang/01-grammar-and-precedence.md`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// The `alloc` essentials that `std`'s prelude provides for free but `no_std`'s
/// does not. Glob-imported per module so the interpreter source reads the same
/// as it did under `std`.
pub(crate) mod prelude {
    pub(crate) use alloc::boxed::Box;
    pub(crate) use alloc::rc::Rc;
    pub(crate) use alloc::string::{String, ToString};
    pub(crate) use alloc::vec::Vec;
    pub(crate) use alloc::{format, vec};
}

pub mod ast;
pub mod bridge;
pub mod env;
pub mod interp;
pub mod lexer;
pub mod line_edit;
pub mod natives;
pub mod ops;
pub mod parser;
pub mod pattern;
pub mod platform;
pub mod render;
pub mod registry;
pub mod runner;
pub mod telemetry;
pub mod value;
pub mod wire;

#[cfg(test)]
mod test_support;
