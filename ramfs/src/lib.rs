//! `RamFs` — the first `Filesystem` implementation. A RAM-backed filesystem
//! with a real inode tree: nodes are `File` or `Dir` (name→inode), so
//! subdirectories are fully supported — `create` makes files or dirs in any
//! dir, `seeded_with_xattrs` does `mkdir -p` per path segment, and callers walk
//! multi-segment paths one `lookup` at a time. (There is no `truncate`: `write`
//! only grows/overwrites.) Host-testable; no cap/IPC types.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use fs_core::{DirEntry, Filesystem, FsError, InodeId, NodeKind, Stat};

const ROOT: InodeId = InodeId::new(0);

/// One entry in an xattr-carrying seed image: `(path, content, xattrs)`, where
/// each xattr is a `(name, value)` pair. A program's entry is its ELF bytes plus a
/// `("user.iface", <manifest note>)` xattr.
pub type SeedEntry<'a> = (&'a str, &'a [u8], &'a [(&'a str, &'a [u8])]);

/// An inode: its content (file bytes or directory entries) plus its extended
/// attributes. Keeping `xattrs` *on* the node makes them inode-attached — they
/// live and die with the inode, and move with it under rename by construction.
struct Node {
    body: Body,
    xattrs: BTreeMap<String, Vec<u8>>,
}

enum Body {
    File(Vec<u8>),
    Dir(BTreeMap<String, InodeId>),
}

impl Node {
    fn new(body: Body) -> Self {
        Self {
            body,
            xattrs: BTreeMap::new(),
        }
    }
}

/// A RAM-backed filesystem. Construct with [`RamFs::new`]; an empty root
/// directory exists from the start.
pub struct RamFs {
    nodes: Vec<Node>,
}

impl Default for RamFs {
    fn default() -> Self {
        Self {
            nodes: alloc::vec![Node::new(Body::Dir(BTreeMap::new()))],
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
            fs.insert_file(path, bytes);
        }
        fs
    }

    /// Like [`seeded`](Self::seeded), but each entry also carries a set of
    /// `(name, value)` extended attributes applied to the leaf file — this is how
    /// a program's `.snitch.iface` note lands in its `user.iface` xattr at seed
    /// time (the manifest the shell reads before spawning).
    #[must_use]
    pub fn seeded_with_xattrs(files: &[SeedEntry]) -> Self {
        let mut fs = Self::new();
        for (path, bytes, xattrs) in files {
            let ino = fs.insert_file(path, bytes);
            for (name, value) in *xattrs {
                fs.setxattr(ino, name, value)
                    .expect("seed: setxattr on a just-created inode cannot fail");
            }
        }
        fs
    }

