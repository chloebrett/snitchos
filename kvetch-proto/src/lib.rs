//! The `kvetch` completion-server IPC protocol — request/reply wire types
//! between a client and the model server, encoded into `[u64; MSG_WORDS]`.
//! Host-testable; no kernel/IPC types. Mirrors `fs-proto` / `glitch-proto`.
//! See `docs/babble-design.md`.
//!
//! Its own crate so a client gets the message types without linking the
//! sampler (and through it the whole Stitch parser).

#![no_std]
#![forbid(unsafe_code)]

/// The IPC message width, re-exported from the shared ABI — the wire layouts
/// here encode into `[u64; MSG_WORDS]`.
pub use snitchos_abi::MSG_WORDS;

/// The largest completion buffer a client may offer, as a sanity bound on a
/// number that arrives from another process.
pub const MAX_BUFFER: u32 = 64 * 1024;

/// Why decoding a kvetch message failed — a malformed message is something to
/// reply to, never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// `w0`'s tag names no known request.
    UnknownRequest(u64),
    /// A reply's status word maps to no known outcome.
    UnknownStatus(u64),
    /// The declared prefix does not fit in the buffer that is meant to hold it.
    PrefixExceedsBuffer,
    /// The offered buffer exceeds [`MAX_BUFFER`].
    BufferTooLarge,
}

/// Ask the server to continue the text already in `buf`.
///
/// **One buffer, in and out**: on entry it holds `prefix_len` bytes of source;
/// on reply the server has appended its completion and says how many bytes it
/// wrote. Read-style, and it fits the four-word message — two separate buffers
/// would not.
///
/// **No seed field, deliberately.** Sampling entropy derives from the server's
/// per-boot root and a request counter (`docs/randomness-and-entropy.md`), so
/// the same boot seed and the same request sequence reproduce byte-identically
/// on snemu, QEMU and hardware. A client-supplied seed would let a caller
/// silently diverge the two engines; itests pin the boot seed instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Complete {
    /// How many tokens to emit. A completion is a *fragment*: the server stops
    /// on this budget, and does not try to finish the program.
    pub max_tokens: u32,
    /// The client's buffer.
    pub ptr: u64,
    /// Its capacity in bytes.
    pub cap: u32,
    /// How much of it is prefix.
    pub prefix_len: u32,
}

impl Complete {
    /// The request tag in `w0`'s low byte. One request in v0; the tag rides the
    /// wire so more can be appended without a format break (append-only, never
    /// renumber — the same rule as `protocol::Frame`'s variants).
    const TAG: u64 = 0;
    const TAG_BITS: u32 = 8;

    #[must_use]
    pub const fn encode(&self) -> [u64; MSG_WORDS] {
        [
            Self::TAG | ((self.max_tokens as u64) << Self::TAG_BITS),
            self.ptr,
            self.cap as u64,
            self.prefix_len as u64,
        ]
    }

    /// # Errors
    /// [`WireError`] if the tag is unknown or the buffer bounds are incoherent.
    pub const fn decode(words: [u64; MSG_WORDS]) -> Result<Self, WireError> {
        let [w0, ptr, cap, prefix_len] = words;
        let tag = w0 & ((1 << Self::TAG_BITS) - 1);
        if tag != Self::TAG {
            return Err(WireError::UnknownRequest(tag));
        }
        if cap > MAX_BUFFER as u64 {
            return Err(WireError::BufferTooLarge);
        }
        if prefix_len > cap {
            return Err(WireError::PrefixExceedsBuffer);
        }
        Ok(Self {
            max_tokens: (w0 >> Self::TAG_BITS) as u32,
            ptr,
            cap: cap as u32,
            prefix_len: prefix_len as u32,
        })
    }
}

/// How a request turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The completion was written into the client's buffer.
    Ok = 0,
    /// The server declined — refusals snitch, they are never silent.
    Refused = 1,
    /// The request did not decode.
    Malformed = 2,
}

impl Status {
    const fn from_u64(raw: u64) -> Option<Self> {
        match raw {
            0 => Some(Self::Ok),
            1 => Some(Self::Refused),
            2 => Some(Self::Malformed),
            _ => None,
        }
    }
}

/// The server's answer: an outcome plus how many bytes it appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    pub status: Status,
    /// Bytes appended after the prefix. Zero unless [`Status::Ok`].
    pub written: u32,
}

impl Reply {
    #[must_use]
    pub const fn encode(&self) -> [u64; MSG_WORDS] {
        [self.status as u64, self.written as u64, 0, 0]
    }

    /// # Errors
    /// [`WireError::UnknownStatus`] if `w0` names no known outcome.
    pub const fn decode(words: [u64; MSG_WORDS]) -> Result<Self, WireError> {
        let [w0, written, _, _] = words;
        match Status::from_u64(w0) {
            Some(status) => Ok(Self { status, written: written as u32 }),
            None => Err(WireError::UnknownStatus(w0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Complete, MAX_BUFFER, Reply, Status, WireError};

    fn request() -> Complete {
        Complete { max_tokens: 12, ptr: 0xdead_0000, cap: 4096, prefix_len: 13 }
    }

    #[test]
    fn a_request_round_trips() {
        assert_eq!(Complete::decode(request().encode()), Ok(request()));
    }

    #[test]
    fn an_unknown_request_tag_is_an_error_not_a_panic() {
        // Forward compatibility is by *appending* tags (never renumbering), so
        // an older server meeting a newer client must refuse, not misread.
        let mut words = request().encode();
        words[0] = 0xff;
        assert_eq!(Complete::decode(words), Err(WireError::UnknownRequest(0xff)));
    }

    #[test]
    fn a_prefix_longer_than_its_buffer_is_rejected() {
        // The prefix lives in the client's buffer, so `prefix_len` must fit
        // inside `cap` — a server that trusted it would read past the end.
        let mut malformed = request();
        malformed.prefix_len = malformed.cap + 1;
        assert_eq!(Complete::decode(malformed.encode()), Err(WireError::PrefixExceedsBuffer));
    }

    #[test]
    fn an_oversized_buffer_is_rejected() {
        let mut malformed = request();
        malformed.cap = MAX_BUFFER + 1;
        assert_eq!(Complete::decode(malformed.encode()), Err(WireError::BufferTooLarge));
    }

    #[test]
    fn a_reply_round_trips_for_every_status() {
        for status in [Status::Ok, Status::Refused, Status::Malformed] {
            let reply = Reply { status, written: 37 };
            assert_eq!(Reply::decode(reply.encode()), Ok(reply));
        }
    }

    #[test]
    fn an_unknown_reply_status_is_an_error_not_a_panic() {
        assert_eq!(Reply::decode([99, 0, 0, 0]), Err(WireError::UnknownStatus(99)));
    }
}
