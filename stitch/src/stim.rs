//! The stim driver — hosts the editor FSM (a Stitch program) as a native event
//! loop: build the interpreter env **once**, then per keystroke call `step` and
//! perform the returned effect (redraw / save). The logic is `Platform`-generic
//! and host-buildable, so it can be driven end-to-end against a fake; the
//! on-target `:stim` command is a thin wrapper that supplies the real backends.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::env::Env;
use crate::interp::{apply_values, build_env, prelude_items};
use crate::parser::parse_program;
use crate::platform::{Handle, Platform};
use crate::telemetry::Telemetry;
use crate::value::{RuntimeError, Value};

/// Run the stim editor: load the FSM `source`, seed it with the file's `content`,
/// then drive the read→step→perform loop against `platform`, saving through the
/// file cap at `file_handle` on `:w`. Returns when input ends — a finite fake
/// session; the on-target console never ends, so there it runs until the process
/// is killed (v1's Ctrl-C exit).
///
/// # Errors
/// Returns the FSM's runtime fault, a parse error in `source`, or a missing FSM
/// entry point (`initialState`/`step`/`renderFrame`).
pub fn run(
    source: &str,
    content: &str,
    file_handle: Handle,
    platform: &dyn Platform,
    telemetry: &dyn Telemetry,
) -> Result<(), RuntimeError> {
    // Build the interpreter env ONCE (prelude + the FSM), then reuse it for every
    // keystroke — never re-`eval_program` per key (that is the B5 per-run leak).
    let mut items = prelude_items();
    items.extend(parse_program(source).map_err(|e| RuntimeError::new(e.message))?);
    let env = build_env(&items);

    let initial = lookup(&env, "initialState")?;
    let step = lookup(&env, "step")?;
    let render = lookup(&env, "renderFrame")?;

    // One span for the whole editing session; each `:w` nests its own (below). The
    // FSM is pure — spans are the *driver's* to emit.
    telemetry.span_open("stim.session");
    let mut state = apply_values(&initial, &[Value::Str(content.into())], &env)?;
    redraw(&render, &state, &env, platform)?;

    // The loop is "the platform"; `step`/`renderFrame`/the state are "the program".
    while let Some(byte) = platform.read_byte() {
        let key = byte_to_key(byte);
        let stepped = apply_values(&step, &[state.clone(), Value::Str(key.into())], &env)?;
        let next = field(&stepped, "state")
            .cloned()
            .ok_or_else(|| RuntimeError::new("stim: step result has no `state`"))?;
        let effect = field(&stepped, "effect")
            .cloned()
            .ok_or_else(|| RuntimeError::new("stim: step result has no `effect`"))?;
        perform(&effect, &next, &render, &env, platform, file_handle, telemetry)?;
        state = next;
        if is_quit(&effect) {
            break;
        }
    }
    telemetry.span_close("stim.session");
    Ok(())
}

/// Whether the effect is `Quit` — the FSM asking the driver to end the session.
fn is_quit(effect: &Value) -> bool {
    matches!(effect, Value::Data(d) if d.variant == "Quit")
}

fn lookup(env: &Env, name: &str) -> Result<Value, RuntimeError> {
    env.lookup(name)
        .ok_or_else(|| RuntimeError::new(format!("stim: the FSM defines no `{name}`")))
}

/// Map a raw input byte to the FSM's key token: symbolic names for the control
/// keys, the character itself for a printable. The FSM dispatches on tokens (its
/// `step`), so byte encodings stay out of the editor logic.
fn byte_to_key(byte: u8) -> String {
    match byte {
        0x1b => "Esc".to_string(),
        0x0d | 0x0a => "Enter".to_string(),
        0x7f | 0x08 => "Backspace".to_string(),
        b => char::from(b).to_string(),
    }
}

/// A named field of a Stitch record value (`prod`/variant), or `None`.
fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Data(d) => d
            .fields
            .iter()
            .find_map(|(n, v)| (n.as_deref() == Some(name)).then_some(v)),
        _ => None,
    }
}

/// Render the current state to a frame and write it to the console.
fn redraw(
    render: &Value,
    state: &Value,
    env: &Env,
    platform: &dyn Platform,
) -> Result<(), RuntimeError> {
    if let Value::Str(frame) = apply_values(render, core::slice::from_ref(state), env)? {
        platform.write(&frame);
    }
    Ok(())
}