    /// Create `path` (mkdir-p on intermediates), write `bytes` to the leaf file,
    /// and return the leaf's inode. Infallible on a freshly-built seed fs.
    fn insert_file(&mut self, path: &str, bytes: &[u8]) -> InodeId {
        let mut dir = self.root();
        let mut leaf = dir;
        let mut parts = path.split('/').filter(|p| !p.is_empty()).peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                // Intermediate component: reuse the dir if it exists, else mkdir.
                dir = match self.lookup(dir, part) {
                    Ok(existing) => existing,
                    Err(_) => self
                        .create(dir, part, NodeKind::Dir)
                        .expect("seed: mkdir on a fresh ramfs cannot fail"),
                };
            } else {
                // Leaf component: the file itself.
                leaf = self
                    .create(dir, part, NodeKind::File)
                    .expect("seed: create on a fresh ramfs cannot fail");
                self.write(leaf, 0, bytes)
                    .expect("seed: write to a just-created file cannot fail");
            }
        }
        leaf
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
        match &self.node(dir)?.body {
            Body::File(_) => Err(FsError::NotADir),
            Body::Dir(entries) => entries.get(name).copied().ok_or(FsError::NotFound),
        }
    }

    fn stat(&self, ino: InodeId) -> Result<Stat, FsError> {
        match &self.node(ino)?.body {
            Body::Dir(_) => Ok(Stat {
                kind: NodeKind::Dir,
                size: 0,
            }),
            Body::File(data) => Ok(Stat {
                kind: NodeKind::File,
                size: data.len() as u64,
            }),
        }
    }

    fn read(&self, ino: InodeId, off: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let data = match &self.node(ino)?.body {
            Body::Dir(_) => return Err(FsError::IsADir),
            Body::File(data) => data,
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
        let node = self
            .nodes
            .get_mut(ino.as_u32() as usize)
            .ok_or(FsError::NotFound)?;
        let file = match &mut node.body {
            Body::Dir(_) => return Err(FsError::IsADir),
            Body::File(file) => file,
        };
        let off = off as usize;
        let end = off + data.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[off..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn truncate(&mut self, ino: InodeId, len: u64) -> Result<(), FsError> {
        let node = self
            .nodes
            .get_mut(ino.as_u32() as usize)
            .ok_or(FsError::NotFound)?;
        match &mut node.body {
            Body::Dir(_) => Err(FsError::IsADir),
            Body::File(file) => {
                file.resize(len as usize, 0); // shrink drops trailing; grow zero-fills
                Ok(())
            }
        }
    }

    fn create(&mut self, dir: InodeId, name: &str, kind: NodeKind) -> Result<InodeId, FsError> {
        // The parent must be a directory — otherwise there's nothing to add to.
        match &self.node(dir)?.body {
            Body::Dir(_) => {}
            Body::File(_) => return Err(FsError::NotADir),
        }
        let ino = InodeId::new(self.nodes.len() as u32);
        self.nodes.push(Node::new(match kind {
            NodeKind::File => Body::File(Vec::new()),
            NodeKind::Dir => Body::Dir(BTreeMap::new()),
        }));
        if let Some(Node {
            body: Body::Dir(entries),
            ..
        }) = self.nodes.get_mut(dir.as_u32() as usize)
        {
            entries.insert(name.into(), ino);
        }
        Ok(ino)
    }

    fn remove(&mut self, dir: InodeId, name: &str) -> Result<(), FsError> {
        match self.nodes.get_mut(dir.as_u32() as usize) {
            None => Err(FsError::NotFound),
            Some(node) => match &mut node.body {
                Body::File(_) => Err(FsError::NotADir),
                Body::Dir(entries) => entries.remove(name).map(|_| ()).ok_or(FsError::NotFound),
            },
        }
    }

    fn readdir(&self, dir: InodeId) -> Result<Vec<DirEntry>, FsError> {
        match &self.node(dir)?.body {
            Body::File(_) => Err(FsError::NotADir),
            Body::Dir(entries) => Ok(entries
                .iter()
                .map(|(name, &ino)| DirEntry {
                    name: name.clone(),
                    ino,
                    kind: match self.nodes.get(ino.as_u32() as usize) {
                        Some(Node {
                            body: Body::Dir(_),
                            ..
                        }) => NodeKind::Dir,
                        _ => NodeKind::File,
                    },
                })
                .collect()),
        }
    }

    fn getxattr(&self, ino: InodeId, name: &str) -> Result<Vec<u8>, FsError> {
        self.node(ino)?
            .xattrs
            .get(name)
            .cloned()
            .ok_or(FsError::NotFound)
    }

    fn setxattr(&mut self, ino: InodeId, name: &str, value: &[u8]) -> Result<(), FsError> {
        let node = self
            .nodes
            .get_mut(ino.as_u32() as usize)
            .ok_or(FsError::NotFound)?;
        node.xattrs.insert(name.into(), value.into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_core::{Filesystem, FsError, InodeId, NodeKind};

    #[test]
    fn setxattr_then_getxattr_round_trips() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let prog = fs.create(root, "prog", NodeKind::File).unwrap();

        fs.setxattr(prog, "user.iface", b"manifest-bytes").unwrap();

        assert_eq!(fs.getxattr(prog, "user.iface").unwrap(), b"manifest-bytes");
    }

    #[test]
    fn getxattr_of_a_missing_name_is_not_found() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let prog = fs.create(root, "prog", NodeKind::File).unwrap();

        assert_eq!(fs.getxattr(prog, "user.iface"), Err(FsError::NotFound));
    }

    #[test]
    fn setxattr_overwrites_and_is_per_inode() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let a = fs.create(root, "a", NodeKind::File).unwrap();
        let b = fs.create(root, "b", NodeKind::File).unwrap();

        fs.setxattr(a, "k", b"1").unwrap();
        fs.setxattr(a, "k", b"2").unwrap();

        assert_eq!(fs.getxattr(a, "k").unwrap(), b"2", "overwrites in place");
        assert_eq!(fs.getxattr(b, "k"), Err(FsError::NotFound), "xattrs are per-inode");
    }

    #[test]
    fn a_directory_can_carry_xattrs_too() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let dir = fs.create(root, "d", NodeKind::Dir).unwrap();

        fs.setxattr(dir, "user.type", b"dir").unwrap();

        assert_eq!(fs.getxattr(dir, "user.type").unwrap(), b"dir");
    }

    #[test]
    fn xattr_ops_on_a_missing_inode_error() {
        let mut fs = RamFs::new();
        assert_eq!(fs.setxattr(InodeId::new(99), "k", b"v"), Err(FsError::NotFound));
        assert_eq!(fs.getxattr(InodeId::new(99), "k"), Err(FsError::NotFound));
    }

    #[test]
    fn seeded_with_xattrs_applies_them_to_the_leaf_file() {
        let fs = RamFs::seeded_with_xattrs(&[
            ("bin/prog", b"ELF", &[("user.iface", b"manifest-bytes")]),
            ("plain", b"data", &[]),
        ]);
        let root = fs.root();

        let bin = fs.lookup(root, "bin").unwrap();
        let prog = fs.lookup(bin, "prog").unwrap();
        assert_eq!(fs.getxattr(prog, "user.iface").unwrap(), b"manifest-bytes");

        // The content is still there alongside the xattr.
        let mut buf = [0u8; 3];
        assert_eq!(fs.read(prog, 0, &mut buf).unwrap(), 3);
        assert_eq!(&buf, b"ELF");

        // A file seeded with no xattrs has none.
        let plain = fs.lookup(root, "plain").unwrap();
        assert_eq!(fs.getxattr(plain, "user.iface"), Err(FsError::NotFound));
    }

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
    fn truncate_shrinks_a_file_dropping_trailing_bytes() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let ino = fs.create(root, "f.txt", NodeKind::File).unwrap();
        fs.write(ino, 0, b"hello").unwrap();

        fs.truncate(ino, 3).unwrap();

        assert_eq!(fs.stat(ino).unwrap().size, 3);
        let mut buf = [0u8; 8];
        let read = fs.read(ino, 0, &mut buf).unwrap();
        assert_eq!(&buf[..read], b"hel"); // the trailing "lo" is gone
    }

    #[test]
    fn truncate_grows_a_file_with_zero_fill() {
        let mut fs = RamFs::new();
        let root = fs.root();
        let ino = fs.create(root, "f.txt", NodeKind::File).unwrap();
        fs.write(ino, 0, b"hi").unwrap();

        fs.truncate(ino, 5).unwrap();

        assert_eq!(fs.stat(ino).unwrap().size, 5);
        let mut buf = [0u8; 5];
        fs.read(ino, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"hi\0\0\0");
    }

    #[test]
    fn truncate_on_a_directory_is_an_error() {
        let mut fs = RamFs::new();
        let root = fs.root();
        assert_eq!(fs.truncate(root, 0), Err(FsError::IsADir));
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
