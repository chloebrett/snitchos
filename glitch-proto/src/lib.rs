//! The `glitch` audio-server IPC protocol — the request/reply wire types between
//! a client and the server, encoded into `[u64; MSG_WORDS]`. Host-testable; no
//! kernel/IPC types. Mirrors `fs-proto`. See `plans/glitch.md`.

#![no_std]
#![forbid(unsafe_code)]

/// The IPC message width, re-exported from the shared ABI — the wire layouts here
/// encode into `[u64; MSG_WORDS]`.
pub use snitchos_abi::MSG_WORDS;

/// Why decoding a glitch message failed — a malformed message is an error to reply
/// to, never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// `w0`'s request tag names no known request.
    UnknownRequest(u64),
    /// A reply's status word maps to no known outcome.
    UnknownStatus(u64),
}

/// A request to the `glitch` server: play a tone at `freq_hz` for `duration_ms`.
/// **Volume is the server's policy** in v1 (no per-request gain — see
/// `plans/glitch.md` non-goals), so a client names the note, not the amplitude.
/// Wire: tag `0` in `w0`, `freq_hz` in `w1`, `duration_ms` in `w2`, `w3` reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Play {
    pub freq_hz: u32,
    pub duration_ms: u32,
}

impl Play {
    /// The request tag in `w0`. One request in v1; the tag rides the wire so more
    /// can be appended without a format break (append-only, never renumber).
    const TAG: u64 = 0;

    #[must_use]
    pub fn encode(&self) -> [u64; MSG_WORDS] {
        [Self::TAG, u64::from(self.freq_hz), u64::from(self.duration_ms), 0]
    }

    /// # Errors
    /// [`WireError::UnknownRequest`] if `w0` is not a known request tag.
    pub fn decode(words: [u64; MSG_WORDS]) -> Result<Play, WireError> {
        let [w0, w1, w2, _] = words;
        if w0 != Self::TAG {
            return Err(WireError::UnknownRequest(w0));
        }
        Ok(Play { freq_hz: w1 as u32, duration_ms: w2 as u32 })
    }
}

/// The server's reply to a [`Play`]. `w0` is the status: `0` = played, `1` =
/// refused (e.g. an unsupported frequency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Played,
    Refused,
}

impl Reply {
    #[must_use]
    pub fn encode(&self) -> [u64; MSG_WORDS] {
        let status = match self {
            Reply::Played => 0,
            Reply::Refused => 1,
        };
        [status, 0, 0, 0]
    }

    /// # Errors
    /// [`WireError::UnknownStatus`] if `w0` maps to no known outcome.
    pub fn decode(words: [u64; MSG_WORDS]) -> Result<Reply, WireError> {
        match words[0] {
            0 => Ok(Reply::Played),
            1 => Ok(Reply::Refused),
            other => Err(WireError::UnknownStatus(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_round_trips() {
        let req = Play { freq_hz: 440, duration_ms: 1000 };
        assert_eq!(Play::decode(req.encode()), Ok(req));
    }

    #[test]
    fn play_word_layout_is_locked() {
        // tag 0 in w0, freq in w1, duration in w2, w3 reserved.
        assert_eq!(Play { freq_hz: 440, duration_ms: 1000 }.encode(), [0, 440, 1000, 0]);
    }

    #[test]
    fn decode_rejects_an_unknown_request_tag() {
        assert_eq!(Play::decode([99, 0, 0, 0]), Err(WireError::UnknownRequest(99)));
    }

    #[test]
    fn reply_round_trips() {
        for r in [Reply::Played, Reply::Refused] {
            assert_eq!(Reply::decode(r.encode()), Ok(r));
        }
    }

    #[test]
    fn reply_status_layout_is_locked() {
        assert_eq!(Reply::Played.encode(), [0, 0, 0, 0]);
        assert_eq!(Reply::Refused.encode(), [1, 0, 0, 0]);
    }

    #[test]
    fn decode_rejects_an_unknown_reply_status() {
        assert_eq!(Reply::decode([7, 0, 0, 0]), Err(WireError::UnknownStatus(7)));
    }
}
