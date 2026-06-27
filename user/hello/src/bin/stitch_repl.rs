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

use alloc::string::String;
use alloc::vec::Vec;

use snitchos_user::{console_read, console_write, entry, yield_now};
use stitch::ast::Item;
use stitch::runner::run_repl_line;

const PROMPT: &[u8] = b"stitch> ";

#[entry]
fn main() {
    let mut defs: Vec<Item> = Vec::new();

    console_write(b"\nStitch on SnitchOS \xE2\x80\x94 the tree-walker runs on the metal.\n");
    // Boot self-test: evaluate one line before any input, so booting alone proves
    // the interpreter works end to end (parse -> eval -> ConsoleWrite).
    console_write(b"  1 + 2  ");
    console_write(run_repl_line(&mut defs, "1 + 2").as_bytes());
    console_write(b"\n");
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
                    console_write(run_repl_line(&mut defs, &line).as_bytes());
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
