//! Test harness, split into two pieces:
//!
//! - [`Boot`] owns one QEMU process + a frame [`Recorder`] (a reader
//!   thread decodes the virtio-console socket and appends to it). `Drop`
//!   kills QEMU and removes the socket, so a panicking test still cleans
//!   up. One `Boot` per scenario (separate mode) or per workload group
//!   (shared mode).
//! - [`View`] is one scenario's read-cursor over a boot's recorded
//!   stream, obtained from [`Boot::view`]. `wait_for` blocks up to a
//!   per-call budget for a frame matching a predicate; several Views
//!   replay the same boot independently from frame 0.
//!
//! The executor in `itest.rs` wires them together: spawn a `Boot`, run
//! each scenario against a fresh `View`, read `max_wait()` / `take_capture()`
//! into a `ScenarioReport`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use protocol::stream::{OwnedFrame, decode_stream};
use protocol::StringId;

use itest_harness::{CaptureLevel, ErrorOrigin, FailureCapture, WaitOutcome};

use crate::qemu;

/// Maps `StringId` → name as we observe `StringRegister` frames. Matchers
/// read this so they can say "is this span 'kernel.boot'?" without
/// hard-coding ids.
pub type StringTable = HashMap<StringId, String>;

/// Append-only recording of the decoded frame stream, shared between the
/// reader thread (which pushes) and the assertion side (which scans).
/// Replaces the old consume-once mpsc channel: frames are *retained*, not
/// drained, so the assertion side advances a cursor over them rather than
/// pulling each frame out once. A single cursor today (`Harness::cursor`);
/// the retention is what will let multiple `View`s replay one boot later.
struct Recorder {
    buf: Mutex<RecordBuf>,
    /// Notified on every append and on close, so a waiter blocked at the
    /// end of the buffer wakes when a frame arrives or the stream ends.
    grew: Condvar,
}

impl Recorder {
    fn new() -> Self {
        Self {
            buf: Mutex::new(RecordBuf { frames: Vec::new(), closed: false }),
            grew: Condvar::new(),
        }
    }

    /// Append one decoded frame and wake any waiter sitting at the end.
    fn push(&self, frame: OwnedFrame) {
        let mut buf = self.buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        buf.frames.push(frame);
        self.grew.notify_all();
    }

    /// Mark the stream ended (socket EOF / QEMU exit) and wake waiters so a
    /// handle caught up at the end sees the disconnect instead of waiting
    /// out its budget.
    fn close(&self) {
        let mut buf = self.buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        buf.closed = true;
        self.grew.notify_all();
    }

