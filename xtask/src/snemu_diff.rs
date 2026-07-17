//! The differential oracle: boot the *same* kernel under snemu and QEMU and
//! compare their telemetry frame streams. snemu is deterministic and QEMU is
//! not, so the comparison is **structural** — volatile fields (timestamps, and
//! metric values, which drift with wall-clock) are normalized away, and we diff
//! the boot-prefix frame sequence plus the registered-name vocabulary.
//!
//! Both emulators boot the *same* workload: with no `--workload`, the kernel's
//! default (`init`); otherwise snemu patches `workload=<name>` into the DTB
//! bootargs it feeds the guest (`snemu::dtb::set_bootargs`, the firmware role)
//! while QEMU gets it via `-append`. `run_all` sweeps the whole [`WORKLOADS`]
//! list this way, so every scenario is compared apples-to-apples.

use std::io::Cursor;
use std::os::unix::net::UnixStream;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use protocol::stream::{OwnedFrame, decode_stream};

use crate::qemu;

/// QEMU `virt` RAM the snemu machine mirrors.
const RAM_SIZE: usize = 128 * 1024 * 1024;
/// Two harts, matching the kernel's `MAX_HARTS` and the QEMU `-smp 2`.
const HART_COUNT: usize = 2;
/// The device tree snemu feeds the guest (dumped at `-smp 2`).
const SNEMU_DTB: &str = "snemu/virt.dtb";

/// Normalize a frame for structural comparison: zero the timestamp everywhere it
/// appears, and zero the (wall-clock-driven) metric value. Everything else — ids,
/// names, kinds, hart/task ids, span topology — is kept and compared.
pub(crate) fn canonical(frame: &OwnedFrame) -> OwnedFrame {
    use OwnedFrame::{
        CapEvent, ContextSwitch, Event, HartRegister, Log, Message, Metric, NotifySignal,
        NotifyWait, SpanEnd, SpanStart, SyscallRefused,
    };
    match frame {
        SpanStart { id, parent, name_id, task_id, hart_id, .. } => SpanStart {
            id: *id,
            parent: *parent,
            name_id: *name_id,
            t: 0,
            task_id: *task_id,
            hart_id: *hart_id,
        },
        SpanEnd { id, .. } => SpanEnd { id: *id, t: 0 },
        Event { span_id, name_id, .. } => Event { span_id: *span_id, name_id: *name_id, t: 0 },
        Metric { name_id, hart_id, .. } => Metric { name_id: *name_id, value: 0, t: 0, hart_id: *hart_id },
        ContextSwitch { from, to, reason, hart_id, .. } => ContextSwitch {
            from: *from,
            to: *to,
            t: 0,
            reason: *reason,
            hart_id: *hart_id,
        },
        CapEvent { kind, cap_id, parent_cap_id, holder, object, rights, badge, hart_id, name, .. } => {
            CapEvent {
                kind: *kind,
                cap_id: *cap_id,
                parent_cap_id: *parent_cap_id,
                holder: *holder,
                object: *object,
                rights: *rights,
                badge: *badge,
                t: 0,
                hart_id: *hart_id,
                name: *name,
            }
        }
        SyscallRefused { syscall, reason, task_id, hart_id, .. } => SyscallRefused {
            syscall: *syscall,
            reason: *reason,
            task_id: *task_id,
            t: 0,
            hart_id: *hart_id,
        },
        Log { msg, task_id, hart_id, .. } => Log {
            msg: msg.clone(),
            task_id: *task_id,
            t: 0,
            hart_id: *hart_id,
        },
        Message { endpoint, from, to, parent_span, hart_id, .. } => Message {
            endpoint: *endpoint,
            from: *from,
            to: *to,
            parent_span: *parent_span,
            t: 0,
            hart_id: *hart_id,
        },
        NotifySignal { notification, mask, from_task, hart_id, .. } => NotifySignal {
            notification: *notification,
            mask: *mask,
            from_task: *from_task,
            t: 0,
            hart_id: *hart_id,
        },
        NotifyWait { notification, bits, to_task, hart_id, .. } => NotifyWait {
            notification: *notification,
            bits: *bits,
            to_task: *to_task,
            t: 0,
            hart_id: *hart_id,
        },
        // `mhartid` is the raw platform boot-hart id (snemu boots on 0, QEMU on 1) —
        // firmware noise, not kernel behavior, so it must not halt a structural diff.
        // Normalize it; keep `id` (the logical hart) and `role`, which are semantic.
        HartRegister { id, role, .. } => HartRegister { id: *id, mhartid: 0, role: *role },
        // No timestamp / volatile field: Hello, StringRegister, MetricRegister,
        // ThreadRegister, Dropped.
        other => other.clone(),
    }
}

/// The outcome of comparing two frame streams.
pub(crate) struct Diff {
    /// Number of leading frames that matched structurally.
    pub common_prefix: usize,
    /// The first structurally-differing pair, if the streams diverge within the
    /// shorter one's length. `(snemu, qemu)`.
    pub divergence: Option<(OwnedFrame, OwnedFrame)>,
}

/// Compare two frame streams by their canonical (timestamp-normalized) sequence,
/// returning how far they agree and the first divergence.
pub(crate) fn diff_streams(snemu: &[OwnedFrame], qemu: &[OwnedFrame]) -> Diff {
    let mut common_prefix = 0;
    let mut divergence = None;
    for (s, q) in snemu.iter().zip(qemu.iter()) {
        if canonical(s) == canonical(q) {
            common_prefix += 1;
        } else {
            divergence = Some((canonical(s), canonical(q)));
            break;
        }
    }
    Diff { common_prefix, divergence }
}

