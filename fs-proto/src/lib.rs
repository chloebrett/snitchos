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

use fs_core::InodeId;

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
}

impl Op {
    /// Every opcode, for exhaustive round-trip testing and dispatch tables.
    pub const ALL: [Op; 7] = [
        Op::Lookup,
        Op::Stat,
        Op::Read,
        Op::Write,
        Op::Create,
        Op::Remove,
        Op::Readdir,
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
            _ => None,
        }
    }
}

/// FS-defined file rights, gated per message by the FS. Distinct from the
/// kernel's endpoint `Rights` (`SEND`/`RECV`/`MINT`): the kernel carries
/// these bits in the badge but never interprets them. First cut is
/// `READ`/`WRITE`; `EXEC` is reserved (not enforced until executables,
/// ~v0.11); directory rights (`LOOKUP`/`LIST`/`CREATE`/`REMOVE`) are
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
    /// Reserved for executables (~v0.11); the bit exists now so badges that
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

#[cfg(test)]
mod tests {
    use super::*;
    use fs_core::InodeId;

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
    }
}
