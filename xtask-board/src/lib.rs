//! `cargo xtask board` — driving the VisionFive 2 from the host.
//!
//! See [plans/board-bridge.md](../../plans/board-bridge.md). This crate is the
//! serial-linked half; lean `xtask` forwards to it.

pub mod reach;
pub mod split;
pub mod stop;

// # Releasing the port
//
// A serial port is exclusive: whatever holds it blocks every other opener, so a
// bridge that exits without releasing turns its own death into the next run's
// mystery — the port reports busy and `lsof` names a process that is gone. That
// is a self-inflicted instance of exactly what [`reach`] exists to explain.
//
// **Ownership already handles the panic case.** `serialport`'s handle closes its
// fd on drop, and Rust runs destructors while unwinding, so a panicking bridge
// releases the port for free. A guard type wrapping that would be ceremony — it
// was written and deleted here rather than shipped.
//
// **The case ownership does *not* handle is `std::process::exit`, which runs no
// destructors.** That is not hypothetical: `xtask-itest/src/itest.rs` already
// installs a Ctrl-C handler that calls `std::process::exit(130)`, and Ctrl-C is
// precisely how a person ends an interactive bridge session. So the rule for the
// CLI (step 4) is: **return an `ExitCode`; never `process::exit` while the port
// is open**, and if a signal handler is needed, it must release first.

#[cfg(test)]
mod reach_tests {
    use super::reach::{Unreachable, check_device, classify_open_failure, holder_pid};
    use std::io::ErrorKind;

    /// The call-in check happens **before** the open, not as a classification of
    /// one — because the failure mode it prevents is an *infinite block*, not an
    /// error. There is nothing to classify if the call never returns.
    ///
    /// Reuses `collector::serial::call_out_alternative` rather than restating the
    /// rule: the collector's `--serial` source already had to learn it, and two
    /// copies of a platform quirk drift.
    #[test]
    fn a_call_in_device_is_refused_before_it_can_block() {
        let e = check_device("/dev/tty.usbserial-A50285BI")
            .expect_err("a tty.* path must be refused, not opened");
        assert_eq!(
            e,
            Unreachable::CallInDevice {
                device: "/dev/tty.usbserial-A50285BI".into(),
                use_instead: "/dev/cu.usbserial-A50285BI".into(),
            },
            "the error must carry the path to use — naming the fix is the point"
        );
    }

    #[test]
    fn a_call_out_device_passes_the_check() {
        assert_eq!(check_device("/dev/cu.usbserial-A50285BI"), Ok(()));
        assert_eq!(check_device("/dev/ttyUSB0"), Ok(()), "Linux nodes must pass");
    }

    /// The bridge's defining distinction: **"could not reach the board" must never
    /// look like "reached it, and it said nothing."**
    ///
    /// On hardware those have identical downstream symptoms — no frames — and
    /// completely different fixes. Every variant here answers "we never got a byte
    /// stream", and each one exists because it demands a *different* action from
    /// whoever is at the desk. A single `OpenFailed` catch-all would be honest
    /// about the failure and useless about the remedy.
    #[test]
    fn a_busy_port_is_reported_as_held_not_as_a_generic_failure() {
        let e = classify_open_failure("/dev/cu.usbserial-1", ErrorKind::ResourceBusy);
        assert_eq!(
            e,
            Unreachable::PortHeld { device: "/dev/cu.usbserial-1".into(), holder: None },
            "a leftover `screen` is the single most common cause; it needs its own variant"
        );
    }

    /// Distinct from `PortHeld`: nothing is holding the port, the user simply
    /// cannot open it. The fix is permissions, not killing a process.
    #[test]
    fn permission_denied_is_not_confused_with_a_held_port() {
        let e = classify_open_failure("/dev/cu.usbserial-1", ErrorKind::PermissionDenied);
        assert_eq!(e, Unreachable::NoPermission { device: "/dev/cu.usbserial-1".into() });
    }

    /// The other everyday one: the adapter is unplugged, or the path is a typo.
    /// "Is it plugged in?" is a different question from "what is holding it?".
    #[test]
    fn a_missing_device_says_so() {
        let e = classify_open_failure("/dev/cu.nope", ErrorKind::NotFound);
        assert_eq!(e, Unreachable::NoSuchDevice { device: "/dev/cu.nope".into() });
    }

