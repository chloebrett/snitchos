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

use fs_proto::{FileRights, NodeKind, Op, Request, Response, UserBuf};
use snitchos_user::{
    Endpoint, Metric, Tracer, clock_freq, clock_now, endpoint, entry, register_counter,
    register_histogram, tracer,
};
use stitch::platform::{Platform, RuntimePlatform};
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
/// server): attach for the root cap, **path-walk** the `/`-separated components
/// (each `Lookup` mints the next cap — descend-only, the cap-faithful
/// resolution), then `Read` the leaf in ≤256-byte chunks (the server's per-copy
/// cap) into a `String`. `None` if there is no fs endpoint, a component doesn't
/// resolve, or the bytes aren't UTF-8.
fn read_file(path: &str) -> Option<String> {
    let (_r, root_cap) = endpoint().call([0, 0, 0, 0]).ok()?;
    let mut cap = Endpoint::from_raw_handle(root_cap?);

    // Walk one component at a time; `cap` ends naming the leaf file.
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let pb = part.as_bytes();
        let lookup = Request::Lookup {
            name: UserBuf { ptr: pb.as_ptr() as u64, len: pb.len() as u64 },
            rights: FileRights::READ,
        };
        let (_l, next) = cap.call(lookup.encode()).ok()?;
        cap = Endpoint::from_raw_handle(next?);
    }
    read_all(&cap)
}

