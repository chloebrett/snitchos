//! The FS IPC front-end protocol — *above* the cap-agnostic `Filesystem`
//! trait, *below* the kernel IPC mechanism.
//!
//! Three things the connecting layer (`user/fs`) needs and the trait
//! deliberately doesn't carry:
//! - [`FileRights`] — FS-defined rights the FS enforces per message. The
//!   kernel carries these in the badge but never interprets them.
//! - [`Badge`] — what a File cap names: `(inode, file_rights)` packed into
//!   the `u64` the kernel delivers unforgeably to the server on every send.
//! - [`Op`] — the FS request opcode (one per `Filesystem` method).
//!
//! Host-testable; no kernel/IPC types. See `docs/filesystem-design.md`.

#![no_std]
#![forbid(unsafe_code)]

pub mod markers;

use fs_core::{FsError, InodeId, Stat};
// Re-export: `Request::Create` exposes `NodeKind` in the public API, so callers get
// it from `fs_proto` without a direct `fs-core` dependency.
pub use fs_core::NodeKind;

/// The IPC message width, re-exported from the shared ABI — the wire layouts
/// here encode into `[u64; MSG_WORDS]`.
pub use snitchos_abi::MSG_WORDS;

/// What a File cap names: an inode plus the file rights granted on it.
/// Packed into the `u64` badge the kernel delivers unforgeably to the FS
/// server on every message (the kernel carries it, never reads it).
///
/// Layout (doc Q4): `inode` in bits `[0..32)`, `rights` in `[32..48)`,
/// the top 16 bits spare. A bare endpoint cap (the server's own `RECV`
/// cap) has badge `0`; a File cap always carries rights, so it's nonzero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Badge {
    pub inode: InodeId,
    pub rights: FileRights,
}

impl Badge {
    const RIGHTS_SHIFT: u32 = 32;

    #[must_use]
    pub const fn pack(self) -> u64 {
        (self.inode.as_u32() as u64) | ((self.rights.bits() as u64) << Self::RIGHTS_SHIFT)
    }

    #[must_use]
    pub const fn unpack(raw: u64) -> Badge {
        let inode = InodeId::new(raw as u32);
        let rights = FileRights::from_bits((raw >> Self::RIGHTS_SHIFT) as u16);
        Badge { inode, rights }
    }
}

/// The structured payload of a rights-gate refusal: which inode was touched
/// and which [`FileRights`] the cap was missing. The FS packs this into the
/// `i64` value of its denial telemetry — refusals snitch, never silent — so the
/// host decoder recovers `(inode, attempted)` the same way it unpacks a
/// [`Badge`]. `MissingRight` is implicit in the denial metric's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denial {
    pub inode: InodeId,
    pub attempted: FileRights,
}

impl Denial {
    const RIGHT_SHIFT: u32 = 32;

    #[must_use]
    pub const fn pack(self) -> i64 {
        (self.inode.as_u32() as i64) | ((self.attempted.bits() as i64) << Self::RIGHT_SHIFT)
    }

    #[must_use]
    pub const fn unpack(raw: i64) -> Denial {
        Denial {
            inode: InodeId::new(raw as u32),
            attempted: FileRights::from_bits((raw >> Self::RIGHT_SHIFT) as u16),
        }
    }
}

/// The FS request opcode — one per [`fs_core::Filesystem`] method (except
/// `root`, which needs no request: the root File cap is handed to init at
/// startup). Discriminants are the wire encoding: append-only, never
/// renumber (mirrors the `abi::Syscall` rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    Lookup = 0,
    Stat = 1,
    Read = 2,
    Write = 3,
    Create = 4,
    Remove = 5,
    Readdir = 6,
    /// Read an inode-attached extended attribute (e.g. the `user.iface` manifest).
    GetXattr = 7,
    /// Resize a file to an exact length (shrink drops trailing bytes, grow
    /// zero-fills). WRITE-gated. **Appended** — opcodes never renumber.
    Truncate = 8,
}

impl Op {
    /// Every opcode, for exhaustive round-trip testing and dispatch tables.
    pub const ALL: [Op; 9] = [
        Op::Lookup,
        Op::Stat,
        Op::Read,
        Op::Write,
        Op::Create,
        Op::Remove,
        Op::Readdir,
        Op::GetXattr,
        Op::Truncate,
    ];

    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub const fn from_u8(byte: u8) -> Option<Op> {
        match byte {
            0 => Some(Op::Lookup),
            1 => Some(Op::Stat),
            2 => Some(Op::Read),
            3 => Some(Op::Write),
            4 => Some(Op::Create),
            5 => Some(Op::Remove),
            6 => Some(Op::Readdir),
            7 => Some(Op::GetXattr),
            8 => Some(Op::Truncate),
            _ => None,
        }
    }
}