/// The set of registered string names in a stream (the kernel's telemetry
/// vocabulary — order-independent and timestamp-free, so a robust agreement
/// signal even where frame *ordering* diverges).
pub(crate) fn string_vocabulary(frames: &[OwnedFrame]) -> std::collections::BTreeSet<String> {
    frames
        .iter()
        .filter_map(|f| match f {
            OwnedFrame::StringRegister { value, .. } => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// Decode a virtio-console byte stream into telemetry frames. A trailing partial
/// frame (a run cut mid-send) decodes cleanly as EOF; only malformed bytes error.
fn decode_frames(bytes: &[u8]) -> Vec<OwnedFrame> {
    let mut frames = Vec::new();
    let mut cursor = Cursor::new(bytes);
    let _ = decode_stream(&mut cursor, |f| frames.push(OwnedFrame::from_borrowed(f)));
    frames
}

/// Frame count that marks "boot telemetry produced" — a milestone both emulators
/// reach, so their time-to-here is a fair apples-to-apples "finish" (unlike a
/// fixed wall-clock window). Covers boot through the first heartbeats.
const MILESTONE_FRAMES: usize = 100;

/// Wall-clock timing for one emulator run.
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    /// Wall time from run start to the first `SpanStart` frame.
    pub(crate) first_span: Option<Duration>,
    /// Wall time to emit [`MILESTONE_FRAMES`] frames — the comparable finish line.
    pub(crate) milestone: Option<Duration>,
    /// Wall time for the whole run (snemu: the step loop; qemu: the window).
    pub(crate) total: Duration,
}

/// Wall-clock timing to the shared milestones for `workload` under **snemu**
/// (patches the DTB bootarg, steps to `max_steps`). The snemu half of the step-4
/// baseline overlay.
pub(crate) fn timing_snemu(
    kernel: &[u8],
    dtb_base: &[u8],
    workload: Option<&str>,
    max_steps: u64,
) -> Result<Timing, String> {
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    let (_frames, _stop, timing) = collect_snemu(kernel, &dtb, max_steps)?;
    Ok(timing)
}

/// Wall-clock timing to the shared milestones for `workload` under **QEMU**
/// (collected over `window`). The baseline snemu is measured against; note the
/// determinism asymmetry — snemu is seeded once, QEMU is not.
pub(crate) fn timing_qemu(
    workload: Option<&str>,
    window: Duration,
    opt: qemu::OptLevel,
) -> Result<Timing, String> {
    let (_frames, timing) = collect_qemu(window, workload, opt)?;
    Ok(timing)
}

fn has_span(frames: &[OwnedFrame]) -> bool {
    frames.iter().any(|f| matches!(f, OwnedFrame::SpanStart { .. }))
}

/// Incremental decode of a growing telemetry buffer: tracks how many complete
/// frames have appeared and whether a `SpanStart` has, decoding **only the newly
/// arrived suffix** each call. Replaces the old whole-buffer re-decode on every
/// growth (≈O(n²) — it re-parsed all prior frames each time a byte landed), which
/// dominated and profile-contaminated the boot-milestone timing. Resumes at the
/// last complete frame boundary, so total decode work is O(bytes).
#[derive(Default)]
struct FrameProgress {
    /// Bytes already consumed into complete frames (the resume offset).
    consumed: usize,
    /// Complete frames seen so far.
    count: usize,
    /// Whether any `SpanStart` has appeared.
    saw_span: bool,
}

impl FrameProgress {
    /// Fold in whatever bytes have arrived since the last call. `tx` is the whole
    /// buffer so far; only `tx[consumed..]` is decoded, one frame at a time, until
    /// the suffix ends mid-frame. `consumed` stops at the last *complete* frame's
    /// boundary, so the trailing partial frame is re-read (just that one) once it
    /// completes — never the whole buffer.
    fn advance(&mut self, tx: &[u8]) {
        while let Ok((frame, n)) = protocol::stream::try_decode_frame(&tx[self.consumed..]) {
            self.count += 1;
            if matches!(frame, protocol::Frame::SpanStart { .. }) {
                self.saw_span = true;
            }
            self.consumed += n;
        }
    }
}

/// Boot the kernel under snemu in-process for up to `max_steps` and return its
/// telemetry frames, how it stopped (step limit vs a fault — a meta-loop signal
/// we keep as data), and wall-clock timing.
fn collect_snemu(
    kernel: &[u8],
    dtb: &[u8],
    max_steps: u64,
) -> Result<(Vec<OwnedFrame>, String, Timing), String> {
    let start = Instant::now();
    let mut machine = snemu::loader::load_machine(kernel, RAM_SIZE, Some(dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
    let mut steps = 0u64;
    let mut stop = format!("step limit ({max_steps})");
    let mut first_span = None;
    let mut milestone = None;
    let mut seen_tx = 0usize;
    let mut progress = FrameProgress::default();
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            Err(e) => {
                stop = format!("{e:?} @ {steps} steps");
                break;
            }
        }
        // Watch the TX buffer for the timing marks (cheap length check per step;
        // decode only the newly-arrived suffix when it grows, and only until both
        // marks are found — O(bytes) total, not a whole-buffer re-decode).
        if first_span.is_none() || milestone.is_none() {
            let tx = machine.virtio_tx_output();
            if tx.len() != seen_tx {
                seen_tx = tx.len();
                progress.advance(tx);
                if first_span.is_none() && progress.saw_span {
                    first_span = Some(start.elapsed());
                }
                if milestone.is_none() && progress.count >= MILESTONE_FRAMES {
                    milestone = Some(start.elapsed());
                }
            }
        }
    }
    let timing = Timing { first_span, milestone, total: start.elapsed() };
    Ok((decode_frames(machine.virtio_tx_output()), stop, timing))
}

/// Boot the kernel under QEMU (with `workload`), collect the telemetry frames
/// for `window`, then kill it. Timing is measured from spawn: `first_span` is
/// when the first `SpanStart` arrives on the wire (a real streaming latency,
/// including QEMU's firmware + DTB-gen startup); `total` is the whole window.
fn collect_qemu(
    window: Duration,
    workload: Option<&str>,
    opt: qemu::OptLevel,
) -> Result<(Vec<OwnedFrame>, Timing), String> {
    let start = Instant::now();
    let socket = std::env::temp_dir().join(format!(
        "snitch-diff-{}-{}.sock",
        std::process::id(),
        workload.unwrap_or("default")
    ));
    let _ = std::fs::remove_file(&socket);
    let chardev = format!(
        "socket,path={},server=on,wait=on,id=telemetry",
        socket.display()
    );
    // QEMU boots the ELF for this opt regime (matching the snemu side), so
    // `--opt mid` diffs release-vs-release.
    let mut cmd = qemu::base_command(&chardev, qemu::DEFAULT_RAM_MB, opt);
    if let Some(w) = workload {
        cmd.args(["-append", &format!("workload={w}")]);
    }
    let mut qemu = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn qemu: {e}"))?;

    let stream = connect_with_deadline(&socket, Duration::from_secs(10));
    let frames = Arc::new(Mutex::new(Vec::new()));
    let first_span = Arc::new(Mutex::new(None));
    let milestone = Arc::new(Mutex::new(None));
    let reader = match stream {
        Ok(stream) => {
            let sink = Arc::clone(&frames);
            let span_at = Arc::clone(&first_span);
            let milestone_at = Arc::clone(&milestone);
            Some(thread::spawn(move || {
                let mut stream = stream;
                let _ = decode_stream(&mut stream, |f| {
                    if matches!(f, protocol::Frame::SpanStart { .. }) {
                        let mut slot = span_at.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(start.elapsed());
                        }
                    }
                    let mut v = sink.lock().unwrap();
                    v.push(OwnedFrame::from_borrowed(f));
                    if v.len() == MILESTONE_FRAMES {
                        *milestone_at.lock().unwrap() = Some(start.elapsed());
                    }
                });
            }))
        }
        Err(e) => {
            let _ = qemu.kill();
            return Err(e);
        }
    };

    thread::sleep(window);
    let _ = qemu.kill();
    let _ = qemu.wait();
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    let _ = std::fs::remove_file(&socket);
    let timing = Timing {
        first_span: *first_span.lock().unwrap(),
        milestone: *milestone.lock().unwrap(),
        total: start.elapsed(),
    };
    let frames = Arc::try_unwrap(frames)
        .map_err(|_| "reader thread still holds frames".to_string())?
        .into_inner()
        .map_err(|_| "frame mutex poisoned".to_string())?;
    Ok((frames, timing))
}

/// Connect to `path` as a client, retrying until it exists or the deadline.
fn connect_with_deadline(path: &std::path::Path, timeout: Duration) -> Result<UnixStream, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("connect {}: {e}", path.display())),
        }
    }
}

