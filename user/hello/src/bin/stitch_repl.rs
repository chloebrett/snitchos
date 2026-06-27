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
use alloc::string::String;

use snitchos_user::{Tracer, clock_now, console_read, console_write, entry, tracer, yield_now};
use stitch::runner::Repl;

const PROMPT: &[u8] = b"stitch> ";

/// Timebase is 10 MHz on QEMU `virt` → 10_000 ticks per millisecond.
const TICKS_PER_MS: u64 = 10_000;

/// Evaluate `src`, timing it with the monotonic clock and bracketing it in a
/// real SnitchOS span. Returns the rendered output and the elapsed ticks.
fn timed(repl: &mut Repl, tr: Tracer, src: &str) -> (String, u64) {
    let _span = tr.span("stitch.eval");
    let start = clock_now();
    let out = repl.eval_line(src);
    (out, clock_now() - start)
}

/// A labelled boot self-test: time `src` and print the result + how long the
/// interpreter took. `label` distinguishes the env-build (first) eval from the
/// cheap cached ones.
fn bench(repl: &mut Repl, tr: Tracer, label: &str, src: &str) {
    let (out, dt) = timed(repl, tr, src);
    let line = format!(
        "  [{label:>8}] {dt:>9} ticks (~{} ms)   {src}  {out}",
        dt / TICKS_PER_MS
    );
    console_write(line.as_bytes());
}

#[entry]
fn main() {
    // One env, built once (prelude registered a single time) and reused for every
    // line — no per-line prelude rebuild.
    let mut repl = Repl::new();
    let tr = tracer();

    console_write(b"\nStitch on SnitchOS \xE2\x80\x94 the tree-walker runs on the metal.\n");
    // Boot self-tests, timed: the FIRST eval also builds the env (registers the
    // whole prelude once) so it's the expensive one; the rest reuse the cached env.
    bench(&mut repl, tr, "buildenv", "1 + 2");
    bench(&mut repl, tr, "cached", "3 * 4");
    bench(&mut repl, tr, "pipeline", "1.. |> map($ * $) |> take(5) |> toList");
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
                    if !line.trim().is_empty() {
                        let (out, dt) = timed(&mut repl, tr, &line);
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
