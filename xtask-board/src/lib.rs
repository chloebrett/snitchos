//! `cargo xtask board` — driving the VisionFive 2 from the host.
//!
//! See [plans/board-bridge.md](../../plans/board-bridge.md). This crate is the
//! serial-linked half; lean `xtask` forwards to it.

pub mod reach;

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
