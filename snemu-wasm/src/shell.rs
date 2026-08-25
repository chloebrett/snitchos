//! The JS boundary. **Deliberately too thin to hide a bug.**
//!
//! Every method here is a direct call into [`crate::budget`], [`crate::cursor`] or
//! [`crate::telemetry`], where the behaviour lives and is host-tested. There is no
//! branching and no arithmetic in this file, and that is a rule rather than an
//! accident: `wasm32` code can only be exercised through a browser or a Node
//! harness, so anything that lives here is effectively untested by the fast suite.
//!
//! The precedent is a cautionary one. `~/c/slay`'s plan said the same thing — "push
//! the logic down" — and its shipped `WasmSession::send` nonetheless grew branching,
//! pile-rendering and save-prompt state, all reachable only through a
//! `#[wasm_bindgen]` method. If a test would be *meaningful* against something in
//! this file, that thing is in the wrong file.
//!
//! The two data types cross as JSON ([`Status`], [`FrameView`]), whose shapes are
//! pinned by tests in their own modules — a `#[wasm_bindgen]` method cannot return
//! an enum with payloads, and open-coding that conversion here is exactly the logic
//! this file must not carry.

use wasm_bindgen::prelude::*;

use crate::budget;
use crate::cursor::Cursor;
use crate::telemetry::Decoder;
use snemu::machine::Machine;

/// A booted guest, plus the three pieces of drain state a page needs to keep
/// between animation frames.
#[wasm_bindgen]
pub struct Handle {
    machine: Machine,
    uart: Cursor,
    virtio: Cursor,
    decoder: Decoder,
}

#[wasm_bindgen]
impl Handle {
    /// Load a kernel ELF and place it in `ram_bytes` of guest RAM.
    ///
    /// The device tree rides along from the lib (`snemu::dtb::VIRT`), so the browser
    /// and the CLI boot the same machine description. Two harts, because the kernel
    /// brings up its secondary unconditionally.
    ///
    /// # Errors
    /// If the image is not a loadable RV64 ELF.
    #[wasm_bindgen(constructor)]
    pub fn new(elf: &[u8], ram_bytes: usize) -> Result<Handle, JsError> {
        let machine =
            snemu::loader::load_machine(elf, ram_bytes, Some(snemu::dtb::VIRT), 2, false)
                .map_err(|e| JsError::new(&format!("{e:?}")))?;
        Ok(Handle {
            machine,
            uart: Cursor::new(),
            virtio: Cursor::new(),
            decoder: Decoder::new(),
        })
    }

    /// Run the guest for at most `instret` instructions; return the outcome as JSON.
    ///
    /// # Errors
    /// If the outcome cannot be serialized.
    pub fn step_budget(&mut self, instret: u64) -> Result<String, JsError> {
        let status = budget::run(&mut self.machine, instret);
        serde_json::to_string(&status).map_err(|e| JsError::new(&e.to_string()))
    }

    /// UART bytes written since the last call, as text for `term.write()`.
    ///
    /// Lossy on invalid UTF-8 by design: this is a terminal byte stream, and a page
    /// that refused to render a boot log because one byte was malformed would be
    /// worse than one that shows a replacement character.
    pub fn drain_uart(&mut self) -> String {
        String::from_utf8_lossy(self.uart.drain(self.machine.uart_output())).into_owned()
    }

    /// Telemetry frames completed since the last call, as a JSON array.
    ///
    /// # Errors
    /// If the frames cannot be serialized.
    pub fn drain_frames(&mut self) -> Result<String, JsError> {
        let bytes = self.virtio.drain(self.machine.virtio_tx_output());
        let views = self.decoder.push(bytes);
        serde_json::to_string(&views).map_err(|e| JsError::new(&e.to_string()))
    }

    /// The guest's cumulative retired-instruction count — its clock.
    #[must_use]
    pub fn instret(&self) -> u64 {
        self.machine.instret()
    }
}

#[cfg(test)]
mod tests {
    /// The shell's own source, so the rule in this file's docs is enforced rather
    /// than merely stated.
    const SOURCE: &str = include_str!("shell.rs");

    /// Everything from `#[wasm_bindgen]` onwards — the part that can only be
    /// exercised through a browser. The tests below deliberately do not look at this
    /// module's doc comment, which discusses the very constructs it bans.
    fn bindgen_body() -> &'static str {
        let start = SOURCE.find("use wasm_bindgen").expect("the shell imports wasm_bindgen");
        // Stop before this test module: it names the banned constructs in order to
        // ban them, and would otherwise convict itself.
        let end = SOURCE.find("#[cfg(test)]").expect("this module exists");
        &SOURCE[start..end]
    }

    /// Code, with comment text removed — so prose about `if` and `match` does not
    /// read as `if` and `match`. Crude on purpose: a real parser would be far more
    /// machinery than a ratchet warrants, and this file is 90 lines.
    fn code_only(body: &str) -> String {
        body.lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **The rule this file exists to keep.** `~/c/slay`'s equivalent said "push the
    /// logic down" in its plan and then grew branching and state anyway, all of it
    /// reachable only through a `#[wasm_bindgen]` method — i.e. only from a browser,
    /// where this project's fast suite cannot see it.
    ///
    /// So: no control flow in the shell. If this test starts failing, the honest fix
    /// is almost never to relax it — it is to move the decision into `budget`,
    /// `cursor` or `telemetry`, where a host test can reach it.
    ///
    /// `map_err` is permitted and is the one deliberate exception: it is error
    /// *plumbing* with no branch of its own to get wrong, and the alternative
    /// (`match` on every `Result`) would be strictly more logic in exactly the file
    /// that should have none.
    #[test]
    fn the_bindgen_layer_contains_no_control_flow() {
        let code = code_only(bindgen_body());
        for banned in ["if ", "match ", "while ", "for ", "loop {", " else"] {
            assert!(
                !code.contains(banned),
                "the shell must stay too thin to hide a bug, but contains `{banned}` — \
                 move that decision into budget/cursor/telemetry where it can be tested"
            );
        }
    }

    /// A companion guard: no arithmetic either. An off-by-one in the shell would be
    /// invisible to `cargo nextest`, and the drain/budget arithmetic all belongs to
    /// modules that pin it.
    #[test]
    fn the_bindgen_layer_does_no_arithmetic() {
        let code = code_only(bindgen_body());
        for banned in [" + ", " - ", " * ", " / ", " % ", "+=", "-=", "saturating_", "wrapping_"] {
            assert!(
                !code.contains(banned),
                "the shell must do no arithmetic, but contains `{banned}`"
            );
        }
    }

    /// The guard has to be able to fail, or it is decoration. Feeds it a body that
    /// obviously violates the rule and asserts it objects — the same negative-control
    /// discipline the probe's oracle needed once mutation testing exposed it.
    #[test]
    fn the_thinness_guard_would_notice_a_violation() {
        let smuggled = code_only("use wasm_bindgen::x;\nfn f() { if a { b } }");
        assert!(smuggled.contains("if "), "a real violation must survive comment-stripping");

        let commented = code_only("use wasm_bindgen::x;\n// if this were code it would fail");
        assert!(!commented.contains("if "), "but prose about `if` must not");
    }
}
