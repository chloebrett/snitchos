//! A soonest-deadline timer wheel multiplexing the one per-hart timer (`mtimecmp`,
//! armed via SBI `set_timer`) between two periodic clients: the scheduler tick
//! (always on) and the audio sample feed (enabled only while a stream plays). The
//! kernel arms the hardware timer at [`TimerWheel::deadline`] — `min(next_audio,
//! next_sched)` — and on each fire calls [`TimerWheel::poll`] to learn which
//! deadline(s) are due, running the scheduler tick and/or feeding one DAC sample
//! accordingly. Pure arithmetic, host-tested; the kernel owns the live instance and
//! the actual CSR/SBI writes.
//!
//! **Why a wheel and not two timers.** The U74s (and QEMU/snemu) expose one
//! supervisor timer per hart, already driving the scheduler. The audio feed needs a
//! second, faster periodic deadline; multiplexing the single timer by soonest
//! deadline is the standard driver answer and generalizes to any future periodic
//! device. See `plans/glitch-v2-async-ring.md` (Architecture decision 3).
//!
//! **Missed deadlines drop their backlog.** If servicing is late (a deadline is
//! already in the past when [`poll`](TimerWheel::poll) runs), the deadline re-arms to
//! the next slot strictly after `now` rather than replaying every missed period — one
//! late tick, not a catch-up storm. For audio this trades a momentary pitch slip for
//! forward progress; the ring's own under-run accounting is the deadline observable.

/// Which of the multiplexed deadlines are due on a given [`poll`](TimerWheel::poll).
/// Both can be true when they coincide; both false when the timer fired early (e.g.
/// re-armed for another hart's work, or clock granularity).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Due {
    /// An audio sample feed is due (only ever true while audio is enabled).
    pub audio: bool,
    /// A scheduler tick is due.
    pub sched: bool,
}

/// Two periodic deadlines multiplexed onto one hardware timer. Deadlines are absolute
/// tick counts (the `time` CSR domain); periods are tick intervals.
pub struct TimerWheel {
    sched_period: u64,
    audio_period: u64,
    next_sched: u64,
    next_audio: Option<u64>,
}

impl TimerWheel {
    /// A wheel whose scheduler tick fires every `sched_period` ticks starting one
    /// period after `now`. Audio starts disabled — [`enable_audio`](Self::enable_audio)
    /// turns it on when a stream begins.
    #[must_use]
    pub const fn new(now: u64, sched_period: u64) -> Self {
        Self {
            sched_period,
            audio_period: 0,
            next_sched: now + sched_period,
            next_audio: None,
        }
    }

    /// Begin feeding audio: the next sample is due `period` ticks after `now`, and
    /// every `period` ticks thereafter.
    pub fn enable_audio(&mut self, now: u64, period: u64) {
        self.audio_period = period;
        self.next_audio = Some(now + period);
    }

    /// Stop feeding audio — the audio deadline drops out of the multiplex until the
    /// next [`enable_audio`](Self::enable_audio).
    pub fn disable_audio(&mut self) {
        self.next_audio = None;
    }

    /// Whether an audio deadline is currently in the multiplex.
    #[must_use]
    pub const fn audio_enabled(&self) -> bool {
        self.next_audio.is_some()
    }

    /// The absolute deadline to arm the hardware timer at — the sooner of the two.
    #[must_use]
    pub fn deadline(&self) -> u64 {
        match self.next_audio {
            Some(audio) => audio.min(self.next_sched),
            None => self.next_sched,
        }
    }

    /// Report which deadlines are due at `now`, re-arming each that fired to the next
    /// slot strictly after `now` (a missed deadline drops its backlog rather than
    /// firing repeatedly to catch up).
    pub fn poll(&mut self, now: u64) -> Due {
        let sched = now >= self.next_sched;
        if sched {
            self.next_sched = rearm_past(self.next_sched, self.sched_period, now);
        }
        let audio = matches!(self.next_audio, Some(deadline) if now >= deadline);
        if audio {
            let deadline = rearm_past(self.next_audio.unwrap_or(now), self.audio_period, now);
            self.next_audio = Some(deadline);
        }
        Due { audio, sched }
    }
}

/// The next multiple of `period` past `deadline` that is strictly greater than `now`,
/// given `deadline <= now`. Computed directly (not by looping) so a long stall costs
/// O(1). `period` is a positive construction invariant.
fn rearm_past(deadline: u64, period: u64, now: u64) -> u64 {
    let missed = (now - deadline) / period + 1;
    deadline + missed * period
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_is_the_sched_period_when_audio_is_disabled() {
        let w = TimerWheel::new(100, 10);
        assert!(!w.audio_enabled());
        assert_eq!(w.deadline(), 110);
    }

    #[test]
    fn enabling_audio_makes_the_deadline_the_sooner_of_the_two() {
        let mut w = TimerWheel::new(100, 10); // next sched = 110
        w.enable_audio(100, 3); // next audio = 103
        assert!(w.audio_enabled());
        assert_eq!(w.deadline(), 103);
    }

    #[test]
    fn a_poll_before_any_deadline_reports_nothing_due() {
        let mut w = TimerWheel::new(100, 10);
        assert_eq!(w.poll(109), Due { audio: false, sched: false });
        assert_eq!(w.deadline(), 110); // unchanged
    }

    #[test]
    fn a_sched_deadline_fires_and_rearms_one_period_on() {
        let mut w = TimerWheel::new(100, 10);
        assert_eq!(w.poll(110), Due { audio: false, sched: true });
        assert_eq!(w.deadline(), 120);
    }

    #[test]
    fn an_audio_deadline_fires_and_rearms_without_disturbing_sched() {
        let mut w = TimerWheel::new(100, 10); // sched at 110
        w.enable_audio(100, 3); // audio at 103
        assert_eq!(w.poll(103), Due { audio: true, sched: false });
        assert_eq!(w.deadline(), 106); // next audio, still sooner than sched 110
    }

    #[test]
    fn coincident_deadlines_both_fire_and_both_rearm() {
        let mut w = TimerWheel::new(100, 10); // sched at 110
        w.enable_audio(100, 10); // audio also at 110
        assert_eq!(w.poll(110), Due { audio: true, sched: true });
        assert_eq!(w.deadline(), 120);
    }

    #[test]
    fn a_missed_deadline_rearms_past_now_not_into_the_past() {
        let mut w = TimerWheel::new(100, 10); // sched at 110
        // Serviced late at 135 — 110, 120, 130 all elapsed. One fire, re-arm to 140.
        assert_eq!(w.poll(135), Due { audio: false, sched: true });
        assert_eq!(w.deadline(), 140); // not 120 — the backlog is dropped
    }

    #[test]
    fn audio_is_never_due_while_disabled_even_past_its_would_be_deadline() {
        let mut w = TimerWheel::new(100, 10);
        assert_eq!(w.poll(1_000), Due { audio: false, sched: true });
    }

    #[test]
    fn disabling_audio_removes_it_from_the_deadline() {
        let mut w = TimerWheel::new(100, 10);
        w.enable_audio(100, 3); // deadline now 103
        w.disable_audio();
        assert!(!w.audio_enabled());
        assert_eq!(w.deadline(), 110); // back to sched only
    }
}
