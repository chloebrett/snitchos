//! The differential oracle: boot the *same* kernel under snemu and QEMU and
//! compare their telemetry frame streams. snemu is deterministic and QEMU is
//! not, so the comparison is **structural** — volatile fields (timestamps, and
//! metric values, which drift with wall-clock) are normalized away, and we diff
//! the boot-prefix frame sequence plus the registered-name vocabulary.
//!
//! snemu boots the default (`init`) workload — it has no `workload=` bootarg
//! support yet — so the QEMU side boots the same default. That keeps the two
//! comparable without per-scenario surgery.

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
        CapEvent { kind, cap_id, parent_cap_id, holder, object, rights, badge, hart_id, .. } => {
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

/// Boot the kernel under snemu in-process for up to `max_steps` and return its
/// telemetry frames plus how it stopped (step limit vs a fault — a meta-loop
/// signal we keep as data rather than swallow).
fn collect_snemu(kernel: &[u8], dtb: &[u8], max_steps: u64) -> Result<(Vec<OwnedFrame>, String), String> {
    let mut machine = snemu::loader::load_machine(kernel, RAM_SIZE, Some(dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
    let mut steps = 0u64;
    let mut stop = format!("step limit ({max_steps})");
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            Err(e) => {
                stop = format!("{e:?} @ {steps} steps");
                break;
            }
        }
    }
    Ok((decode_frames(machine.virtio_tx_output()), stop))
}

/// Boot the kernel under QEMU (default `init` workload), collect the telemetry
/// frames for `window`, then kill it.
fn collect_qemu(window: Duration, workload: Option<&str>) -> Result<Vec<OwnedFrame>, String> {
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
    let reader = match stream {
        Ok(stream) => {
            let sink = Arc::clone(&frames);
            Some(thread::spawn(move || {
                let mut stream = stream;
                let _ = decode_stream(&mut stream, |f| {
                    sink.lock().unwrap().push(OwnedFrame::from_borrowed(f));
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
    let frames = Arc::try_unwrap(frames)
        .map_err(|_| "reader thread still holds frames".to_string())?
        .into_inner()
        .map_err(|_| "frame mutex poisoned".to_string())?;
    Ok(frames)
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
const WORKLOADS: &[&str] = &[
    "demo", "init", "smp", "smp-spsc", "smp-spsc-batch", "priorities", "block-wake", "workers",
    "heap-grow", "frame-oom", "heap-oom", "spawn-storm", "ipi-pong", "shootdown-storm",
    "mutex-storm", "virtio-storm", "tlb-shootdown", "ping-pong", "userspace", "userspace-fault",
    "userspace-bad-ptr", "userspace-span-flood", "user-hog", "syscall-hog", "console-echo",
    "spawn-image", "manifest-iface", "probe", "stack-guard", "stack-overflow-deep",
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
    /// How snemu's run ended (step limit vs a fault).
    snemu_stop: String,
    divergence: Option<(OwnedFrame, OwnedFrame)>,
}

impl Comparison {
    /// Faithful ⇔ snemu invented no telemetry QEMU never emitted. (Names only
    /// QEMU has are behavior snemu didn't reach in its budget, not a divergence.)
    fn faithful(&self) -> bool {
        self.only_snemu.is_empty()
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
    let (snemu, snemu_stop) = collect_snemu(kernel, &dtb, max_steps)?;
    let qemu = collect_qemu(Duration::from_secs(qemu_secs), workload)?;

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
        snemu_stop,
        divergence: d.divergence,
    })
}

/// Build the kernel and read the base DTB the emulators share.
fn prepare(with_workloads: bool) -> Result<(Vec<u8>, Vec<u8>), String> {
    let features: &[&str] = if with_workloads { &["itest-workloads"] } else { &[] };
    if !qemu::build_kernel(features).is_ok_and(|s| s.success()) {
        return Err("kernel build failed".to_string());
    }
    let kernel = std::fs::read(qemu::KERNEL_BIN).map_err(|e| format!("read kernel: {e}"))?;
    let dtb = std::fs::read(SNEMU_DTB).map_err(|e| format!("read {SNEMU_DTB}: {e}"))?;
    Ok((kernel, dtb))
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
                "{} (snemu {} frames, {})",
                if c.faithful() { "PASS" } else { "FAIL" },
                c.snemu_frames,
                c.snemu_stop
            ),
            Err(e) => eprintln!("ERROR: {e}"),
        }
        results.push((w.to_string(), cmp));
    }

    print_summary(&results)
}

/// The detailed single-workload report.
fn print_detailed(cmp: &Comparison) {
    eprintln!(
        "snemu-diff: snemu {} frames ({}), qemu {} frames",
        cmp.snemu_frames, cmp.snemu_stop, cmp.qemu_frames
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
    if cmp.faithful() {
        eprintln!("snemu-diff: PASS — snemu faithful to QEMU (nothing only-in-snemu).");
    } else {
        eprintln!("snemu-diff: FAIL — snemu emitted telemetry QEMU never did.");
    }
}

/// The sweep summary table + verdict counts.
fn print_summary(results: &[(String, Result<Comparison, String>)]) -> ExitCode {
    println!();
    println!(
        "{:<22} {:<7} {:>6} {:>16} {:>10}  {}",
        "WORKLOAD", "VERDICT", "PREFIX", "SNEMU→QEMU", "VOCAB", "SNEMU STOP"
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
                let frames = format!("{}→{}", cmp.snemu_frames, cmp.qemu_frames);
                let vocab = format!(
                    "{}/{}q/{}s",
                    cmp.vocab_shared,
                    cmp.only_qemu.len(),
                    cmp.only_snemu.len()
                );
                println!(
                    "{name:<22} {verdict:<7} {:>6} {frames:>16} {vocab:>10}  {}",
                    cmp.common_prefix, cmp.snemu_stop
                );
            }
            Err(e) => {
                errored += 1;
                println!("{name:<22} {:<7} {:>6} {:>16} {:>10}  {e}", "ERROR", "-", "-", "-");
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
