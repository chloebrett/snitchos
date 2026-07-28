//! `kvetch-drivel-server` — the completion server, backed by **weights**.
//!
//! Rung 1 of the ladder where `kvetch-server` (its sibling binary) is rung 0: same
//! endpoint, same protocol, same guarantee that a completion is legal Stitch. The
//! only difference is where the opinion comes from, which is what makes the two
//! comparable on the same prompt.
//!
//! The checkpoint is embedded rather than read from the filesystem because this
//! process spends its one endpoint slot on *receiving* completion requests — reading
//! a file would need a second capability, which is the manifest work. Both halves are
//! committed to git (see `.gitignore`): once weights are inside a program the kernel
//! boots, they stop being a training by-product and become a contract.

#![no_std]
#![no_main]

use snitchos_user::entry;

/// The blessed pair. They travel together and the checkpoint carries the vocab's
/// fingerprint, so a half-updated pair is refused at startup rather than served as
/// fluent nonsense.
const CHECKPOINT: &[u8] = include_bytes!("../../../../checkpoints/drivel-all-30k.kvetch");
const VOCAB: &[u8] = include_bytes!("../../../../checkpoints/drivel-all-30k.vocab");

#[entry]
fn main() {
    kvetch::serve_model(CHECKPOINT, VOCAB)
}