/// Which extended attribute a [`Request::GetXattr`] reads. The FS's xattr
/// namespace is small and OS-defined, so a compact enum rides the wire in one
/// word (a `UserBuf` name plus the `UserBuf` dst wouldn't fit `MSG_WORDS`).
/// Discriminants are the wire encoding: append-only, never renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum XattrKey {
    /// `user.iface` — a program's typed-interface manifest (from its
    /// `.snitch.iface` note).
    UserIface = 0,
    /// `user.type` — a per-file type hint (e.g. "is this a program?"), the
    /// lighter cousin the FS design doc anticipates.
    UserType = 1,
}

impl XattrKey {
    /// Every key, for exhaustive round-trip testing.
    pub const ALL: [XattrKey; 2] = [XattrKey::UserIface, XattrKey::UserType];

    #[must_use]
    pub const fn to_u64(self) -> u64 {
        self as u64
    }

    #[must_use]
    pub const fn from_u64(raw: u64) -> Option<XattrKey> {
        match raw {
            0 => Some(XattrKey::UserIface),
            1 => Some(XattrKey::UserType),
            _ => None,
        }
    }

    /// The attribute name this key names, as the [`fs_core::Filesystem`] xattr
    /// API takes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            XattrKey::UserIface => "user.iface",
            XattrKey::UserType => "user.type",
        }
    }
}

/// FS-defined file rights, gated per message by the FS. Distinct from the
/// kernel's endpoint `Rights` (`SEND`/`RECV`/`MINT`): the kernel carries
/// these bits in the badge but never interprets them. First cut is
/// `READ`/`WRITE`; `EXEC` is reserved (the bit exists but no op enforces
/// it yet); directory rights (`LOOKUP`/`LIST`/`CREATE`/`REMOVE`) are
/// additive bits with no trait change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRights(u16);

impl FileRights {
    /// The empty set — names an inode but grants no operation on it.
    pub const NONE: FileRights = FileRights(0b000);
    /// May `read` a file inode.
    pub const READ: FileRights = FileRights(0b001);
    /// May `write` a file inode.
    pub const WRITE: FileRights = FileRights(0b010);
    /// Reserved for executables; the bit exists now so badges that
    /// pack it stay stable, but no op enforces it yet.
    pub const EXEC: FileRights = FileRights(0b100);

    /// Whether `self` grants every right in `other`.
    #[must_use]
    pub const fn contains(self, other: FileRights) -> bool {
        self.0 & other.0 == other.0
    }

    /// The raw bitmask, for packing into a [`Badge`].
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Rebuild a rights set from raw bits (the inverse of [`bits`](Self::bits)).
    #[must_use]
    pub const fn from_bits(bits: u16) -> FileRights {
        FileRights(bits)
    }
}