/// The runtime workloads snemu can select (mirrors `kernel_core::workloads::
/// bootargs::select`). The sweep runs the oracle over every one.
pub(crate) const WORKLOADS: &[&str] = &[
    "demo", "init", "smp", "smp-spsc", "smp-spsc-batch", "priorities", "block-wake", "workers",
    "heap-grow", "frame-oom", "heap-oom", "spawn-storm", "ipi-pong", "shootdown-storm",
    "mutex-storm", "virtio-storm", "tlb-shootdown", "ping-pong", "userspace", "userspace-fault",
    "userspace-bad-ptr", "userspace-span-flood", "user-hog", "syscall-hog", "console-echo",
    "spawn-image", "manifest-iface", "probe", "panic-now", "stack-guard", "stack-overflow-deep",
    "boot-stack-guard", "spawn-demo", "spawn-reap", "wait-any", "endpoint-create", "ipc",
    "ipc-rpc", "badge-mint", "badge-handout", "fs", "notify-smoke", "stitch-repl", "stitch-fs",
];

/// The structured outcome of comparing one workload under snemu vs QEMU.
struct Comparison {
    snemu_frames: usize,
    qemu_frames: usize,
    common_prefix: usize,
    vocab_shared: usize,
    only_snemu: Vec<String>,
    only_qemu: Vec<String>,
    /// Whether snemu's stream shows it reached the crash (a `kernel panic` Log) —
    /// gates whether `kernel.heartbeat`-only-in-snemu is a benign truncation or a
    /// failed-to-halt bug. See [`snemu_reached_crash`].
    snemu_crashed: bool,
    /// How snemu's run ended (step limit vs a fault).
    snemu_stop: String,
    divergence: Option<(OwnedFrame, OwnedFrame)>,
    snemu_timing: Timing,
    qemu_timing: Timing,
}

/// Telemetry names a *living* kernel emits on a recurring cadence (the heartbeat
/// loop), carrying no workload-specific meaning. Their appearing **only** in
/// snemu's vocabulary is never evidence of invented telemetry: it means QEMU
/// halted — a deliberate-crash workload (`panic-now`, the stack-guard family) —
/// before its slower wall-clock reached the first heartbeat, while snemu's
/// instruction-clock passed several "seconds" of the same boot and emitted a few.
/// Boot-to-crash is ~40M instructions ≈ ~4 "seconds" on snemu's `rdtime = instret`
/// clock (10 MHz timebase → 10M instr/heartbeat), but ~0.2 s of real time in QEMU
/// (first heartbeat not due until 1 s). Reset-on-panic wouldn't change it: snemu
/// heartbeats *before* the crash. See
/// `notes/snemu-guard-page-fail-is-timing-not-mmu.md`.
///
/// Forgiven **only when snemu reached the crash** ([`snemu_reached_crash`]): the
/// benign case is "QEMU halted before its first heartbeat, snemu emitted a few
/// then crashed too." If snemu emits `kernel.heartbeat` QEMU lacks but *never*
/// crashes, that's the opposite — snemu **failed to halt** and ran past where it
/// should have died — and must FAIL. Conditioning on the panic frame is what
/// tells the two apart (it closes the caveat the unconditional filter left open).
const BENIGN_ONLY_SNEMU: &[&str] = &["kernel.heartbeat"];

/// The panic handler emits `Log("kernel panic …")` on the wire (see
/// `plans/panic-emits-telemetry.md`), so its presence in snemu's stream proves
/// snemu ran all the way to the crash — just later than QEMU on the
/// instruction-clock — rather than hanging past where it should have halted.
fn snemu_reached_crash(frames: &[OwnedFrame]) -> bool {
    frames
        .iter()
        .any(|f| matches!(f, OwnedFrame::Log { msg, .. } if msg.contains("kernel panic")))
}

/// The only-snemu names that actually indicate snemu invented telemetry QEMU
/// would never emit — the only-snemu set minus the recurring-infra names a crash
/// can truncate ([`BENIGN_ONLY_SNEMU`]), and only when snemu is proven to have
/// crashed (`snemu_crashed`). Empty ⇒ faithful.
fn invented_names(only_snemu: &[String], snemu_crashed: bool) -> Vec<String> {
    only_snemu
        .iter()
        .filter(|n| !(snemu_crashed && BENIGN_ONLY_SNEMU.contains(&n.as_str())))
        .cloned()
        .collect()
}

