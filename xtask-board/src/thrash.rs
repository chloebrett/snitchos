//! The thrash guard: whether a reboot is allowed *right now*.
//!
//! A board that reboots on command is a board a bad build can hammer. An image
//! that panics during boot looks, to an unattended loop, exactly like an image
//! that has not finished booting — so the loop reboots, and does it again, at
//! whatever rate the host can drive. The guard makes that unrepresentable.
//!
//! **Pure by construction**: `(history, now, policy) -> Verdict`. No clock, no
//! serial port, no filesystem. The caller reads the wall clock and owns the
//! history; this decides. That keeps the interesting cases — back-to-back
//! reboots, the cap boundary, a clock that moves backwards — table-testable
//! rather than reachable only by waiting.

use std::time::Duration;

/// Reboot rate limits. Both are enforced; either can deny.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Minimum time between two reboots.
    pub min_interval: Duration,
    /// Total reboots allowed in this session. `0` forbids every reboot, which is
    /// a legitimate way to run the loop read-only.
    pub cap: usize,
}

/// Why a reboot was refused. Each variant carries the numbers a human needs to
/// act, because "denied" alone leaves the operator to guess which limit fired
/// and by how much.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denial {
    /// The previous reboot was too recent.
    TooSoon { since_last: Duration, required: Duration },
    /// This session has spent its reboot budget.
    CapReached { cap: usize },
}

/// The guard's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Denied(Denial),
}

/// Decide whether a reboot may proceed.
///
/// `history` is the reboot instants already spent this session and `now` the
/// current instant, both in milliseconds on a shared, arbitrary epoch — the
/// caller's only obligation is that they come from the same clock.
///
/// The cap is checked **before** the interval so an exhausted budget reports
/// itself rather than being masked by a "too soon" that a little patience would
/// clear. The two denials call for opposite actions — wait, versus stop and look
/// at the build — and a guard that reports the recoverable one first sends the
/// operator to wait out a limit that will never lift.
#[must_use]
pub fn judge(history: &[u64], now: u64, policy: &Policy) -> Verdict {
    if history.len() >= policy.cap {
        return Verdict::Denied(Denial::CapReached { cap: policy.cap });
    }
    let Some(&last) = history.iter().max() else {
        return Verdict::Allow;
    };
    // `saturating_sub`, not `-`: a wall clock can move backwards (NTP, a laptop
    // waking, a manual change), and a subtraction that underflows would wrap to
    // ~584 million years and wave the reboot through — the guard failing open at
    // exactly the moment its input became untrustworthy.
    let since_last = Duration::from_millis(now.saturating_sub(last));
    if since_last < policy.min_interval {
        return Verdict::Denied(Denial::TooSoon { since_last, required: policy.min_interval });
    }
    Verdict::Allow
}

/// The reboot instants that still count, given a rolling `window`.
///
/// **Why the cap needs a window at all.** A one-shot CLI has no "session": if the
/// history file simply accumulated, the cap would eventually deny every reboot
/// forever, and the guard meant to stop a boot-loop would instead brick the tool
/// on a quiet Tuesday. Pruning makes the cap mean "this many reboots in the last
/// `window`", which is the property actually wanted, while leaving [`judge`] pure
/// and unaware — the caller decides what counts, the guard decides on it.
///
/// Entries in the future (a clock that moved backwards between runs) are kept:
/// they are evidence of recent activity, and discarding them would loosen the
/// guard exactly when its input is least trustworthy.
#[must_use]
pub fn prune(history: &[u64], now: u64, window: Duration) -> Vec<u64> {
    let cutoff = now.saturating_sub(window.as_millis().try_into().unwrap_or(u64::MAX));
    history.iter().copied().filter(|&t| t >= cutoff).collect()
}

/// Parse a history file: one instant per line, in milliseconds.
///
/// Unparseable lines are skipped rather than refused. This **fails open**, and
/// deliberately: a truncated or hand-edited state file would otherwise wedge a
/// bench tool permanently, and the cost of the alternative — a dev board that
/// cannot be rebooted until a file is deleted — is worse than a guard that is
/// briefly more permissive. Recorded here so it is a decision, not an accident.
#[must_use]
pub fn parse_history(text: &str) -> Vec<u64> {
    text.lines().filter_map(|line| line.trim().parse::<u64>().ok()).collect()
}

