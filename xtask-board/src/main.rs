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

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use xtask_board::echo::visible;
use xtask_board::outcome::{Outcome, condition_from, exit_code};
use xtask_board::reach::{
    Unreachable, check_device, classify_open_failure, holder_pid, io_kind, refine_with_holder,
};
use xtask_board::split::split;
use xtask_board::stop::StopCondition;
use xtask_board::thrash::{self, Policy};
use xtask_board::wire::{Ended, ReadWrite, capture};

/// The console line the kernel's `RebootDetector` matches. Must stay byte-identical
/// to `kernel_devices::console::REBOOT_TOKEN` — the kernel crate is `no_std` and
/// riscv-only, so the host cannot import the constant and the two are kept in step
/// by this comment and the itest that exercises the pair.
const REBOOT_LINE: &str = "~~~reboot\n";

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

    /// Reboot the board over the console and wait until it is booting again.
    ///
    /// Writes the kernel's reboot line, which `RebootDetector` matches and the
    /// heartbeat acts on: reason frame, TX flush, then SBI SRST. Success means
    /// U-Boot's SPL banner was seen — i.e. the platform genuinely reset and is
    /// loading a **fresh** image over TFTP, not merely that the kernel replied.
    ///
    /// Exit codes as `exec`: `0` rebooted, `1` the board took the line but never
    /// came back, `2` never reached — which includes a guard refusal, since the
    /// board was not contacted and the fix is host-side. A refusal must never
    /// read as `1`, or an unattended loop answers it by rebooting again.
    Reboot {
        /// Serial device. A **call-out** node (`/dev/cu.*`) on macOS.
        #[arg(long)]
        device: String,

        /// Line rate.
        #[arg(long, default_value_t = 115_200)]
        baud: u32,

        /// How long to wait for the board to come back, in milliseconds. Generous
        /// by default: a VF2 reset runs DDR training, U-Boot, DHCP and a TFTP
        /// transfer before the SPL banner's successor appears.
        #[arg(long, default_value_t = 60_000)]
        timeout: u64,

        /// Minimum milliseconds between reboots.
        #[arg(long, default_value_t = 10_000)]
        min_interval: u64,

        /// Most reboots allowed within `--window`.
        #[arg(long, default_value_t = 10)]
        cap: usize,

        /// Rolling window for `--cap`, in milliseconds. Without a window the cap
        /// would accumulate forever and eventually refuse every reboot.
        #[arg(long, default_value_t = 3_600_000)]
        window: u64,

        /// Where reboot history is kept, so the guard applies across invocations
        /// rather than resetting every time the process starts.
        #[arg(long, default_value = ".board-reboots")]
        history: std::path::PathBuf,
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
        Cmd::Reboot { device, baud, timeout, min_interval, cap, window, history } => {
            reboot(&RebootRequest {
                device: &device,
                baud,
                timeout: Duration::from_millis(timeout),
                policy: Policy { min_interval: Duration::from_millis(min_interval), cap },
                window: Duration::from_millis(window),
                history_path: &history,
            })
        }
    }
}

/// Everything `reboot` needs. A config struct rather than seven positional
/// arguments, three of which are interchangeable `u64` durations — exactly the
/// shape where a transposition compiles and misbehaves at runtime.
struct RebootRequest<'a> {
    device: &'a str,
    baud: u32,
    timeout: Duration,
    policy: Policy,
    window: Duration,
    history_path: &'a std::path::Path,
}

/// Milliseconds since the Unix epoch. The guard's only requirement is that its
/// instants share a clock; this is that clock.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Ask the guard, send the reboot line, and wait for the board to come back.
fn reboot(req: &RebootRequest<'_>) -> ExitCode {
    let now = now_ms();
    let recorded = std::fs::read_to_string(req.history_path).unwrap_or_default();
    let history = thrash::prune(&thrash::parse_history(&recorded), now, req.window);

    if let thrash::Verdict::Denied(denial) = thrash::judge(&history, now, &req.policy) {
        // Reported as itself, with the numbers: the two denials call for opposite
        // actions — wait, versus stop and look at the build.
        match denial {
            thrash::Denial::TooSoon { since_last, required } => eprintln!(
                "board: reboot refused — last reboot was {}ms ago, minimum is {}ms",
                since_last.as_millis(),
                required.as_millis()
            ),
            thrash::Denial::CapReached { cap } => eprintln!(
                "board: reboot refused — budget spent: at most {cap} reboot(s) per {}s, and \
                 that many have already happened. A build that boot-loops must not be able to \
                 hammer the board; raise --cap or --window if this is deliberate",
                req.window.as_secs()
            ),
        }
        return ExitCode::from(2);
    }

    // U-Boot's SPL banner, not a kernel marker: it is the first thing a *reset*
    // platform prints, so it proves the reboot actually happened. Waiting on
    // anything the running kernel emits would be satisfied by a kernel that never
    // reset at all.
    let condition = StopCondition::new(req.timeout).marker(b"U-Boot SPL");
    let outcome = exec(REBOOT_LINE, req.device, req.baud, &condition);
    report(&outcome);

    let code = exit_code(&condition, &outcome);
    // Record the attempt whenever the line reached the board — including a board
    // that took it and never came back. That is precisely the boot-loop case the
    // cap exists to bound, so *not* recording it would disarm the guard exactly
    // when it is needed.
    if !matches!(outcome, Outcome::Unreachable(_)) {
        let mut updated = history;
        updated.push(now);
        if let Err(e) = std::fs::write(req.history_path, thrash::render_history(&updated)) {
            eprintln!("board: could not record the reboot in {}: {e}", req.history_path.display());
        }
    }
    ExitCode::from(code)
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

    // Echo what is *actually* going on the wire, after every layer of quoting has
    // had its turn. Before the write, so a board that swallows the line still
    // leaves a record of what it swallowed — and so "sent, nothing came back"
    // becomes a case the transcript can express.
    eprintln!("board: > {}", visible(input.as_bytes()));

    if let Err(e) = port.write_all(input.as_bytes()) {
        return Outcome::Unreachable(classify_open_failure(device, e.kind()));
    }

    let (ended, raw) = capture(&mut *port, condition);
    emit(&raw);
    match ended {
        Ended::Stopped(reason) => Outcome::Stopped(reason),
        // The port opened and then died under us. That is an `Unreachable`, not a
        // capture that ended — and it exits 2, so an unattended loop retries the
        // host rather than rebooting a board that was answering fine.
        Ended::TransportFailed(kind) => {
            Outcome::Unreachable(classify_open_failure(device, kind))
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

    // The decision lives in `reach` where it is host-tested; this is only the
    // shelling-out. Mutation testing is what moved it: as an inline `match` here
    // the whole `NoSuchDevice` upgrade could be deleted with no test noticing.
    *unreachable = refine_with_holder(unreachable.clone(), pid);
}