/// The only-qemu names that indicate snemu **dropped** telemetry QEMU emits — a
/// fidelity gap, the mirror of [`invented_names`]. A vocabulary name only QEMU has is
/// behaviour QEMU reached that snemu did not, in a *deterministic* workload: either
/// snemu was **budget-truncated** (raise `--steps` and it clears) or snemu
/// **diverged** and will never emit it (raise `--steps` and it persists — e.g.
/// `supervised` under `--opt mid`, where release-snemu stalls after the crasher's 4th
/// exit and never reaches the escalate trio, even at 25× the budget a faithful run
/// needs). We can't tell the two apart from a single run — snemu's instruction budget
/// and QEMU's wall-clock window produce *incomparable* frame counts, so counting
/// frames doesn't help — so **any** non-empty only-qemu breaks faithfulness, with the
/// caller printing a "raise `--steps` to rule out truncation" hint. (Requires a
/// `--qemu-secs` long enough that QEMU itself reached the behaviour; too short a window
/// truncates *QEMU* and shows up as false only-snemu instead.) Empty ⇒ no gap.
fn dropped_names(only_qemu: &[String]) -> Vec<String> {
    only_qemu.to_vec()
}

impl Comparison {
    /// Faithful ⇔ snemu neither **invented** telemetry QEMU never emits
    /// ([`invented_names`]) **nor dropped** telemetry QEMU does ([`dropped_names`]).
    /// A name only-snemu-has is a genuine invention unless it's recurring infra a
    /// crash truncated (and snemu reached the crash). A name only-QEMU-has is a
    /// genuine drop unless snemu was legitimately budget-truncated before reaching it
    /// (snemu produced fewer frames than QEMU) — if snemu ran *at least as long* and
    /// still lacks it, it diverged. Both directions must be clean.
    fn faithful(&self) -> bool {
        invented_names(&self.only_snemu, self.snemu_crashed).is_empty()
            && dropped_names(&self.only_qemu).is_empty()
    }
}

/// Boot `kernel` (with `workload`) under both emulators and structurally compare.
fn compare(
    kernel: &[u8],
    dtb_base: &[u8],
    workload: Option<&str>,
    max_steps: u64,
    qemu_secs: u64,
    opt: qemu::OptLevel,
) -> Result<Comparison, String> {
    // Firmware role: inject the workload into the DTB snemu boots (QEMU gets it
    // via `-append`), so both emulators run the same scenario.
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    let (snemu, snemu_stop, snemu_timing) = collect_snemu(kernel, &dtb, max_steps)?;
    let (qemu, qemu_timing) = collect_qemu(Duration::from_secs(qemu_secs), workload, opt)?;

    let d = diff_streams(&snemu, &qemu);
    let sv = string_vocabulary(&snemu);
    let qv = string_vocabulary(&qemu);
    Ok(Comparison {
        snemu_frames: snemu.len(),
        qemu_frames: qemu.len(),
        common_prefix: d.common_prefix,
        vocab_shared: sv.intersection(&qv).count(),
        only_snemu: sv.difference(&qv).cloned().collect(),
        only_qemu: qv.difference(&sv).cloned().collect(),
        snemu_crashed: snemu_reached_crash(&snemu),
        snemu_stop,
        divergence: d.divergence,
        snemu_timing,
        qemu_timing,
    })
}

/// Boot the kernel under snemu (optionally selecting `workload`) and return the
/// decoded telemetry frames after `max_steps`. Used by `diagram trace`/`switches`,
/// whose folds collapse by name/task so a fixed budget captures the structure.
pub(crate) fn collect_frames(
    workload: Option<&str>,
    max_steps: u64,
) -> Result<Vec<OwnedFrame>, String> {
    let (kernel, dtb_base) = prepare(workload.is_some())?;
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(&dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base,
    };
    let (frames, _stop, _timing) = collect_snemu(&kernel, &dtb, max_steps)?;
    Ok(frames)
}

/// Boot the kernel under snemu (optionally selecting `workload`) and return the
/// decoded telemetry frames, stopping early once `CapEvent` emission goes
/// quiescent — `quiescence_window` steps elapse with no new cap event after at
/// least one is seen — or at `max_steps`. Reused by `diagram caps`: init emits
/// its authority graph during early boot then just heartbeats, so running the
/// full ceiling would waste most of the wall-clock. Also returns the step count
/// actually reached. The snemu half of the oracle, minus the QEMU side.
pub(crate) fn collect_frames_until_cap_quiescence(
    workload: Option<&str>,
    max_steps: u64,
    quiescence_window: u64,
) -> Result<(Vec<OwnedFrame>, u64), String> {
    let (kernel, dtb_base) = prepare(workload.is_some())?;
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(&dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base,
    };
    let mut machine = snemu::loader::load_machine(&kernel, RAM_SIZE, Some(&dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
    let mut quiescence = diagram::caps::CapQuiescence::new(quiescence_window);
    let mut steps = 0u64;
    let mut seen_tx = 0usize;
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            Err(_) => break,
        }
        let tx = machine.virtio_tx_output();
        if tx.len() != seen_tx {
            seen_tx = tx.len();
            let caps =
                decode_frames(tx).iter().filter(|f| matches!(f, OwnedFrame::CapEvent { .. })).count();
            if quiescence.observe(caps, steps) {
                break;
            }
        }
    }
    Ok((decode_frames(machine.virtio_tx_output()), steps))
}

/// Build the kernel and read the base DTB the emulators share.
pub(crate) fn prepare(with_workloads: bool) -> Result<(Vec<u8>, Vec<u8>), String> {
    prepare_profiled(with_workloads, qemu::OptLevel::Low)
}

/// Like [`prepare`], but builds the kernel at the given [`qemu::OptLevel`] and reads
/// the matching ELF. Used by `snemu-itest --opt=<low|mid|high>` to run the same
/// scenarios under three optimization regimes with distinct failure modes.
pub(crate) fn prepare_profiled(
    with_workloads: bool,
    opt: qemu::OptLevel,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let features: &[&str] = if with_workloads { &["itest-workloads"] } else { &[] };
    if !qemu::build_kernel_profiled(features, opt).is_ok_and(|s| s.success()) {
        return Err("kernel build failed".to_string());
    }
    let kernel =
        std::fs::read(qemu::kernel_bin(opt.is_release())).map_err(|e| format!("read kernel: {e}"))?;
    let dtb = std::fs::read(SNEMU_DTB).map_err(|e| format!("read {SNEMU_DTB}: {e}"))?;
    Ok((kernel, dtb))
}

