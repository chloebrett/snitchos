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
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use xtask_board::echo::visible;
use xtask_board::knock::{Answer, Knock};
use xtask_board::script;
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

    /// Catch the U-Boot prompt across a power cycle, run commands at it, then
    /// stream whatever the board does next — **all on one open port**.
    ///
    /// `exec` opens, writes once, captures, closes. That is the wrong shape for
    /// driving U-Boot, and each way it is wrong cost a boot on 2026-08-28:
    ///
    /// - **`bootdelay` is two seconds.** Reaching the prompt means already typing
    ///   when the window opens, so the knocking has to start before the board
    ///   does — which means holding the port across the power cycle.
    /// - **Releasing the port between commands loses the boot log.** The adapter
    ///   buffers it, and the next process to open reads a stale burst as though it
    ///   were live. One owner for the whole sequence, or the record lies.
    /// - **U-Boot's console has no flow control.** Commands go one at a time,
    ///   waiting for the prompt between them ([`xtask_board::script`]); a pasted
    ///   block drops characters, and a `setenv` that loses its first five is
    ///   accepted silently as a different variable.
    ///
    /// Exit codes as `exec`: `0` the sequence ran, `1` the board was reached but
    /// never gave the prompt (or a step's wait went unanswered), `2` never reached.
    Uboot {
        /// Serial device. A **call-out** node (`/dev/cu.*`) on macOS.
        #[arg(long)]
        device: String,

        /// Line rate.
        #[arg(long, default_value_t = 115_200)]
        baud: u32,

        /// A command to type at the prompt. Repeatable, run in order, each
        /// awaiting the prompt before the next is sent.
        #[arg(long = "cmd")]
        cmds: Vec<String>,

        /// How long to keep knocking for the prompt, in milliseconds. Generous by
        /// default: the whole point is to be armed *before* a human power-cycles.
        #[arg(long, default_value_t = 600_000)]
        catch: u64,

        /// How long to keep reading after the last command, in milliseconds —
        /// which is how a `run bootcmd` boot log gets captured.
        #[arg(long, default_value_t = 45_000)]
        stream: u64,

        /// The prompt to wait for.
        #[arg(long, default_value = "StarFive #")]
        prompt: String,
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
        Cmd::Uboot { device, baud, cmds, catch, stream, prompt } => uboot(&UbootRequest {
            device: &device,
            baud,
            cmds: &cmds,
            catch: Duration::from_millis(catch),
            stream: Duration::from_millis(stream),
            prompt: &prompt,
        }),
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

/// Everything `uboot` needs. Same config-struct reasoning as [`RebootRequest`]:
/// two of these are interchangeable `u64` durations.
struct UbootRequest<'a> {
    device: &'a str,
    baud: u32,
    cmds: &'a [String],
    catch: Duration,
    stream: Duration,
    prompt: &'a str,
}

/// How often to send a key while knocking. Comfortably inside `bootdelay`, and
/// slow enough that the echo does not swamp the line.
const KNOCK_INTERVAL: Duration = Duration::from_millis(50);

