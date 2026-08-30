//! Getting a board's attention, and knowing which thing answered.
//!
//! U-Boot's `bootdelay` is two seconds, so the only reliable way to reach its
//! prompt is to already be typing when the window opens. That much is a glue
//! concern — write a key, wait, repeat. The part worth its own module is deciding
//! **what the board said back**, because three situations look identical to a
//! naive reader and need opposite responses:
//!
//! | what is true | what to do |
//! |---|---|
//! | we are at the target prompt | send the commands |
//! | something else is running (`SnitchOS` answered) | it autobooted past the window — power-cycle |
//! | nothing is answering | keep knocking, then give up |
//!
//! # Why answers are scoped to a probe
//!
//! The board prints its prompt once and then says nothing. So "the prompt is
//! somewhere in what we have read" is not the question — a prompt printed before
//! the port was opened, or before this knock, proves only that the board *was*
//! there. Asking properly means asking *again*: send a bare carriage return, then
//! judge only the bytes that arrive after it. [`Knock::probe`] is that question
//! and it discards the previous answer.
//!
//! This is not hypothetical tidiness. On 2026-08-28 a catch loop that scanned a
//! rolling buffer sat for six minutes against a board that had autobooted into
//! `SnitchOS` and was cheerfully echoing every keystroke, because a `StarFive #`
//! from a previous power cycle was still in the window. The board was fine; the
//! question was wrong.
//!
//! # Why other prompts are named rather than merely absent
//!
//! Reporting "no prompt" for a board that is plainly answering is the same
//! failure the crate spends [`crate::reach`] avoiding one layer down: a symptom
//! that fits several causes, offered as though it named one.

/// What answered a [`Knock::probe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The prompt we are trying to reach.
    Target,
    /// A different prompt we know how to recognise, under the name it was
    /// registered with — so the caller can say *what* is running, not just that
    /// it isn't what we wanted.
    Other(String),
}

/// Watches for a prompt in the bytes since the last probe.
///
/// Holds no timing policy: when to knock and when to give up belong to the
/// caller, which is what keeps this host-testable.
pub struct Knock {
    target: Vec<u8>,
    others: Vec<(String, Vec<u8>)>,
    /// Bytes seen since the last [`probe`](Self::probe). Grows only within one
    /// probe interval — a knock cadence of about a second bounds it in practice,
    /// and a fixed-size window would be wrong here anyway: echoed keystrokes
    /// would push the prompt out of it.
    since_probe: Vec<u8>,
    /// Whether a probe has been sent at all. Until one has, there is no question
    /// outstanding and so no answer to be had — bytes arriving before we asked are
    /// the board talking to itself (a boot log, an echo), never a reply.
    asked: bool,
}

impl Knock {
    /// Watch for `target`.
    #[must_use]
    pub fn new(target: impl Into<Vec<u8>>) -> Self {
        Self {
            target: target.into(),
            others: Vec::new(),
            since_probe: Vec::new(),
            asked: false,
        }
    }

    /// Also recognise `prompt`, reporting it as `name`.
    #[must_use]
    pub fn also(mut self, prompt: impl Into<Vec<u8>>, name: impl Into<String>) -> Self {
        self.others.push((name.into(), prompt.into()));
        self
    }

    /// Ask again: discard whatever the last probe collected. Call immediately
    /// after writing the probe byte, so the answer window starts there.
    pub fn probe(&mut self) {
        self.since_probe.clear();
        self.asked = true;
    }

    /// Feed freshly-read bytes; returns what answered, if anything has yet.
    ///
    /// Accumulates rather than matching per-read, because a prompt straddles read
    /// boundaries as often as not on a real line.
    pub fn observe(&mut self, bytes: &[u8]) -> Option<Answer> {
        if !self.asked {
            return None;
        }
        self.since_probe.extend_from_slice(bytes);
        if contains(&self.since_probe, &self.target) {
            return Some(Answer::Target);
        }
        self.others
            .iter()
            .find(|(_, prompt)| contains(&self.since_probe, prompt))
            .map(|(name, _)| Answer::Other(name.clone()))
    }
}

/// Whether `haystack` contains `needle`. An empty needle never matches — it would
/// otherwise report every board as answering, including a dead one.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}
