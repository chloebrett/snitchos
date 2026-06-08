//! Failure-signature classification.
//!
//! The integration suite's residual flake rate is not one bug — it is a
//! blend of several causes (see `plans/itest-flake-reduction.md`). This
//! module attributes each *failed* iteration to a cause-bucket so the
//! blended rate can be partitioned and each slice driven down
//! independently.
//!
//! Classification is a pure function over the evidence the harness can
//! capture at failure time. Crucially, the QEMU **log file is not
//! sufficient on its own**: kernel telemetry leaves over virtio (not the
//! UART log), the kernel does not UART-log per heartbeat, and a clean
//! harness `SIGKILL` leaves no marker — so every captured failure log
//! tail looks identical ("…entering heartbeat" then silence). The
//! load-bearing signal is whether the frame socket *disconnected* (QEMU
//! died → wedge) or merely *timed out* (QEMU alive, kernel still
//! emitting → alive-but-slow), which `Harness::wait_for` knows at the
//! moment of failure.

/// Which cause-bucket a failed iteration is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    /// QEMU's frame socket disconnected: the kernel stopped emitting and
    /// the process exited. The classic cross-hart-wedge signature — the
    /// only bucket that points at a genuine kernel residual.
    Wedge,
    /// Timed out with QEMU still alive and the kernel still emitting; the
    /// awaited frame never arrived inside the wall-clock budget. The
    /// "alive but slow" family: a too-tight budget, host-CPU starvation
    /// under parallelism, or cooperative-throughput variance. These are
    /// only separable from each other *across* runs (correlate with
    /// `--jobs`/host load), not from a single failure's evidence.
    BudgetExhausted,
    /// Timed out with QEMU still alive, but the kernel had gone *quiet*
    /// well before the deadline — it stopped making progress (deadlock,
    /// spin, lock held across a yield) rather than running slowly. A
    /// soft wedge: distinct from `Wedge` (process still up) and from
    /// `BudgetExhausted` (frames were still flowing at the deadline).
    Stalled,
    /// Infrastructure failure unrelated to kernel behaviour: QEMU spawn
    /// or socket-connect error, child-reap failure, or an external
    /// interrupt (SIGINT) tearing the run down mid-scenario.
    Harness,
    /// Evidence insufficient to attribute — e.g. a log captured before
    /// the harness recorded the disconnect/timeout signal. Honest
    /// non-answer rather than a forced guess.
    Unknown,
}

/// Where a failure's error originated. The harness stamps this at the
/// site that produces the error, so the classifier never has to infer
/// infra-vs-kernel from error text. This is the robust path; the error
/// string is only sniffed as a fallback for untagged historical data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorOrigin {
    /// Produced by harness infrastructure — QEMU launch, socket connect,
    /// kernel build, child reap. Not about the kernel under test.
    Harness,
    /// Produced by a scenario assertion — an awaited frame never arrived
    /// or an observed value didn't match. About kernel behaviour.
    Scenario,
}

/// Evidence captured about a single failed iteration. All fields are
/// optional where the harness may not have recorded them (older runs,
/// partial captures), so the classifier degrades gracefully on
/// historical data.
#[derive(Debug, Clone, Default)]
pub struct FailureEvidence<'a> {
    /// Whether the error came from the harness or from a scenario
    /// assertion, stamped by the harness. Authoritative when present;
    /// `None` only for captures that predate tagging.
    pub error_origin: Option<ErrorOrigin>,
    /// The scenario's returned error string (the `Err(_)` payload), e.g.
    /// `"no kernel.heartbeat within 30s"`.
    pub error: Option<&'a str>,
    /// Tail of the QEMU log file (kernel UART + QEMU's own stderr).
    pub log_tail: Option<&'a str>,
    /// `Some(true)` if the frame-reader channel disconnected (QEMU
    /// exited) rather than timing out; `Some(false)` for a clean
    /// timeout; `None` if the harness did not record it.
    pub disconnected: Option<bool>,
    /// Number of telemetry frames observed before the failure, if known.
    pub frames_seen: Option<u32>,
    /// Wall-clock gap, in milliseconds, between the last frame received
    /// and the deadline. Small → frames were flowing right up to the
    /// timeout (alive but slow). Large → the kernel went quiet long
    /// before the deadline (stalled). `None` if the harness did not
    /// record it.
    pub last_frame_wall_age_ms: Option<u32>,
}

/// Wall-clock silence, in milliseconds, beyond which a still-alive
/// kernel that timed out is treated as stalled rather than merely slow.
/// The heartbeat cadence is one guest-second (≈ one host-second
/// uncontended); silence this long is anomalous even allowing for heavy
/// host-CPU contention. Heuristic — the precise fix is to compare
/// against the scenario's own observed inter-frame cadence once that is
/// captured.
const STALL_QUIET_MS: u32 = 10_000;

