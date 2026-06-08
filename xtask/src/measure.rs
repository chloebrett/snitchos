//! `cargo xtask measure --workload <name>` — boot a runtime-selected
//! workload, capture its telemetry for a fixed window, and print
//! steady-state stats (throughput, lock-wait fraction, queue depth).
//!
//! Replicable version of the one-off boot+reader+parse measurement.
//! See `docs/v0.6-mutex-vs-spsc-measurements.md`.
//!
//! The stats math (`summarize`) is pure and host-tested; the QEMU /
//! socket plumbing is integration glue.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use protocol::stream::{decode_stream, OwnedFrame};
use protocol::StringId;

use crate::qemu;

/// QEMU `virt` rv64 timebase (DTB `timebase-frequency`): 10 MHz, so
/// 1 tick = 100 ns. Our `cargo xtask` QEMU invocation is fixed to this
/// machine; override with `--timebase-hz` if that ever changes.
pub const DEFAULT_TIMEBASE_HZ: u64 = 10_000_000;

const MEASURE_SOCKET: &str = "/tmp/snitch-measure.sock";

/// Workload metrics this command tracks, keyed by wire name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Metric {
    LockWait,
    Consumed,
    Produced,
    QueueDepth,
}

impl Metric {
    fn from_wire(name: &str) -> Option<Self> {
        match name {
            "snitchos.workload.lock_wait_ticks_total" => Some(Self::LockWait),
            "snitchos.workload.samples_consumed_total" => Some(Self::Consumed),
            "snitchos.workload.samples_produced_total" => Some(Self::Produced),
            "snitchos.workload.queue_depth" => Some(Self::QueueDepth),
            _ => None,
        }
    }
}

/// One observed metric value at a kernel timestamp.
#[derive(Clone, Copy, Debug)]
struct Point {
    metric: Metric,
    t: u64,
    value: i64,
}

/// Steady-state summary over the measurement window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Summary {
    window_secs: f64,
    heartbeat_period_ms: f64,
    throughput_consumed: f64,
    throughput_produced: f64,
    lock_wait_ticks: i64,
    lock_wait_fraction_pct: f64,
    lock_wait_ns_per_sample: f64,
    qdepth_min: i64,
    qdepth_max: i64,
    qdepth_avg: f64,
    final_consumed: i64,
    final_produced: i64,
}

fn series(points: &[Point], metric: Metric) -> Vec<(u64, i64)> {
    let mut s: Vec<(u64, i64)> = points
        .iter()
        .filter(|p| p.metric == metric)
        .map(|p| (p.t, p.value))
        .collect();
    s.sort_by_key(|(t, _)| *t);
    s
}

/// Compute steady-state stats from captured points, skipping the first
/// `warmup_secs` of the consumed series (boot transient). Ratios are
/// timebase-independent; throughput uses `timebase_hz`.
fn summarize(points: &[Point], warmup_secs: f64, timebase_hz: u64) -> Result<Summary, String> {
    let hz = timebase_hz as f64;
    let consumed = series(points, Metric::Consumed);
    if consumed.len() < 2 {
        return Err("not enough consumed samples — capture a longer window".to_string());
    }
    let capture_start = consumed[0].0;
    let warmup_ticks = (warmup_secs * hz) as u64;
    let window_start_t = capture_start + warmup_ticks;

    let in_window = |s: &[(u64, i64)]| -> Vec<(u64, i64)> {
        s.iter().copied().filter(|(t, _)| *t >= window_start_t).collect()
    };
    let cons_w = in_window(&consumed);
    if cons_w.len() < 2 {
        return Err(format!(
            "not enough consumed samples after {warmup_secs}s warmup — capture longer or lower --warmup"
        ));
    }

    let bounds = |metric: Metric| -> Option<((u64, i64), (u64, i64))> {
        let w = in_window(&series(points, metric));
        match (w.first(), w.last()) {
            (Some(&a), Some(&b)) => Some((a, b)),
            _ => None,
        }
    };

    let ((t0, c0), (t1, c1)) = (cons_w[0], cons_w[cons_w.len() - 1]);
    let dt = t1.saturating_sub(t0);
    if dt == 0 {
        return Err("zero-length window".to_string());
    }
    let window_secs = dt as f64 / hz;

    let heartbeat_period_ms = {
        let periods: Vec<u64> = cons_w.windows(2).map(|w| w[1].0 - w[0].0).collect();
        let mean_ticks = periods.iter().sum::<u64>() as f64 / periods.len() as f64;
        mean_ticks / hz * 1000.0
    };

    let dcons = (c1 - c0) as f64;
    let throughput_consumed = dcons / window_secs;

    let (throughput_produced, final_produced) = match bounds(Metric::Produced) {
        Some(((_, p0), (_, p1))) => ((p1 - p0) as f64 / window_secs, p1),
        None => (0.0, 0),
    };

    let (lock_wait_ticks, lock_wait_fraction_pct, lock_wait_ns_per_sample) =
        match bounds(Metric::LockWait) {
            Some(((_, l0), (_, l1))) => {
                let dlw = l1 - l0;
                let frac = dlw as f64 / dt as f64 * 100.0;
                let ns_per_sample = if dcons > 0.0 {
                    (dlw as f64 / dcons) * (1.0e9 / hz)
                } else {
                    0.0
                };
                (dlw, frac, ns_per_sample)
            }
            None => (0, 0.0, 0.0),
        };

    let qd_w = in_window(&series(points, Metric::QueueDepth));
    let (qdepth_min, qdepth_max, qdepth_avg) = if qd_w.is_empty() {
        (0, 0, 0.0)
    } else {
        let vals: Vec<i64> = qd_w.iter().map(|(_, v)| *v).collect();
        let min = *vals.iter().min().unwrap();
        let max = *vals.iter().max().unwrap();
        let avg = vals.iter().sum::<i64>() as f64 / vals.len() as f64;
        (min, max, avg)
    };

    Ok(Summary {
        window_secs,
        heartbeat_period_ms,
        throughput_consumed,
        throughput_produced,
        lock_wait_ticks,
        lock_wait_fraction_pct,
        lock_wait_ns_per_sample,
        qdepth_min,
        qdepth_max,
        qdepth_avg,
        final_consumed: c1,
        final_produced,
    })
}

