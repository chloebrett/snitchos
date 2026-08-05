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

/// The checkpoint the on-target server embeds — file stem, no extension, paired
/// with a `.vocab` of the same name.
///
/// Named here so host-side callers (the itest's byte-identity oracle, the
/// real-checkpoint test) don't each carry their own copy of the string. The server
/// binary still needs a *literal* path for `include_bytes!`, so one copy is
/// unavoidable — but a divergence between it and this one cannot hide: the itest
/// recomputes the completion from *this* stem and asserts byte-equality against what
/// the guest served, so embedding a different checkpoint fails the checksum rather
/// than passing quietly with the wrong weights.
pub const CANONICAL_CHECKPOINT: &str = "drivel-b9b10-30k";
