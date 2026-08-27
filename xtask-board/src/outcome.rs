//! How a capture ended, and what the shell should make of it.
//!
//! The bridge exists to be driven by something that is not a person, so its exit
//! code is a real interface rather than a formality. Three outcomes need to stay
//! distinguishable, because each one implies a *different next action* for an
//! unattended loop:
//!
//! | code | meaning | what the loop should do |
//! |---|---|---|
//! | 0 | the capture ended the way it was asked to | carry on |
//! | 1 | reached the board; the awaited event never came | reboot, or give up on this build |
//! | 2 | never reached the board at all | fix the host — retrying the *board* is useless |
//!
//! Collapsing 1 and 2 is the confusion [`crate::reach`] exists to prevent, one
//! layer up: a loop that reboots the board because a stray `screen` holds the
//! port burns a boot cycle every iteration and never gets anywhere.

use std::time::Duration;

use crate::reach::Unreachable;
use crate::stop::{StopCondition, StopReason};

/// Build a [`StopCondition`] from the CLI's flags.
///
/// Kept here, over plain integers and strings, so the mapping is testable without
/// standing up a `clap` parse — the surface it serves is `board exec`'s
/// `--timeout` / `--until` / `--until-quiet`.
///
/// # Errors
/// An empty `--until`. [`StopCondition::marker`] would drop it silently (an empty
/// substring matches at offset zero and would stop every capture on its first
/// byte), which is the right backstop and the wrong diagnostic: the operator
/// would see a capture that ran to its timeout with no clue why. Refuse it where
/// it was typed.
pub fn condition_from(
    timeout_ms: u64,
    until: Option<&str>,
    until_quiet_ms: Option<u64>,
) -> Result<StopCondition, String> {
    if until.is_some_and(str::is_empty) {
        return Err("--until needs a non-empty marker; an empty one would match immediately".into());
    }
    let mut condition = StopCondition::new(Duration::from_millis(timeout_ms));
    if let Some(marker) = until {
        condition = condition.marker(marker.as_bytes());
    }
    if let Some(ms) = until_quiet_ms {
        condition = condition.quiet_after(Duration::from_millis(ms));
    }
    Ok(condition)
}

/// What a `board exec` attempt produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A byte stream was obtained and the capture ran to one of its conditions.
    Stopped(StopReason),
    /// No byte stream was ever obtained.
    Unreachable(Unreachable),
}

/// The process exit code for `outcome`, given what the caller asked for.
///
/// A timeout is the one outcome whose meaning depends on the request:
/// `--timeout 3000` alone *asks* to capture for three seconds, so reaching the
/// deadline is exactly what was wanted, while `--until "=> "` reaching the same
/// deadline means the prompt never came. Same [`StopReason::Timeout`], opposite
/// verdicts — see [`StopCondition::awaits_an_event`].
#[must_use]
pub fn exit_code(condition: &StopCondition, outcome: &Outcome) -> u8 {
    match outcome {
        Outcome::Unreachable(_) => 2,
        Outcome::Stopped(reason) if condition.satisfied_by(*reason) => 0,
        Outcome::Stopped(_) => 1,
    }
}