fn connect_with_deadline(path: &Path, budget: Duration) -> Result<UnixStream, String> {
    let deadline = Instant::now() + budget;
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("connect {}: {e}", path.display())),
        }
    }
}

fn print_report(workload: &str, s: &Summary, markdown: bool) {
    if markdown {
        println!("\n### `workload={workload}` ({:.1}s window)\n", s.window_secs);
        println!("| metric | value |");
        println!("|---|---|");
        println!("| heartbeat period | {:.0} ms |", s.heartbeat_period_ms);
        println!("| throughput (consumed) | {:.0} samples/s |", s.throughput_consumed);
        println!("| throughput (produced) | {:.0} samples/s |", s.throughput_produced);
        println!("| lock_wait_ticks Δ | {} |", s.lock_wait_ticks);
        println!("| lock-wait fraction of wall time | {:.4} % |", s.lock_wait_fraction_pct);
        println!("| lock-wait per sample | {:.0} ns |", s.lock_wait_ns_per_sample);
        println!(
            "| queue_depth (min/max/avg) | {} / {} / {:.1} |",
            s.qdepth_min, s.qdepth_max, s.qdepth_avg
        );
        return;
    }
    println!("\n=== workload={workload}  (steady window {:.1}s) ===", s.window_secs);
    println!("  heartbeat period       {:.0} ms", s.heartbeat_period_ms);
    println!("  throughput consumed    {:.0} samples/s", s.throughput_consumed);
    println!("  throughput produced    {:.0} samples/s", s.throughput_produced);
    println!("  lock_wait Δ            {} ticks", s.lock_wait_ticks);
    println!("  lock-wait fraction     {:.4} % of wall time", s.lock_wait_fraction_pct);
    println!("  lock-wait per sample   {:.0} ns", s.lock_wait_ns_per_sample);
    println!(
        "  queue_depth            min {} / max {} / avg {:.1}",
        s.qdepth_min, s.qdepth_max, s.qdepth_avg
    );
    println!("  final consumed/produced {}/{}", s.final_consumed, s.final_produced);
}

