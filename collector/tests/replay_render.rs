//! End-to-end render check for the reader's "collector as terminal" render half
//! (Step 5, `plans/uart-telemetry.md`): a recorded wire stream, replayed through
//! the real `collector` binary in `--text` mode, prints `Frame::Log` lines
//! verbatim (like a console) and telemetry frames as `Debug`.

use std::io::Write;
use std::process::Command;

use protocol::{Frame, SpanId};

#[test]
fn reader_renders_log_frames_as_plain_lines() {
    // A recording: a Log the guest "printed", a telemetry frame between, another Log.
    let frames = [
        Frame::Hello { timebase_hz: 10_000_000, protocol_version: protocol::PROTOCOL_VERSION },
        Frame::Log { msg: "entering heartbeat", task_id: 0, t: 1, hart_id: 0 },
        Frame::SpanEnd { id: SpanId(7), t: 2 },
        Frame::Log { msg: "hb 1", task_id: 0, t: 3, hart_id: 0 },
    ];
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    let mut buf = [0u8; 128];
    for f in &frames {
        file.write_all(protocol::wire_encode(f, &mut buf).expect("encode")).expect("write");
    }
    file.flush().expect("flush");

    // Run the real binary, replaying the file, text output only.
    let out = Command::new(env!("CARGO_BIN_EXE_collector"))
        .args(["--replay", &file.path().to_string_lossy(), "--text", "--no-otlp", "--no-loki", "--no-prometheus"])
        .output()
        .expect("run collector");
    assert!(out.status.success(), "collector exited non-zero: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");

    // The two Log frames render as their bare message lines...
    assert!(stdout.contains("\nentering heartbeat\n") || stdout.starts_with("entering heartbeat\n"), "{stdout:?}");
    assert!(stdout.contains("\nhb 1\n"), "{stdout:?}");
    // ...not as a `Log { … }` struct dump.
    assert!(!stdout.contains("Log {"), "a Log frame leaked its Debug form: {stdout:?}");
    // The telemetry frame is still shown (as Debug), so nothing is silently dropped.
    assert!(stdout.contains("SpanEnd"), "the telemetry frame should still print: {stdout:?}");
}
