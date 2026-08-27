//! `cargo xtask board` — the host bridge to the VisionFive 2.
//!
//! Lean `xtask` forwards here (`cargo run -p xtask-board -- …`) so the tool that
//! runs `cargo xtask test` never compiles `serialport` or this crate's parser.
//!
//! **This binary must never call [`std::process::exit`] while the port is open.**
//! Ownership is what releases the port — `serialport`'s handle closes its fd on
//! drop, and Rust runs destructors while unwinding, so even a panic gives it
//! back. `process::exit` runs no destructors, and a port left held makes the
//! *next* run report `PortHeld` against a process that no longer exists. Hence
//! `main` returns an [`ExitCode`] rather than exiting, all the way down.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use xtask_board::outcome::{Outcome, condition_from, exit_code};
use xtask_board::reach::{Unreachable, check_device, classify_open_failure, holder_pid, io_kind};
use xtask_board::split::split;
use xtask_board::stop::{Capture, StopCondition, StopReason};

/// Drive the board over its UART and get structured frames back.
#[derive(Parser)]
#[command(about, version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write `input` to the board, then capture until a stop condition.
    ///
    /// Prints the board's text on stdout and a summary on stderr. Exit codes are
    /// an interface, not a formality: `0` the capture ended as asked, `1` the
    /// board was reached but the awaited event never came, `2` the board was
    /// never reached at all. An unattended loop needs `1` and `2` apart —
    /// rebooting a board whose port is merely held fixes nothing.
    Exec {
        /// Text to write before capturing. A trailing newline is *not* added:
        /// whether a line is submitted is the caller's decision, since U-Boot and
        /// the Stitch REPL differ on it.
        input: String,

        /// Serial device. A **call-out** node (`/dev/cu.*`) on macOS.
        #[arg(long)]
        device: String,

        /// Line rate. Default matches the kernel's (B3 Step 6's measured choice).
        #[arg(long, default_value_t = 115_200)]
        baud: u32,

        /// Hard deadline for the capture, in milliseconds. Always applies — it is
        /// what makes "a capture that never returns" unrepresentable.
        #[arg(long, default_value_t = 10_000)]
        timeout: u64,

        /// Stop on this marker, e.g. U-Boot's `"=> "`.
        #[arg(long)]
        until: Option<String>,

        /// Stop once the board has been silent this long, in milliseconds.
        ///
        /// **Serial-tuned.** Over Phase 2's WiFi transport a stall in the radio
        /// looks identical to a quiet board, so prefer `--until` there.
        #[arg(long)]
        until_quiet: Option<u64>,
    },
}

fn main() -> ExitCode {
    match Cli::parse().cmd {
        Cmd::Exec { input, device, baud, timeout, until, until_quiet } => {
            let condition = match condition_from(timeout, until.as_deref(), until_quiet) {
                Ok(c) => c,
                Err(message) => {
                    eprintln!("board: {message}");
                    return ExitCode::from(2);
                }
            };
            let outcome = exec(&input, &device, baud, &condition);
            report(&outcome);
            ExitCode::from(exit_code(&condition, &outcome))
        }
    }
}

/// Open the port, write `input`, capture until `condition`.
///
/// The port is a local: it closes when this function returns, however it returns.
fn exec(input: &str, device: &str, baud: u32, condition: &StopCondition) -> Outcome {
    if let Err(unreachable) = check_device(device) {
        return Outcome::Unreachable(unreachable);
    }

    // `Box<dyn Read + Write>` rather than a concrete port: Phase 2 swaps a TCP
    // socket in behind this exact seam, and everything below it is transport-blind.
    let mut port: Box<dyn ReadWrite> = match serialport::new(device, baud)
        .timeout(collector::serial::READ_TIMEOUT)
        .open()
    {
        Ok(port) => Box::new(port),
        Err(e) => {
            let mut unreachable = classify_open_failure(device, io_kind(e.kind));
            consult_lsof(&mut unreachable, device);
            return Outcome::Unreachable(unreachable);
        }
    };

    if let Err(e) = port.write_all(input.as_bytes()) {
        return Outcome::Unreachable(classify_open_failure(device, e.kind()));
    }

    Outcome::Stopped(capture(&mut *port, condition))
}