    /// Step `cursor` one frame forward, blocking until the next frame is
    /// available, the stream closes, or `deadline` passes. `cursor` is the
    /// caller's own position — the buffer is never drained, so independent
    /// cursors (the future multi-`View` case) each replay from 0.
    fn advance(&self, cursor: &mut usize, deadline: Instant) -> Advance {
        let mut buf = self.buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(frame) = buf.frames.get(*cursor) {
                let frame = frame.clone();
                *cursor += 1;
                return Advance::Frame(frame);
            }
            if buf.closed {
                return Advance::Disconnected;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Advance::Timeout;
            };
            buf = self
                .grew
                .wait_timeout(buf, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// The append-only buffer plus its end-of-stream flag, under one mutex so
/// the "frame available or stream closed" predicate the condvar guards is
/// checked atomically.
struct RecordBuf {
    frames: Vec<OwnedFrame>,
    /// Set by the reader thread when `decode_stream` returns (socket EOF /
    /// QEMU exit). A waiter that reaches the end with `closed == true` sees
    /// a disconnect rather than waiting out its budget.
    closed: bool,
}

/// Outcome of advancing the cursor by one — the record-and-replay analogue
/// of `mpsc::RecvTimeoutError`. `Frame` carries the next frame (cloned out
/// of the buffer); `Timeout` / `Disconnected` mirror the old channel ends.
enum Advance {
    Frame(OwnedFrame),
    Timeout,
    Disconnected,
}

/// Owns one live QEMU process and its frame `Recorder`. Spawned per
/// scenario (separate mode) or per workload group (shared mode); `Drop`
/// kills QEMU and unlinks the socket. Hand out one `View` per scenario
/// via `view()` — several Views replay the same recorded boot.
pub struct Boot {
    qemu: Child,
    socket_path: PathBuf,
    /// Shared recording of the frame stream (the reader thread appends);
    /// each `View` holds its own `Arc` clone + cursor.
    recorder: Arc<Recorder>,
    /// QEMU log (kernel UART + QEMU stderr); the runner dumps its tail on
    /// failure. Surfaced to the executor via `log_path()`.
    log_path: PathBuf,
    /// The runtime workload this boot selected (`-append workload=<name>`),
    /// or `None` for the default demo. Copied into each `View` for capture.
    workload: Option<String>,
}

/// One scenario's read-cursor over a `Boot`'s recorded frame stream, plus
/// the per-scenario assertion bookkeeping. Obtained from `Boot::view`.
/// The frame-assertion API scenarios use: `wait_for`, `assert_absent`,
/// `name_of`, `timebase_hz`. The executor reads `max_wait()` /
/// `take_capture()` afterwards to build the scenario's report.
pub struct View {
    /// Shared with the owning `Boot`; retained so a `View` can keep
    /// scanning buffered frames even after `Boot` drops (kills QEMU).
    recorder: Arc<Recorder>,
    /// This view's position in `recorder.buf.frames`. Advancing it is
    /// what `wait_for` does; `absorb` runs once per frame stepped over.
    cursor: usize,
    strings: StringTable,
    timebase_hz: Option<u64>,
    /// Rolling window of the last few frames received. Printed on
    /// timeout so failures say "boot reached Hello + `SpanStart`, then
    /// nothing" rather than just "no heartbeat within 10s".
    recent: VecDeque<OwnedFrame>,
    /// The longest single `wait_for` this scenario has issued so far,
    /// together with the budget for that wait. The runner reads this
    /// after each scenario to print "max waited 1.6s of 30s budget",
    /// which surfaces over-sized budgets without anyone digging
    /// through logs.
    max_wait: (Duration, Duration),
    /// Total telemetry frames absorbed this scenario — the
    /// `frames_seen` field of a failure capture.
    frames_seen: u32,
    /// When the most recent frame arrived, for computing the wall-clock
    /// silence before a failing wait's deadline (stalled vs slow).
    last_frame_at: Option<Instant>,
    /// How much frame transcript to retain — fixed at spawn from the
    /// process-wide level. `Full` lets `recent` grow unbounded.
    capture_level: CaptureLevel,
    /// Running count of every frame by variant — the accurate (not
    /// tail-truncated) histogram for a failure capture.
    frame_histogram: BTreeMap<String, u32>,
    /// Most recent kernel timestamp seen per hart id, from frames that
    /// carry both — pins which hart went quiet and how far it got.
    last_t_per_hart: BTreeMap<u32, u64>,
    /// The runtime workload this view's boot selected, copied from `Boot`.
    /// Recorded into a failure capture so a flake says which variant ran.
    workload: Option<String>,
    /// Structured failure capture, set by a failing `wait_for` /
    /// `assert_absent` and drained by the executor (`take_capture`) into
    /// the scenario's `ScenarioReport`. Replaces the old thread-local.
    captured: Option<FailureCapture>,
}

impl Boot {
    /// Spawn QEMU on the up-front `itest-workloads` kernel build. `workload`
    /// is the `workload=<name>` bootarg (`None` = the default demo, the
    /// continuous proof of the additive guarantee); `kmain` reads
    /// `/chosen/bootargs` and dispatches. No rebuild — the whole suite
    /// shares one binary. `label` names the socket/log files (the scenario
    /// or group name). See `docs/runtime-workload-selection-design.md`.
    pub fn spawn(label: &str, workload: Option<&str>) -> Result<Self, String> {
        // No build here: the kernel is built once up-front in
        // `itest::run` (so `--repeat N` doesn't race with mid-run source
        // edits or burn time on per-iteration build checks). Scenarios
        // differ only by the `workload=` bootarg, not the binary.
        let socket_path = socket_path_for(label);
        let _ = std::fs::remove_file(&socket_path);

        // QEMU is the listener (server=on,wait=on); we connect to it
        // as a client. Matches the `cargo xtask boot` setup the collector
        // already uses — so we exercise the same wire path.
        let chardev = format!(
            "socket,path={},server=on,wait=on,id=telemetry",
            socket_path.display(),
        );

        // Redirect QEMU's stdout (kernel UART via `-nographic`) and
        // stderr (QEMU's own warnings) to a per-scenario log file.
        // The runner dumps this file when a scenario fails — clean
        // runs stay quiet, failures get the kernel's last words.
        let log_path = log_path_for(label);
        let _ = std::fs::remove_file(&log_path);
        let stdout_log = std::fs::File::create(&log_path)
            .map_err(|e| format!("open log {}: {e}", log_path.display()))?;
        let stderr_log = stdout_log
            .try_clone()
            .map_err(|e| format!("clone log handle: {e}"))?;

        let mut qemu_cmd = qemu::base_command(&chardev);
        if let Some(workload) = workload {
            // Lands in /chosen/bootargs; `kmain` reads it to pick the
            // runtime workload.
            qemu_cmd.args(["-append", &format!("workload={workload}")]);
        }
        let qemu = qemu_cmd
            .stdout(Stdio::from(stdout_log))
            .stderr(Stdio::from(stderr_log))
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn qemu: {e}"))?;

        // Wait for QEMU to create the socket, then connect.
        let stream = connect_with_deadline(&socket_path, Duration::from_secs(10))?;

        let recorder = Arc::new(Recorder::new());
        let reader_recorder = Arc::clone(&recorder);
        thread::spawn(move || {
            let mut stream = stream;
            let _ = decode_stream(&mut stream, |frame| {
                reader_recorder.push(OwnedFrame::from_borrowed(frame));
            });
            // Stream ended (socket EOF / QEMU exit): mark closed so a waiter
            // sitting at the end sees the disconnect instead of waiting out
            // its full budget.
            reader_recorder.close();
        });

        Ok(Self {
            qemu,
            socket_path,
            recorder,
            log_path,
            workload: workload.map(str::to_string),
        })
    }

    /// A fresh cursor over this boot's recorded stream. Each `View` starts
    /// at frame 0 with its own assertion state, so several Views replay the
    /// same boot independently (the shared-boot case).
    pub fn view(&self) -> View {
        View {
            recorder: Arc::clone(&self.recorder),
            cursor: 0,
            strings: HashMap::new(),
            timebase_hz: None,
            recent: VecDeque::new(),
            max_wait: (Duration::ZERO, Duration::ZERO),
            frames_seen: 0,
            last_frame_at: None,
            capture_level: capture_level(),
            frame_histogram: BTreeMap::new(),
            last_t_per_hart: BTreeMap::new(),
            workload: self.workload.clone(),
            captured: None,
        }
    }

    /// Path to this boot's QEMU log (kernel UART + QEMU stderr). The runner
    /// dumps its tail and copies it into the run-dir on failure.
    pub fn log_path(&self) -> PathBuf {
        self.log_path.clone()
    }
}

impl View {
    /// Block up to `budget` for a frame matching `pred`. Returns the
    /// matching frame, or `None` on deadline. Every frame consumed
    /// along the way updates the internal string table — later
    /// matchers can resolve any `StringId` seen so far.
    ///
    /// Records the actual wait elapsed against the budget; the runner
    /// reads the max-so-far via `max_wait()` to surface tight budgets.
    pub fn wait_for(
        &mut self,
        budget: Duration,
        pred: impl Fn(&OwnedFrame, &StringTable) -> bool,
    ) -> Option<OwnedFrame> {
        let start = Instant::now();
        let deadline = start + budget;
        let result = loop {
            match self.advance(deadline) {
                Advance::Frame(frame) => {
                    self.absorb(&frame);
                    if pred(&frame, &self.strings) {
                        break Some(frame);
                    }
                }
                Advance::Timeout => {
                    self.dump_recent("timeout");
                    self.record_failure_capture(WaitOutcome::Timeout);
                    break None;
                }
                Advance::Disconnected => {
                    self.dump_recent("QEMU disconnected");
                    self.record_failure_capture(WaitOutcome::Disconnected);
                    break None;
                }
            }
        };
        let elapsed = start.elapsed();
        if elapsed > self.max_wait.0 {
            self.max_wait = (elapsed, budget);
        }
        result
    }

    /// Step this handle's cursor one frame forward. The record-and-replay
    /// replacement for `Receiver::recv_timeout`: frames already behind the
    /// cursor stay in the buffer (a future `View` can rescan them), and
    /// only this handle's `cursor` advances. Delegates to `Recorder` so the
    /// blocking/timeout logic is host-testable without a live QEMU.
    fn advance(&mut self, deadline: Instant) -> Advance {
        let recorder = Arc::clone(&self.recorder);
        recorder.advance(&mut self.cursor, deadline)
    }

    /// Negative oracle: assert a "bad" frame never appears within `window`.
    ///
    /// For inverted-assertion scenarios (a TLB shootdown leaving no stale
    /// read, an invariant never violated) the *success* path is the window
    /// elapsing with no match. Plain `wait_for` expresses this as
    /// `if wait_for(..).is_some() { Err } else { Ok }`, but its timeout branch
    /// then dumps the alarming `[timeout: last N frames]` block and records a
    /// failure capture — on what is actually a pass. `assert_absent` inverts
    /// that: a clean-elapsed window logs `negative-oracle window elapsed clean`
    /// and returns `Ok(())`; a matching (bad) frame dumps the offending tail
    /// and returns `Err(on_present)`. `what` is a short label for the logs.
    ///
    /// A mid-window disconnect is a failure, not a clean pass: QEMU dying means
    /// we cannot conclude the bad event was absent.
    pub fn assert_absent(
        &mut self,
        window: Duration,
        what: &str,
        on_present: impl Into<String>,
        pred: impl Fn(&OwnedFrame, &StringTable) -> bool,
    ) -> Result<(), String> {
        let start = Instant::now();
        let deadline = start + window;
        let outcome = loop {
            match self.advance(deadline) {
                Advance::Frame(frame) => {
                    self.absorb(&frame);
                    if pred(&frame, &self.strings) {
                        self.dump_recent("negative oracle tripped");
                        break Err(on_present.into());
                    }
                }
                Advance::Timeout => break Ok(()),
                Advance::Disconnected => {
                    self.dump_recent("QEMU disconnected");
                    self.record_failure_capture(WaitOutcome::Disconnected);
                    break Err(format!(
                        "QEMU disconnected while asserting absence of {what} — \
                         cannot conclude the bad event never happened"
                    ));
                }
            }
        };
        let elapsed = start.elapsed();
        if elapsed > self.max_wait.0 {
            self.max_wait = (elapsed, window);
        }
        if outcome.is_ok() {
            eprintln!(
                "  [negative-oracle window elapsed clean: {:.1}s, no {what}]",
                elapsed.as_secs_f64()
            );
        }
        outcome
    }

    /// (`actual`, `budget`) of the longest `wait_for` issued so far. The
    /// executor packages this into the scenario's report so the runner can
    /// print `max wait 1.6s of 30s budget` — flagging over-sized (much
    /// bigger than actual) or tight (actual close to budget) budgets.
    pub fn max_wait(&self) -> (Duration, Duration) {
        self.max_wait
    }

    /// Drain the structured failure capture recorded by a failing
    /// `wait_for` / `assert_absent` (or `None` on a clean run / a
    /// value-mismatch with no wait). The executor folds this into the
    /// scenario's `ScenarioReport`.
    pub fn take_capture(&mut self) -> Option<FailureCapture> {
        self.captured.take()
    }

    /// Look up a name in the string table. Useful for matchers that
    /// want to assert "this `SpanStart`'s `name_id` resolves to ...".
    pub fn name_of(&self, id: StringId) -> Option<&str> {
        self.strings.get(&id).map(String::as_str)
    }

    /// Timebase frequency from the most recent `Hello` frame, or `None`
    /// if `Hello` has not arrived yet. Use this to convert kernel tick
    /// deltas to wall-clock durations inside scenarios.
    pub fn timebase_hz(&self) -> Option<u64> {
        self.timebase_hz
    }

    fn absorb(&mut self, frame: &OwnedFrame) {
        self.frames_seen = self.frames_seen.saturating_add(1);
        self.last_frame_at = Some(Instant::now());
        *self
            .frame_histogram
            .entry(variant_name(frame).to_string())
            .or_insert(0) += 1;
        match frame {
            OwnedFrame::StringRegister { id, value } => {
                self.strings.insert(*id, value.clone());
            }
            OwnedFrame::Hello { timebase_hz, .. } => {
                self.timebase_hz = Some(*timebase_hz);
            }
            // Frames carrying both a hart id and a timestamp pin per-hart
            // progress — used to spot which hart fell silent.
            OwnedFrame::SpanStart { hart_id, t, .. }
            | OwnedFrame::ContextSwitch { hart_id, t, .. } => {
                self.last_t_per_hart.insert(u32::from(*hart_id), *t);
            }
            _ => {}
        }
        // `Tail`/`Summary` keep a bounded ring; `Full` retains everything.
        if !matches!(self.capture_level, CaptureLevel::Full)
            && self.recent.len() >= TRANSCRIPT_TAIL_FRAMES
        {
            self.recent.pop_front();
        }
        self.recent.push_back(frame.clone());
    }

    /// Snapshot the current scenario state into `self.captured`, which the
    /// executor drains (`take_capture`) into the scenario's report. Records
    /// the load-bearing summary (outcome, frames seen, wall-silence before
    /// the deadline) plus a frame-tail transcript and histogram for
    /// debugging. `error_origin` is `Scenario` — a failing `wait_for` is
    /// a scenario assertion, not infra.
    fn record_failure_capture(&mut self, outcome: WaitOutcome) {
        let last_frame_wall_age_ms = self
            .last_frame_at
            .map(|t| u32::try_from(t.elapsed().as_millis()).unwrap_or(u32::MAX));

        // The histogram and per-hart timestamps are accurate running
        // summaries (always captured). The transcript is the heavy part,
        // gated by the capture level: none under `Summary`, the bounded
        // ring under `Tail`, the full retained stream under `Full`.
        let transcript = match self.capture_level {
            CaptureLevel::Summary => Vec::new(),
            CaptureLevel::Tail | CaptureLevel::Full => {
                self.recent.iter().map(|f| self.describe(f)).collect()
            }
        };

        self.captured = Some(FailureCapture {
            outcome: Some(outcome),
            error_origin: Some(ErrorOrigin::Scenario),
            error: None,
            workload: self.workload.clone(),
            frames_seen: self.frames_seen,
            last_frame_wall_age_ms,
            last_t_per_hart: self.last_t_per_hart.clone(),
            frame_histogram: self.frame_histogram.clone(),
            transcript,
        });
    }

    fn dump_recent(&self, reason: &str) {
        // The ring may hold up to `TRANSCRIPT_TAIL_FRAMES` (or everything,
        // under `Full`); the inline dump only needs the last handful. The
        // persisted `.capture.json` sidecar carries the rest.
        const DUMP_TAIL: usize = 8;
        if self.recent.is_empty() {
            eprintln!("  [{reason}: no frames arrived]");
            return;
        }
        let skip = self.recent.len().saturating_sub(DUMP_TAIL);
        let shown = self.recent.len() - skip;
        eprintln!("  [{reason}: last {shown} of {} frame(s) seen]", self.recent.len());
        for frame in self.recent.iter().skip(skip) {
            eprintln!("    {}", self.describe(frame));
        }
    }

    fn describe(&self, frame: &OwnedFrame) -> String {
        match frame {
            OwnedFrame::Hello { timebase_hz, protocol_version } =>
                format!("Hello {{ timebase_hz={timebase_hz}, protocol_version={protocol_version} }}"),
            OwnedFrame::StringRegister { id, value } =>
                format!("StringRegister {{ {id:?} = {value:?} }}"),
            OwnedFrame::MetricRegister { name_id, kind } => {
                let name = self.strings.get(name_id).map_or("?", String::as_str);
                format!("MetricRegister {{ {name:?} kind={kind:?} }}")
            }
            OwnedFrame::SpanStart { id, parent, name_id, t, task_id, hart_id } => {
                let name = self.strings.get(name_id).map_or("?", String::as_str);
                format!("SpanStart {{ {name:?} id={id:?} parent={parent:?} t={t} task={task_id} hart={hart_id} }}")
            }
            OwnedFrame::ThreadRegister { id, name } =>
                format!("ThreadRegister {{ id={id} name={name:?} }}"),
            OwnedFrame::ContextSwitch { from, to, t, reason, hart_id } =>
                format!("ContextSwitch {{ from={from} to={to} reason={reason:?} t={t} hart={hart_id} }}"),
            OwnedFrame::SpanEnd { id, t } =>
                format!("SpanEnd {{ id={id:?} t={t} }}"),
            OwnedFrame::Event { span_id, name_id, t } => {
                let name = self.strings.get(name_id).map_or("?", String::as_str);
                format!("Event {{ {name:?} span={span_id:?} t={t} }}")
            }
            OwnedFrame::Metric { name_id, value, t, hart_id } => {
                let name = self.strings.get(name_id).map_or("?", String::as_str);
                format!("Metric {{ {name:?} value={value} t={t} hart={hart_id} }}")
            }
            OwnedFrame::Dropped { count } =>
                format!("Dropped {{ count={count} }}"),
            OwnedFrame::HartRegister { id, mhartid, role } =>
                format!("HartRegister {{ id={id} mhartid={mhartid} role={role:?} }}"),
            OwnedFrame::CapEvent { kind, cap_id, parent_cap_id, holder, object, rights, t, hart_id } =>
                format!("CapEvent {{ {kind:?} cap_id={cap_id} parent={parent_cap_id} holder={holder} object={object:?} rights={rights:#b} t={t} hart={hart_id} }}"),
            OwnedFrame::SyscallRefused { syscall, reason, task_id, t, hart_id } =>
                format!("SyscallRefused {{ syscall={syscall} reason={reason:?} task={task_id} t={t} hart={hart_id} }}"),
        }
    }
}

impl Drop for Boot {
    fn drop(&mut self) {
        const REAPING_TIMEOUT: Duration = Duration::from_secs(5);

        // Signal the child. `Child::kill` on Unix is SIGKILL, which
        // can't be caught — should produce a corpse promptly.
        let _ = self.qemu.kill();

        // Poll for the corpse with a deadline. `wait()` alone would
        // block indefinitely if QEMU somehow refused to die (e.g.,
        // PID stuck in `D` state in a host kernel call). That's
        // exotic but if it happens we want to know loudly rather
        // than hang the test runner — and the next scenario would
        // run alongside a live competing QEMU, which is exactly the
        // kind of host-CPU contention that causes spurious flakes.
        let deadline = Instant::now() + REAPING_TIMEOUT;
        let mut reaped = false;
        while Instant::now() < deadline {
            match self.qemu.try_wait() {
                Ok(Some(_)) => {
                    reaped = true;
                    break;
                }
                Ok(None) => {
                    // Still running — brief sleep before next poll.
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    // try_wait failed (child already reaped by some
                    // other code path). Treat as reaped.
                    eprintln!("Boot::Drop: try_wait error {e:?}; treating as reaped");
                    reaped = true;
                    break;
                }
            }
        }

        assert!(reaped,
            "Boot::Drop: QEMU PID {} did not exit within {:?} \
             after SIGKILL — refusing to leak it into the next \
             scenario.",
            self.qemu.id(),
            REAPING_TIMEOUT
        );

        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Ring capacity for the `Tail` / `Summary` capture levels. The full
/// transcript (and the persisted sidecar) is what holds more; this is
/// just the bounded in-memory tail the harness keeps cheaply.
const TRANSCRIPT_TAIL_FRAMES: usize = 64;

/// Process-wide capture level, set once from the CLI before scenarios
/// run and read by every `Harness::spawn`. Stored as the discriminant
/// `u8` so it lives in an `AtomicU8` with no lock. `0` = the `Tail`
/// default until `set_capture_level` is called.
static CAPTURE_LEVEL: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn level_to_u8(level: CaptureLevel) -> u8 {
    match level {
        CaptureLevel::Tail => 0,
        CaptureLevel::Summary => 1,
        CaptureLevel::Full => 2,
    }
}

/// Set the process-wide capture level. Call once at startup, before any
/// `Harness::spawn`.
pub fn set_capture_level(level: CaptureLevel) {
    CAPTURE_LEVEL.store(level_to_u8(level), std::sync::atomic::Ordering::Relaxed);
}

fn capture_level() -> CaptureLevel {
    match CAPTURE_LEVEL.load(std::sync::atomic::Ordering::Relaxed) {
        1 => CaptureLevel::Summary,
        2 => CaptureLevel::Full,
        _ => CaptureLevel::Tail,
    }
}

/// Short, stable variant name for a frame — the histogram key and a
/// compact label. Mirrors the `OwnedFrame` variants.
fn variant_name(frame: &OwnedFrame) -> &'static str {
    match frame {
        OwnedFrame::Hello { .. } => "Hello",
        OwnedFrame::StringRegister { .. } => "StringRegister",
        OwnedFrame::MetricRegister { .. } => "MetricRegister",
        OwnedFrame::SpanStart { .. } => "SpanStart",
        OwnedFrame::SpanEnd { .. } => "SpanEnd",
        OwnedFrame::Event { .. } => "Event",
        OwnedFrame::Metric { .. } => "Metric",
        OwnedFrame::Dropped { .. } => "Dropped",
        OwnedFrame::ThreadRegister { .. } => "ThreadRegister",
        OwnedFrame::ContextSwitch { .. } => "ContextSwitch",
        OwnedFrame::HartRegister { .. } => "HartRegister",
        OwnedFrame::CapEvent { .. } => "CapEvent",
        OwnedFrame::SyscallRefused { .. } => "SyscallRefused",
    }
}

/// Per-`Boot`-spawn unique counter. Two parallel `Boot::spawn`
/// calls (same scenario, different iteration or worker) would
/// otherwise collide on `(label, pid)`. Each helper increment yields
/// a fresh suffix.
static SPAWN_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn next_spawn_id() -> u64 {
    SPAWN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn socket_path_for(label: &str) -> PathBuf {
    PathBuf::from(format!(
        "/tmp/snitch-itest-{}-{}-{}.sock",
        label,
        std::process::id(),
        next_spawn_id()
    ))
}

fn log_path_for(label: &str) -> PathBuf {
    PathBuf::from(format!(
        "/tmp/snitch-itest-{}-{}-{}.log",
        label,
        std::process::id(),
        next_spawn_id()
    ))
}

fn connect_with_deadline(
    path: &std::path::Path,
    budget: Duration,
) -> Result<UnixStream, String> {
    let deadline = Instant::now() + budget;
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("connect {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod recorder_tests {
    //! The cursor/condvar logic of `Recorder::advance`, exercised without a
    //! live QEMU. Validates the record-and-replay semantics the prefactor
    //! introduced: retained (not drained) frames, per-cursor positions,
    //! and the timeout/disconnect edges that mirror the old mpsc channel.
    use super::{Advance, OwnedFrame, Recorder};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// `OwnedFrame::Dropped` is the simplest variant — use its `count` as a
    /// per-frame tag so tests can assert ordering without `PartialEq`.
    fn tagged(count: u32) -> OwnedFrame {
        OwnedFrame::Dropped { count }
    }

    fn count_of(adv: Advance) -> u32 {
        match adv {
            Advance::Frame(OwnedFrame::Dropped { count }) => count,
            Advance::Frame(other) => panic!("unexpected frame variant: {other:?}"),
            Advance::Timeout => panic!("expected a frame, got Timeout"),
            Advance::Disconnected => panic!("expected a frame, got Disconnected"),
        }
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn advance_yields_buffered_frames_in_order() {
        let rec = Recorder::new();
        rec.push(tagged(10));
        rec.push(tagged(20));
        let mut cursor = 0;
        assert_eq!(count_of(rec.advance(&mut cursor, soon())), 10);
        assert_eq!(count_of(rec.advance(&mut cursor, soon())), 20);
        assert_eq!(cursor, 2);
    }

    #[test]
    fn advance_times_out_when_no_frame_and_stream_open() {
        let rec = Recorder::new();
        let mut cursor = 0;
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(matches!(rec.advance(&mut cursor, deadline), Advance::Timeout));
        assert_eq!(cursor, 0, "a timeout must not advance the cursor");
    }

    #[test]
    fn advance_reports_disconnect_only_after_draining_buffered_frames() {
        let rec = Recorder::new();
        rec.push(tagged(1));
        rec.close();
        let mut cursor = 0;
        // The buffered frame comes out first, even though the stream is closed.
        assert_eq!(count_of(rec.advance(&mut cursor, soon())), 1);
        // Only now, caught up at the end of a closed stream, is it a disconnect.
        assert!(matches!(rec.advance(&mut cursor, soon()), Advance::Disconnected));
    }

    #[test]
    fn advance_blocks_then_wakes_on_a_late_push() {
        let rec = Arc::new(Recorder::new());
        let writer = Arc::clone(&rec);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            writer.push(tagged(99));
        });
        let mut cursor = 0;
        // Deadline is well past the writer's 50ms sleep, so this must block
        // and then wake with the pushed frame — not time out.
        assert_eq!(count_of(rec.advance(&mut cursor, soon())), 99);
        handle.join().unwrap();
    }

    #[test]
    fn independent_cursors_each_replay_from_zero() {
        // The whole point of recording over consuming: two cursors over one
        // buffer each see every frame from the start. This is the multi-View
        // foundation, proven on the prefactor's own machinery.
        let rec = Recorder::new();
        rec.push(tagged(1));
        rec.push(tagged(2));
        let mut a = 0;
        let mut b = 0;
        assert_eq!(count_of(rec.advance(&mut a, soon())), 1);
        assert_eq!(count_of(rec.advance(&mut a, soon())), 2);
        // Cursor `b` is untouched by `a`'s scan — it still sees frame 1 first.
        assert_eq!(count_of(rec.advance(&mut b, soon())), 1);
        assert_eq!(count_of(rec.advance(&mut b, soon())), 2);
    }
}
