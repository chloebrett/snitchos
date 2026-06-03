//! Test harness: spawns QEMU, reads the virtio-console socket on a
//! reader thread, decodes frames, and surfaces them to the main
//! (assertion) thread via a channel.
//!
//! Lifecycle: `Harness::spawn` returns a live handle. `wait_for` blocks
//! up to a per-call wallclock budget for a frame matching a predicate.
//! `Drop` always kills QEMU and removes the socket, so a panicking
//! test still cleans up.

use std::collections::{HashMap, VecDeque};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use protocol::stream::{OwnedFrame, decode_stream};
use protocol::StringId;

use crate::qemu;

/// Maps `StringId` → name as we observe `StringRegister` frames. Matchers
/// read this so they can say "is this span 'kernel.boot'?" without
/// hard-coding ids.
pub type StringTable = HashMap<StringId, String>;

/// Live integration-test handle. Killing the child and unlinking the
/// socket happens in `Drop`.
pub struct Harness {
    qemu: Child,
    rx: Receiver<OwnedFrame>,
    strings: StringTable,
    socket_path: PathBuf,
    timebase_hz: Option<u64>,
    /// Rolling window of the last few frames received. Printed on
    /// timeout so failures say "boot reached Hello + SpanStart, then
    /// nothing" rather than just "no heartbeat within 10s".
    recent: VecDeque<OwnedFrame>,
}

impl Harness {
    /// Build the kernel (if needed), spawn QEMU pointing at a fresh
    /// per-test socket, accept the connection, and start the reader
    /// thread.
    pub fn spawn(label: &str) -> Result<Self, String> {
        Self::spawn_with_features(label, &[])
    }

    /// Like `spawn`, but builds the kernel with the given cargo
    /// features enabled. Used by scenarios that need a non-default
    /// kernel variant — currently just `frame-allocator-oom`, which
    /// opts in to the `oom-leak` feature.
    pub fn spawn_with_features(label: &str, features: &[&str]) -> Result<Self, String> {
        build_kernel(features)?;

        let socket_path = socket_path_for(label);
        let _ = std::fs::remove_file(&socket_path);

        // QEMU is the listener (server=on,wait=on); we connect to it
        // as a client. Matches the `cargo xtask boot` setup the collector
        // already uses — so we exercise the same wire path.
        let chardev = format!(
            "socket,path={},server=on,wait=on,id=telemetry",
            socket_path.display(),
        );

        let qemu = qemu::base_command(&chardev)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn qemu: {e}"))?;

        // Wait for QEMU to create the socket, then connect.
        let stream = connect_with_deadline(&socket_path, Duration::from_secs(10))?;

        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut stream = stream;
            let _ = decode_stream(&mut stream, |frame| {
                let _ = tx.send(OwnedFrame::from_borrowed(frame));
            });
        });

        Ok(Self {
            qemu,
            rx,
            strings: HashMap::new(),
            socket_path,
            timebase_hz: None,
            recent: VecDeque::new(),
        })
    }

    /// Block up to `budget` for a frame matching `pred`. Returns the
    /// matching frame, or `None` on deadline. Every frame consumed
    /// along the way updates the internal string table — later
    /// matchers can resolve any `StringId` seen so far.
    pub fn wait_for(
        &mut self,
        budget: Duration,
        pred: impl Fn(&OwnedFrame, &StringTable) -> bool,
    ) -> Option<OwnedFrame> {
        let deadline = Instant::now() + budget;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.rx.recv_timeout(remaining) {
                Ok(frame) => {
                    self.absorb(&frame);
                    if pred(&frame, &self.strings) {
                        return Some(frame);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.dump_recent("timeout");
                    return None;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.dump_recent("QEMU disconnected");
                    return None;
                }
            }
        }
    }

    /// Look up a name in the string table. Useful for matchers that
    /// want to assert "this SpanStart's name_id resolves to ...".
    pub fn name_of(&self, id: StringId) -> Option<&str> {
        self.strings.get(&id).map(String::as_str)
    }

    /// Timebase frequency from the most recent `Hello` frame, or `None`
    /// if `Hello` has not arrived yet. Use this to convert kernel tick
    /// deltas to wall-clock durations inside scenarios.
    pub fn timebase_hz(&self) -> Option<u64> {
        self.timebase_hz
    }

    fn absorb(&mut self, frame: &OwnedFrame) {
        match frame {
            OwnedFrame::StringRegister { id, value } => {
                self.strings.insert(*id, value.clone());
            }
            OwnedFrame::Hello { timebase_hz, .. } => {
                self.timebase_hz = Some(*timebase_hz);
            }
            _ => {}
        }
        if self.recent.len() >= 8 {
            self.recent.pop_front();
        }
        self.recent.push_back(frame.clone());
    }

    fn dump_recent(&self, reason: &str) {
        if self.recent.is_empty() {
            eprintln!("  [{reason}: no frames arrived]");
            return;
        }
        eprintln!("  [{reason}: last {} frame(s) seen]", self.recent.len());
        for frame in &self.recent {
            eprintln!("    {}", self.describe(frame));
        }
    }

    fn describe(&self, frame: &OwnedFrame) -> String {
        match frame {
            OwnedFrame::Hello { timebase_hz, protocol_version } =>
                format!("Hello {{ timebase_hz={timebase_hz}, protocol_version={protocol_version} }}"),
            OwnedFrame::StringRegister { id, value } =>
                format!("StringRegister {{ {id:?} = {value:?} }}"),
            OwnedFrame::MetricRegister { name_id, kind } => {
                let name = self.strings.get(name_id).map(String::as_str).unwrap_or("?");
                format!("MetricRegister {{ {name:?} kind={kind:?} }}")
            }
            OwnedFrame::SpanStart { id, parent, name_id, t } => {
                let name = self.strings.get(name_id).map(String::as_str).unwrap_or("?");
                format!("SpanStart {{ {name:?} id={id:?} parent={parent:?} t={t} }}")
            }
            OwnedFrame::SpanEnd { id, t } =>
                format!("SpanEnd {{ id={id:?} t={t} }}"),
            OwnedFrame::Event { span_id, name_id, t } => {
                let name = self.strings.get(name_id).map(String::as_str).unwrap_or("?");
                format!("Event {{ {name:?} span={span_id:?} t={t} }}")
            }
            OwnedFrame::Metric { name_id, value, t } => {
                let name = self.strings.get(name_id).map(String::as_str).unwrap_or("?");
                format!("Metric {{ {name:?} value={value} t={t} }}")
            }
            OwnedFrame::Dropped { count } =>
                format!("Dropped {{ count={count} }}"),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.qemu.kill();
        let _ = self.qemu.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn build_kernel(features: &[&str]) -> Result<(), String> {
    let status = qemu::build_kernel(features).map_err(|e| format!("build kernel: {e}"))?;
    if !status.success() {
        return Err("kernel build failed".to_string());
    }
    Ok(())
}

fn socket_path_for(label: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/snitch-itest-{}-{}.sock", label, std::process::id()))
}

fn connect_with_deadline(
    path: &std::path::Path,
    budget: Duration,
) -> Result<UnixStream, String> {
    let deadline = Instant::now() + budget;
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("connect {}: {e}", path.display())),
        }
    }
}
