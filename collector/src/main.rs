//! collector: connects to the kernel's telemetry socket, decodes
//! `Frame`s from the byte stream, and routes them to one or more
//! output sinks (stdout / OTLP / Prometheus).
//!
//! v0.2 scope: `--text` works; `--otlp` and `--prometheus` are stubs
//! that print "not yet implemented" — wired up in later steps.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use clap::Parser;
use protocol::Frame;
use protocol::stream::decode_stream;

mod otlp;
mod prom;
mod state;

/// Sink for completed spans. Implement this to add a new output format.
/// Each implementation receives every `CompletedSpan` produced by the
/// kernel session; routing (enable/disable, endpoint config) is the
/// caller's responsibility.
pub trait SpanExporter: Send {
    fn export(&self, span: &state::CompletedSpan);
}

const SOCKET_PATH: &str = "/tmp/snitch-telemetry.sock";

/// Connect to the kernel's telemetry socket, decode `Frame`s, and route
/// them to the configured output sinks. OTLP export is on by default
/// (pointing at the docker-compose Tempo instance); disable with
/// `--no-otlp`. Prometheus exposition is off by default until v0.2
/// step 7 is implemented.
#[derive(Parser)]
#[command(about, version)]
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

    /// TCP port to serve Prometheus `/metrics` on. Default matches
    /// the docker-compose Prometheus scrape config.
    #[arg(long, default_value_t = 9091)]
    prometheus: u16,

    /// Disable the Prometheus /metrics endpoint.
    #[arg(long)]
    no_prometheus: bool,
}

#[cfg_attr(test, mutants::skip)] // I/O entry point — not unit-testable
fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let mut exporters: Vec<Box<dyn SpanExporter>> = Vec::new();
    if !args.no_otlp {
        exporters.push(Box::new(otlp::Exporter::new(&args.otlp)));
    }
    let state = Arc::new(Mutex::new(state::State::new(state::SystemWallClock)));

    if !args.no_prometheus {
        prom::serve(state.clone(), args.prometheus)?;
    }

    eprintln!("collector: connecting to {SOCKET_PATH}");
    if !args.no_otlp {
        eprintln!("collector: exporting OTLP traces to {}", &args.otlp);
        eprintln!("collector: view traces at http://localhost:3000 (Grafana → Explore → Tempo)");
    }
    if !args.no_prometheus {
        eprintln!("collector: serving Prometheus /metrics on :{}", args.prometheus);
    }
    if args.text {
        eprintln!("collector: text output enabled");
    }

    let mut stream = UnixStream::connect(SOCKET_PATH)?;
    eprintln!("collector: connected; waiting for frames");
    decode_stream(&mut stream, |frame| {
        if args.text {
            print_frame(frame, args.pretty);
        }
        let mut state = state.lock().unwrap();
        if let Some(completed) = state.handle(frame) {
            for exporter in &exporters {
                exporter.export(&completed);
            }
        }
    })?;

    eprintln!("kernel disconnected; restart with `cargo xtask collect`");
    Ok(())
}

/// Print a decoded frame to stdout. Uses the derived `Debug` impl; with
/// `pretty=true`, multi-line pretty format for easier inspection.
#[cfg_attr(test, mutants::skip)] // pure stdout I/O — behaviour verified by running the binary
fn print_frame(frame: &Frame<'_>, pretty: bool) {
    if pretty {
        println!("{frame:#?}");
    } else {
        println!("{frame:?}");
    }
}

// Stream-decoding tests moved with the impl to `protocol::stream`.