/// Drain a File cap's whole contents into a `String` via ≤256-byte `Read` chunks
/// (the server's per-copy cap). `None` if the bytes aren't UTF-8.
fn read_all(file: &Endpoint) -> Option<String> {
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

/// The write side of [`read_file`]: resolve `path` to a File cap carrying
/// `READ|WRITE`, **creating the leaf file if it is absent**, and read its current
/// contents. Returns `(cap_handle, content)`, or `None` if the FS endpoint or a
/// directory component doesn't resolve. Every component is walked with `WRITE` so
/// the leaf cap keeps it — a minted cap's rights are `parent ∩ requested`, so a
/// `READ`-only hop would strip write authority from everything below it.
fn open_file_rw(path: &str) -> Option<(u32, String)> {
    let (_r, root_cap) = endpoint().call([0, 0, 0, 0]).ok()?;
    let mut cap = Endpoint::from_raw_handle(root_cap?);
    let rw = FileRights::READ | FileRights::WRITE;

    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let (leaf, dirs) = parts.split_last()?;

    // Descend to the parent directory, keeping WRITE at every hop.
    for part in dirs {
        let pb = part.as_bytes();
        let lookup = Request::Lookup {
            name: UserBuf { ptr: pb.as_ptr() as u64, len: pb.len() as u64 },
            rights: rw,
        };
        let (_l, next) = cap.call(lookup.encode()).ok()?;
        cap = Endpoint::from_raw_handle(next?);
    }

    // Look up the leaf with WRITE; create an empty File if it's absent.
    let lb = leaf.as_bytes();
    let lookup = Request::Lookup {
        name: UserBuf { ptr: lb.as_ptr() as u64, len: lb.len() as u64 },
        rights: rw,
    };
    let (words, next) = cap.call(lookup.encode()).ok()?;
    let file_handle = match Response::decode(Op::Lookup, words) {
        Ok(Response::Inode(_)) => next?,
        _ => {
            let create = Request::Create {
                name: UserBuf { ptr: lb.as_ptr() as u64, len: lb.len() as u64 },
                kind: NodeKind::File,
            };
            let (cwords, created) = cap.call(create.encode()).ok()?;
            match Response::decode(Op::Create, cwords) {
                Ok(Response::Inode(_)) => created?,
                _ => return None,
            }
        }
    };

    let content = read_all(&Endpoint::from_raw_handle(file_handle)).unwrap_or_default();
    Some((u32::try_from(file_handle).ok()?, content))
}

const PROMPT: &str = "stitch> ";

/// Clock ticks per millisecond, derived from the platform timebase the kernel
/// reports (`ClockFreq`) rather than hardcoding QEMU's 10 MHz. `.max(1)` guards
/// the `dt / ticks_per_ms()` divide on an implausibly slow clock.
fn ticks_per_ms() -> u64 {
    (clock_freq() / 1000).max(1)
}

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
fn bench(
    repl: &mut Repl,
    tr: Tracer,
    metrics: &mut EvalMetrics,
    platform: &dyn Platform,
    label: &str,
    src: &str,
) {
    let (out, dt) = timed(repl, tr, metrics, src);
    // `eval_line` ends a *result* line with a newline but a Unit line (the
    // `span(...)` self-test evaluates to Unit, and on the metal its telemetry goes
    // to the wire, not the local render) with none — so normalize to exactly one
    // trailing newline, or the final `stitch>` prompt runs onto this line.
    let out = out.trim_end();
    platform.write(&format!(
        "  [{label:>8}] {dt:>9} ticks (~{} ms)   {src}  {out}\n",
        dt / ticks_per_ms()
    ));
}

#[entry]
fn main() {
    // One platform instance, shared between the REPL's own line I/O and any
    // `print`/`readLine` a Stitch program runs — so both draw from one input
    // stream (one `LineEditor`), and batched input can't deadlock.
    let platform = Rc::new(RuntimePlatform::new());
    // One env, built once (prelude registered a single time) and reused for every
    // line. Telemetry routes through the capability-backed backend (spans/metrics
    // become real wire frames); console routes through the same `platform`.
    // The UART console renders ANSI, so color rights glyphs (green mint / blue
    // read / amber write) in result tables.
    let mut repl = Repl::with_backends(Rc::new(RuntimeTelemetry::default()), platform.clone())
        .color(true);
    let tr = tracer();
    // The interpreter's own metrics (independent of any loaded program), plus a
    // counter of `:load`s served off the filesystem.
    let mut metrics = EvalMetrics::new();
    let loads = register_counter("stitch.loads.count");
    let mut loads_n = 0u64;

    platform.write("\nStitch on SnitchOS \u{2014} the tree-walker runs on the metal.\n");
    // Boot self-tests, timed: the FIRST eval also builds the env (registers the
    // whole prelude once) so it's the expensive one; the rest reuse the cached env.
    bench(&mut repl, tr, &mut metrics, &*platform, "buildenv", "1 + 2");
    bench(&mut repl, tr, &mut metrics, &*platform, "cached", "3 * 4");
    bench(&mut repl, tr, &mut metrics, &*platform, "pipeline", "1.. |> map($ * $) |> take(5) |> toList");
    // A Stitch program's own `span`/`emit` — routed through the capability-backed
    // RuntimeTelemetry, so they cross the wire as real frames (a "stitch.demo"
    // span bracketing a "stitch.answer" gauge), attributed to this process.
    bench(
        &mut repl,
        tr,
        &mut metrics,
        &*platform,
        "telemetry",
        "span(\"stitch.demo\", () -> emit(\"stitch.answer\", 42))",
    );
    platform.write(PROMPT);

    // The REPL loop reads each line *through the platform* — the same path a
    // `readLine()` inside an evaluated line uses, so they share one input stream.
    while let Some(line) = platform.read_line() {
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
            platform.write(&msg);
        } else if let Some(path) = trimmed.strip_prefix(":stim ") {
            // Open `path` in stim: resolve/create it with a WRITE cap, then hand
            // that cap + the FSM to the driver. In-process (phase 1) — stim uses
            // this REPL's platform; read-only is still real via the file cap's
            // rights. The FSM source is compiled in (also seeded at /stim/stim.st).
            const STIM_SRC: &str = include_str!("../../../../fs-image/stim/stim.st");
            let path = path.trim();
            match open_file_rw(path) {
                Some((handle, content)) => {
                    platform.write(&format!("\u{1b}[2J\u{1b}[Hstim: {path} (Ctrl-C to exit)\r\n"));
                    if let Err(e) = stitch::stim::run(STIM_SRC, &content, handle, &*platform) {
                        platform.write(&format!("stim: {}\n", e.message()));
                    }
                }
                None => platform.write(&format!("stim: cannot open `{path}`\n")),
            }
        } else if !trimmed.is_empty() {
            let (out, dt) = timed(&mut repl, tr, &mut metrics, &line);
            platform.write(&out);
            platform.write(&format!("  ({dt} ticks, ~{} ms)\n", dt / ticks_per_ms()));
        }
        platform.write(PROMPT);
    }
}
