//! The `Filesystem` trait (shipped in v0.10).
//!
//! Cap-agnostic and host-testable: this crate imports **no** capability or
//! IPC types, so it compiles and unit-tests on the host like `kernel-core`.
//! The trait speaks only of inodes and bytes; the meaning of a "file
//! capability" (badge packing, rights enforcement, cap minting on lookup)
//! lives entirely in the IPC connecting layer above it (`user/fs`).
//!
//! Design: [docs/filesystem-design.md](../../docs/filesystem-design.md).
//! Implementations live in sibling crates (`ramfs` is the first).

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Stable identity of a node, decoupled from any name it's reached by.
///
/// An inode *is* the file; a filename is a directory entry pointing at one.
/// This is what a File cap names (so it survives rename), and what the badge
/// packs (`inode: u32 | rights`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct InodeId(u32);

impl InodeId {
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw id, for badge packing in the connecting layer.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    File,
    Dir,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stat {
    pub kind: NodeKind,
    pub size: u64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    pub name: String,
    pub ino: InodeId,
    pub kind: NodeKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsError {
    NotFound,
    NotADir,
    IsADir,
    Exists,
    Unsupported,
    NameTooLong,
    /// The caller's File cap lacks a right the op requires (the rights gate).
    /// Enforced by the FS, never the kernel — the kernel carries the badge's
    /// rights but never interprets them.
    Denied,
    /// The server couldn't complete the op for an internal/transport reason —
    /// a cross-AS copy failed, a cap mint failed, or the request didn't decode.
    /// Distinct from [`Unsupported`](Self::Unsupported), which means the op
    /// itself isn't implemented for that inode.
    Internal,
}

/// Inode-addressed storage operations. No `open`/`close` — opening is a
/// *capability* operation (minting a File cap), not a storage one. Ops are
/// stateless: the offset is passed in, no cursor lives in the FS.
///
/// `&self` for reads, `&mut self` for mutations — the server owns the FS
/// single-threaded behind its receive loop.
pub trait Filesystem {
    fn root(&self) -> InodeId;
    fn lookup(&self, dir: InodeId, name: &str) -> Result<InodeId, FsError>;
    fn stat(&self, ino: InodeId) -> Result<Stat, FsError>;
    fn read(&self, ino: InodeId, off: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, ino: InodeId, off: u64, data: &[u8]) -> Result<usize, FsError>;
    fn create(&mut self, dir: InodeId, name: &str, kind: NodeKind) -> Result<InodeId, FsError>;
    fn remove(&mut self, dir: InodeId, name: &str) -> Result<(), FsError>;
    fn readdir(&self, dir: InodeId) -> Result<Vec<DirEntry>, FsError>;

    /// Read an inode-attached extended attribute (e.g. `user.iface`, the typed
    /// interface a program's `.snitch.iface` note is lifted into). A filesystem
    /// with no xattr support returns [`FsError::Unsupported`]; a missing name is
    /// [`FsError::NotFound`]. Default: unsupported.
    fn getxattr(&self, _ino: InodeId, _name: &str) -> Result<Vec<u8>, FsError> {
        Err(FsError::Unsupported)
    }

    /// Set (create or overwrite) an extended attribute on an inode. Inode-attached,
    /// so it moves with the file under rename, and needs only the file itself — not
    /// its parent directory. Default: unsupported.
    fn setxattr(&mut self, _ino: InodeId, _name: &str, _value: &[u8]) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
}