/// Read until the condition fires, echoing text as it arrives.
///
/// `SerialReader` is deliberately *not* wrapped around the port here: this loop
/// wants to see an idle read as an idle read so it can advance the quiescence
/// clock. Absorbing timeouts is right for a decode loop that must not mistake
/// quiet for EOF, and wrong for one whose whole job is noticing quiet.
fn capture(port: &mut dyn ReadWrite, condition: &StopCondition) -> StopReason {
    let mut capture = Capture::new(condition.clone());
    let mut buf = [0u8; 1024];
    let mut raw: Vec<u8> = Vec::new();
    let started = Instant::now();

    loop {
        let elapsed = started.elapsed();
        let stop = match port.read(&mut buf) {
            Ok(0) => capture.tick(elapsed),
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                capture.observe(elapsed, &buf[..n])
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => capture.tick(elapsed),
            // A genuine read failure ends the capture; the deadline still bounds
            // it, so this cannot spin.
            Err(_) => Some(StopReason::Timeout),
        };
        if let Some(reason) = stop {
            emit(&raw);
            return reason;
        }
    }
}

/// Print what the board said: its own text on stdout, frames on stderr.
///
/// The split is the Unix one — stdout is the data a pipeline consumes, stderr is
/// commentary about the run. `Split::resyncs` counts *lost frames*, so a non-zero
/// count is a transport fault worth surfacing rather than a tidy detail.
fn emit(raw: &[u8]) {
    let out = split(raw);
    print!("{}", out.io_text);
    let _ = std::io::stdout().flush();
    for frame in &out.frames {
        eprintln!("{frame:?}");
    }
    if out.resyncs > 0 {
        eprintln!("board: {} frame(s) lost to decode errors", out.resyncs);
    }
}

/// Say what happened, in the terms the operator has to act on.
fn report(outcome: &Outcome) {
    match outcome {
        Outcome::Stopped(reason) => eprintln!("board: capture ended ({reason:?})"),
        Outcome::Unreachable(u) => eprintln!("board: {u:?}"),
    }
}

/// Use `lsof` as evidence: name the holder, and settle `NoDevice`'s ambiguity.
///
/// Two jobs, both upgrades rather than corrections. It turns "something has the
/// port" into "pid 4242 has it" — the difference between a puzzle and a `kill`.
/// And because `serialport` reports both *busy* and *absent* as `NoDevice`
/// (see [`io_kind`]), a device we classified as missing but which `lsof` says is
/// held was never missing: that is a `PortHeld`, and saying "no such device"
/// would send someone hunting a cable that is fine.
///
/// Best-effort by nature — `lsof` may be absent, and it exits non-zero when
/// nothing holds the file. Silence just leaves the original classification.
fn consult_lsof(unreachable: &mut Unreachable, device: &str) {
    let Ok(out) = std::process::Command::new("lsof").arg(device).output() else { return };
    let Some(pid) = holder_pid(&String::from_utf8_lossy(&out.stdout)) else { return };

    match unreachable {
        Unreachable::PortHeld { holder, .. } => *holder = Some(pid),
        // Classified missing, but something has it open — so it exists.
        Unreachable::NoSuchDevice { device } => {
            *unreachable = Unreachable::PortHeld { device: device.clone(), holder: Some(pid) };
        }
        _ => {}
    }
}

/// The transport seam. Phase 2's TCP socket implements this too.
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// Silences the unused-import warning for `Duration` in builds where the
/// capture loop's types are inferred; keeps the intent visible.
const _: Option<Duration> = None;