    /// Anything unclassified still carries its kind, so an unfamiliar failure is
    /// reported rather than flattened into one of the above.
    #[test]
    fn an_unrecognised_failure_keeps_its_kind() {
        let e = classify_open_failure("/dev/cu.usbserial-1", ErrorKind::BrokenPipe);
        assert_eq!(
            e,
            Unreachable::OpenFailed {
                device: "/dev/cu.usbserial-1".into(),
                kind: ErrorKind::BrokenPipe
            }
        );
    }

    /// `lsof` on the device node names the process holding it. Parsing its first
    /// data row turns "the port is busy" into "wezterm (pid 4242) has it", which
    /// is the difference between a puzzle and a `kill`.
    #[test]
    fn the_holder_pid_is_read_from_lsof_output() {
        let out = "COMMAND   PID  USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\n\
                   screen  12345 chloe    5u   CHR   18,3      0t0  456 /dev/cu.usbserial-1\n";
        assert_eq!(holder_pid(out), Some(12345));
    }

    /// `lsof` prints nothing (not an error) when no process holds the file, so
    /// "no output" must mean "nobody", never a parse failure.
    #[test]
    fn no_lsof_output_means_no_identifiable_holder() {
        assert_eq!(holder_pid(""), None);
        assert_eq!(holder_pid("COMMAND   PID  USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\n"), None);
    }

    /// A malformed row must not panic the bridge. `lsof` is an external tool whose
    /// output we do not control, and a diagnostic path that crashes is worse than
    /// one that shrugs.
    #[test]
    fn a_malformed_lsof_row_yields_no_holder_rather_than_panicking() {
        assert_eq!(holder_pid("COMMAND PID\nscreen not-a-number\n"), None);
        assert_eq!(holder_pid("COMMAND PID\nscreen\n"), None);
    }
}

#[cfg(test)]
mod stop_tests {
    use super::stop::{Capture, StopCondition, StopReason};
    use std::time::Duration;

    /// Elapsed-since-capture-start, the only clock this layer knows.
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Every capture has a deadline; the other two conditions are optional. That
    /// asymmetry is deliberate — it makes "a capture that never returns"
    /// unrepresentable, which is the property an *unattended* bridge needs most.
    fn until(marker: &[u8]) -> StopCondition {
        StopCondition::new(ms(10_000)).marker(marker)
    }

    #[test]
    fn a_marker_fires_on_the_first_match() {
        let mut c = Capture::new(until(b"=> "));
        assert_eq!(c.observe(ms(10), b"Hit any key to stop autoboot\r\n"), None);
        assert_eq!(c.observe(ms(20), b"=> "), Some(StopReason::Marker));
    }

    /// The straddle case, and the reason this evaluator holds any state at all: a
    /// serial read boundary falls wherever the OS says it does, so a marker split
    /// across two reads is the *normal* case, not an adversarial one. Missing it
    /// leaves step 4b sitting at the U-Boot prompt believing it never arrived.
    #[test]
    fn a_marker_split_across_two_arrivals_still_fires() {
        let mut c = Capture::new(until(b"=> "));
        assert_eq!(c.observe(ms(10), b"boot\r\n="), None);
        assert_eq!(c.observe(ms(20), b"> "), Some(StopReason::Marker));
    }

    /// The degenerate end of the same case — one byte per read, which a slow or
    /// congested link really does produce.
    #[test]
    fn a_marker_delivered_one_byte_at_a_time_still_fires() {
        let mut c = Capture::new(until(b"=> "));
        assert_eq!(c.observe(ms(10), b"="), None);
        assert_eq!(c.observe(ms(11), b">"), None);
        assert_eq!(c.observe(ms(12), b" "), Some(StopReason::Marker));
    }

    /// Substring semantics would match the empty marker at offset zero and stop
    /// every capture on its first byte. "Never fires" is the safe reading; step
    /// 4's CLI rejects an empty `--until` at the surface rather than relying on it.
    #[test]
    fn an_empty_marker_never_fires() {
        let mut c = Capture::new(StopCondition::new(ms(100)).marker(b""));
        assert_eq!(c.observe(ms(10), b"anything at all"), None);
        assert_eq!(c.tick(ms(99)), None);
    }