impl core::ops::BitOr for FileRights {
    type Output = FileRights;
    fn bitor(self, rhs: FileRights) -> FileRights {
        FileRights(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for FileRights {
    type Output = FileRights;
    fn bitand(self, rhs: FileRights) -> FileRights {
        FileRights(self.0 & rhs.0)
    }
}

/// The file right an [`Op`] requires, or `None` if it is ungated. The flat
/// core gates only the file data ops — `Read` needs `READ`, `Write` needs
/// `WRITE`; the metadata/directory ops (`Stat`/`Lookup`/`Create`/`Remove`/
/// `Readdir`) are ungated (directory rights are a deferred follow-on).
#[must_use]
pub const fn required_right(op: Op) -> Option<FileRights> {
    match op {
        Op::Read => Some(FileRights::READ),
        Op::Write | Op::Truncate => Some(FileRights::WRITE),
        Op::Stat | Op::Lookup | Op::Create | Op::Remove | Op::Readdir | Op::GetXattr => None,
    }
}

/// The rights gate, enforced by the FS per message. `Ok` if `held` grants what
/// `op` requires; `Err(missing)` names the right the cap lacks — the *attempted
/// right* the FS snitches alongside the inode on a refusal.
///
/// # Errors
/// Returns the missing [`FileRights`] when a gated op's required right is absent.
pub const fn check_rights(op: Op, held: FileRights) -> Result<(), FileRights> {
    match required_right(op) {
        Some(r) if !held.contains(r) => Err(r),
        _ => Ok(()),
    }
}

/// A `(ptr, len)` reference into the *caller's* address space — a filename or a
/// data buffer the kernel copies across the boundary (option D). Carried as
/// plain words on the wire; dereferenced only by the kernel copy primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserBuf {
    pub ptr: u64,
    pub len: u64,
}

/// Why decoding a request failed — a malformed wire message is an error to
/// reply to, never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// `w0`'s opcode byte names no [`Op`].
    UnknownOp(u8),
    /// A `Create` carried a node-kind value that is neither `File` (0) nor
    /// `Dir` (1).
    BadKind(u64),
    /// A response carried a status word that maps to no `FsError` (and isn't
    /// `0` = Ok).
    BadStatus(u64),
}

/// An FS request, decoded from the IPC message. The target **inode is not
/// here** — it rides in the badge (`Badge::unpack`). Names and data buffers are
/// [`UserBuf`] refs the kernel copies (option D). See `docs/filesystem-design.md`
/// → *Message framing* for the locked word layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Stat,
    Read { offset: u64, dst: UserBuf },
    Write { offset: u64, src: UserBuf },
    Lookup { name: UserBuf, rights: FileRights },
    Create { name: UserBuf, kind: NodeKind },
    Remove { name: UserBuf },
    Readdir { index: u64, name_dst: UserBuf },
    GetXattr { key: XattrKey, dst: UserBuf },
    /// Resize the badged file to `len` bytes. WRITE-gated.
    Truncate { len: u64 },
}

impl Request {
    /// The opcode of this request.
    #[must_use]
    pub const fn op(&self) -> Op {
        match self {
            Request::Stat => Op::Stat,
            Request::Read { .. } => Op::Read,
            Request::Write { .. } => Op::Write,
            Request::Lookup { .. } => Op::Lookup,
            Request::Create { .. } => Op::Create,
            Request::Remove { .. } => Op::Remove,
            Request::Readdir { .. } => Op::Readdir,
            Request::GetXattr { .. } => Op::GetXattr,
            Request::Truncate { .. } => Op::Truncate,
        }
    }

    #[must_use]
    pub fn encode(&self) -> [u64; MSG_WORDS] {
        let op = u64::from(self.op().to_u8());
        match *self {
            Request::Stat => [op, 0, 0, 0],
            Request::Read { offset, dst } => [op, offset, dst.ptr, dst.len],
            Request::Write { offset, src } => [op, offset, src.ptr, src.len],
            Request::Lookup { name, rights } => [op, name.ptr, name.len, u64::from(rights.bits())],
            Request::Remove { name } => [op, name.ptr, name.len, 0],
            Request::Create { name, kind } => [op, name.ptr, name.len, kind_to_wire(kind)],
            Request::Readdir { index, name_dst } => [op, index, name_dst.ptr, name_dst.len],
            Request::GetXattr { key, dst } => [op, key.to_u64(), dst.ptr, dst.len],
            Request::Truncate { len } => [op, len, 0, 0],
        }
    }

    pub fn decode(words: [u64; MSG_WORDS]) -> Result<Request, WireError> {
        let [w0, w1, w2, w3] = words;
        let op = Op::from_u8(w0 as u8).ok_or(WireError::UnknownOp(w0 as u8))?;
        Ok(match op {
            Op::Stat => Request::Stat,
            Op::Read => Request::Read { offset: w1, dst: UserBuf { ptr: w2, len: w3 } },
            Op::Write => Request::Write { offset: w1, src: UserBuf { ptr: w2, len: w3 } },
            Op::Lookup => Request::Lookup {
                name: UserBuf { ptr: w1, len: w2 },
                rights: FileRights::from_bits(w3 as u16),
            },
            Op::Create => Request::Create {
                name: UserBuf { ptr: w1, len: w2 },
                kind: kind_from_wire(w3)?,
            },
            Op::Remove => Request::Remove { name: UserBuf { ptr: w1, len: w2 } },
            Op::Readdir => Request::Readdir { index: w1, name_dst: UserBuf { ptr: w2, len: w3 } },
            Op::GetXattr => Request::GetXattr {
                key: XattrKey::from_u64(w1).ok_or(WireError::BadKind(w1))?,
                dst: UserBuf { ptr: w2, len: w3 },
            },
            Op::Truncate => Request::Truncate { len: w1 },
        })
    }
}