/// How often to send a bare carriage return and judge the reply. See
/// [`xtask_board::knock`] for why the question has to be re-asked rather than
/// answered from a rolling buffer.
const PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// Catch the prompt, run the commands, stream the aftermath — one port throughout.
fn uboot(req: &UbootRequest<'_>) -> ExitCode {
    if let Err(unreachable) = check_device(req.device) {
        report(&Outcome::Unreachable(unreachable));
        return ExitCode::from(2);
    }
    let mut port: Box<dyn ReadWrite> = match serialport::new(req.device, req.baud)
        .timeout(collector::serial::READ_TIMEOUT)
        .open()
    {
        Ok(port) => Box::new(port),
        Err(e) => {
            let mut unreachable = classify_open_failure(req.device, io_kind(e.kind));
            consult_lsof(&mut unreachable, req.device);
            report(&Outcome::Unreachable(unreachable));
            return ExitCode::from(2);
        }
    };

    match catch_prompt(&mut *port, req) {
        Err(code) => return code,
        Ok(()) => eprintln!("board: at the {} prompt", req.prompt.trim()),
    }

    // One line at a time, waiting for the prompt between them. `script::run`
    // abandons the rest if a step goes unanswered — a half-written environment is
    // worse than none, and that rule lives there rather than in this loop.
    let steps: Vec<script::Step> = req
        .cmds
        .iter()
        .map(|cmd| {
            script::Step::new(
                format!("{cmd}\r").into_bytes(),
                StopCondition::new(PROMPT_TIMEOUT).marker(req.prompt.as_bytes()),
            )
        })
        .collect();

    let mut failed = false;
    // Kept rather than discarded: `script::run`'s closure signals transport death
    // with `None`, which says *where* it died but not why. The kind is what
    // `classify_open_failure` turns into an action, so it must survive the trip.
    let mut transport_error: Option<std::io::ErrorKind> = None;
    let outcome = script::run(&steps, |step| {
        eprintln!("board: > {}", visible(step.send()));
        if let Err(e) = port.write_all(step.send()) {
            transport_error = Some(e.kind());
            return None;
        }
        let (ended, raw) = capture(&mut *port, step.until());
        emit(&raw);
        match ended {
            Ended::Stopped(reason) => Some(reason),
            Ended::TransportFailed(kind) => {
                transport_error = Some(kind);
                None
            }
        }
    });

    match &outcome {
        script::ScriptOutcome::Completed { .. } => {}
        script::ScriptOutcome::Abandoned { index, reason } => {
            failed = true;
            eprintln!(
                "board: no prompt back after {:?} ({reason:?}) — {} later command(s) unsent",
                req.cmds.get(*index).map_or("?", String::as_str),
                steps.len() - index - 1
            );
        }
        script::ScriptOutcome::Interrupted { index } => {
            eprintln!("board: transport died during command {index}");
            let kind = transport_error.unwrap_or(std::io::ErrorKind::Other);
            report(&Outcome::Unreachable(classify_open_failure(req.device, kind)));
            // Exit 2: the board was answering, we stopped being able to hear it.
            // Fix the host, do not power-cycle.
            return ExitCode::from(2);
        }
    }

    // Stream the aftermath. The last command is usually `run bootcmd`, whose
    // whole output arrives *after* it — and never returns to a prompt, so this is
    // a plain timed read rather than another step.
    if req.stream > Duration::ZERO {
        let (_, raw) = capture(&mut *port, &StopCondition::new(req.stream));
        emit(&raw);
    }

    ExitCode::from(u8::from(failed))
}

/// How long a single U-Boot command may take to give the prompt back. `saveenv`
/// writes SPI flash and `dhcp` waits on a link, so this is not instant.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Knock until the target prompt answers. `Err` carries the exit code.
fn catch_prompt(port: &mut dyn ReadWrite, req: &UbootRequest<'_>) -> Result<(), ExitCode> {
    let mut knock = Knock::new(req.prompt.as_bytes()).also(b"stitch>".as_slice(), "SnitchOS");
    let mut buf = [0u8; 1024];
    let started = Instant::now();
    let mut last_key = Instant::now();
    let mut announced_other = false;

    eprintln!(
        "board: knocking for {:?} — power-cycle the board now",
        req.catch
    );
    // Ask once up front rather than back-dating the clock: if the board is
    // already sitting at a prompt this answers immediately, and a `Duration`
    // subtracted from `Instant::now()` is a panic waiting for a slow clock.
    let _ = port.write_all(b"\r");
    knock.probe();
    let mut last_probe = Instant::now();
    while started.elapsed() < req.catch {
        match port.read(&mut buf) {
            Ok(0) => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Ok(n) => {
                // Tee the boot log as it happens: the bytes before the prompt are
                // the SPL banner and DDR training, which is exactly what you want
                // when the prompt never comes.
                let _ = std::io::stdout().write_all(&buf[..n]);
                let _ = std::io::stdout().flush();
                match knock.observe(&buf[..n]) {
                    Some(Answer::Target) => return Ok(()),
                    // Say it once. A board that autobooted past the window will
                    // answer every probe, and the operator needs the fact, not a
                    // line a second.
                    Some(Answer::Other(name)) if !announced_other => {
                        announced_other = true;
                        eprintln!(
                            "board: {name} answered — it autobooted past the window. \
                             Power-cycle while this is still knocking."
                        );
                    }
                    _ => {}
                }
            }
            Err(e) => {
                report(&Outcome::Unreachable(classify_open_failure(req.device, e.kind())));
                return Err(ExitCode::from(2));
            }
        }
        if last_key.elapsed() >= KNOCK_INTERVAL {
            let _ = port.write_all(b" ");
            last_key = Instant::now();
        }
        if last_probe.elapsed() >= PROBE_INTERVAL {
            let _ = port.write_all(b"\r");
            knock.probe();
            last_probe = Instant::now();
        }
    }
    eprintln!("board: never reached the {} prompt", req.prompt.trim());
    // Exit 1, not 2: the port opened and we listened: the board did not answer.
    // That is the same distinction `exec` draws, and an unattended loop acts on it
    // by power-cycling rather than by fixing the host.
    Err(ExitCode::from(1))
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