    /// The wrong-moment hazard, stated as behaviour: a 300 ms window must not cut
    /// off a board that takes 800 ms to begin answering. Quiescence means "it
    /// spoke and then stopped", so it cannot be armed before the first byte.
    #[test]
    fn quiescence_is_armed_by_the_first_byte_not_by_capture_start() {
        let mut c = Capture::new(StopCondition::new(ms(10_000)).quiet_after(ms(300)));
        assert_eq!(c.tick(ms(800)), None, "silence before the first byte is not quiescence");
        assert_eq!(c.observe(ms(800), b"first output"), None);
        assert_eq!(c.tick(ms(1099)), None);
        assert_eq!(c.tick(ms(1100)), Some(StopReason::Quiet));
    }

    /// A read that returned zero bytes is a timer tick, not new data. Getting this
    /// wrong is subtle and total: a polling capture loop would refresh the quiet
    /// clock on every empty read, and quiescence would never fire at all.
    #[test]
    fn a_zero_byte_read_does_not_refresh_the_quiet_clock() {
        let mut c = Capture::new(StopCondition::new(ms(10_000)).quiet_after(ms(300)));
        assert_eq!(c.observe(ms(100), b"output"), None);
        assert_eq!(c.observe(ms(200), b""), None);
        assert_eq!(c.observe(ms(400), b""), Some(StopReason::Quiet));
    }

    #[test]
    fn a_stream_still_producing_never_goes_quiet() {
        let mut c = Capture::new(StopCondition::new(ms(10_000)).quiet_after(ms(300)));
        for tenth_second in 1..50u64 {
            assert_eq!(
                c.observe(ms(tenth_second * 100), b"chatter\r\n"),
                None,
                "bytes every 100 ms can never satisfy a 300 ms window"
            );
        }
    }

    /// ⚠ The condition that does not survive the move to Wi-Fi. Over a UART,
    /// silence means the board is silent; over Phase 2's TCP transport it may be a
    /// few-hundred-millisecond stall in the radio. A window that false-fires on
    /// that reports a wedged board that is fine — the exact failure this tool
    /// exists to prevent. Encoded now, while it is free.
    #[test]
    fn a_stall_shorter_than_the_window_does_not_stop_the_capture() {
        let mut c = Capture::new(StopCondition::new(ms(10_000)).quiet_after(ms(500)));
        assert_eq!(c.observe(ms(100), b"before the stall"), None);
        assert_eq!(c.tick(ms(400)), None, "a 300 ms stall is not a 500 ms silence");
        assert_eq!(c.observe(ms(599), b"after the stall"), None);
        assert_eq!(c.tick(ms(1000)), None, "the window restarts from the later bytes");
    }

    #[test]
    fn quiescence_fires_at_the_window_boundary_exactly() {
        let mut c = Capture::new(StopCondition::new(ms(10_000)).quiet_after(ms(300)));
        assert_eq!(c.observe(ms(100), b"output"), None);
        assert_eq!(c.tick(ms(399)), None, "one millisecond short of the window must not fire");
        assert_eq!(c.tick(ms(400)), Some(StopReason::Quiet), "the window is inclusive");
    }

    /// The answer to "the board said nothing at all", and why the timeout is
    /// mandatory: with quiescence armed only by a first byte that never came, this
    /// is the only condition left, and it reports as itself.
    #[test]
    fn a_silent_board_stops_at_the_timeout_not_at_quiescence() {
        let mut c = Capture::new(StopCondition::new(ms(5_000)).quiet_after(ms(300)));
        assert_eq!(c.tick(ms(4_999)), None);
        assert_eq!(c.tick(ms(5_000)), Some(StopReason::Timeout));
    }

    #[test]
    fn the_timeout_fires_at_its_boundary_exactly() {
        let mut c = Capture::new(StopCondition::new(ms(1_000)));
        assert_eq!(c.tick(ms(999)), None, "one millisecond short of the deadline must not fire");
        assert_eq!(c.tick(ms(1_000)), Some(StopReason::Timeout), "the deadline is inclusive");
    }

    /// The timeout is an outer bound on the whole capture, not a silence detector,
    /// so a stream that never stops talking must still hit it.
    #[test]
    fn the_timeout_fires_on_a_stream_still_producing() {
        let mut c = Capture::new(StopCondition::new(ms(500)));
        assert_eq!(c.observe(ms(200), b"chatter"), None);
        assert_eq!(c.observe(ms(500), b"more chatter"), Some(StopReason::Timeout));
    }

