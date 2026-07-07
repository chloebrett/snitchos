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
        CapEvent, ContextSwitch, Event, Log, Message, Metric, NotifySignal, NotifyWait, SpanEnd,
        SpanStart, SyscallRefused,
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
        // No timestamp / volatile field: Hello, StringRegister, MetricRegister,
        // ThreadRegister, HartRegister, Dropped.
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
struct Timing {
    /// Wall time from run start to the first `SpanStart` frame.
    first_span: Option<Duration>,
    /// Wall time to emit [`MILESTONE_FRAMES`] frames — the comparable finish line.
    milestone: Option<Duration>,
    /// Wall time for the whole run (snemu: the step loop; qemu: the window).
    total: Duration,
}

fn has_span(frames: &[OwnedFrame]) -> bool {
    frames.iter().any(|f| matches!(f, OwnedFrame::SpanStart { .. }))
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
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            Err(e) => {
                stop = format!("{e:?} @ {steps} steps");
                break;
            }
        }
        // Watch the TX buffer for the timing marks (cheap length check per step;
        // decode only when it grows, and only until both marks are found).
        if first_span.is_none() || milestone.is_none() {
            let tx = machine.virtio_tx_output();
            if tx.len() != seen_tx {
                seen_tx = tx.len();
                let frames = decode_frames(tx);
                if first_span.is_none() && has_span(&frames) {
                    first_span = Some(start.elapsed());
                }
                if milestone.is_none() && frames.len() >= MILESTONE_FRAMES {
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
    let mut cmd = qemu::base_command(&chardev);
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

impl Comparison {
    /// Faithful ⇔ snemu invented no telemetry QEMU never emitted. Names only QEMU
    /// has are behavior snemu didn't reach in its budget; recurring-infra names
    /// (heartbeat) only snemu has are behavior *QEMU* didn't reach before halting —
    /// but that excuse holds *only if snemu itself reached the crash*
    /// (`snemu_crashed`). Otherwise, and for any other only-snemu name, it's a
    /// genuine invention ([`invented_names`]) and breaks faithfulness.
    fn faithful(&self) -> bool {
        invented_names(&self.only_snemu, self.snemu_crashed).is_empty()
    }
}

/// Boot `kernel` (with `workload`) under both emulators and structurally compare.
fn compare(
    kernel: &[u8],
    dtb_base: &[u8],
    workload: Option<&str>,
    max_steps: u64,
    qemu_secs: u64,
) -> Result<Comparison, String> {
    // Firmware role: inject the workload into the DTB snemu boots (QEMU gets it
    // via `-append`), so both emulators run the same scenario.
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    let (snemu, snemu_stop, snemu_timing) = collect_snemu(kernel, &dtb, max_steps)?;
    let (qemu, qemu_timing) = collect_qemu(Duration::from_secs(qemu_secs), workload)?;

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
    let features: &[&str] = if with_workloads { &["itest-workloads"] } else { &[] };
    if !qemu::build_kernel(features).is_ok_and(|s| s.success()) {
        return Err("kernel build failed".to_string());
    }
    let kernel = std::fs::read(qemu::KERNEL_BIN).map_err(|e| format!("read kernel: {e}"))?;
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
) -> Result<Vec<OwnedFrame>, String> {
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    let mut machine = snemu::loader::load_machine(kernel, RAM_SIZE, Some(&dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
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
) -> Result<snemu::bench::Sample, String> {
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(dtb_base, &format!("workload={w}"))
            .ok_or("DTB patch failed")?,
        None => dtb_base.to_vec(),
    };
    let mut machine = snemu::loader::load_machine(kernel, RAM_SIZE, Some(&dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
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
pub fn run(max_steps: u64, qemu_secs: u64, workload: Option<&str>) -> ExitCode {
    let (kernel, dtb) = match prepare(workload.is_some()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu-diff: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!("snemu-diff: workload = {}", workload.unwrap_or("default (init)"));
    let cmp = match compare(&kernel, &dtb, workload, max_steps, qemu_secs) {
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
pub fn run_all(max_steps: u64, qemu_secs: u64, limit: Option<usize>) -> ExitCode {
    let (kernel, dtb) = match prepare(true) {
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
        let cmp = compare(&kernel, &dtb, Some(w), max_steps, qemu_secs);
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
    let invented = invented_names(&cmp.only_snemu, cmp.snemu_crashed);
    if cmp.faithful() {
        if cmp.only_snemu.is_empty() {
            eprintln!("snemu-diff: PASS — snemu faithful to QEMU (nothing only-in-snemu).");
        } else {
            eprintln!(
                "snemu-diff: PASS — only-in-snemu is recurring infra QEMU halted before \
                 reaching ({:?}); snemu reached the crash too (panic frame present), so it's \
                 a benign clock-ordering truncation, not invented telemetry.",
                cmp.only_snemu
            );
        }
    } else {
        eprintln!("snemu-diff: FAIL — snemu invented telemetry QEMU never emits: {invented:?}");
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
}