/// Perform the effect the FSM returned: `Redraw` repaints, `Save(text)` writes the
/// buffer through the file cap, `Noop` does nothing.
fn perform(
    effect: &Value,
    state: &Value,
    render: &Value,
    env: &Env,
    platform: &dyn Platform,
    file_handle: Handle,
    telemetry: &dyn Telemetry,
) -> Result<(), RuntimeError> {
    let Value::Data(d) = effect else {
        return Ok(());
    };
    match d.variant.as_str() {
        "Redraw" => redraw(render, state, env, platform)?,
        "Edit" => {
            // A buffer mutation: span it by the FSM-supplied name, then repaint. The
            // edit already happened (the FSM is pure); the span is a zero-duration
            // marker on the wire — the edit history *is* the trace.
            if let Some((_, Value::Str(name))) = d.fields.first() {
                telemetry.span_open(name);
                redraw(render, state, env, platform)?;
                telemetry.span_close(name);
            }
        }
        "Save" => {
            if let Some((_, Value::Str(text))) = d.fields.first() {
                // Each save is its own span, nested in the session span.
                telemetry.span_open("stim.save");
                // A refused write (read-only cap) returns `false`; the kernel has
                // already snitched the `SyscallRefused`. v1 does not surface it.
                let _saved = platform.fs_write(file_handle, text.as_bytes());
                telemetry.span_close("stim.save");
            }
        }
        _ => {} // Noop
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::platform::FakePlatform;
    use crate::telemetry::RecordingTelemetry;

    /// The canonical FSM source — the same file the ramfs seeds.
    const STIM: &str = include_str!("../../fs-image/stim/stim.st");

    #[test]
    fn a_scripted_session_edits_the_buffer_and_saves_it() {
        // Seed "ab"; script: `i` (insert), `Z` (→ "Zab"), Esc, then `:w` (save).
        // Esc is 0x1b — the driver maps it to the "Esc" key token.
        let fake = FakePlatform::with_bytes(b"iZ\x1b:w");
        run(STIM, "ab", 7, &fake, &RecordingTelemetry::default()).expect("stim session should run");
        // `:w` saved the edited buffer through the (fake) file cap at handle 7.
        assert_eq!(fake.writes(), vec![(7u32, b"Zab".to_vec())]);
        // A redraw drew the edited buffer to the console.
        assert!(fake.output().contains("Zab"), "a frame should have drawn the buffer");
    }

    #[test]
    fn without_a_w_command_nothing_is_saved() {
        // Edit but never `:w` — the buffer changes on screen, but no write happens.
        let fake = FakePlatform::with_bytes(b"iZ\x1b");
        run(STIM, "ab", 7, &fake, &RecordingTelemetry::default()).expect("stim session should run");
        assert!(fake.writes().is_empty(), "no `:w` → no save");
        assert!(fake.output().contains("Zab"), "the edit still drew");
    }

    #[test]
    fn a_session_span_wraps_the_run_and_each_save_nests_a_span() {
        use crate::telemetry::Telemetry;
        use crate::value::TelemetryEvent;

        // Two `:w`s in one session → one session span with two nested save spans.
        let fake = FakePlatform::with_bytes(b":w:w");
        let tel = RecordingTelemetry::default();
        run(STIM, "hi", 7, &fake, &tel).expect("stim session should run");

        let spans: Vec<String> = tel
            .snapshot()
            .iter()
            .filter_map(|e| match e {
                TelemetryEvent::SpanOpen { name } => Some(format!("open {name}")),
                TelemetryEvent::SpanClose { name } => Some(format!("close {name}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            spans,
            [
                "open stim.session",
                "open stim.save",
                "close stim.save",
                "open stim.save",
                "close stim.save",
                "close stim.session",
            ]
        );
    }

    #[test]
    fn a_buffer_edit_emits_a_named_span_nested_in_the_session() {
        use crate::telemetry::Telemetry;
        use crate::value::TelemetryEvent;

        // `x` deletes the char under the cursor — an observable Edit. The driver
        // opens the span the FSM named ("stim.delete-char"), nested in the session
        // span: the edit history is a trace on the wire.
        let fake = FakePlatform::with_bytes(b"x");
        let tel = RecordingTelemetry::default();
        run(STIM, "abc", 7, &fake, &tel).expect("stim session should run");

        let spans: Vec<String> = tel
            .snapshot()
            .iter()
            .filter_map(|e| match e {
                TelemetryEvent::SpanOpen { name } => Some(format!("open {name}")),
                TelemetryEvent::SpanClose { name } => Some(format!("close {name}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            spans,
            [
                "open stim.session",
                "open stim.delete-char",
                "close stim.delete-char",
                "close stim.session",
            ]
        );
    }

    #[test]
    fn the_two_key_r_replace_flows_through_the_per_byte_loop() {
        // `r` enters Replace mode, `Z` replaces the char under the cursor ('a'→'Z'),
        // then `:w` saves — proving a two-key command survives the one-byte-at-a-time
        // driver loop (the first multi-keystroke command in stim).
        let fake = FakePlatform::with_bytes(b"rZ:w");
        run(STIM, "abc", 7, &fake, &RecordingTelemetry::default()).expect("stim session should run");
        assert_eq!(fake.writes(), vec![(7u32, b"Zbc".to_vec())]);
    }

    #[test]
    fn the_enter_and_backspace_bytes_map_to_their_key_tokens() {
        // Type "ab", then CR (0x0d) splits into two lines, then DEL (0x7f) joins
        // them back, then `:w`. The saved buffer is "ab" only if the raw CR/DEL
        // bytes reached the FSM as the "Enter"/"Backspace" tokens.
        let fake = FakePlatform::with_bytes(b"iab\x0d\x7f\x1b:w");
        run(STIM, "", 7, &fake, &RecordingTelemetry::default()).expect("stim session should run");
        assert_eq!(fake.writes(), vec![(7u32, b"ab".to_vec())]);
    }

    #[test]
    fn colon_q_quits_the_loop_and_ignores_later_keys() {
        // `:q` breaks the driver loop; the trailing `iZ\x1b:w` — which would
        // otherwise insert and save "Z" — is never read.
        let fake = FakePlatform::with_bytes(b":qiZ\x1b:w");
        run(STIM, "hi", 7, &fake, &RecordingTelemetry::default()).expect("stim session should run");
        assert!(fake.writes().is_empty(), ":q must stop before the later :w");
    }

    #[test]
    fn a_read_only_cap_refuses_the_save_and_records_nothing() {
        // `deny_writes` models a read-only file cap (the kernel refusal the metal
        // surfaces as `false`). `:w` routes to `fs_write`, which is refused.
        let fake = FakePlatform::with_bytes(b":w");
        fake.deny_writes();
        run(STIM, "ab", 7, &fake, &RecordingTelemetry::default()).expect("stim session should run");
        assert!(fake.writes().is_empty(), "a refused save records nothing");
    }
}
