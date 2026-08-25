//! collector: connects to the kernel's telemetry socket, decodes
//! `Frame`s from the byte stream, and routes them to one or more
//! output sinks (stdout / OTLP / Prometheus).
//!
//! Four sinks, all live: stdout (`--text`), OTLP/HTTP traces to Tempo,
//! Loki log push, and a Prometheus `/metrics` endpoint. OTLP, Loki, and
//! Prometheus are on by default; each has a `--no-*` flag to disable it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{ArgGroup, Parser};
use protocol::Frame;

use collector::source::{SerialConfig, Source, SourceSelection, run_source};
use collector::{SpanExporter, loki, otlp, prom, state};

const SOCKET_PATH: &str = "/tmp/snitch-telemetry.sock";

/// Connect to the kernel's telemetry socket, decode `Frame`s, and route
/// them to the configured output sinks. OTLP export is on by default
/// (pointing at the docker-compose Tempo instance); disable with
/// `--no-otlp`. Prometheus exposition is also on by default (serving on
/// `--prometheus`'s port); disable with `--no-prometheus`.
#[derive(Parser)]
#[command(about, version)]
// Exactly one source, or none (the default socket). Silent precedence between
// transports would let `--replay x --udp 9000` discard one without saying so; the
// group makes that a usage error naming both. `--serial` joins this list in Step 10b.
#[command(group(ArgGroup::new("source").multiple(false).args(["replay", "udp", "serial"])))]
struct Args {
    /// Print decoded frames to stdout in addition to other outputs.
    #[arg(long)]
    text: bool,

    /// Use multi-line Debug format when `--text` is enabled.
    #[arg(long)]
    pretty: bool,

    /// OTLP/HTTP endpoint for trace export. Default matches the
    /// docker-compose Tempo instance.
    #[arg(long, default_value = "http://localhost:4318")]
    otlp: String,

    /// Disable OTLP export (e.g. for the `reader` xtask shortcut).
    #[arg(long)]
    no_otlp: bool,

    /// Loki HTTP endpoint for log push. Default matches the
    /// docker-compose Loki instance.
    #[arg(long, default_value = "http://localhost:3100")]
    loki: String,

    /// Disable Loki log push.
    #[arg(long)]
    no_loki: bool,

    /// TCP port to serve Prometheus `/metrics` on. Default matches
    /// the docker-compose Prometheus scrape config.
    #[arg(long, default_value_t = 9091)]
    prometheus: u16,

    /// Disable the Prometheus /metrics endpoint.
    #[arg(long)]
    no_prometheus: bool,

    /// Replay a recorded wire stream from a file instead of connecting to the
    /// live socket. Decodes with resync tolerance, so a recording of a lossy
    /// serial capture still replays. Pairs with `--text` to inspect a boot with
    /// no board or QEMU attached.
    #[arg(long, value_name = "FILE")]
    replay: Option<PathBuf>,

    /// Listen for telemetry datagrams on this UDP port instead of the live
    /// socket — the `net=` transport (M2.5). Each datagram is a COBS batch;
    /// decode is resync-tolerant, since UDP may drop or reorder datagrams.
    #[arg(long, value_name = "PORT")]
    udp: Option<u16>,

    /// Read the board's telemetry from a serial line — the VisionFive 2's UART
    /// (M2). On macOS this must be a **call-out** (`/dev/cu.*`) device: opening
    /// the `tty.*` node blocks until carrier detect, which a USB-TTL adapter
    /// never asserts. Decode is resync-tolerant, since a physical line drops
    /// bytes for reasons no software prevents.
    #[arg(long, value_name = "DEVICE")]
    serial: Option<String>,

    /// Line rate for `--serial`. The default is the measured choice: steady-state
    /// telemetry runs ~5.5 KB/s against 115200's ~11.5 KB/s (B3 Step 6).
    #[arg(long, default_value_t = 115_200, requires = "serial")]
    baud: u32,
}

