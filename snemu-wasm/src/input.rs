//! What the reader typed, and when.
//!
//! A keystroke is delivered at whatever instret the animation-frame pump happens
//! to have reached, so two readers typing the same characters produce two
//! different guests. Stamping each delivery with the guest's clock turns a session
//! into a **replay script**: initial conditions plus `(instret, bytes)` reproduce
//! it exactly, because the guest is deterministic.
//!
//! That is what makes a diverged world-state shareable as a URL, and it is the
//! same log a scrubber seeks over. See `plans/tour-v1.md`.

use serde::Serialize;
use snemu::machine::Machine;

/// One delivery: what was typed, and the guest clock it landed on.
///
/// `text` rather than bytes because that is what a replay hands back to
/// [`deliver`] — storing bytes would force a fallible conversion on the way out
/// for nothing gained on the way in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputEvent {
    /// The guest's retired-instruction count at the moment of delivery.
    pub instret: u64,
    /// What was typed.
    pub text: String,
}

/// Every delivery so far, in the order it happened.
#[derive(Debug, Default, Clone)]
pub struct InputLog {
    entries: Vec<InputEvent>,
}

impl InputLog {
    /// The deliveries, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[InputEvent] {
        &self.entries
    }
}

/// Deliver typed text to the guest's console, recording when it landed.
///
/// The stamp is read before handing the bytes over. Today that ordering is not
/// observable — `push_console_input` buffers into the UART and retires nothing, so
/// the clock reads the same either side of it — and no test here could tell the two
/// apart. Stating it as a *contract* would be a guard that checks nothing; it is a
/// convention, kept because it stays correct if delivery ever costs the guest time.
///
/// Lives here rather than in the `#[wasm_bindgen]` shell because it is a decision
/// — *when* to stamp — and the shell is deliberately too thin to hold one, which
/// also means nothing in the fast suite could check it there.
pub fn deliver(machine: &mut Machine, log: &mut InputLog, text: &str) {
    log.entries.push(InputEvent { instret: machine.instret(), text: text.to_owned() });
    machine.push_console_input(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::{InputLog, deliver};
    use snemu::machine::Machine;
    use snemu::mem::{Memory, RAM_BASE};

    /// `jal x0, 0` — branch to self, retiring an instruction every round.
    const SPIN: u32 = 0x0000_006F;

    fn machine_running(program: &[u32]) -> Machine {
        let mut m = Machine::new(Memory::new(64 * 1024), 1);
        let mut image = Vec::new();
        for w in program {
            image.extend_from_slice(&w.to_le_bytes());
        }
        m.write_ram(RAM_BASE, &image).expect("program fits");
        m.set_pc(0, RAM_BASE);
        m
    }

    /// **A delivery is stamped with the clock it landed on, and they keep order.**
    ///
    /// Without the stamp a session cannot be replayed: the same keystrokes
    /// delivered at different instrets are different guests, and "the same thing
    /// the reader typed" is not enough to reproduce what they saw.
    #[test]
    fn input_is_stamped_with_the_instret_at_which_it_was_delivered() {
        let mut machine = machine_running(&[SPIN]);
        let mut log = InputLog::default();

        deliver(&mut machine, &mut log, "a");
        for _ in 0..16 {
            machine.step().expect("the spin loop keeps running");
        }
        deliver(&mut machine, &mut log, "b");

        let entries = log.entries();
        assert_eq!(entries.len(), 2, "both deliveries are recorded");
        assert_eq!(entries[0].text, "a", "oldest first");
        assert_eq!(entries[1].text, "b");
        assert_eq!(entries[0].instret, 0, "the first landed before the guest ran");
        assert_eq!(entries[1].instret, 16, "the second after sixteen instructions");
    }

    /// The JSON shape is a contract with the page, which has no compiler to notice
    /// a renamed key.
    #[test]
    fn the_input_log_json_is_what_the_page_reads() {
        let mut machine = machine_running(&[SPIN]);
        let mut log = InputLog::default();

        deliver(&mut machine, &mut log, "hi");

        let json = serde_json::to_string(log.entries()).expect("the log serializes");
        assert_eq!(json, r#"[{"instret":0,"text":"hi"}]"#);
    }
}