/// Render a history back to the file format [`parse_history`] reads.
#[must_use]
pub fn render_history(history: &[u64]) -> String {
    let mut out = String::new();
    for t in history {
        out.push_str(&t.to_string());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Denial, Policy, Verdict, judge, parse_history, prune, render_history};
    use std::time::Duration;

    #[test]
    fn pruning_drops_entries_older_than_the_window() {
        assert_eq!(prune(&[1_000, 50_000], 60_000, Duration::from_millis(20_000)), vec![50_000]);
    }

    /// The boundary: an entry exactly at the cutoff is still inside the window.
    #[test]
    fn pruning_keeps_an_entry_exactly_at_the_cutoff() {
        assert_eq!(prune(&[40_000], 60_000, Duration::from_millis(20_000)), vec![40_000]);
    }

    /// A clock that moved backwards leaves future-dated entries. Keep them —
    /// dropping them would loosen the guard precisely when the clock is suspect.
    #[test]
    fn pruning_keeps_entries_in_the_future() {
        assert_eq!(prune(&[90_000], 60_000, Duration::from_millis(20_000)), vec![90_000]);
    }

    #[test]
    fn a_history_round_trips_through_the_file_format() {
        let history = vec![1_000, 2_000, 3_000];
        assert_eq!(parse_history(&render_history(&history)), history);
    }

    #[test]
    fn an_empty_history_round_trips_as_an_empty_file() {
        assert!(render_history(&[]).is_empty());
        assert!(parse_history("").is_empty());
    }

    /// Fails open by design — see [`parse_history`]. A half-written last line
    /// must cost one entry, not the ability to reboot.
    #[test]
    fn a_corrupt_history_line_is_skipped_rather_than_refused() {
        assert_eq!(parse_history("1000\nnot-a-number\n\n3000\n"), vec![1_000, 3_000]);
    }

    fn policy() -> Policy {
        Policy { min_interval: Duration::from_millis(1000), cap: 3 }
    }

    #[test]
    fn the_first_reboot_of_a_session_is_allowed() {
        assert_eq!(judge(&[], 0, &policy()), Verdict::Allow);
    }

    #[test]
    fn a_reboot_long_after_the_last_is_allowed() {
        assert_eq!(judge(&[1_000], 9_000, &policy()), Verdict::Allow);
    }

    #[test]
    fn a_reboot_too_soon_after_the_last_is_denied_with_both_numbers() {
        assert_eq!(
            judge(&[1_000], 1_400, &policy()),
            Verdict::Denied(Denial::TooSoon {
                since_last: Duration::from_millis(400),
                required: Duration::from_millis(1000),
            })
        );
    }

    /// The boundary, stated explicitly: the interval is a *minimum*, so landing
    /// exactly on it is allowed. An off-by-one here is invisible in every other
    /// test.
    #[test]
    fn a_reboot_exactly_at_the_interval_is_allowed() {
        assert_eq!(judge(&[1_000], 2_000, &policy()), Verdict::Allow);
    }

    #[test]
    fn a_reboot_one_millisecond_short_of_the_interval_is_denied() {
        assert!(matches!(
            judge(&[1_000], 1_999, &policy()),
            Verdict::Denied(Denial::TooSoon { .. })
        ));
    }

    /// The cap counts reboots already spent, so a history at the cap denies —
    /// the classic off-by-one would allow one more.
    #[test]
    fn a_history_at_the_cap_is_denied() {
        assert_eq!(
            judge(&[1_000, 3_000, 5_000], 900_000, &policy()),
            Verdict::Denied(Denial::CapReached { cap: 3 })
        );
    }

    #[test]
    fn a_history_one_below_the_cap_still_allows_one_more() {
        assert_eq!(judge(&[1_000, 3_000], 900_000, &policy()), Verdict::Allow);
    }

    /// A zero cap is a real configuration — the read-only loop — and must deny
    /// even the first reboot, where there is no history to compare against.
    #[test]
    fn a_zero_cap_denies_the_very_first_reboot() {
        let p = Policy { min_interval: Duration::from_millis(0), cap: 0 };
        assert_eq!(judge(&[], 0, &p), Verdict::Denied(Denial::CapReached { cap: 0 }));
    }

    /// **Which limit is reported matters.** When both would deny, the cap wins:
    /// waiting clears "too soon" but never clears an exhausted budget, so
    /// reporting the recoverable one would send the operator to wait out a limit
    /// that will not lift.
    #[test]
    fn an_exhausted_cap_is_reported_even_when_the_interval_also_denies() {
        assert_eq!(
            judge(&[1_000, 2_000, 3_000], 3_001, &policy()),
            Verdict::Denied(Denial::CapReached { cap: 3 })
        );
    }

    /// A clock that jumps backwards must deny, not wave the reboot through. The
    /// naive subtraction underflows and yields an enormous interval, which reads
    /// as "ages since the last reboot" — failing open exactly when the input
    /// became untrustworthy.
    #[test]
    fn a_clock_that_moves_backwards_denies_rather_than_underflowing() {
        assert!(matches!(
            judge(&[9_000], 1_000, &policy()),
            Verdict::Denied(Denial::TooSoon { .. })
        ));
    }

    /// The guard reads the *most recent* reboot, not the last element, so an
    /// unordered history cannot smuggle a reboot past the interval.
    #[test]
    fn an_unordered_history_is_judged_against_its_most_recent_entry() {
        assert!(matches!(
            judge(&[9_000, 1_000], 9_200, &policy()),
            Verdict::Denied(Denial::TooSoon { .. })
        ));
    }
}