const fn kind_to_wire(kind: NodeKind) -> u64 {
    match kind {
        NodeKind::File => 0,
        NodeKind::Dir => 1,
    }
}

const fn kind_from_wire(raw: u64) -> Result<NodeKind, WireError> {
    match raw {
        0 => Ok(NodeKind::File),
        1 => Ok(NodeKind::Dir),
        other => Err(WireError::BadKind(other)),
    }
}

/// An FS reply, decoded against the [`Op`] that was sent (the Ok payload's shape
/// depends on it). `w0` is the status: `0` = Ok, else an `FsError` code. New
/// inodes from `Lookup`/`Create` ride here for information; the actual child
/// File cap is transferred out-of-band via `reply_with_cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Err(FsError),
    /// `Stat`.
    Stat(Stat),
    /// `Read` / `Write`: bytes transferred.
    Count(u64),
    /// `Lookup` / `Create`: the resolved/created inode.
    Inode(InodeId),
    /// `Remove`: success, no payload.
    Removed,
    /// `Readdir`: one entry; `name_len` bytes were copied into the request's
    /// `name_dst` buffer.
    Entry { ino: InodeId, kind: NodeKind, name_len: u64 },
}

impl Response {
    #[must_use]
    pub fn encode(&self) -> [u64; MSG_WORDS] {
        match *self {
            Response::Err(e) => [status_from_error(e), 0, 0, 0],
            Response::Stat(s) => [0, kind_to_wire(s.kind), s.size, 0],
            Response::Count(n) => [0, n, 0, 0],
            Response::Inode(ino) => [0, u64::from(ino.as_u32()), 0, 0],
            Response::Removed => [0, 0, 0, 0],
            Response::Entry { ino, kind, name_len } => {
                [0, u64::from(ino.as_u32()), kind_to_wire(kind), name_len]
            }
        }
    }

    pub fn decode(op: Op, words: [u64; MSG_WORDS]) -> Result<Response, WireError> {
        let [w0, w1, w2, w3] = words;
        if w0 != 0 {
            return Ok(Response::Err(error_from_status(w0)?));
        }
        Ok(match op {
            Op::Stat => Response::Stat(Stat { kind: kind_from_wire(w1)?, size: w2 }),
            Op::Read | Op::Write | Op::GetXattr | Op::Truncate => Response::Count(w1),
            Op::Lookup | Op::Create => Response::Inode(InodeId::new(w1 as u32)),
            Op::Remove => Response::Removed,
            Op::Readdir => {
                Response::Entry { ino: InodeId::new(w1 as u32), kind: kind_from_wire(w2)?, name_len: w3 }
            }
        })
    }
}

const fn status_from_error(e: FsError) -> u64 {
    match e {
        FsError::NotFound => 1,
        FsError::NotADir => 2,
        FsError::IsADir => 3,
        FsError::Exists => 4,
        FsError::Unsupported => 5,
        FsError::NameTooLong => 6,
        FsError::Denied => 7,
        FsError::Internal => 8,
    }
}