    /// Precedence, when two conditions come due on the same event. A marker found
    /// in bytes that genuinely arrived within the capture is a *success*;
    /// reporting it as a timeout would make a working round-trip look like a
    /// wedged board.
    #[test]
    fn a_marker_wins_over_a_timeout_due_on_the_same_event() {
        let mut c = Capture::new(StopCondition::new(ms(100)).marker(b"OK"));
        assert_eq!(c.observe(ms(100), b"OK"), Some(StopReason::Marker));
    }

    /// The other collision, and not a degenerate one: quiescence came due at 400
    /// ms but nobody polled until 1000 ms, by which point the deadline had passed
    /// too. The outer bound is the more informative answer.
    #[test]
    fn a_timeout_wins_over_quiescence_when_both_are_due() {
        let mut c = Capture::new(StopCondition::new(ms(1_000)).quiet_after(ms(300)));
        assert_eq!(c.observe(ms(100), b"output"), None);
        assert_eq!(c.tick(ms(1_000)), Some(StopReason::Timeout));
    }

    /// The three conditions are one composed value, not three call sites choosing
    /// between them: the same `StopCondition` stops on whichever comes first, and
    /// the reason says which. That is what lets step 4 pass a single condition
    /// down and still tell a prompt from a wedge.
    #[test]
    fn one_condition_value_stops_on_whichever_fires_first() {
        let all = || StopCondition::new(ms(5_000)).quiet_after(ms(300)).marker(b"=> ");

        let mut on_marker = Capture::new(all());
        assert_eq!(on_marker.observe(ms(10), b"=> "), Some(StopReason::Marker));

        let mut on_quiet = Capture::new(all());
        assert_eq!(on_quiet.observe(ms(10), b"some output"), None);
        assert_eq!(on_quiet.tick(ms(400)), Some(StopReason::Quiet));

        let mut on_timeout = Capture::new(all());
        assert_eq!(on_timeout.tick(ms(5_000)), Some(StopReason::Timeout));
    }

    /// The memory bound, made checkable rather than merely asserted in a comment.
    /// A capture may run for minutes over a serial line; it must hold straddle
    /// context, not history. `marker.len() - 1` is the most that can ever be
    /// needed — a full match spanning more than that would already have fired.
    ///
    /// Asserted as an **equality**, not a ceiling. A ceiling is satisfied by
    /// retaining nothing at all, which is a different bug wearing the same number:
    /// it costs no memory and silently breaks every split match. Mutation testing
    /// found exactly that — a `retained_bytes -> 0` mutant survived the `<` form.
    #[test]
    fn exactly_the_straddle_context_is_retained_never_more_and_never_less() {
        let straddle = b"=> ".len() - 1;
        let mut c = Capture::new(until(b"=> "));
        assert_eq!(c.retained_bytes(), 0, "nothing is retained before the first read");
        for tick in 1..200u64 {
            assert_eq!(c.observe(ms(tick), b"a kilobyte of boot log would go here"), None);
            assert_eq!(
                c.retained_bytes(),
                straddle,
                "retention is pinned to the marker, neither growing with the stream \
                 nor dropping the context a split match needs"
            );
        }
    }

    /// The bound is the marker's, so a longer marker holds more — the same
    /// property stated where a hardcoded constant could not be mistaken for it.
    #[test]
    fn a_longer_marker_retains_proportionally_more_context() {
        let marker = b"Hit any key to stop autoboot";
        let mut c = Capture::new(until(marker));
        assert_eq!(c.observe(ms(10), b"U-Boot SPL 2021.10\r\n"), None);
        assert_eq!(c.retained_bytes(), b"U-Boot SPL 2021.10\r\n".len());
        assert_eq!(c.observe(ms(20), &vec![b'x'; 4096]), None);
        assert_eq!(c.retained_bytes(), marker.len() - 1);
    }

    #[test]
    fn a_capture_with_no_marker_retains_nothing() {
        let mut c = Capture::new(StopCondition::new(ms(10_000)).quiet_after(ms(300)));
        assert_eq!(c.observe(ms(10), b"plenty of output to not remember"), None);
        assert_eq!(c.retained_bytes(), 0);
    }
}

#[cfg(test)]
mod split_tests {
    use super::split::split;
    use protocol::stream::OwnedFrame;
    use protocol::{CapEventKind, CapObject, Frame, SpanId, StringId, wire_encode};

