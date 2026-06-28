//! `unhitch` must reject bytes that are not a valid encoding rather than
//! fabricate a `Value`. The error path is the half of the codec the round-trip
//! tests never exercise.

use hitch::unhitch;

#[test]
fn empty_input_is_rejected() {
    // No bytes at all: there isn't even a discriminant to read.
    assert!(unhitch(&[]).is_err());
}

#[test]
fn truncated_varint_is_rejected() {
    // A lone continuation byte promises more varint and delivers none; the
    // decoder must fail rather than guess.
    assert!(unhitch(&[0xff]).is_err());
}

#[test]
fn decode_error_displays_a_useful_message() {
    // An error that renders as the empty string is useless in a log or on the
    // UART; the message must name what went wrong.
    let err = unhitch(&[0xff]).expect_err("invalid bytes are rejected");
    assert!(format!("{err}").contains("hitch"));
}
