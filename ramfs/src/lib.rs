//! `RamFs` — the first `Filesystem` implementation: a RAM-backed, flat
//! single-root filesystem. Subdirectories will be `Unsupported` (additive
//! later — no trait change). Host-testable; no cap/IPC types.
//!
//! Scaffold only: the trait is wired up so the crate boundary is fixed;
//! method bodies are driven in via TDD.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

use fs_core::{DirEntry, Filesystem, FsError, InodeId, NodeKind, Stat};

/// A RAM-backed filesystem. Construct with [`RamFs::new`].
#[derive(Default)]
pub struct RamFs {}

impl RamFs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Filesystem for RamFs {
    fn root(&self) -> InodeId {
        todo!()
    }

    fn lookup(&self, _dir: InodeId, _name: &str) -> Result<InodeId, FsError> {
        todo!()
    }

    fn stat(&self, _ino: InodeId) -> Result<Stat, FsError> {
        todo!()
    }

    fn read(&self, _ino: InodeId, _off: u64, _buf: &mut [u8]) -> Result<usize, FsError> {
        todo!()
    }

    fn write(&mut self, _ino: InodeId, _off: u64, _data: &[u8]) -> Result<usize, FsError> {
        todo!()
    }

    fn create(&mut self, _dir: InodeId, _name: &str, _kind: NodeKind) -> Result<InodeId, FsError> {
        todo!()
    }

    fn remove(&mut self, _dir: InodeId, _name: &str) -> Result<(), FsError> {
        todo!()
    }

    fn readdir(&self, _dir: InodeId) -> Result<Vec<DirEntry>, FsError> {
        todo!()
    }
}