    /// Fixtures are built by encoding real `Frame`s, never by hand-typing bytes:
    /// the properties under test are properties of *the wire format*, and a
    /// hardcoded array would keep passing after the format moved out from under
    /// it.
    fn wire(frame: &Frame<'_>) -> Vec<u8> {
        let mut buf = [0u8; 520];
        wire_encode(frame, &mut buf).expect("frame fits the UART sink's scratch").to_vec()
    }

    fn stream(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn hello() -> Frame<'static> {
        Frame::Hello { timebase_hz: 4_000_000, protocol_version: 1 }
    }

    /// What the cable really carries across `booti` — no `0x00` anywhere in it,
    /// which is the whole reason the first frame shares a chunk with it.
    const UBOOT: &str = "\r\nU-Boot SPL 2021.10\r\nStarting kernel ...\r\n";

    #[test]
    fn a_capture_of_only_frames_yields_no_text() {
        let bytes = stream(&[&wire(&hello()), &wire(&Frame::Dropped { count: 0 })]);
        let out = split(&bytes);
        assert_eq!(out.io_text, "");
        assert_eq!(out.frames.len(), 2);
        assert_eq!(out.resyncs, 0);
    }

    /// **The case this step exists for.** U-Boot's log contains no `0x00`, so the
    /// first terminator on the wire is the *first frame's* — text and `Hello`
    /// land in one chunk. A decoder that drops that chunk loses `Hello`, and
    /// `Hello` is sent exactly once per boot and carries `timebase_hz`. Measured
    /// against the collector's own decoder, which does lose it.
    #[test]
    fn the_uboot_handoff_keeps_both_the_log_and_the_first_frame() {
        let bytes = stream(&[
            UBOOT.as_bytes(),
            &wire(&hello()),
            &wire(&Frame::StringRegister { id: StringId(1), value: "kernel.boot" }),
        ]);
        let out = split(&bytes);
        assert_eq!(out.io_text, UBOOT, "the boot log must survive intact");
        assert_eq!(
            out.frames.first(),
            Some(&OwnedFrame::Hello { timebase_hz: 4_000_000, protocol_version: 1 }),
            "Hello must be recovered from the chunk it shares with the text"
        );
        assert_eq!(out.frames.len(), 2);
        assert_eq!(out.resyncs, 0, "nothing was lost, so nothing may be reported lost");
    }

    /// Text is not only *in front of* the frames. `open_stream` (which sends
    /// `Hello`) runs long before `console=frames` is applied, so early-boot
    /// `println!`s go raw to the same UART while frames are already flowing.
    /// The rule has to be uniform, not a one-time handoff detector.
    #[test]
    fn text_interleaved_between_frames_is_recovered() {
        let bytes = stream(&[
            &wire(&hello()),
            b"virtio-console: ready\n",
            &wire(&Frame::Dropped { count: 0 }),
        ]);
        let out = split(&bytes);
        assert_eq!(out.io_text, "virtio-console: ready\n");
        assert_eq!(out.frames.len(), 2);
        assert_eq!(out.resyncs, 0);
    }

    /// COBS strips `0x00` and nothing else, so `0x0A` rides through a frame body
    /// freely — `SpanId(10)` *is* a newline byte. Any splitter that treats `\n`
    /// as a boundary cuts this frame in half. Measured: 2.7% of `SpanEnd` frames
    /// carry one by timestamp alone, plus every frame holding a 10 in any field.
    #[test]
    fn a_frame_whose_body_contains_a_newline_byte_survives_intact() {
        let span = Frame::SpanStart {
            id: SpanId(10),
            parent: SpanId(0),
            name_id: StringId(3),
            t: 1_234_567,
            task_id: 1,
            hart_id: 0,
        };
        let encoded = wire(&span);
        assert!(encoded.contains(&b'\n'), "fixture must actually exercise the hazard");

        let out = split(&stream(&[UBOOT.as_bytes(), &encoded]));
        assert_eq!(out.io_text, UBOOT);
        assert_eq!(out.frames, vec![OwnedFrame::from_borrowed(&span)]);
        assert_eq!(out.resyncs, 0);
    }

