//! Serving completions from a trained checkpoint, under the grammar.
//!
//! babble's opposite number: same request, same guarantee, a model instead of a
//! uniform walk. The guarantee is the part worth stating, because it is what does
//! *not* change when weights arrive — **a completion is always legal Stitch**. The
//! model chooses among continuations the oracle permits; it never decides what is
//! permitted.
//!
//! Host-testable throughout. `user/kvetch` does the target-side glue (embedding the
//! checkpoint, the receive loop) and links this for the thinking.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod model;
pub mod sample;
pub mod serve;
