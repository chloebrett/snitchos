//! `kitsch-proto` — what a client and the compositor say to each other.
//!
//! Four `u64` words per IPC message, the same framing the FS server uses. The
//! verbs are deliberately the ones `glitch` uses for audio — `Attach`, `Commit`,
//! `Tap` — because the two servers are the same shape: one holder mediating a
//! single scarce output for many contributors. Same words now means a shared
//! crate is liftable later; different words for the same idea means it never
//! happens. See `docs/kitsch-design.md` §2.
//!
//! **Text, not pixels.** A commit carries the surface's cells as UTF-8 rows. A
//! client never sees a pixel and never learns where on screen it sits — a
//! surface is a texture, not a piece of the screen, so moving a window touches
//! none of its content.

#![no_std]

/// Words per IPC message — the kernel's fixed frame.
pub const MSG_WORDS: usize = 4;

/// A borrowed buffer in the caller's address space. The kernel copies it; the
/// server never dereferences a client pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserBuf {
    pub ptr: u64,
    pub len: u64,
}

/// Why a message could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The opcode names nothing this version knows.
    UnknownOp(u8),
    /// A geometry field was zero or beyond what a surface may be.
    BadGeometry,
}

/// Message opcodes. **Append only** — the numbers are the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    Attach = 0,
    Commit = 1,
    Detach = 2,
}

impl Op {
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub const fn from_u8(n: u8) -> Option<Self> {
        match n {
            0 => Some(Self::Attach),
            1 => Some(Self::Commit),
            2 => Some(Self::Detach),
            _ => None,
        }
    }
}

/// The largest surface a client may ask for, in cells. Bounds the server's
/// allocation on a number a client chose — a compositor that trusts a client's
/// geometry is a compositor a client can exhaust.
pub const MAX_COLS: u64 = 256;
pub const MAX_ROWS: u64 = 128;

/// A client's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Ask for a surface `cols` x `rows` cells. The reply carries its id.
    Attach { cols: u64, rows: u64 },
    /// Hand over the surface's content as UTF-8 rows separated by `\n`.
    /// Coherent by construction: a commit is a whole frame, so there is no
    /// window in which the server can read a half-drawn one.
    Commit { text: UserBuf },
    /// Give the surface up. The compositor keeps the last committed frame —
    /// that is what makes a tombstone possible.
    Detach,
}

impl Request {
    #[must_use]
    pub const fn op(&self) -> Op {
        match self {
            Request::Attach { .. } => Op::Attach,
            Request::Commit { .. } => Op::Commit,
            Request::Detach => Op::Detach,
        }
    }

    #[must_use]
    pub const fn encode(&self) -> [u64; MSG_WORDS] {
        let op = self.op().to_u8() as u64;
        match *self {
            Request::Attach { cols, rows } => [op, cols, rows, 0],
            Request::Commit { text } => [op, text.ptr, text.len, 0],
            Request::Detach => [op, 0, 0, 0],
        }
    }

    /// Decode a message, rejecting geometry a client should never send rather
    /// than letting it reach an allocation.
    pub fn decode(words: [u64; MSG_WORDS]) -> Result<Request, WireError> {
        let [w0, w1, w2, _w3] = words;
        let op = Op::from_u8(w0 as u8).ok_or(WireError::UnknownOp(w0 as u8))?;
        Ok(match op {
            Op::Attach => {
                if w1 == 0 || w2 == 0 || w1 > MAX_COLS || w2 > MAX_ROWS {
                    return Err(WireError::BadGeometry);
                }
                Request::Attach { cols: w1, rows: w2 }
            }
            Op::Commit => Request::Commit { text: UserBuf { ptr: w1, len: w2 } },
            Op::Detach => Request::Detach,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_survives_a_round_trip() {
        for request in [
            Request::Attach { cols: 80, rows: 24 },
            Request::Commit { text: UserBuf { ptr: 0xdead_beef, len: 1234 } },
            Request::Detach,
        ] {
            assert_eq!(Request::decode(request.encode()), Ok(request), "{request:?}");
        }
    }

    #[test]
    fn an_unknown_opcode_is_refused_not_guessed() {
        // A newer client talking to an older server must fail loudly. Decoding
        // an unknown op as anything at all is how a protocol quietly diverges.
        assert_eq!(Request::decode([99, 0, 0, 0]), Err(WireError::UnknownOp(99)));
    }

    #[test]
    fn a_surface_larger_than_the_cap_is_refused_at_decode() {
        // The server allocates on this number. A client that asks for a
        // billion-cell surface must be stopped at the wire, not at the
        // allocator — and definitely not at the OOM killer.
        assert_eq!(
            Request::decode(Request::Attach { cols: MAX_COLS + 1, rows: 1 }.encode()),
            Err(WireError::BadGeometry)
        );
        assert_eq!(
            Request::decode(Request::Attach { cols: 1, rows: MAX_ROWS + 1 }.encode()),
            Err(WireError::BadGeometry)
        );
    }

    #[test]
    fn a_zero_sized_surface_is_refused() {
        // Zero is not a small surface, it is a surface that cannot be drawn.
        // Accepting it means every later loop has to defend against it.
        assert_eq!(
            Request::decode(Request::Attach { cols: 0, rows: 4 }.encode()),
            Err(WireError::BadGeometry)
        );
        assert_eq!(
            Request::decode(Request::Attach { cols: 4, rows: 0 }.encode()),
            Err(WireError::BadGeometry)
        );
    }

    #[test]
    fn opcode_numbers_are_the_wire_and_do_not_move() {
        // Renumbering these silently breaks every peer built against the old
        // numbers, with no compile error anywhere. Pinned, like `protocol`'s
        // positional frames.
        assert_eq!(Op::Attach.to_u8(), 0);
        assert_eq!(Op::Commit.to_u8(), 1);
        assert_eq!(Op::Detach.to_u8(), 2);
        assert_eq!(Op::from_u8(3), None);
    }
}