/// Boot `kernel` under snemu (selecting `workload` via a DTB bootarg patch, or
/// the default `init` boot for `None`), step to `max_steps`, and return the
/// whole decoded telemetry stream. The snemu half of the fidelity audit: the
/// frames a scenario's assertion body then replays against (via
/// `View::replay`). Unlike [`collect_snemu`] this keeps no timing marks — the
/// audit cares only about the frame *content*.
pub(crate) fn collect_workload_frames(
    kernel: &[u8],
    dtb_base: &[u8],
    workload: Option<&str>,
    max_steps: u64,
    decode_cache: bool,
) -> Result<Vec<OwnedFrame>, String> {
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    let mut machine = snemu::loader::load_machine(kernel, RAM_SIZE, Some(&dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
    machine.set_decode_cache(decode_cache);
    let mut steps = 0u64;
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            // A guest fault (e.g. a deliberate-panic workload halting a hart) is
            // the end of this run's stream, not an audit error — return what the
            // guest emitted before it stopped.
            Err(_) => break,
        }
    }
    Ok(decode_frames(machine.virtio_tx_output()))
}

/// Load a fresh, unstepped snemu machine for `workload` (DTB bootarg patched,
/// decode cache on). The interactive audit builds a live `View` over this and
/// steps it per scenario, so each scenario gets its own machine to drive with
/// its own console input.
/// RAM (MiB) a workload's machine boots with. The default is deliberately **small**
/// (SnitchOS should work on modest RAM, and a small machine makes per-scenario
/// fork-clones cheaper) — the `snemu-itest` RAM right-sizing report confirms most
/// scenarios' peak footprint is well under it. The `frame-oom` workload is pinned
/// **large** as the suite's one large-RAM coverage point: it leaks frames until the
/// pool is exhausted, so on a big machine it genuinely fills ~all of it, exercising
/// the large frame bitmap + high-address frame handling the small default never hits.
///
/// Shared with the QEMU harness (`-m`) so **both** engines run the identical
/// machine — the test is only meaningful if snemu and QEMU agree on the RAM size.
pub(crate) fn ram_mb_for(workload: Option<&str>) -> u32 {
    // Right-sized from the `snemu-itest` RAM report: every scenario except the two
    // OOM-teeth workloads peaks at ≤12 MiB of guest footprint, so 16 MiB is ~1.3×
    // headroom — keeping fork-clones cheap without crowding any real usage.
    const DEFAULT_MB: u32 = 16;
    match workload {
        // Large-RAM coverage: frame-oom fills whatever pool it's given before OOMing.
        Some("frame-oom") => 128,
        // `spawn-reap`'s leak-vs-reclaim teeth need the children's total (15 × 4 MiB
        // = 60 MiB) to exceed RAM. On a small machine that holds with margin (60 >
        // 48) *and* halves the child count — so the per-spawn frame-zeroing `memset`
        // that dominates this scenario's instret (the suite pole) drops ~2×, with
        // teeth that clearly exceed the machine rather than leaning on kernel
        // overhead (the old 30 × 4 = 120 barely under 128). See `reaper.rs`.
        Some("spawn-reap") => 48,
        _ => DEFAULT_MB,
    }
}

pub(crate) fn load_workload_machine(
    kernel: &[u8],
    dtb_base: &[u8],
    workload: Option<&str>,
) -> Result<snemu::machine::Machine, String> {
    let ram_bytes = u64::from(ram_mb_for(workload)) * 1024 * 1024;
    let mut dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    // Match the DTB's declared RAM to the host Memory so the kernel sizes its frame
    // pool to the smaller machine (else it manages frames that don't exist).
    if ram_bytes != RAM_SIZE as u64 {
        dtb = snemu::dtb::set_memory_size(&dtb, ram_bytes).ok_or("DTB memory patch failed")?;
    }
    let mut machine = snemu::loader::load_machine(kernel, ram_bytes as usize, Some(&dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
    machine.set_decode_cache(true);
    // Mirrors the real QEMU harness's `ramfb` scenario tag → `-device ramfb`
    // (`Boot::spawn`, xtask/src/itest/harness.rs): only the `framebuffer-
    // presents` scenario's dedicated `{"ramfb"}` workload should see
    // `etc/ramfb`. Every other workload (including `None`, the default)
    // stays off, matching real QEMU's `ramfb: false` default.
    if workload == Some("ramfb") {
        machine.enable_fwcfg_ramfb();
    }
    Ok(machine)
}

/// Boot `workload` under snemu and step up to `max_steps` rounds, timing the
/// wall-clock — one measurement sample (`instret` + elapsed) for the `snemu
/// bench` harness. The step loop is the *only* work timed: no per-step frame
/// decode (that's observability-mode perturbation), so MIPS reflects the raw
/// interpreter. `instret` is deterministic for a given workload+budget, so
/// repeated calls yield identical counts — the invariant `bench::BenchReport`
/// enforces.
pub(crate) fn measure_workload(
    kernel: &[u8],
    dtb_base: &[u8],
    workload: Option<&str>,
    max_steps: u64,
    decode_cache: bool,
) -> Result<snemu::bench::Sample, String> {
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    let mut machine = snemu::loader::load_machine(kernel, RAM_SIZE, Some(&dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
    machine.set_decode_cache(decode_cache);
    let start = Instant::now();
    let mut steps = 0u64;
    let mut startup: Option<snemu::bench::StartupMark> = None;
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            Err(_) => break,
        }
        // Boot-to-first-telemetry: the first non-empty virtio TX buffer marks
        // when the guest emitted its first frame. Cheap length check, and only
        // until the mark is taken.
        if startup.is_none() && !machine.virtio_tx_output().is_empty() {
            startup = Some(snemu::bench::StartupMark {
                instret: machine.instret(),
                wall: start.elapsed(),
            });
        }
    }
    let wall = start.elapsed();
    Ok(snemu::bench::Sample { instret: machine.instret(), wall, startup })
}

/// Single-workload oracle: boot under both, diff, print the detailed report.
pub fn run(max_steps: u64, qemu_secs: u64, workload: Option<&str>, opt: qemu::OptLevel) -> ExitCode {
    let (kernel, dtb) = match prepare_profiled(workload.is_some(), opt) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu-diff: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!("snemu-diff: workload = {}", workload.unwrap_or("default (init)"));
    let cmp = match compare(&kernel, &dtb, workload, max_steps, qemu_secs, opt) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("snemu-diff: {e}");
            return ExitCode::from(1);
        }
    };
    print_detailed(&cmp);
    if cmp.faithful() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// Sweep the workloads (all, or the first `limit`) through the oracle and
/// tabulate agree/disagree.
pub fn run_all(max_steps: u64, qemu_secs: u64, limit: Option<usize>, opt: qemu::OptLevel) -> ExitCode {
    let (kernel, dtb) = match prepare_profiled(true, opt) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu-diff: {e}");
            return ExitCode::from(1);
        }
    };

    let count = limit.unwrap_or(WORKLOADS.len()).min(WORKLOADS.len());
    let mut results: Vec<(String, Result<Comparison, String>)> = Vec::new();
    for (i, &w) in WORKLOADS.iter().take(count).enumerate() {
        eprint!("snemu-diff: [{}/{count}] {w:<22} ", i + 1);
        let cmp = compare(&kernel, &dtb, Some(w), max_steps, qemu_secs, opt);
        match &cmp {
            Ok(c) => eprintln!(
                "{} (snemu {} frames | 1st-span snemu {} vs qemu {})",
                if c.faithful() { "PASS" } else { "FAIL" },
                c.snemu_frames,
                fmt_opt(c.snemu_timing.first_span),
                fmt_opt(c.qemu_timing.first_span),
            ),
            Err(e) => eprintln!("ERROR: {e}"),
        }
        results.push((w.to_string(), cmp));
    }

    print_summary(&results)
}

