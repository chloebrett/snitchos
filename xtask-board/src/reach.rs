//! Why the bridge could not reach the board.
//!
//! The distinction this module exists to preserve: **"could not reach the board"
//! must never look like "reached it, and it said nothing."** On hardware both
//! present as no frames, and they have completely different fixes — one is a
//! stray `screen` holding the port, the other is a kernel that did not boot. A
//! bridge that reports them identically sends you looking in the wrong place,
//! and an unattended one cannot tell them apart at all.
//!
//! The variants partition by **what the operator must do differently**, which is
//! the only useful axis: kill a process, fix permissions, plug the adapter in.

use std::io::ErrorKind;

/// A failure to obtain a byte stream from the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unreachable {
    /// A macOS *call-in* (`tty.*`) device path. Opening it blocks until carrier
    /// detect that a USB-TTL adapter never asserts, so this is caught *before*
    /// opening rather than as a failure — an infinite hang is not an error.
    /// Carries the `cu.*` path to use instead.
    CallInDevice { device: String, use_instead: String },
    /// Another process holds the port. `holder` names its pid where `lsof` could
    /// identify it. Fix: kill the holder.
    PortHeld { device: String, holder: Option<u32> },
    /// The port exists and is free, but this user may not open it. Fix:
    /// permissions — *not* killing anything.
    NoPermission { device: String },
    /// No such device. Fix: plug the adapter in, or correct the path.
    NoSuchDevice { device: String },
    /// Something else the OS reported, kept with its kind rather than flattened
    /// into one of the above.
    OpenFailed { device: String, kind: ErrorKind },
}

/// Reject a device path that would block forever instead of failing.
///
/// Runs **before** the open, not after: a macOS `tty.*` node blocks in `open()`
/// until carrier detect that a USB-TTL adapter never asserts, so there is no
/// error to classify — the call simply never returns, which for an unattended
/// bridge is indistinguishable from a board that never booted.
///
/// Delegates the rule to `collector::serial::call_out_alternative`, which the
/// collector's `--serial` source already needed. Two copies of a platform quirk
/// drift; one does not.
///
/// # Errors
/// [`Unreachable::CallInDevice`], carrying the `cu.*` path to use instead.
pub fn check_device(device: &str) -> Result<(), Unreachable> {
    match collector::serial::call_out_alternative(device) {
        Some(use_instead) => {
            Err(Unreachable::CallInDevice { device: device.to_string(), use_instead })
        }
        None => Ok(()),
    }
}

/// The `std::io` kind behind a `serialport` error.
///
/// `serialport` wraps real I/O errors in `Io(kind)` — which is where
/// `ResourceBusy` and `PermissionDenied` arrive, the two that
/// [`classify_open_failure`] turns into actionable advice — and adds three of its
/// own.
///
/// **`NoDevice` is ambiguous by the crate's own admission**: its documentation
/// says it covers both "in use by another process" *and* "disconnected while
/// performing I/O", which are opposite diagnoses. It maps to `NotFound` because
/// absence is the commoner cause; the caller is expected to resolve the ambiguity
/// with evidence, since a port `lsof` can name a holder for is held, not missing.
#[must_use]
pub fn io_kind(kind: serialport::ErrorKind) -> ErrorKind {
    match kind {
        serialport::ErrorKind::Io(kind) => kind,
        serialport::ErrorKind::NoDevice => ErrorKind::NotFound,
        serialport::ErrorKind::InvalidInput => ErrorKind::InvalidInput,
        serialport::ErrorKind::Unknown => ErrorKind::Other,
    }
}

/// Classify a failed serial open into an [`Unreachable`].
///
/// Takes the [`ErrorKind`] rather than the error so it stays pure and total —
/// the `serialport` error type it comes from is an I/O detail of the caller.
#[must_use]
pub fn classify_open_failure(device: &str, kind: ErrorKind) -> Unreachable {
    let device = device.to_string();
    match kind {
        ErrorKind::ResourceBusy => Unreachable::PortHeld { device, holder: None },
        ErrorKind::PermissionDenied => Unreachable::NoPermission { device },
        ErrorKind::NotFound => Unreachable::NoSuchDevice { device },
        kind => Unreachable::OpenFailed { device, kind },
    }
}

/// Refine a classification with evidence that `pid` holds the device.
///
/// Two upgrades, both driven by the same fact — something has the port open:
///
/// - A [`Unreachable::PortHeld`] learns *who*, turning "something has it" into a
///   `kill` target.
/// - A [`Unreachable::NoSuchDevice`] was **wrong**, and this is the only way to
///   know. `serialport` reports busy and absent alike as `NoDevice` (see
///   [`io_kind`]), so a device `lsof` can name a holder for plainly exists. Left
///   uncorrected it sends someone hunting a cable that is fine, when the fix is
///   to kill a process.
///
/// Everything else is returned untouched: a permissions failure is not about who
/// holds the port, and rewriting it would replace a correct diagnosis with a
/// wrong one.
#[must_use]
pub fn refine_with_holder(unreachable: Unreachable, pid: u32) -> Unreachable {
    match unreachable {
        Unreachable::PortHeld { device, .. } | Unreachable::NoSuchDevice { device } => {
            Unreachable::PortHeld { device, holder: Some(pid) }
        }
        other => other,
    }
}

/// The pid in the first data row of `lsof` output, if there is one.
///
/// `lsof` prints only a header (or nothing at all) when no process holds the
/// file, and exits non-zero — so "no output" means "nobody", not "failed".
/// Anything unparseable yields `None`: this is a diagnostic path, and an
/// external tool's output is not worth panicking over.
#[must_use]
pub fn holder_pid(lsof_output: &str) -> Option<u32> {
    lsof_output
        .lines()
        .skip(1)
        .find(|line| !line.trim().is_empty())?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}
