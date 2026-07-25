//! A minimal RISC-V PLIC: enough of the register set and gateway semantics to
//! deliver the guest's UART interrupt deterministically. Egress of the model is a
//! single signal — [`Plic::seip`], whether a context has a deliverable interrupt —
//! which the [`Bus`](crate::bus::Bus) feeds to the hart's `sip.SEIP` on the
//! interrupt check (the same derived shape as the timer's `cycle >= stimecmp`).
//!
//! Sources are level-driven: a device raises its line via [`set_source`], the PLIC
//! forwards it (pending) until it's claimed, and re-forwards after completion iff
//! the line is still asserted — the gateway model the real UART's level-triggered
//! THRE needs.
//!
//! Bounded to what the kernel touches (source 10 = UART0, contexts 0..4 = two
//! harts × M/S). Registers outside the modelled range read 0 / ignore writes.

/// Highest interrupt source id modelled (UART0 is 10). Sources pack 32 per word,
/// so 64 covers two enable/pending words.
const N_SOURCES: usize = 64;
/// Contexts modelled: hart 0 M/S = 0/1, hart 1 M/S = 2/3.
const N_CONTEXTS: usize = 4;
/// Enable/pending words needed to cover `N_SOURCES`.
const N_WORDS: usize = N_SOURCES / 32;

/// Register-block boundaries (bytes from the PLIC base), per the PLIC spec.
const ENABLE_BASE: usize = 0x2000;
const CONTEXT_BASE: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000;
const ENABLE_STRIDE: usize = 0x80;

pub struct Plic {
    /// Per-source priority (0 = never delivered).
    priority: [u32; N_SOURCES],
    /// Per-context priority threshold: a source delivers only if its priority is
    /// **strictly greater** than this.
    threshold: [u32; N_CONTEXTS],
    /// Per-context enable bits, packed 32 sources per word.
    enable: [[u32; N_WORDS]; N_CONTEXTS],
    /// Device-driven interrupt line level, per source (set by [`set_source`]).
    line: [bool; N_SOURCES],
    /// Claimed-but-not-completed, per source: the gateway stops forwarding a source
    /// between claim and complete, even if the line stays asserted.
    in_progress: [bool; N_SOURCES],
}

impl Default for Plic {
    fn default() -> Self {
        Self::new()
    }
}

impl Plic {
    #[must_use]
    pub fn new() -> Self {
        Self {
            priority: [0; N_SOURCES],
            threshold: [0; N_CONTEXTS],
            enable: [[0; N_WORDS]; N_CONTEXTS],
            line: [false; N_SOURCES],
            in_progress: [false; N_SOURCES],
        }
    }

    /// Set device interrupt `source`'s line level. A rising edge makes it a
    /// forwarding candidate; a falling edge stops it forwarding (unless mid-claim,
    /// which `in_progress` already gates).
    pub fn set_source(&mut self, source: u32, asserted: bool) {
        if let Some(slot) = self.line.get_mut(source as usize) {
            *slot = asserted;
        }
    }

    /// Whether `source` is currently forwarding to the PLIC core: line asserted and
    /// not mid-claim. (A source can be enabled by several contexts; this is the
    /// gateway state, independent of context.)
    fn forwarding(&self, source: usize) -> bool {
        self.line[source] && !self.in_progress[source]
    }

    /// Whether `context` enables `source`.
    fn enabled(&self, context: usize, source: usize) -> bool {
        self.enable[context][source / 32] & (1 << (source % 32)) != 0
    }

    /// The highest-priority source deliverable to `context` — forwarding, enabled,
    /// and above the context threshold. Ties break to the lowest id (spec: lowest
    /// id wins at equal priority).
    fn top_pending(&self, context: usize) -> Option<u32> {
        if context >= N_CONTEXTS {
            return None;
        }
        (1..N_SOURCES)
            .filter(|&s| {
                self.forwarding(s) && self.enabled(context, s) && self.priority[s] > self.threshold[context]
            })
            .max_by_key(|&s| (self.priority[s], std::cmp::Reverse(s)))
            .map(|s| s as u32)
    }

    /// Whether `context` has any deliverable interrupt — the `sip.SEIP` signal.
    #[must_use]
    pub fn seip(&self, context: u32) -> bool {
        self.top_pending(context as usize).is_some()
    }

    /// Claim the top interrupt for `context` (the claim-register read): returns its
    /// id (0 = none) and marks it in-progress so the gateway stops forwarding it
    /// until [`complete`](Self::complete).
    fn claim(&mut self, context: usize) -> u32 {
        match self.top_pending(context) {
            Some(source) => {
                self.in_progress[source as usize] = true;
                source
            }
            None => 0,
        }
    }

    /// Complete `source` for `context` (the claim-register write): the gateway may
    /// forward it again if its line is still asserted.
    fn complete(&mut self, source: u32) {
        if let Some(slot) = self.in_progress.get_mut(source as usize) {
            *slot = false;
        }
    }

    /// MMIO read (32-bit). Reading a context's claim register **claims** — hence
    /// `&mut`.
    pub fn read(&mut self, offset: usize) -> u32 {
        if offset < ENABLE_BASE {
            // Priority block: 4 bytes per source.
            return *self.priority.get(offset / 4).unwrap_or(&0);
        }
        if offset >= CONTEXT_BASE {
            let context = (offset - CONTEXT_BASE) / CONTEXT_STRIDE;
            let within = (offset - CONTEXT_BASE) % CONTEXT_STRIDE;
            return match within {
                0 => *self.threshold.get(context).unwrap_or(&0),
                4 => self.claim(context),
                _ => 0,
            };
        }
        // Enable block (0x2000..0x20_0000): read back the stored word.
        let context = (offset - ENABLE_BASE) / ENABLE_STRIDE;
        let word = ((offset - ENABLE_BASE) % ENABLE_STRIDE) / 4;
        self.enable
            .get(context)
            .and_then(|w| w.get(word))
            .copied()
            .unwrap_or(0)
    }

