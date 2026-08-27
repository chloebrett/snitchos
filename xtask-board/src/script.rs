//! A conversation with the board: send, wait for a specific answer, send the next.
//!
//! `exec` sends one thing and captures one answer, which is the right primitive
//! and the wrong shape for most of what the bridge has to do. Every remaining
//! command in [plans/board-bridge.md](../../plans/board-bridge.md) is a
//! send/expect sequence:
//!
//! | command | the conversation it is |
//! |---|---|
//! | `uboot "<cmd>"` | keystroke → until `=> ` → cmd → until `=> ` |
//! | `provision` | N × (`setenv …` → until `=> `), then `saveenv` → until `=> ` |
//! | `boot --workload X` | setenv bootargs → until `=> ` → `boot` → until a boot marker |
//!
//! Written as three hand-rolled loops those are three chances to get the same
//! thing wrong. Written as [`Step`] values they are configurations, and the one
//! loop that runs them is tested once — the same move [`crate::stop`] already
//! made one level down, where composing the three stop conditions into a single
//! value meant there was never a special case to unify later.
//!
//! # The rule that makes this worth a module
//!
//! **A step that never saw what it awaited abandons the rest, unsent.** If the
//! prompt did not arrive, the board is not at a prompt — it is booting, or
//! wedged, or halfway through a `saveenv`. Typing the next command into it is not
//! a failed step, it is an *unpredictable* one, and a `provision` that writes half
//! an environment is worse than one that writes none.
//!
//! # Why the I/O is a closure
//!
//! [`run`] takes what to do with a step rather than a port, so the sequencing is
//! pure and host-tested while the writing and capturing stay glue. That matters
//! here specifically: the property worth testing is which steps *never reached the
//! wire*, and that is only observable if a test can stand where the wire does.

use crate::stop::{StopCondition, StopReason};

/// One turn: bytes to send, and what to wait for afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    send: Vec<u8>,
    until: StopCondition,
}

impl Step {
    /// A turn. `send` may be empty — a step that only waits is a legitimate move
    /// (letting a board finish booting before typing at it).
    ///
    /// No trailing newline is added: whether a command needs `\r`, `\n` or
    /// nothing is the caller's business, and a hidden one is the kind of thing
    /// that works at a U-Boot prompt and doubles a line in a REPL.
    #[must_use]
    pub fn new(send: impl Into<Vec<u8>>, until: StopCondition) -> Self {
        Self { send: send.into(), until }
    }

    #[must_use]
    pub fn send(&self) -> &[u8] {
        &self.send
    }

    #[must_use]
    pub const fn until(&self) -> &StopCondition {
        &self.until
    }
}

/// How a whole conversation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptOutcome {
    /// Every step reached what it was waiting for, in order.
    Completed { reasons: Vec<StopReason> },
    /// Step `index` did not, and nothing after it was sent. `reason` is kept
    /// rather than flattened: "the prompt never came" and "it went quiet" call
    /// for different next moves.
    Abandoned { index: usize, reason: StopReason },
    /// The transport failed part-way through step `index`.
    ///
    /// Deliberately **not** the same variant as [`ScriptOutcome::Abandoned`]:
    /// one means the board did not answer, the other means we stopped listening,
    /// and collapsing them is the confusion [`crate::reach`] exists to prevent.
    /// This layer says *where* it died; the caller — which holds the `io::Error`
    /// or the [`crate::reach::Unreachable`] — says why.
    Interrupted { index: usize },
}

/// Run a conversation, performing each step until one fails or all succeed.
///
/// `perform` writes the step and captures until its condition, returning the
/// [`StopReason`] that ended the capture, or `None` if the transport failed.
///
/// Stops at the first step that is not satisfied — see the module docs for why
/// that is a safety property rather than an optimisation.
pub fn run<F>(steps: &[Step], mut perform: F) -> ScriptOutcome
where
    F: FnMut(&Step) -> Option<StopReason>,
{
    let mut reasons = Vec::with_capacity(steps.len());

    for (index, step) in steps.iter().enumerate() {
        let Some(reason) = perform(step) else {
            return ScriptOutcome::Interrupted { index };
        };
        if !step.until.satisfied_by(reason) {
            return ScriptOutcome::Abandoned { index, reason };
        }
        reasons.push(reason);
    }

    ScriptOutcome::Completed { reasons }
}
