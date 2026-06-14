//! `RamFs` — the first `Filesystem` implementation. A RAM-backed, flat
//! single-root filesystem. Subdirectories are `Unsupported` for now
//! (additive later — no trait change). Host-testable; no cap/IPC types.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use fs_core::{DirEntry, Filesystem, FsError, InodeId, NodeKind, Stat};

const ROOT: InodeId = InodeId::new(0);

enum Node {
    File(Vec<u8>),
    Dir(BTreeMap<String, InodeId>),
}

/// A RAM-backed filesystem. Construct with [`RamFs::new`]; an empty root
/// directory exists from the start.
pub struct RamFs {
    nodes: Vec<Node>,
}

impl Default for RamFs {
    fn default() -> Self {
        Self {
            nodes: alloc::vec![Node::Dir(BTreeMap::new())],
        }
    }
}

impl RamFs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn node(&self, ino: InodeId) -> Result<&Node, FsError> {
        self.nodes
            .get(ino.as_u32() as usize)
            .ok_or(FsError::NotFound)
    }
}

impl Filesystem for RamFs {
    fn root(&self) -> InodeId {
        ROOT
    }

    fn lookup(&self, dir: InodeId, name: &str) -> Result<InodeId, FsError> {
        match self.node(dir)? {
            Node::File(_) => Err(FsError::NotADir),
            Node::Dir(entries) => entries.get(name).copied().ok_or(FsError::NotFound),
        }
    }

    fn stat(&self, ino: InodeId) -> Result<Stat, FsError> {
        match self.node(ino)? {
            Node::Dir(_) => Ok(Stat {
                kind: NodeKind::Dir,
                size: 0,
            }),
            Node::File(data) => Ok(Stat {
                kind: NodeKind::File,
                size: data.len() as u64,
            }),
        }
    }

    fn read(&self, ino: InodeId, off: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let data = match self.node(ino)? {
            Node::Dir(_) => return Err(FsError::IsADir),
            Node::File(data) => data,
        };
        let off = off as usize;
        if off >= data.len() {
            return Ok(0);
        }
        let n = (data.len() - off).min(buf.len());
        buf[..n].copy_from_slice(&data[off..off + n]);
        Ok(n)
    }

    fn write(&mut self, ino: InodeId, off: u64, data: &[u8]) -> Result<usize, FsError> {
        let file = match self.nodes.get_mut(ino.as_u32() as usize) {
            None => return Err(FsError::NotFound),
            Some(Node::Dir(_)) => return Err(FsError::IsADir),
            Some(Node::File(file)) => file,
        };
        let off = off as usize;
        let end = off + data.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[off..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn create(&mut self, dir: InodeId, name: &str, kind: NodeKind) -> Result<InodeId, FsError> {
        if kind == NodeKind::Dir {
            return Err(FsError::Unsupported);
        }
        let ino = InodeId::new(self.nodes.len() as u32);
        self.nodes.push(Node::File(Vec::new()));
        if let Some(Node::Dir(entries)) = self.nodes.get_mut(dir.as_u32() as usize) {
            entries.insert(name.into(), ino);
        }
        Ok(ino)
    }

    fn remove(&mut self, dir: InodeId, name: &str) -> Result<(), FsError> {
        match self.nodes.get_mut(dir.as_u32() as usize) {
            None => Err(FsError::NotFound),
            Some(Node::File(_)) => Err(FsError::NotADir),
            Some(Node::Dir(entries)) => entries.remove(name).map(|_| ()).ok_or(FsError::NotFound),
        }
    }

    fn readdir(&self, dir: InodeId) -> Result<Vec<DirEntry>, FsError> {
        match self.node(dir)? {
            Node::File(_) => Err(FsError::NotADir),
            // Flat FS: every directory entry is a file (subdirs are
            // `Unsupported`). When hierarchy lands, resolve each entry's kind.
            Node::Dir(entries) => Ok(entries
                .iter()
                .map(|(name, &ino)| DirEntry {
                    name: name.clone(),
                    ino,
                    kind: NodeKind::File,
                })
                .collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_core::{Filesystem, NodeKind};

    #[test]
    fn fresh_fs_root_is_an_empty_directory() {
        let fs = RamFs::new();
        let root = fs.root();

        assert_eq!(fs.stat(root).unwrap().kind, NodeKind::Dir);
        assert!(fs.readdir(root).unwrap().is_empty());
    }

    #[test]
    fn create_file_adds_it_to_the_directory() {
        let mut fs = RamFs::new();
        let root = fs.root();

        let ino = fs.create(root, "notes.txt", NodeKind::File).unwrap();

        assert_eq!(fs.stat(ino).unwrap().kind, NodeKind::File);
        assert_eq!(fs.stat(ino).unwrap().size, 0);
        let entries = fs.readdir(root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "notes.txt");
        assert_eq!(entries[0].ino, ino);
        assert_eq!(entries[0].kind, NodeKind::File);
    }

    #[test]
    fn creating_a_subdirectory_is_unsupported_while_flat() {
        let mut fs = RamFs::new();
        let root = fs.root();

        assert_eq!(
            fs.create(root, "sub", NodeKind::Dir),
            Err(FsError::Unsupported)
        );
        assert!(fs.readdir(root).unwrap().is_empty());
    }

    #[test]
    fn write_grows_the_file_and_read_returns_the_bytes() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let ino = fs.create(root, "f.txt", NodeKind::File).unwrap();

        let written = fs.write(ino, 0, b"hello").unwrap();
        assert_eq!(written, 5);
        assert_eq!(fs.stat(ino).unwrap().size, 5);

        let mut buf = [0u8; 5];
        let read = fs.read(ino, 0, &mut buf).unwrap();
        assert_eq!(read, 5);
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn write_at_offset_overwrites_in_place_and_extends() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let ino = fs.create(root, "f.txt", NodeKind::File).unwrap();
        fs.write(ino, 0, b"hello").unwrap();

        fs.write(ino, 3, b"XYZ").unwrap();

        let mut buf = [0u8; 6];
        let read = fs.read(ino, 0, &mut buf).unwrap();
        assert_eq!(read, 6);
        assert_eq!(&buf, b"helXYZ");
    }

    #[test]
    fn read_past_end_returns_what_is_available() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let ino = fs.create(root, "f.txt", NodeKind::File).unwrap();
        fs.write(ino, 0, b"hi").unwrap();

        let mut buf = [0u8; 8];
        let read = fs.read(ino, 1, &mut buf).unwrap();
        assert_eq!(read, 1);
        assert_eq!(buf[0], b'i');
    }

    #[test]
    fn remove_unlinks_a_name() {
        let mut fs = RamFs::new();
        let root = fs.root();
        fs.create(root, "f.txt", NodeKind::File).unwrap();

        fs.remove(root, "f.txt").unwrap();

        assert_eq!(fs.lookup(root, "f.txt"), Err(FsError::NotFound));
        assert!(fs.readdir(root).unwrap().is_empty());
        assert_eq!(fs.remove(root, "f.txt"), Err(FsError::NotFound));
    }

    #[test]
    fn lookup_resolves_a_name_to_its_inode() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let ino = fs.create(root, "notes.txt", NodeKind::File).unwrap();

        assert_eq!(fs.lookup(root, "notes.txt"), Ok(ino));
        assert_eq!(fs.lookup(root, "missing.txt"), Err(FsError::NotFound));
    }
}
