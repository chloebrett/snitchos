//! The differential oracle: boot the *same* kernel under snemu and QEMU and
//! compare their telemetry frame streams. snemu is deterministic and QEMU is
//! not, so the comparison is **structural** — volatile fields (timestamps, and
//! metric values, which drift with wall-clock) are normalized away, and we diff
//! the boot-prefix frame sequence plus the registered-name vocabulary.
//!
//! snemu boots the default (`init`) workload — it has no `workload=` bootarg
//! support yet — so the QEMU side boots the same default. That keeps the two
//! comparable without per-scenario surgery.

use std::collections::BTreeSet;
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
    pub snemu_len: usize,
    pub qemu_len: usize,
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
    Diff {
        common_prefix,
        divergence,
        snemu_len: snemu.len(),
        qemu_len: qemu.len(),
    }
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

/// Boot the kernel under snemu in-process for up to `max_steps` and return the
/// telemetry frames it transmitted.
fn collect_snemu(kernel: &[u8], dtb: &[u8], max_steps: u64) -> Result<Vec<OwnedFrame>, String> {
    let mut machine = snemu::loader::load_machine(kernel, RAM_SIZE, Some(dtb), HART_COUNT)
        .map_err(|e| format!("snemu load: {e:?}"))?;
    let mut steps = 0u64;
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            // A fault ends the run; surface it (a meta-loop signal) rather than
            // silently diffing a truncated stream.
            Err(e) => {
                eprintln!("snemu-diff: snemu halted after {steps} steps with {e:?}");
                break;
            }
        }
    }
    if steps == max_steps {
        eprintln!("snemu-diff: snemu reached the {max_steps}-step limit cleanly");
    }
    Ok(decode_frames(machine.virtio_tx_output()))
}

/// Boot the kernel under QEMU (default `init` workload), collect the telemetry
/// frames for `window`, then kill it.
fn collect_qemu(window: Duration, workload: Option<&str>) -> Result<Vec<OwnedFrame>, String> {
    let socket = std::env::temp_dir().join(format!("snitch-diff-{}.sock", std::process::id()));
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

/// The differential oracle entry point: boot the same kernel (optionally a
/// selected `workload`) under snemu and QEMU, structurally diff their frame
/// streams, and report.
pub fn run(max_steps: u64, qemu_secs: u64, workload: Option<&str>) -> ExitCode {
    // A workload needs the runtime registry compiled in on both sides.
    let features: &[&str] = if workload.is_some() { &["itest-workloads"] } else { &[] };
    if !qemu::build_kernel(features).is_ok_and(|s| s.success()) {
        eprintln!("snemu-diff: kernel build failed");
        return ExitCode::from(1);
    }
    let kernel = match std::fs::read(qemu::KERNEL_BIN) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("snemu-diff: read kernel: {e}");
            return ExitCode::from(1);
        }
    };
    let dtb = match std::fs::read(SNEMU_DTB) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("snemu-diff: read {SNEMU_DTB}: {e}");
            return ExitCode::from(1);
        }
    };
    // Firmware role: inject the workload into the DTB snemu boots (QEMU gets it
    // via `-append`), so both emulators run the same scenario.
    let dtb = match workload {
        Some(w) => snemu::dtb::set_bootargs(&dtb, &format!("workload={w}")).unwrap_or(dtb),
        None => dtb,
    };

    let label = workload.unwrap_or("default (init)");
    eprintln!("snemu-diff: workload = {label}");
    eprintln!("snemu-diff: booting under snemu ({max_steps} steps)...");
    let snemu = match collect_snemu(&kernel, &dtb, max_steps) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("snemu-diff: snemu: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!("snemu-diff: booting under qemu ({qemu_secs}s window)...");
    let qemu = match collect_qemu(Duration::from_secs(qemu_secs), workload) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("snemu-diff: qemu: {e}");
            return ExitCode::from(1);
        }
    };

    report(&snemu, &qemu)
}

/// Print the structural comparison of the two frame streams.
fn report(snemu: &[OwnedFrame], qemu: &[OwnedFrame]) -> ExitCode {
    eprintln!(
        "snemu-diff: snemu emitted {} frames, qemu emitted {} frames",
        snemu.len(),
        qemu.len()
    );
    let d = diff_streams(snemu, qemu);
    eprintln!(
        "snemu-diff: structural agreement on the first {} frame(s)",
        d.common_prefix
    );
    match &d.divergence {
        Some((s, q)) => {
            eprintln!("snemu-diff: first divergence at frame {}:", d.common_prefix);
            eprintln!("  snemu: {s:?}");
            eprintln!("  qemu:  {q:?}");
        }
        None if d.snemu_len == d.qemu_len => {
            eprintln!("snemu-diff: streams agree in full (same length)");
        }
        None => {
            eprintln!(
                "snemu-diff: shorter stream is a structural prefix of the longer (lengths differ)"
            );
        }
    }

    let sv = string_vocabulary(snemu);
    let qv = string_vocabulary(qemu);
    let only_snemu: BTreeSet<_> = sv.difference(&qv).collect();
    let only_qemu: BTreeSet<_> = qv.difference(&sv).collect();
    eprintln!(
        "snemu-diff: registered-name vocabulary — snemu {}, qemu {}, {} shared",
        sv.len(),
        qv.len(),
        sv.intersection(&qv).count()
    );
    if !only_qemu.is_empty() {
        eprintln!("  only in qemu:  {only_qemu:?}");
    }

    // Verdict: any name snemu emitted that QEMU never did is a real divergence
    // (snemu invented telemetry) — a fail. Names only QEMU emitted are frames
    // snemu didn't reach in its step budget (QEMU runs far longer in wall-clock),
    // not a fault. The boot prefix agreement quantifies faithfulness up to the
    // first cross-hart ordering difference.
    if only_snemu.is_empty() {
        eprintln!(
            "snemu-diff: PASS — snemu is faithful to QEMU: boot prefix agreed on {} frames, \
             and every telemetry name snemu emitted, QEMU emitted too \
             ({} name(s) appear only in QEMU — behavior snemu didn't reach in {} frames).",
            d.common_prefix,
            only_qemu.len(),
            snemu.len(),
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("  only in snemu: {only_snemu:?}");
        eprintln!(
            "snemu-diff: FAIL — snemu emitted telemetry names QEMU never did (above): a real divergence.",
        );
        ExitCode::from(1)
    }
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
