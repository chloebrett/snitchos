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

    /// A filesystem pre-populated from the build-time fs-image seed. Each entry is
    /// a `(path, bytes)` pair where `path` may contain `/` — intermediate
    /// directories are created `mkdir -p` style (and shared across entries), so
    /// `"bin/view"` lands a file `view` inside a directory `bin`. A path with no
    /// `/` is just a root-level file.
    #[must_use]
    pub fn seeded(files: &[(&str, &[u8])]) -> Self {
        let mut fs = Self::new();
        for (path, bytes) in files {
            let mut dir = fs.root();
            let mut parts = path.split('/').filter(|p| !p.is_empty()).peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_some() {
                    // Intermediate component: reuse the dir if it exists, else mkdir.
                    dir = match fs.lookup(dir, part) {
                        Ok(existing) => existing,
                        Err(_) => fs
                            .create(dir, part, NodeKind::Dir)
                            .expect("seed: mkdir on a fresh ramfs cannot fail"),
                    };
                } else {
                    // Leaf component: the file itself.
                    let ino = fs
                        .create(dir, part, NodeKind::File)
                        .expect("seed: create on a fresh ramfs cannot fail");
                    fs.write(ino, 0, bytes)
                        .expect("seed: write to a just-created file cannot fail");
                }
            }
        }
        fs
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
        // The parent must be a directory — otherwise there's nothing to add to.
        match self.node(dir)? {
            Node::Dir(_) => {}
            Node::File(_) => return Err(FsError::NotADir),
        }
        let ino = InodeId::new(self.nodes.len() as u32);
        self.nodes.push(match kind {
            NodeKind::File => Node::File(Vec::new()),
            NodeKind::Dir => Node::Dir(BTreeMap::new()),
        });
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
            Node::Dir(entries) => Ok(entries
                .iter()
                .map(|(name, &ino)| DirEntry {
                    name: name.clone(),
                    ino,
                    kind: match self.nodes.get(ino.as_u32() as usize) {
                        Some(Node::Dir(_)) => NodeKind::Dir,
                        _ => NodeKind::File,
                    },
                })
                .collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_core::{Filesystem, FsError, NodeKind};

    #[test]
    fn seeded_prepopulates_files_readable_from_the_root() {
        let fs = RamFs::seeded(&[("primes.st", b"isPrime"), ("notes.txt", b"hello")]);
        let root = fs.root();

        let ino = fs.lookup(root, "primes.st").unwrap();
        let mut buf = [0u8; 7];
        assert_eq!(fs.read(ino, 0, &mut buf).unwrap(), 7);
        assert_eq!(&buf, b"isPrime");
        assert_eq!(fs.readdir(root).unwrap().len(), 2);
    }

    #[test]
    fn seeded_creates_nested_paths_with_mkdir_p() {
        let fs = RamFs::seeded(&[
            ("bin/view", b"ELF1"),
            ("bin/cat", b"ELF2"),
            ("notes", b"hi"),
        ]);
        let root = fs.root();

        // `bin` is a directory holding both programs (shared intermediate).
        let bin = fs.lookup(root, "bin").unwrap();
        assert_eq!(fs.stat(bin).unwrap().kind, NodeKind::Dir);
        assert_eq!(fs.readdir(bin).unwrap().len(), 2);

        // The leaf file is readable through the path.
        let view = fs.lookup(bin, "view").unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(fs.read(view, 0, &mut buf).unwrap(), 4);
        assert_eq!(&buf, b"ELF1");

        // A root-level (no-slash) entry still works alongside nested ones.
        assert_eq!(fs.stat(fs.lookup(root, "notes").unwrap()).unwrap().kind, NodeKind::File);
    }

    #[test]
    fn create_makes_a_subdirectory_that_nests() {
        let mut fs = RamFs::new();
        let root = fs.root();

        let bin = fs.create(root, "bin", NodeKind::Dir).unwrap();
        assert_eq!(fs.stat(bin).unwrap().kind, NodeKind::Dir);

        // A file created inside the subdirectory resolves through it.
        let view = fs.create(bin, "view", NodeKind::File).unwrap();
        fs.write(view, 0, b"ELF").unwrap();
        assert_eq!(fs.lookup(root, "bin").unwrap(), bin);
        assert_eq!(fs.lookup(bin, "view").unwrap(), view);

        // readdir lists each directory's own entries, not the whole tree.
        let bin_entries = fs.readdir(bin).unwrap();
        assert_eq!(bin_entries.len(), 1);
        assert_eq!(bin_entries[0].name, "view");
        assert_eq!(bin_entries[0].kind, NodeKind::File);

        let root_entries = fs.readdir(root).unwrap();
        assert_eq!(root_entries.len(), 1);
        assert_eq!(root_entries[0].name, "bin");
        assert_eq!(root_entries[0].kind, NodeKind::Dir);
    }

    #[test]
    fn creating_inside_a_file_is_not_a_directory() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let f = fs.create(root, "f", NodeKind::File).unwrap();

        assert_eq!(fs.create(f, "x", NodeKind::File), Err(FsError::NotADir));
    }

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
