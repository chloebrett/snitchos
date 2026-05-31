//! Prometheus `/metrics` endpoint.
//!
//! Tiny HTTP server (one thread, blocking) that serves the current
//! state's metric values in Prometheus text exposition format. Scraped
//! by the docker-compose Prometheus instance every 5 seconds.

use std::sync::{Arc, Mutex};
use std::thread;

use protocol::MetricKind;

use crate::state::State;

/// Spawn the metrics server on the given port. Runs until the process
/// exits. Errors during request handling are logged to stderr but don't
/// take the server down.
pub fn serve(state: Arc<Mutex<State>>, port: u16) -> std::io::Result<()> {
    let addr = format!("0.0.0.0:{port}");
    let server = tiny_http::Server::http(&addr).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("tiny_http bind: {e}"),
        )
    })?;

    thread::spawn(move || {
        for request in server.incoming_requests() {
            let response = match request.url() {
                "/metrics" => {
                    let body = {
                        let state = state.lock().unwrap();
                        format_metrics(&state)
                    };
                    tiny_http::Response::from_string(body)
                        .with_header(
                            "Content-Type: text/plain; version=0.0.4"
                                .parse::<tiny_http::Header>()
                                .unwrap(),
                        )
                }
                _ => tiny_http::Response::from_string("not found")
                    .with_status_code(404),
            };
            if let Err(e) = request.respond(response) {
                eprintln!("prom: respond failed: {e}");
            }
        }
    });

    Ok(())
}

/// Format `State`'s metric tables as Prometheus exposition text.
///
/// One metric family per registered name. Names like
/// `snitchos.heartbeat.count` become `snitchos_heartbeat_count` —
/// Prometheus forbids dots.
fn format_metrics(state: &State) -> String {
    let mut out = String::new();
    for (name_id, value) in state.metric_values.iter() {
        let Some(raw_name) = state.name(*name_id) else {
            continue;
        };
        let Some(kind) = state.metric_kind(*name_id) else {
            continue;
        };
        let prom_name = sanitize(raw_name);
        let kind_str = match kind {
            MetricKind::Counter => "counter",
            MetricKind::Gauge => "gauge",
            MetricKind::Histogram => "histogram",
        };
        out.push_str(&format!("# HELP {prom_name} {raw_name}\n"));
        out.push_str(&format!("# TYPE {prom_name} {kind_str}\n"));
        out.push_str(&format!("{prom_name} {value}\n"));
    }
    out
}

/// Replace any character not in `[a-zA-Z0-9_:]` with `_`. Required so
/// our dotted names like `snitchos.heartbeat.count` become valid
/// Prometheus identifiers.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_dots() {
        assert_eq!(sanitize("snitchos.heartbeat.count"), "snitchos_heartbeat_count");
    }

    #[test]
    fn sanitize_preserves_underscores_and_colons() {
        assert_eq!(sanitize("foo_bar:baz"), "foo_bar:baz");
    }

    #[test]
    fn sanitize_replaces_other_punctuation() {
        assert_eq!(sanitize("a-b/c d"), "a_b_c_d");
    }
}
