//! When to stop capturing.
//!
//! The heart of `board exec`, and the part most likely to be subtly wrong. Three
//! conditions, composed into one value so a caller passes a single
//! [`StopCondition`] down and still learns *which* one fired:
//!
//! | Condition | Means | Reported as |
//! |---|---|---|
//! | marker | the bytes we were waiting for arrived | [`StopReason::Marker`] |
//! | quiescence | it spoke, then stopped | [`StopReason::Quiet`] |
//! | timeout | the whole capture ran out of time | [`StopReason::Timeout`] |
//!
//! **The timeout is mandatory and the other two are not.** That asymmetry makes
//! "a capture that never returns" unrepresentable, which is the property an
//! unattended bridge needs most — a wedged board must always produce an answer,
//! and that answer must say *timeout* rather than hang.
//!
//! **Time is a parameter, not a clock.** Every entry point takes elapsed-since-
//! capture-start, so this layer is a pure function of its arguments and its tests
//! are instant and exact. The one real `Instant::now()` lives in step 4's capture
//! loop, where the I/O is.
//!
//! **Quiescence is armed by the first byte, never by capture start.** Silence
//! before the board has said anything is not the board going quiet — it is the
//! board taking its time, and a 300 ms window would cut off an 800 ms boot. The
//! condition that answers total silence is the timeout, and it reports as itself.
//!
//! ⚠ **The quiescence window is a parameter of the *transport*, not a constant.**
//! Over a UART, silence means the board is silent. Over Phase 2's TCP transport it
//! may be a stall in the radio, and a window tuned for serial will false-fire and
//! report a wedged board that is fine — the exact failure this tool exists to
//! prevent. Prefer a marker over quiescence when on TCP.

use std::time::Duration;

/// Which condition ended the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The marker was found in the captured bytes.
    Marker,
    /// The board spoke, then said nothing for the whole quiescence window.
    Quiet,
    /// The capture's deadline passed.
    Timeout,
}

/// When a capture should stop. Built by [`StopCondition::new`] plus the two
/// optional builders; the fields are private so the invariants below hold for
/// every value [`Capture`] can be handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopCondition {
    timeout: Duration,
    quiet_after: Option<Duration>,
    /// Never `Some` of an empty slice — see [`StopCondition::marker`].
    marker: Option<Vec<u8>>,
}

impl StopCondition {
    /// A capture bounded only by its deadline.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self { timeout, quiet_after: None, marker: None }
    }

    /// Also stop once the board has been silent for `window` — measured from the
    /// last byte that actually arrived, and only after at least one has.
    #[must_use]
    pub fn quiet_after(self, window: Duration) -> Self {
        Self { quiet_after: Some(window), ..self }
    }

    /// Also stop on the first occurrence of `marker` in the byte stream,
    /// including one split across reads.
    ///
    /// **An empty marker is no marker.** Substring semantics would match it at
    /// offset zero and stop every capture on its first byte, so it is dropped
    /// here rather than allowed to become a silent misfire. Step 4's CLI rejects
    /// an empty `--until` at the surface; this is the backstop.
    #[must_use]
    pub fn marker(self, marker: impl Into<Vec<u8>>) -> Self {
        let marker = marker.into();
        Self { marker: (!marker.is_empty()).then_some(marker), ..self }
    }

    /// Was the caller waiting for the board to *do* something, or just watching
    /// it for a fixed window?
    ///
    /// This is what makes a timeout meaningful. Reaching the deadline is success
    /// for "capture for three seconds" and failure for "wait for the `=>`
    /// prompt", and the two are the same [`StopReason::Timeout`] — only the
    /// request tells them apart. See `crate::outcome::exit_code`.
    #[must_use]
    pub const fn awaits_an_event(&self) -> bool {
        self.marker.is_some() || self.quiet_after.is_some()
    }
}

/// Evaluates a [`StopCondition`] against a stream of arrivals.
///
/// Holds straddle context, never history: at most `marker.len() - 1` bytes, which
/// is the most a split match can ever need, since anything longer would already
/// have matched in full. A capture may run for minutes on a serial line, so this
/// bound is a requirement rather than an optimisation — see
/// [`Capture::retained_bytes`].
#[derive(Debug)]
pub struct Capture {
    condition: StopCondition,
    /// Recent bytes, retained only far enough to complete a split marker.
    tail: Vec<u8>,
    /// When the last non-empty arrival landed. `None` until the board speaks —
    /// this is what arms quiescence.
    last_bytes_at: Option<Duration>,
}

impl Capture {
    #[must_use]
    pub fn new(condition: StopCondition) -> Self {
        Self { condition, tail: Vec::new(), last_bytes_at: None }
    }

    /// Feed an arrival and learn whether the capture should stop.
    ///
    /// `since_start` is elapsed time since the capture began. **An empty `bytes`
    /// is a timer tick, not data** — a serial read that returned zero bytes has
    /// told us nothing new, and treating it as an arrival would refresh the quiet
    /// clock on every poll and stop quiescence from ever firing. [`Capture::tick`]
    /// says the same thing more plainly.
    ///
    /// Precedence when several conditions come due on the same event is
    /// `Marker` > `Timeout` > `Quiet`: a marker found in bytes that genuinely
    /// arrived within the capture is a success, and reporting it as a timeout
    /// would make a working round-trip look like a wedged board.
    pub fn observe(&mut self, since_start: Duration, bytes: &[u8]) -> Option<StopReason> {
        if !bytes.is_empty() {
            self.last_bytes_at = Some(since_start);
            if self.absorb(bytes) {
                return Some(StopReason::Marker);
            }
        }

        if since_start >= self.condition.timeout {
            return Some(StopReason::Timeout);
        }

        let window = self.condition.quiet_after?;
        let silent_since = self.last_bytes_at?;
        (since_start.saturating_sub(silent_since) >= window).then_some(StopReason::Quiet)
    }

    /// Time passed and nothing arrived.
    ///
    /// Spelled out rather than left to `observe(t, &[])` because a capture loop
    /// polling a quiet port calls this far more often than it sees data, and the
    /// empty-slice-means-tick convention should not have to be remembered at
    /// every call site.
    pub fn tick(&mut self, since_start: Duration) -> Option<StopReason> {
        self.observe(since_start, &[])
    }

    /// How many bytes of straddle context are currently held. Always less than
    /// the marker's length, and zero when there is no marker.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.tail.len()
    }

    /// Append `bytes` to the straddle context, report whether the marker now
    /// appears, and trim back to the bound.
    ///
    /// Searching `tail ++ bytes` rather than `bytes` alone is what catches a
    /// marker split across reads. A match cannot lie wholly inside `tail` — it is
    /// shorter than the marker — so no match is ever reported twice.
    fn absorb(&mut self, bytes: &[u8]) -> bool {
        let Some(marker) = &self.condition.marker else { return false };

        self.tail.extend_from_slice(bytes);
        let found = self.tail.windows(marker.len()).any(|window| window == marker.as_slice());

        let keep = marker.len() - 1;
        let excess = self.tail.len().saturating_sub(keep);
        drop(self.tail.drain(..excess));

        found
    }
}
