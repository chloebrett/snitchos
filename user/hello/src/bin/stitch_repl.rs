//! `workload=stitch-repl` — the Stitch tree-walk interpreter running as a
//! userspace REPL **on the metal**. The first on-target run of the ported
//! `no_std` interpreter.
//!
//! On boot it prints a banner and a self-test (`1 + 2 => 3`) — so *just booting*
//! proves the interpreter parses + evaluates in a SnitchOS userspace process,
//! with output going out the real UART terminal via `ConsoleWrite`. Then it
//! loops: poll `ConsoleRead` for a line (echoing keystrokes), evaluate it through
//! the interpreter's REPL, and `ConsoleWrite` the result.
//!
//! Caveat (the path-3 stepping stone): the tree-walker has no GC and leaks per
//! evaluated line (Rc cycles from closures), and re-registers the whole prelude
//! each line — fine for a demo session, an accumulator for a long one. The
//! bytecode VM + collector is the eventual fix.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use fs_proto::{FileRights, Op, Request, Response, UserBuf};
use snitchos_user::{
    Endpoint, Metric, Tracer, clock_now, console_read, console_write, endpoint, entry,
    register_counter, register_histogram, tracer, yield_now,
};
use stitch::runner::Repl;
use stitch::telemetry::RuntimeTelemetry;

/// The interpreter narrating *itself* — per-eval duration and a running eval
/// count, emitted as real metrics (not just printed). Program-independent: these
/// describe the Stitch runtime, whatever `.st` it happens to be running. The
/// tree-walk down payment on the VM's eventual GC/dispatch self-telemetry.
struct EvalMetrics {
    /// `stitch.eval.duration_ticks` — a histogram of per-line eval cost.
    duration_ticks: Metric,
    /// `stitch.eval.count` — cumulative evals (a counter; we hold the running
    /// total and emit it, since the wire metric carries an absolute value).
    count: Metric,
    n: u64,
}

impl EvalMetrics {
    fn new() -> Self {
        Self {
            duration_ticks: register_histogram("stitch.eval.duration_ticks"),
            count: register_counter("stitch.eval.count"),
            n: 0,
        }
    }

    fn record(&mut self, dt: u64) {
        self.duration_ticks.emit(dt as i64);
        self.n += 1;
        self.count.emit(self.n as i64);
    }
}

/// Read a whole file off the FS endpoint (the `stitch-fs` workload's seeded
/// server): attach for the root cap, `Lookup` the name (READ), then `Read` it in
/// ≤256-byte chunks (the server's per-copy cap) into a `String`. `None` if there
/// is no fs endpoint, the file doesn't exist, or the bytes aren't UTF-8.
fn read_file(name: &str) -> Option<String> {
    let (_r, root_cap) = endpoint().call([0, 0, 0, 0]).ok()?;
    let root = Endpoint::from_raw_handle(root_cap?);

    let nb = name.as_bytes();
    let lookup = Request::Lookup {
        name: UserBuf { ptr: nb.as_ptr() as u64, len: nb.len() as u64 },
        rights: FileRights::READ,
    };
    let (_l, file_cap) = root.call(lookup.encode()).ok()?;
    let file = Endpoint::from_raw_handle(file_cap?);

    let mut bytes = Vec::new();
    let mut offset = 0u64;
    let mut chunk = [0u8; 256];
    loop {
        let read = Request::Read {
            offset,
            dst: UserBuf { ptr: chunk.as_mut_ptr() as u64, len: chunk.len() as u64 },
        };
        let (words, _) = file.call(read.encode()).ok()?;
        let n = match Response::decode(Op::Read, words) {
            Ok(Response::Count(n)) => n as usize,
            _ => break,
        };
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        offset += n as u64;
        if n < chunk.len() {
            break;
        }
    }
    String::from_utf8(bytes).ok()
}

const PROMPT: &[u8] = b"stitch> ";

/// Timebase is 10 MHz on QEMU `virt` → 10_000 ticks per millisecond.
const TICKS_PER_MS: u64 = 10_000;