/// Format an optional `Duration` in seconds (or `—` if never reached).
fn fmt_opt(d: Option<Duration>) -> String {
    d.map_or_else(|| "—".to_string(), |d| format!("{:.2}s", d.as_secs_f64()))
}

/// Format a `Timing` as `first-span/milestone` seconds — the two comparable
/// marks (time to first telemetry, time to the boot-telemetry milestone).
fn fmt_timing(t: &Timing) -> String {
    format!("{}/{}", fmt_opt(t.first_span), fmt_opt(t.milestone))
}

/// Build a DTB whose `workload=<name>` bootarg is padded to a **fixed 40-char
/// field**, so every workload's DTB is byte-for-byte the same size. That makes
/// writing one over another in a booted snapshot's RAM layout-preserving — the
/// guest re-parses an identically-shaped blob. Trailing spaces are ignored by
/// the kernel's whitespace-split bootarg parser.
fn workload_dtb(base: &[u8], name: &str) -> Option<Vec<u8>> {
    const FIELD: usize = 40;
    let arg = format!("workload={name}");
    // `{:<FIELD}` pads but does NOT truncate — an over-long name would silently
    // produce a different-size DTB and break the layout-preserving invariant.
    assert!(
        arg.len() <= FIELD,
        "workload bootarg {arg:?} ({} chars) exceeds the {FIELD}-char fixed field",
        arg.len(),
    );
    snemu::dtb::set_bootargs(base, &format!("{arg:<FIELD$}"))
}

/// The snapshot/fork harness: boot the common prefix **once**, snapshot it, then
/// fork every workload by cloning the snapshot, patching its `workload=` bootarg
/// into RAM, and resuming. Proves boot amortization — one boot, N workloads —
/// the snemu-only fast path (no QEMU). All in-process; no per-workload reboot.
pub fn run_fork(max_steps: u64) -> ExitCode {
    let (kernel, base_dtb) = match prepare(true) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu-fork: {e}");
            return ExitCode::from(1);
        }
    };
    // Fixed-width bootarg → every workload's DTB is the SAME size, so patching
    // one over another in the snapshot's RAM is layout-preserving (no re-parse
    // wedge). The base workload is irrelevant (overwritten before the guest
    // reads it), but must be this same fixed size.
    let Some(boot_dtb) = workload_dtb(&base_dtb, "init") else {
        eprintln!("snemu-fork: DTB patch failed");
        return ExitCode::from(1);
    };
    let mut base = match snemu::loader::load_machine(&kernel, RAM_SIZE, Some(&boot_dtb), HART_COUNT) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("snemu-fork: load: {e:?}");
            return ExitCode::from(1);
        }
    };

    // Boot once to the "I am alive" marker — the last checkpoint before the
    // kernel reads the workload bootarg (kernel/src/main.rs:228 vs :339).
    let boot_start = Instant::now();
    let boot_steps = match base.run_until_uart(b"I am alive", 60_000_000) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("snemu-fork: boot: {e}");
            return ExitCode::from(1);
        }
    };
    let boot_time = boot_start.elapsed();
    let snapshot = base; // the machine, parked at the pre-workload checkpoint
    eprintln!(
        "snemu-fork: booted common prefix once in {:.3}s ({boot_steps} steps), snapshotting.",
        boot_time.as_secs_f64()
    );

    let sweep_start = Instant::now();
    println!();
    println!("{:<22} {:>7} {:>9} {:>8}  {}", "WORKLOAD", "FRAMES", "FORK+RUN", "1ST-SPAN", "STOP");
    for &w in WORKLOADS {
        let mut m = snapshot.clone();
        if let Some(dtb) = workload_dtb(&base_dtb, w) {
            if let Err(e) = m.write_ram(snemu::loader::DTB_ADDR, &dtb) {
                println!("{w:<22} patch failed: {e}");
                continue;
            }
        }
        let (frames, stop, span_at) = fork_collect(&mut m, max_steps);
        println!(
            "{w:<22} {:>7} {:>8.3}s {:>7}  {stop}",
            frames.len(),
            span_at.0.as_secs_f64(),
            fmt_opt(span_at.1),
        );
    }
    let sweep_time = sweep_start.elapsed();
    println!();
    println!(
        "snemu-fork: 1 boot ({:.3}s) + {} forks in {:.3}s = {:.3}s total.",
        boot_time.as_secs_f64(),
        WORKLOADS.len(),
        sweep_time.as_secs_f64(),
        (boot_time + sweep_time).as_secs_f64(),
    );
    println!(
        "snemu-fork: booting each fresh would re-run the ~{boot_steps}-step prefix {}× \
         (~{:.1}s of boot saved).",
        WORKLOADS.len(),
        boot_time.as_secs_f64() * (WORKLOADS.len() - 1) as f64,
    );
    ExitCode::SUCCESS
}