    /// The ambiguous chunk, taken from a sweep rather than imagined: `CapEvent`
    /// admits more than one start offset that decodes cleanly to the terminator,
    /// and the *earliest* of them is wrong. Latest-wins was measured correct on
    /// all 96 mixed chunks swept; this is the one that discriminates.
    #[test]
    fn the_latest_valid_frame_start_wins_where_several_decode() {
        let cap = Frame::CapEvent {
            kind: CapEventKind::Granted,
            cap_id: 9,
            parent_cap_id: 0,
            holder: 2,
            object: CapObject::TelemetrySink,
            rights: 3,
            badge: 0,
            t: 42,
            hart_id: 0,
            name: [0u8; snitchos_abi::CAP_NAME_LEN],
        };
        let out = split(&stream(&[UBOOT.as_bytes(), &wire(&cap)]));
        assert_eq!(out.io_text, UBOOT, "an earliest-wins tie-break eats 24 bytes of the log");
        assert_eq!(out.frames, vec![OwnedFrame::from_borrowed(&cap)]);
        assert_eq!(out.resyncs, 0);
    }

    /// U-Boot's `=> ` prompt never gets a newline and never gets a `0x00`. If a
    /// trailing run with no terminator were dropped, step 4b's output would look
    /// empty at exactly the moment it worked.
    #[test]
    fn trailing_text_with_no_terminator_is_kept() {
        let out = split(&stream(&[&wire(&hello()), b"=> "]));
        assert_eq!(out.io_text, "=> ");
        assert_eq!(out.frames.len(), 1);
    }

    #[test]
    fn a_capture_with_no_frames_at_all_is_all_text() {
        let out = split(UBOOT.as_bytes());
        assert_eq!(out.io_text, UBOOT);
        assert!(out.frames.is_empty());
        assert_eq!(out.resyncs, 0);
    }

    #[test]
    fn an_empty_capture_is_empty() {
        let out = split(b"");
        assert_eq!(out.io_text, "");
        assert!(out.frames.is_empty());
        assert_eq!(out.resyncs, 0);
    }

    /// Corruption is counted and survived, never fatal — a serial line drops
    /// bytes for reasons no software prevents, and one bad frame must not end
    /// the capture. COBS resyncs at the next delimiter by construction.
    #[test]
    fn corruption_costs_one_resync_and_the_stream_continues() {
        let bytes = stream(&[
            &wire(&hello()),
            &[0xF0, 0x9F, 0x92, 0xA9, 0xFE, 0xFD, 0x00],
            &wire(&Frame::Dropped { count: 7 }),
        ]);
        let out = split(&bytes);
        assert_eq!(out.resyncs, 1, "a chunk that was frame-shaped and undecodable is a lost frame");
        assert_eq!(
            out.frames,
            vec![
                OwnedFrame::Hello { timebase_hz: 4_000_000, protocol_version: 1 },
                OwnedFrame::Dropped { count: 7 },
            ],
            "the frame after the corruption must not be swallowed with it"
        );
    }

    /// `resyncs` counts **frames lost**, not "chunks that were not frames". A
    /// stray `0x00` inside U-Boot text costs nothing — there was never a frame
    /// there — so reporting it as a loss would send someone hunting for a
    /// transport fault that does not exist.
    #[test]
    fn a_nul_inside_plain_text_is_text_not_a_lost_frame() {
        let out = split(b"boot\0log\r\n");
        assert_eq!(out.io_text, "bootlog\r\n");
        assert!(out.frames.is_empty());
        assert_eq!(out.resyncs, 0);
    }

    /// The reason the plan chose per-chunk recovery over one-time handoff
    /// detection: a board that resets mid-capture starts a whole second session,
    /// text and all, and the capture has to keep making sense across it.
    #[test]
    fn a_reboot_mid_capture_recovers_the_second_session() {
        let bytes = stream(&[
            &wire(&hello()),
            &wire(&Frame::Dropped { count: 1 }),
            UBOOT.as_bytes(),
            &wire(&hello()),
        ]);
        let out = split(&bytes);
        assert_eq!(out.io_text, UBOOT);
        assert_eq!(
            out.frames,
            vec![
                OwnedFrame::Hello { timebase_hz: 4_000_000, protocol_version: 1 },
                OwnedFrame::Dropped { count: 1 },
                OwnedFrame::Hello { timebase_hz: 4_000_000, protocol_version: 1 },
            ],
            "the second session's Hello must be recovered exactly like the first"
        );
        assert_eq!(out.resyncs, 0);
    }
}