    /// MMIO write (32-bit).
    pub fn write(&mut self, offset: usize, value: u32) {
        if offset < ENABLE_BASE {
            if let Some(p) = self.priority.get_mut(offset / 4) {
                *p = value;
            }
            return;
        }
        if offset >= CONTEXT_BASE {
            let context = (offset - CONTEXT_BASE) / CONTEXT_STRIDE;
            let within = (offset - CONTEXT_BASE) % CONTEXT_STRIDE;
            match within {
                0 => {
                    if let Some(t) = self.threshold.get_mut(context) {
                        *t = value;
                    }
                }
                4 => self.complete(value),
                _ => {}
            }
            return;
        }
        let context = (offset - ENABLE_BASE) / ENABLE_STRIDE;
        let word = ((offset - ENABLE_BASE) % ENABLE_STRIDE) / 4;
        if let Some(w) = self.enable.get_mut(context).and_then(|c| c.get_mut(word)) {
            *w = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The kernel's exemplar: UART0 = source 10, hart-0 S = context 1. Register
    // offsets from `kernel_devices::plic` (spec-pinned there).
    const SOURCE: u32 = 10;
    const CONTEXT: u32 = 1;
    const PRIORITY_OFF: usize = SOURCE as usize * 4;
    const THRESHOLD_OFF: usize = CONTEXT_BASE + CONTEXT as usize * CONTEXT_STRIDE;
    const CLAIM_OFF: usize = THRESHOLD_OFF + 4;
    const ENABLE_OFF: usize = ENABLE_BASE + CONTEXT as usize * ENABLE_STRIDE; // word 0

    /// Route the source to the context the way the kernel driver does.
    fn route(plic: &mut Plic) {
        plic.write(PRIORITY_OFF, 1);
        plic.write(THRESHOLD_OFF, 0);
        plic.write(ENABLE_OFF, 1 << (SOURCE % 32));
    }

    #[test]
    fn an_unrouted_or_unasserted_source_never_delivers() {
        let mut plic = Plic::new();
        assert!(!plic.seip(CONTEXT), "nothing routed");
        route(&mut plic);
        assert!(!plic.seip(CONTEXT), "routed but line low");
        plic.set_source(SOURCE, true);
        assert!(plic.seip(CONTEXT), "routed and asserted");
    }

    #[test]
    fn a_disabled_or_below_threshold_source_does_not_deliver() {
        let mut plic = Plic::new();
        route(&mut plic);
        plic.set_source(SOURCE, true);
        // Raise the threshold to the source's priority: strictly-greater fails.
        plic.write(THRESHOLD_OFF, 1);
        assert!(!plic.seip(CONTEXT), "threshold == priority blocks it");
        plic.write(THRESHOLD_OFF, 0);
        // Disable the source.
        plic.write(ENABLE_OFF, 0);
        assert!(!plic.seip(CONTEXT), "disabled blocks it");
    }

    #[test]
    fn claim_returns_the_source_and_stops_it_re_delivering_until_complete() {
        let mut plic = Plic::new();
        route(&mut plic);
        plic.set_source(SOURCE, true);
        assert_eq!(plic.read(CLAIM_OFF), SOURCE, "claim returns the pending source");
        // Still asserted, but claimed → the gateway won't forward it again.
        assert!(!plic.seip(CONTEXT), "no re-deliver while in progress");
        // Complete it; line is still high → it forwards again.
        plic.write(CLAIM_OFF, SOURCE);
        assert!(plic.seip(CONTEXT), "re-forwards after completion while line high");
    }

    #[test]
    fn completing_a_source_whose_line_dropped_leaves_it_quiet() {
        let mut plic = Plic::new();
        route(&mut plic);
        plic.set_source(SOURCE, true);
        assert_eq!(plic.read(CLAIM_OFF), SOURCE);
        plic.set_source(SOURCE, false); // e.g. kernel disabled IER.THRE
        plic.write(CLAIM_OFF, SOURCE); // complete
        assert!(!plic.seip(CONTEXT), "line low → stays quiet");
        assert_eq!(plic.read(CLAIM_OFF), 0, "nothing to claim");
    }

    #[test]
    fn claim_is_empty_when_nothing_pends() {
        let mut plic = Plic::new();
        route(&mut plic);
        assert_eq!(plic.read(CLAIM_OFF), 0, "id 0 = no interrupt");
    }

    #[test]
    fn highest_priority_source_wins() {
        let mut plic = Plic::new();
        // Enable sources 10 and 11 for the context; give 11 the higher priority.
        plic.write(THRESHOLD_OFF, 0);
        plic.write(ENABLE_OFF, (1 << 10) | (1 << 11));
        plic.write(10 * 4, 1);
        plic.write(11 * 4, 5);
        plic.set_source(10, true);
        plic.set_source(11, true);
        assert_eq!(plic.read(CLAIM_OFF), 11, "priority 5 beats priority 1");
    }

    #[test]
    fn enable_write_read_round_trips() {
        let mut plic = Plic::new();
        plic.write(ENABLE_OFF, 0xABCD);
        assert_eq!(plic.read(ENABLE_OFF), 0xABCD);
    }
}