/// Boot `workload`, capture telemetry for `seconds`, print steady-state
/// stats (skipping `warmup_secs`). Builds the `itest-workloads` kernel.
pub fn measure(
    workload: &str,
    seconds: u64,
    warmup_secs: f64,
    timebase_hz: u64,
    markdown: bool,
) -> ExitCode {
    match qemu::build_kernel(&["itest-workloads"]) {
        Ok(s) if s.success() => {}
        Ok(_) => return ExitCode::from(1),
        Err(e) => {
            eprintln!("build kernel: {e}");
            return ExitCode::from(1);
        }
    }

    let _ = std::fs::remove_file(MEASURE_SOCKET);
    let chardev = format!("socket,path={MEASURE_SOCKET},server=on,wait=on,id=telemetry");
    let mut cmd = qemu::base_command(&chardev);
    cmd.args(["-append", &format!("workload={workload}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spawn qemu (is qemu-system-riscv64 on PATH?): {e}");
            return ExitCode::from(1);
        }
    };

    let stream = match connect_with_deadline(Path::new(MEASURE_SOCKET), Duration::from_secs(10)) {
        Ok(s) => s,
        Err(e) => {
            let _ = child.kill();
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };

    let (tx, rx) = channel();
    thread::spawn(move || {
        let mut stream = stream;
        let _ = decode_stream(&mut stream, |frame| {
            let _ = tx.send(OwnedFrame::from_borrowed(frame));
        });
    });

    eprintln!("measuring workload={workload} for {seconds}s (warmup {warmup_secs}s)…");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut names: HashMap<StringId, String> = HashMap::new();
    let mut points: Vec<Point> = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(OwnedFrame::StringRegister { id, value }) => {
                names.insert(id, value);
            }
            Ok(OwnedFrame::Metric { name_id, value, t }) => {
                if let Some(metric) = names.get(&name_id).and_then(|n| Metric::from_wire(n)) {
                    points.push(Point { metric, t, value });
                }
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(MEASURE_SOCKET);

    match summarize(&points, warmup_secs, timebase_hz) {
        Ok(s) => {
            print_report(workload, &s, markdown);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("measurement failed: {e} (captured {} points)", points.len());
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(metric: Metric, t: u64, value: i64) -> Point {
        Point { metric, t, value }
    }

    #[test]
    fn summarize_computes_steady_state_stats() {
        // 3 heartbeats, 1 s apart at 10 MHz; clean synthetic numbers.
        let hz = 10_000_000;
        let pts = vec![
            pt(Metric::Consumed, 0, 0),
            pt(Metric::Consumed, 10_000_000, 100),
            pt(Metric::Consumed, 20_000_000, 200),
            pt(Metric::Produced, 0, 0),
            pt(Metric::Produced, 10_000_000, 128),
            pt(Metric::Produced, 20_000_000, 256),
            pt(Metric::LockWait, 0, 0),
            pt(Metric::LockWait, 10_000_000, 1_000_000),
            pt(Metric::LockWait, 20_000_000, 2_000_000),
            pt(Metric::QueueDepth, 0, 10),
            pt(Metric::QueueDepth, 10_000_000, 20),
            pt(Metric::QueueDepth, 20_000_000, 30),
        ];
        let s = summarize(&pts, 0.0, hz).expect("summary");
        assert!((s.window_secs - 2.0).abs() < 1e-9);
        assert!((s.heartbeat_period_ms - 1000.0).abs() < 1e-6);
        assert!((s.throughput_consumed - 100.0).abs() < 1e-6);
        assert!((s.throughput_produced - 128.0).abs() < 1e-6);
        assert_eq!(s.lock_wait_ticks, 2_000_000);
        assert!((s.lock_wait_fraction_pct - 10.0).abs() < 1e-6);
        // 2_000_000 ticks / 200 samples = 10_000 ticks/sample × 100 ns.
        assert!((s.lock_wait_ns_per_sample - 1_000_000.0).abs() < 1e-3);
        assert_eq!((s.qdepth_min, s.qdepth_max), (10, 30));
        assert!((s.qdepth_avg - 20.0).abs() < 1e-9);
        assert_eq!(s.final_consumed, 200);
    }

    #[test]
    fn warmup_skips_early_samples() {
        let hz = 10_000_000;
        // Early sample at t=0 (transient), steady from t=10ms on.
        let pts = vec![
            pt(Metric::Consumed, 0, 0),
            pt(Metric::Consumed, 10_000_000, 50),
            pt(Metric::Consumed, 20_000_000, 150),
        ];
        // 1 s warmup drops the t=0 point; window is t=10ms..20ms.
        let s = summarize(&pts, 1.0, hz).expect("summary");
        assert!((s.window_secs - 1.0).abs() < 1e-9);
        assert!((s.throughput_consumed - 100.0).abs() < 1e-6); // (150-50)/1s
    }

    #[test]
    fn too_little_data_errors() {
        let pts = vec![pt(Metric::Consumed, 0, 0)];
        assert!(summarize(&pts, 0.0, 10_000_000).is_err());
    }

    #[test]
    fn missing_lock_wait_series_is_zero_not_panic() {
        let hz = 10_000_000;
        let pts = vec![
            pt(Metric::Consumed, 0, 0),
            pt(Metric::Consumed, 10_000_000, 64),
        ];
        let s = summarize(&pts, 0.0, hz).expect("summary");
        assert_eq!(s.lock_wait_ticks, 0);
        assert_eq!(s.lock_wait_fraction_pct, 0.0);
    }
}