/// Attribute a failed iteration to a cause-bucket.
#[must_use]
pub fn classify(evidence: &FailureEvidence) -> Signature {
    // An infra failure from the harness itself (QEMU launch, socket
    // connect, kernel build) is not about the kernel under test. Trust
    // the harness's explicit tag when present; only fall back to the
    // (fragile) error-string heuristic for untagged historical captures.
    match evidence.error_origin {
        Some(ErrorOrigin::Harness) => return Signature::Harness,
        Some(ErrorOrigin::Scenario) => {}
        None => {
            if evidence.error.is_some_and(is_harness_error) {
                return Signature::Harness;
            }
        }
    }

    // An external catchable signal (Ctrl-C, fail-fast teardown) tearing
    // the run down outranks every kernel-behaviour signal: the socket
    // disconnects, but the kernel didn't wedge. The harness's own
    // teardown is SIGKILL, which QEMU can't catch and so prints nothing
    // — any "terminating on signal" line is therefore external.
    if let Some(tail) = evidence.log_tail {
        if tail.contains("terminating on signal") {
            return Signature::Harness;
        }
        // The panic handler's UART marker is a definitive wedge — it
        // outranks the socket signal and resolves historical captures
        // that predate the disconnect/timeout field.
        if tail.contains("Kernel panic:") {
            return Signature::Wedge;
        }
    }

    match evidence.disconnected {
        Some(true) => Signature::Wedge,
        Some(false) => match evidence.last_frame_wall_age_ms {
            Some(age) if age >= STALL_QUIET_MS => Signature::Stalled,
            _ => Signature::BudgetExhausted,
        },
        None => Signature::Unknown,
    }
}

/// Markers that identify an error string as originating in the harness
/// infrastructure rather than a scenario assertion. Mirrors the `Err`
/// prefixes produced by `Harness::spawn` and the kernel build step.
fn is_harness_error(error: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "spawn qemu",
        "connect ",
        "build kernel",
        "kernel build failed",
        "clone log handle",
    ];
    MARKERS.iter().any(|m| error.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_disconnect_is_a_wedge() {
        let ev = FailureEvidence {
            error: Some("no kernel.heartbeat within 30s"),
            disconnected: Some(true),
            frames_seen: Some(3),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::Wedge);
    }

    #[test]
    fn external_sigint_teardown_is_harness_not_wedge() {
        // The run was torn down by a catchable signal (Ctrl-C, fail-fast
        // propagation) mid-scenario: QEMU prints "terminating on signal
        // N" and the socket disconnects. The harness's own teardown uses
        // SIGKILL, which is silent — so any "terminating on signal" line
        // means an *external* kill, not a kernel wedge. Must not be
        // counted as one.
        let ev = FailureEvidence {
            error: Some("no ThreadRegister for 'main' within 20s"),
            log_tail: Some(
                "virtio-console: ready\nI am alive — entering heartbeat\n\
                 qemu-system-riscv64: terminating on signal 2 from pid 1005",
            ),
            disconnected: Some(true),
            frames_seen: Some(2),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::Harness);
    }

    #[test]
    fn tagged_harness_origin_is_authoritative() {
        // The harness stamped this error as infra. Trust the tag — no
        // string inspection, no kernel-signal analysis.
        let ev = FailureEvidence {
            error_origin: Some(ErrorOrigin::Harness),
            error: Some("anything at all"),
            disconnected: Some(true),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::Harness);
    }

    #[test]
    fn scenario_tag_overrides_coincidental_infra_substring() {
        // A scenario assertion error that happens to contain an infra
        // marker substring must NOT be misattributed: the tag says it
        // came from the scenario, so the substring heuristic is moot.
        let ev = FailureEvidence {
            error_origin: Some(ErrorOrigin::Scenario),
            error: Some("no frame showing the harness could connect to peer within 30s"),
            disconnected: Some(false),
            frames_seen: Some(50),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::BudgetExhausted);
    }

    #[test]
    fn harness_spawn_error_is_harness_not_a_kernel_signal() {
        // The failure originated in the harness itself (QEMU launch),
        // not in kernel behaviour or a scenario assertion. No frames, no
        // disconnect signal — just an infra error string.
        let ev = FailureEvidence {
            error: Some("spawn qemu: No such file or directory (os error 2)"),
            disconnected: None,
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::Harness);
    }

    #[test]
    fn kernel_panic_in_log_is_a_wedge_even_without_disconnect_signal() {
        // Historical capture with no disconnect/timeout field recorded,
        // but the UART caught the panic handler's output. Unambiguously
        // a kernel wedge regardless of the socket signal.
        let ev = FailureEvidence {
            error: Some("no kernel.heartbeat within 30s"),
            log_tail: Some(
                "I am alive — entering heartbeat\n\
                 Kernel panic: index out of bounds: the len is 4 but the index is 9",
            ),
            disconnected: None,
            frames_seen: Some(4),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::Wedge);
    }

    #[test]
    fn timeout_after_long_silence_is_stalled() {
        // QEMU alive, but the kernel emitted nothing for well over a
        // heartbeat interval before the deadline — it stopped making
        // progress rather than running slowly.
        let ev = FailureEvidence {
            error: Some("no kernel.heartbeat within 30s"),
            disconnected: Some(false),
            frames_seen: Some(6),
            last_frame_wall_age_ms: Some(20_000),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::Stalled);
    }

    #[test]
    fn timeout_without_recorded_signals_is_unknown() {
        // Old capture: a scenario assertion timed out but the harness
        // recorded neither the disconnect/timeout flag nor a usable log.
        // Honest non-answer rather than a forced guess.
        let ev = FailureEvidence {
            error: Some("no kernel.heartbeat within 30s"),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::Unknown);
    }

    #[test]
    fn clean_timeout_with_frames_is_budget_exhausted() {
        // QEMU alive (no disconnect), kernel had been emitting frames —
        // the awaited frame just never arrived inside the budget. The
        // "alive but slow" family.
        let ev = FailureEvidence {
            error: Some("no kernel.heartbeat within 30s"),
            disconnected: Some(false),
            frames_seen: Some(120),
            ..Default::default()
        };
        assert_eq!(classify(&ev), Signature::BudgetExhausted);
    }
}