/// Run a forked machine to its step budget, returning frames, stop reason, and
/// (total run time, first-span-after-resume).
fn fork_collect(
    machine: &mut snemu::machine::Machine,
    max_steps: u64,
) -> (Vec<OwnedFrame>, String, (Duration, Option<Duration>)) {
    let start = Instant::now();
    let mut steps = 0u64;
    let mut stop = format!("step limit ({max_steps})");
    let mut first_span = None;
    let mut seen_tx = 0usize;
    let base_frames = decode_frames(machine.virtio_tx_output()).len(); // already in snapshot
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            Err(e) => {
                stop = format!("{e:?} @ {steps} steps");
                break;
            }
        }
        if first_span.is_none() {
            let tx = machine.virtio_tx_output();
            if tx.len() != seen_tx {
                seen_tx = tx.len();
                // "first span after resume" = past whatever the snapshot held.
                if decode_frames(tx).len() > base_frames && has_span(&decode_frames(tx)) {
                    first_span = Some(start.elapsed());
                }
            }
        }
    }
    (decode_frames(machine.virtio_tx_output()), stop, (start.elapsed(), first_span))
}

/// The detailed single-workload report.
fn print_detailed(cmp: &Comparison) {
    eprintln!(
        "snemu-diff: snemu {} frames ({}), qemu {} frames",
        cmp.snemu_frames, cmp.snemu_stop, cmp.qemu_frames
    );
    eprintln!(
        "snemu-diff: wall time first-span / {MILESTONE_FRAMES}-frame milestone — snemu {}, qemu {}",
        fmt_timing(&cmp.snemu_timing),
        fmt_timing(&cmp.qemu_timing),
    );
    eprintln!(
        "snemu-diff: snemu total run {:.2}s (qemu window {:.2}s)",
        cmp.snemu_timing.total.as_secs_f64(),
        cmp.qemu_timing.total.as_secs_f64(),
    );
    eprintln!(
        "snemu-diff: structural agreement on the first {} frame(s)",
        cmp.common_prefix
    );
    if let Some((s, q)) = &cmp.divergence {
        eprintln!("snemu-diff: first divergence at frame {}:", cmp.common_prefix);
        eprintln!("  snemu: {s:?}");
        eprintln!("  qemu:  {q:?}");
    }
    eprintln!(
        "snemu-diff: vocabulary — {} shared, {} only-qemu, {} only-snemu",
        cmp.vocab_shared,
        cmp.only_qemu.len(),
        cmp.only_snemu.len()
    );
    if !cmp.only_snemu.is_empty() {
        eprintln!("  only in snemu: {:?}", cmp.only_snemu);
    }
    if !cmp.only_qemu.is_empty() {
        eprintln!("  only in qemu:  {:?}", cmp.only_qemu);
    }
    let invented = invented_names(&cmp.only_snemu, cmp.snemu_crashed);
    let dropped = dropped_names(&cmp.only_qemu);
    if cmp.faithful() {
        if cmp.only_snemu.is_empty() {
            eprintln!("snemu-diff: PASS — snemu faithful to QEMU (vocabularies match).");
        } else {
            eprintln!(
                "snemu-diff: PASS — only-in-snemu is recurring infra QEMU halted before \
                 reaching ({:?}); snemu reached the crash too (panic frame present), so it's \
                 a benign clock-ordering truncation, not invented telemetry.",
                cmp.only_snemu
            );
        }
    } else {
        if !invented.is_empty() {
            eprintln!("snemu-diff: FAIL — snemu invented telemetry QEMU never emits: {invented:?}");
        }
        if !dropped.is_empty() {
            eprintln!(
                "snemu-diff: FAIL — snemu DROPPED telemetry QEMU emits: {dropped:?}. Either snemu \
                 was budget-truncated (raise --steps and re-check — the drop should clear) or it \
                 diverged and will never emit it (the drop persists). Ensure --qemu-secs was long \
                 enough that QEMU itself reached this behaviour."
            );
        }
    }
}