#[cfg_attr(test, mutants::skip)] // I/O entry point — not unit-testable
fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let mut exporters: Vec<Box<dyn SpanExporter>> = Vec::new();
    if !args.no_otlp {
        exporters.push(Box::new(otlp::Exporter::new(&args.otlp)));
    }
    if !args.no_loki {
        exporters.push(Box::new(loki::Exporter::new(&args.loki)));
    }
    let state = Arc::new(Mutex::new(state::State::new(state::SystemWallClock)));

    if !args.no_prometheus {
        prom::serve(state.clone(), args.prometheus)?;
    }

    let source = Source::resolve(SourceSelection {
        replay: args.replay.clone(),
        udp: args.udp,
        serial: args
            .serial
            .clone()
            .map(|device| SerialConfig { device, baud: args.baud }),
        socket: PathBuf::from(SOCKET_PATH),
    });
    eprintln!("collector: source = {}", source.describe());
    if !args.no_otlp {
        eprintln!("collector: exporting OTLP traces to {}", &args.otlp);
        eprintln!("collector: view traces at http://localhost:3000 (Grafana → Explore → Tempo)");
    }
    if !args.no_loki {
        eprintln!("collector: pushing logs to {}", &args.loki);
    }
    if !args.no_prometheus {
        eprintln!("collector: serving Prometheus /metrics on :{}", args.prometheus);
    }
    if args.text {
        eprintln!("collector: text output enabled");
    }

    run_source(&source, |frame| {
        if args.text {
            print_frame(frame, args.pretty);
        }
        let mut state = state.lock().unwrap();
        // Flush open cap holdings before the session anchor resets.
        if matches!(frame, Frame::Hello { .. }) {
            for cap_span in state.flush_caps() {
                for exporter in &exporters {
                    exporter.export(&cap_span);
                }
            }
        }
        if let Some(completed) = state.handle(frame) {
            for exporter in &exporters {
                exporter.export(&completed);
            }
        }
        for cap_span in state.drain_cap_spans() {
            for exporter in &exporters {
                exporter.export(&cap_span);
            }
        }
    })?;

    // Flush any still-open cap holdings at stream EOF.
    {
        let mut state = state.lock().unwrap();
        for cap_span in state.flush_caps() {
            for exporter in &exporters {
                exporter.export(&cap_span);
            }
        }
    }

    match args.replay {
        Some(_) => eprintln!("collector: replay complete"),
        None => eprintln!("kernel disconnected; restart with `cargo xtask collect`"),
    }
    Ok(())
}

/// Print a decoded frame to stdout. Uses the derived `Debug` impl; with
/// `pretty=true`, multi-line pretty format for easier inspection.
///
/// A `Frame::Log` is the guest's own console line — printed verbatim (via
/// [`collector::log_text`]) so `console=frames` output reads like a console, not
/// a struct dump. Telemetry frames still show `Debug`.
#[cfg_attr(test, mutants::skip)] // pure stdout I/O — behaviour verified by running the binary
fn print_frame(frame: &Frame<'_>, pretty: bool) {
    if let Some(line) = collector::log_text(frame) {
        println!("{line}");
    } else if pretty {
        println!("{frame:#?}");
    } else {
        println!("{frame:?}");
    }
}

// Stream-decoding tests moved with the impl to `protocol::stream`.

/// The collector reads from exactly **one** source, and says so when asked for two.
///
/// Before `--serial` joins them (Step 10b of `plans/uart-telemetry.md`), resolving
/// three live transports by silent precedence is a footgun: `--replay boot.bin --udp
/// 9000` would quietly pick one and discard the other, and the operator's first clue
/// would be telemetry arriving from somewhere they did not ask for. A usage error
/// naming both flags is the honest answer.
#[cfg(test)]
mod cli_tests {
    use super::Args;
    use clap::Parser;

    #[test]
    fn two_sources_at_once_is_a_usage_error() {
        let Err(err) = Args::try_parse_from(["collector", "--replay", "/rec.bin", "--udp", "9000"])
        else {
            panic!("--replay with --udp must be refused, not silently resolved by precedence");
        };
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "wrong error kind — the operator needs to be told the flags conflict: {err}"
        );
    }

    #[test]
    fn each_source_alone_still_parses() {
        // The conflict must not over-fire. Absence of any source flag is also valid:
        // that is the live socket, the default.
        Args::try_parse_from(["collector", "--replay", "/rec.bin"])
            .expect("--replay alone is a valid source");
        Args::try_parse_from(["collector", "--udp", "9000"]).expect("--udp alone is a valid source");
        Args::try_parse_from(["collector"]).expect("no source flag means the default socket");
    }
}