/// Evaluate `src`, timing it with the monotonic clock and bracketing it in a
/// real SnitchOS span. Returns the rendered output and the elapsed ticks.
fn timed(repl: &mut Repl, tr: Tracer, metrics: &mut EvalMetrics, src: &str) -> (String, u64) {
    let _span = tr.span("stitch.eval");
    let start = clock_now();
    let out = repl.eval_line(src);
    let dt = clock_now() - start;
    metrics.record(dt);
    (out, dt)
}

/// A labelled boot self-test: time `src` and print the result + how long the
/// interpreter took. `label` distinguishes the env-build (first) eval from the
/// cheap cached ones.
fn bench(repl: &mut Repl, tr: Tracer, metrics: &mut EvalMetrics, label: &str, src: &str) {
    let (out, dt) = timed(repl, tr, metrics, src);
    let line = format!(
        "  [{label:>8}] {dt:>9} ticks (~{} ms)   {src}  {out}",
        dt / TICKS_PER_MS
    );
    console_write(line.as_bytes());
}

#[entry]
fn main() {
    // One env, built once (prelude registered a single time) and reused for every
    // line — no per-line prelude rebuild. Telemetry routes through the process's
    // capability-backed backend, so a Stitch program's own `span`/`emit` become
    // real frames on the wire (interned + timestamped + attributed kernel-side).
    let mut repl = Repl::with_telemetry(Rc::new(RuntimeTelemetry::default()));
    let tr = tracer();
    // The interpreter's own metrics (independent of any loaded program), plus a
    // counter of `:load`s served off the filesystem.
    let mut metrics = EvalMetrics::new();
    let loads = register_counter("stitch.loads.count");
    let mut loads_n = 0u64;

    console_write(b"\nStitch on SnitchOS \xE2\x80\x94 the tree-walker runs on the metal.\n");
    // Boot self-tests, timed: the FIRST eval also builds the env (registers the
    // whole prelude once) so it's the expensive one; the rest reuse the cached env.
    bench(&mut repl, tr, &mut metrics, "buildenv", "1 + 2");
    bench(&mut repl, tr, &mut metrics, "cached", "3 * 4");
    bench(&mut repl, tr, &mut metrics, "pipeline", "1.. |> map($ * $) |> take(5) |> toList");
    // A Stitch program's own `span`/`emit` — routed through the capability-backed
    // RuntimeTelemetry, so they cross the wire as real frames (a "stitch.demo"
    // span bracketing a "stitch.answer" gauge), attributed to this process.
    bench(
        &mut repl,
        tr,
        &mut metrics,
        "telemetry",
        "span(\"stitch.demo\", () -> emit(\"stitch.answer\", 42))",
    );
    console_write(PROMPT);

    let mut line = String::new();
    let mut buf = [0u8; 64];
    loop {
        let n = console_read(&mut buf);
        if n == 0 {
            yield_now();
            continue;
        }
        for &byte in &buf[..n] {
            match byte {
                b'\r' | b'\n' => {
                    console_write(b"\n");
                    let trimmed = line.trim();
                    if let Some(name) = trimmed.strip_prefix(":load ") {
                        // Read the file off the filesystem and register its defs.
                        let name = name.trim();
                        let msg = match read_file(name) {
                            Some(src) => {
                                loads_n += 1;
                                loads.emit(loads_n as i64);
                                repl.load_source(&src)
                            }
                            None => format!("load error: cannot read `{name}` from fs\n"),
                        };
                        console_write(msg.as_bytes());
                    } else if !trimmed.is_empty() {
                        let (out, dt) = timed(&mut repl, tr, &mut metrics, &line);
                        console_write(out.as_bytes());
                        let timing = format!("  ({dt} ticks, ~{} ms)\n", dt / TICKS_PER_MS);
                        console_write(timing.as_bytes());
                    }
                    line.clear();
                    console_write(PROMPT);
                }
                // Backspace / delete: drop the last char and erase it on screen.
                0x08 | 0x7f => {
                    if line.pop().is_some() {
                        console_write(b"\x08 \x08");
                    }
                }
                // Printable ASCII: echo it and add it to the line.
                0x20..=0x7e => {
                    console_write(&[byte]);
                    line.push(byte as char);
                }
                // Ignore other control bytes.
                _ => {}
            }
        }
    }
}