/// The sweep summary table + verdict counts.
fn print_summary(results: &[(String, Result<Comparison, String>)]) -> ExitCode {
    // Timing columns are `first-span / {MILESTONE_FRAMES}-frame` wall seconds.
    println!();
    println!(
        "{:<22} {:<5} {:>5} {:>9} {:>15} {:>15}  {}",
        "WORKLOAD", "VERD", "PREFX", "VOCAB", "SNEMU sp/ms", "QEMU sp/ms", "SNEMU STOP"
    );
    let (mut pass, mut fail, mut errored) = (0, 0, 0);
    for (name, result) in results {
        match result {
            Ok(cmp) => {
                let verdict = if cmp.faithful() {
                    pass += 1;
                    "PASS"
                } else {
                    fail += 1;
                    "FAIL"
                };
                let vocab = format!(
                    "{}/{}q/{}s",
                    cmp.vocab_shared,
                    cmp.only_qemu.len(),
                    cmp.only_snemu.len()
                );
                println!(
                    "{name:<22} {verdict:<5} {:>5} {vocab:>9} {:>15} {:>15}  {}",
                    cmp.common_prefix,
                    fmt_timing(&cmp.snemu_timing),
                    fmt_timing(&cmp.qemu_timing),
                    cmp.snemu_stop
                );
            }
            Err(e) => {
                errored += 1;
                println!("{name:<22} {:<5} {:>5} {:>9} {:>15} {:>15}  {e}", "ERR", "-", "-", "-", "-");
            }
        }
    }
    println!();
    println!("snemu-diff: {pass} PASS, {fail} FAIL, {errored} ERROR of {} workloads", results.len());
    if fail == 0 && errored == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{SpanId, StringId};

    fn hello() -> OwnedFrame {
        OwnedFrame::Hello { timebase_hz: 10_000_000, protocol_version: 4 }
    }
    fn span_start(id: u64, t: u64) -> OwnedFrame {
        OwnedFrame::SpanStart {
            id: SpanId(id),
            parent: SpanId(0),
            name_id: StringId(1),
            t,
            task_id: 0,
            hart_id: 0,
        }
    }
    fn metric(value: i64, t: u64) -> OwnedFrame {
        OwnedFrame::Metric { name_id: StringId(4), value, t, hart_id: 0 }
    }
    fn strreg(id: u32, value: &str) -> OwnedFrame {
        OwnedFrame::StringRegister { id: StringId(id), value: value.to_string() }
    }

    fn log(msg: &str) -> OwnedFrame {
        OwnedFrame::Log { msg: msg.to_string(), task_id: 0, t: 0, hart_id: 0 }
    }

    #[test]
    fn kernel_heartbeat_is_benign_only_when_snemu_reached_the_crash() {
        // A deliberate-crash workload halts QEMU before its wall-clock reaches the
        // first heartbeat, while snemu's instruction-clock emits a few first. That's
        // recurring infra QEMU truncated — benign — but ONLY if snemu actually
        // reached the crash. With the crash observed, `kernel.heartbeat` only in
        // snemu is forgiven.
        let only_snemu = vec!["kernel.heartbeat".to_string()];
        assert!(invented_names(&only_snemu, true).is_empty());

        // If snemu did NOT crash yet still emits heartbeats QEMU lacks, that's the
        // "failed to halt" bug — snemu ran past where it should have died. Not
        // forgiven: it counts as an invention.
        assert_eq!(invented_names(&only_snemu, false), vec!["kernel.heartbeat"]);
    }

    #[test]
    fn a_workload_specific_only_snemu_name_is_always_an_invention() {
        // Anything other than the recurring-infra names IS snemu emitting telemetry
        // QEMU never would — kept regardless of whether snemu crashed.
        let only_snemu = vec![
            "kernel.heartbeat".to_string(),
            "snitchos.task.ghost.runs_total".to_string(),
        ];
        assert_eq!(invented_names(&only_snemu, true), vec!["snitchos.task.ghost.runs_total"]);
    }

    #[test]
    fn snemu_reached_crash_keys_on_the_panic_log() {
        // The panic handler emits `Log("kernel panic …")`; its presence proves
        // snemu ran to the crash (just later than QEMU), not that it hung past it.
        assert!(snemu_reached_crash(&[hello(), log("kernel panic: deliberate")]));
        assert!(!snemu_reached_crash(&[hello(), span_start(1, 1)]));
    }

    #[test]
    fn identical_streams_modulo_timestamps_fully_agree() {
        // Same frames, different timestamps (snemu's deterministic clock vs
        // QEMU's cycles) — must be treated as agreement.
        let snemu = vec![hello(), span_start(1, 100), span_start(2, 200)];
        let qemu = vec![hello(), span_start(1, 7777), span_start(2, 9999)];
        let d = diff_streams(&snemu, &qemu);
        assert_eq!(d.common_prefix, 3);
        assert!(d.divergence.is_none());
    }

    #[test]
    fn metric_value_drift_is_ignored() {
        // The same metric with different values (heartbeat counts drift) is a match.
        let snemu = vec![metric(5, 10)];
        let qemu = vec![metric(9999, 20)];
        let d = diff_streams(&snemu, &qemu);
        assert_eq!(d.common_prefix, 1);
        assert!(d.divergence.is_none());
    }

    #[test]
    fn hart_register_mhartid_drift_is_ignored() {
        // snemu and QEMU boot on different physical harts, so `mhartid` differs (0 vs
        // 1) while the logical topology is identical. That's firmware noise, not a
        // structural divergence — canonicalization must normalize it so the diff sees
        // past early boot instead of halting on it (`id`, the logical hart, is kept).
        let snemu = vec![OwnedFrame::HartRegister { id: 0, mhartid: 0, role: protocol::HartRole::Boot }];
        let qemu = vec![OwnedFrame::HartRegister { id: 0, mhartid: 1, role: protocol::HartRole::Boot }];
        let d = diff_streams(&snemu, &qemu);
        assert_eq!(d.common_prefix, 1);
        assert!(d.divergence.is_none());
    }

    #[test]
    fn a_structural_difference_is_reported_at_the_first_divergence() {
        // Frame 2 differs in a non-volatile field (name_id via a different span
        // topology): parent differs.
        let diverging = OwnedFrame::SpanStart {
            id: SpanId(2),
            parent: SpanId(1), // snemu had parent 0
            name_id: StringId(1),
            t: 0,
            task_id: 0,
            hart_id: 0,
        };
        let snemu = vec![hello(), span_start(1, 1), span_start(2, 2)];
        let qemu = vec![hello(), span_start(1, 1), diverging];
        let d = diff_streams(&snemu, &qemu);
        assert_eq!(d.common_prefix, 2);
        assert!(d.divergence.is_some());
    }

    #[test]
    fn vocabulary_captures_registered_names_regardless_of_order() {
        let a = vec![strreg(0, "kernel.boot"), strreg(1, "console_init")];
        let b = vec![strreg(9, "console_init"), strreg(3, "kernel.boot")];
        assert_eq!(string_vocabulary(&a), string_vocabulary(&b));
    }

    #[test]
    fn frame_progress_matches_a_whole_buffer_decode() {
        use protocol::Frame;
        let frames = [
            Frame::Dropped { count: 0 },
            Frame::SpanStart {
                id: SpanId(1),
                parent: SpanId(0),
                name_id: StringId(7),
                t: 3,
                task_id: 0,
                hart_id: 0,
            },
            Frame::Dropped { count: 42 },
        ];
        let mut bytes = Vec::new();
        for f in &frames {
            bytes.extend_from_slice(&postcard::to_allocvec(f).expect("encode"));
        }
        // Feed the buffer one byte at a time — the pathological growth the old
        // O(n²) re-decode suffered — and assert the incremental tracker ends at
        // the same frame count + span flag as a single whole-buffer decode,
        // proving it resumes at frame boundaries instead of re-parsing.
        let mut p = FrameProgress::default();
        for end in 1..=bytes.len() {
            p.advance(&bytes[..end]);
        }
        let full = decode_frames(&bytes);
        assert_eq!(full.len(), 3);
        assert_eq!(p.count, full.len(), "same count as whole-buffer decode");
        assert!(p.saw_span, "the middle SpanStart was seen");
    }
}