const fn error_from_status(status: u64) -> Result<FsError, WireError> {
    match status {
        1 => Ok(FsError::NotFound),
        2 => Ok(FsError::NotADir),
        3 => Ok(FsError::IsADir),
        4 => Ok(FsError::Exists),
        5 => Ok(FsError::Unsupported),
        6 => Ok(FsError::NameTooLong),
        7 => Ok(FsError::Denied),
        8 => Ok(FsError::Internal),
        other => Err(WireError::BadStatus(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_core::{FsError, InodeId, NodeKind, Stat};

    #[test]
    fn getxattr_op_is_appended_at_seven() {
        assert_eq!(Op::GetXattr.to_u8(), 7, "append-only: never renumber 0..=6");
        assert_eq!(Op::from_u8(7), Some(Op::GetXattr));
    }

    #[test]
    fn xattr_key_round_trips_and_names_its_attribute() {
        for key in XattrKey::ALL {
            assert_eq!(XattrKey::from_u64(key.to_u64()), Some(key));
        }
        assert_eq!(XattrKey::UserIface.name(), "user.iface");
    }

    #[test]
    fn getxattr_request_round_trips() {
        let req = Request::GetXattr {
            key: XattrKey::UserIface,
            dst: UserBuf { ptr: 0x7000, len: 1024 },
        };
        assert_eq!(Request::decode(req.encode()), Ok(req));
    }

    #[test]
    fn getxattr_is_ungated_metadata() {
        // Reading a program's manifest rides the file cap you already hold; no
        // separate right (like `Stat`/`Lookup`). A dedicated `XATTR` right is a
        // deferred refinement.
        assert_eq!(required_right(Op::GetXattr), None);
    }

    #[test]
    fn getxattr_replies_with_a_byte_count() {
        let resp = Response::Count(1024);
        assert_eq!(Response::decode(Op::GetXattr, resp.encode()), Ok(resp));
    }

    #[test]
    fn read_and_write_are_distinct_rights() {
        assert!(!FileRights::READ.contains(FileRights::WRITE));
        assert!(!FileRights::WRITE.contains(FileRights::READ));
    }

    #[test]
    fn union_grants_both_and_contains_each() {
        let rw = FileRights::READ | FileRights::WRITE;

        assert!(rw.contains(FileRights::READ));
        assert!(rw.contains(FileRights::WRITE));
        assert!(!rw.contains(FileRights::EXEC));
    }

    #[test]
    fn union_is_idempotent() {
        // Distinguishes OR from XOR: re-adding a held right keeps it
        // (`x ^ x` would clear it).
        let rw = FileRights::READ | FileRights::WRITE;
        let still_rw = rw | FileRights::WRITE;

        assert!(still_rw.contains(FileRights::READ));
        assert!(still_rw.contains(FileRights::WRITE));
    }

    #[test]
    fn intersection_keeps_only_rights_in_both() {
        // The attenuation primitive: a minted child cap gets parent ∩ requested.
        let rw = FileRights::READ | FileRights::WRITE;

        assert_eq!(rw & FileRights::READ, FileRights::READ);
        assert_eq!(rw & FileRights::WRITE, FileRights::WRITE);
        assert_eq!(FileRights::READ & FileRights::WRITE, FileRights::NONE);
        assert_eq!(rw & rw, rw);
    }

    #[test]
    fn none_grants_nothing_and_is_contained_by_all() {
        assert!(!FileRights::NONE.contains(FileRights::READ));
        assert!(FileRights::READ.contains(FileRights::NONE));
    }

    #[test]
    fn badge_round_trips_inode_and_rights() {
        let badge = Badge {
            inode: InodeId::new(42),
            rights: FileRights::READ | FileRights::WRITE,
        };

        let unpacked = Badge::unpack(badge.pack());

        assert_eq!(unpacked.inode, InodeId::new(42));
        assert_eq!(unpacked.rights, FileRights::READ | FileRights::WRITE);
    }

    #[test]
    fn badge_layout_is_inode_low_rights_next_spare_high() {
        let badge = Badge {
            inode: InodeId::new(0xABCD),
            rights: FileRights::from_bits(0x0003),
        };

        // inode in bits [0..32), rights in [32..48), spare [48..64) = 0.
        assert_eq!(badge.pack(), 0x0000_0003_0000_ABCD);
    }

    #[test]
    fn badge_ignores_the_spare_bits_on_unpack() {
        let with_spare = 0xFFFF_0003_0000_ABCD;
        let unpacked = Badge::unpack(with_spare);

        assert_eq!(unpacked.inode, InodeId::new(0xABCD));
        assert_eq!(unpacked.rights, FileRights::from_bits(0x0003));
    }

    #[test]
    fn every_op_round_trips_through_its_byte() {
        for op in Op::ALL {
            assert_eq!(Op::from_u8(op.to_u8()), Some(op));
        }
    }

    #[test]
    fn unknown_opcode_byte_decodes_to_none() {
        assert_eq!(Op::from_u8(200), None);
    }

    #[test]
    fn opcode_discriminants_are_stable_wire_values() {
        // Never renumber: old captures + clients encode these. Append only.
        assert_eq!(Op::Lookup.to_u8(), 0);
        assert_eq!(Op::Stat.to_u8(), 1);
        assert_eq!(Op::Read.to_u8(), 2);
        assert_eq!(Op::Write.to_u8(), 3);
        assert_eq!(Op::Create.to_u8(), 4);
        assert_eq!(Op::Remove.to_u8(), 5);
        assert_eq!(Op::Readdir.to_u8(), 6);
        assert_eq!(Op::GetXattr.to_u8(), 7);
    }

    fn buf(ptr: u64, len: u64) -> UserBuf {
        UserBuf { ptr, len }
    }

    #[test]
    fn every_request_round_trips() {
        let reqs = [
            Request::Stat,
            Request::Read { offset: 64, dst: buf(0x1000, 512) },
            Request::Write { offset: 8, src: buf(0x2000, 16) },
            Request::Lookup { name: buf(0x3000, 7), rights: FileRights::READ | FileRights::WRITE },
            Request::Create { name: buf(0x4000, 9), kind: NodeKind::File },
            Request::Remove { name: buf(0x5000, 5) },
            Request::Readdir { index: 3, name_dst: buf(0x6000, 256) },
            Request::Truncate { len: 128 },
        ];

        for req in reqs {
            assert_eq!(Request::decode(req.encode()), Ok(req));
        }
    }

    #[test]
    fn lookup_carries_requested_rights_in_the_reserved_slot() {
        // The client asks for the rights it wants on the child cap; the server
        // mints parent ∩ requested. Rights ride w3 (previously a 0 pad).
        let req = Request::Lookup { name: buf(0x3000, 7), rights: FileRights::READ };

        assert_eq!(Request::decode(req.encode()), Ok(req));
        assert_eq!(req.encode()[3], u64::from(FileRights::READ.bits()));
    }

    #[test]
    fn request_word_layout_is_locked() {
        // op in w0; inode is NOT in the message (it rides in the badge).
        assert_eq!(
            Request::Read { offset: 64, dst: buf(0x1000, 512) }.encode(),
            [u64::from(Op::Read.to_u8()), 64, 0x1000, 512]
        );
        assert_eq!(
            Request::Create { name: buf(0x4000, 9), kind: NodeKind::Dir }.encode(),
            [u64::from(Op::Create.to_u8()), 0x4000, 9, 1]
        );
    }

    #[test]
    fn decoding_an_unknown_opcode_is_an_error_not_a_panic() {
        assert_eq!(Request::decode([200, 0, 0, 0]), Err(WireError::UnknownOp(200)));
    }

    #[test]
    fn decoding_a_bad_node_kind_is_an_error() {
        let words = [u64::from(Op::Create.to_u8()), 0x4000, 9, 2];
        assert_eq!(Request::decode(words), Err(WireError::BadKind(2)));
    }

    #[test]
    fn every_response_round_trips() {
        let cases = [
            (Op::Stat, Response::Stat(Stat { kind: NodeKind::Dir, size: 0 })),
            (Op::Read, Response::Count(512)),
            (Op::Write, Response::Count(16)),
            (Op::Lookup, Response::Inode(InodeId::new(7))),
            (Op::Create, Response::Inode(InodeId::new(9))),
            (Op::Remove, Response::Removed),
            (Op::Readdir, Response::Entry { ino: InodeId::new(3), kind: NodeKind::File, name_len: 5 }),
            (Op::Truncate, Response::Count(128)),
        ];

        for (op, resp) in cases {
            assert_eq!(Response::decode(op, resp.encode()), Ok(resp));
        }
    }

    #[test]
    fn error_responses_round_trip_under_any_op() {
        let errors = [
            FsError::NotFound,
            FsError::NotADir,
            FsError::IsADir,
            FsError::Exists,
            FsError::Unsupported,
            FsError::NameTooLong,
            FsError::Denied,
            FsError::Internal,
        ];

        for e in errors {
            assert_eq!(Response::decode(Op::Stat, Response::Err(e).encode()), Ok(Response::Err(e)));
        }
    }

    #[test]
    fn error_status_codes_are_stable_wire_values() {
        // Never renumber: clients map status → FsError off these. 0 = Ok.
        assert_eq!(Response::Err(FsError::NotFound).encode()[0], 1);
        assert_eq!(Response::Err(FsError::NotADir).encode()[0], 2);
        assert_eq!(Response::Err(FsError::IsADir).encode()[0], 3);
        assert_eq!(Response::Err(FsError::Exists).encode()[0], 4);
        assert_eq!(Response::Err(FsError::Unsupported).encode()[0], 5);
        assert_eq!(Response::Err(FsError::NameTooLong).encode()[0], 6);
        assert_eq!(Response::Err(FsError::Denied).encode()[0], 7);
        assert_eq!(Response::Err(FsError::Internal).encode()[0], 8);
    }

    #[test]
    fn denial_round_trips_inode_and_attempted_right() {
        let d = Denial { inode: InodeId::new(7), attempted: FileRights::WRITE };

        assert_eq!(Denial::unpack(d.pack()), d);
    }

    #[test]
    fn denial_layout_mirrors_badge_inode_low_right_next() {
        // The snitch value the host decoder reads off the denial metric:
        // inode in [0..32), attempted right in [32..48) — same shape as Badge.
        let d = Denial { inode: InodeId::new(0xABCD), attempted: FileRights::WRITE };

        assert_eq!(d.pack(), 0x0000_0002_0000_ABCD);
    }

    #[test]
    fn read_write_and_truncate_are_the_gated_ops() {
        assert_eq!(required_right(Op::Read), Some(FileRights::READ));
        assert_eq!(required_right(Op::Write), Some(FileRights::WRITE));
        // Truncate mutates file data, so it is WRITE-gated like Write.
        assert_eq!(required_right(Op::Truncate), Some(FileRights::WRITE));
        for op in [Op::Stat, Op::Lookup, Op::Create, Op::Remove, Op::Readdir, Op::GetXattr] {
            assert_eq!(required_right(op), None);
        }
    }

    #[test]
    fn truncate_request_round_trips_and_carries_its_length() {
        let req = Request::Truncate { len: 42 };
        assert_eq!(Request::decode(req.encode()), Ok(req));
        // op in w0, the new length in w1, the rest padding.
        assert_eq!(req.encode(), [u64::from(Op::Truncate.to_u8()), 42, 0, 0]);
        // Refused without WRITE, allowed with it.
        assert_eq!(check_rights(Op::Truncate, FileRights::READ), Err(FileRights::WRITE));
        assert_eq!(check_rights(Op::Truncate, FileRights::WRITE), Ok(()));
    }

    #[test]
    fn gate_refuses_write_without_write_right_and_reports_it() {
        assert_eq!(check_rights(Op::Write, FileRights::READ), Err(FileRights::WRITE));
        assert_eq!(check_rights(Op::Write, FileRights::READ | FileRights::WRITE), Ok(()));
    }

    #[test]
    fn gate_refuses_read_without_read_right_and_reports_it() {
        assert_eq!(check_rights(Op::Read, FileRights::WRITE), Err(FileRights::READ));
        assert_eq!(check_rights(Op::Read, FileRights::READ), Ok(()));
    }

    #[test]
    fn gate_allows_ungated_ops_on_even_empty_rights() {
        for op in [Op::Stat, Op::Lookup, Op::Create, Op::Remove, Op::Readdir] {
            assert_eq!(check_rights(op, FileRights::NONE), Ok(()));
        }
    }

    #[test]
    fn denied_status_code_is_seven_appended() {
        // The rights gate's refusal. Appended after NameTooLong(6); 1–6 never
        // renumber.
        assert_eq!(Response::Err(FsError::Denied).encode()[0], 7);
    }

    #[test]
    fn denied_response_round_trips() {
        assert_eq!(
            Response::decode(Op::Write, Response::Err(FsError::Denied).encode()),
            Ok(Response::Err(FsError::Denied))
        );
    }

    #[test]
    fn internal_status_code_is_eight_appended() {
        // The FS server's "I couldn't complete this" — a copy/mint/decode
        // failure, distinct from `Unsupported` (the op isn't implemented).
        // Appended after Denied(7); 1–7 never renumber.
        assert_eq!(Response::Err(FsError::Internal).encode()[0], 8);
    }

    #[test]
    fn internal_response_round_trips() {
        assert_eq!(
            Response::decode(Op::Read, Response::Err(FsError::Internal).encode()),
            Ok(Response::Err(FsError::Internal))
        );
    }

    #[test]
    fn response_word_layout_is_locked() {
        // status 0 (Ok) in w0; Dir kind = 1.
        assert_eq!(
            Response::Stat(Stat { kind: NodeKind::Dir, size: 42 }).encode(),
            [0, 1, 42, 0]
        );
        assert_eq!(
            Response::Entry { ino: InodeId::new(3), kind: NodeKind::File, name_len: 5 }.encode(),
            [0, 3, 0, 5]
        );
    }

    #[test]
    fn an_out_of_range_status_is_an_error_not_a_panic() {
        assert_eq!(Response::decode(Op::Stat, [99, 0, 0, 0]), Err(WireError::BadStatus(99)));
    }
}
