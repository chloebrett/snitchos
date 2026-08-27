//! snemu in a browser tab: the shim between the emulator core and JS.
//!
//! The split this crate exists to enforce — see `plans/legacy/snemu-wasm.md`:
//! every non-trivial behaviour is a **pure function tested on the host**, and
//! the `#[wasm_bindgen]` layer is a shell too thin to hide a bug. `itest-harness`
//! is the precedent to mirror: it is already a second embedder of
//! [`snemu::machine::Machine`], driving `step()` in a loop. The browser plays the
//! identical role.

pub mod boot;
pub mod budget;
pub mod cursor;
pub mod input;
pub mod probe;
pub mod series;
pub mod shell;
pub mod store;
pub mod telemetry;

#[cfg(test)]
mod tests {
    use snemu::machine::Machine;
    use snemu::mem::Memory;

    /// A machine that has not been stepped has retired no instructions.
    ///
    /// Trivial on its face, and that is the point: it is the first proof that
    /// this crate links the emulator core and that the host gate *runs* its
    /// suite without anyone editing a list.
    #[test]
    fn a_fresh_machine_has_retired_nothing() {
        let machine = Machine::new(Memory::new(1024 * 1024), 1);
        assert_eq!(machine.instret(), 0);
    }
}
