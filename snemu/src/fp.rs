//! Floating-point semantics: the parts of RV64F/D where the architecture does
//! **not** simply agree with the host FPU.
//!
//! Everything here is pure — no `Hart`, no bus — because these are the rules that
//! need enumerating rather than emulating. Ordinary arithmetic (`a + b`) is left to
//! Rust's `f32`/`f64`, which is the same IEEE-754 hardware QEMU runs on; there is
//! nothing to model. What *does* need modelling is where RISC-V and the host
//! deliberately differ:
//!
//! - **Canonical NaN.** RISC-V generates one canonical NaN rather than propagating
//!   an operand's payload; aarch64 and x86 both propagate.
//! - **NaN boxing.** A single held in a 64-bit register carries all-ones in its
//!   upper half; a register that isn't properly boxed reads as canonical NaN when
//!   used as a single.
//! - **Rounding modes.** Only round-to-nearest-even is implemented; anything else
//!   must fail loudly host-side rather than silently rounding the host's way. See
//!   `docs/floating-point-design.md` for why this asymmetry is deliberate.

use crate::decode::nan_box;

/// The canonical double-precision NaN: quiet, positive, zero payload. RISC-V
/// generates *this* whenever an operation produces a NaN, rather than propagating an
/// operand's payload the way the host FPU does.
pub(crate) const CANONICAL_NAN_D: u64 = 0x7ff8_0000_0000_0000;

/// The canonical single-precision NaN — the same value one width down.
pub(crate) const CANONICAL_NAN_S: u32 = 0x7fc0_0000;

/// Rounding-mode encodings, `instr[14:12]` on an arithmetic FP instruction.
///
/// The complete field is enumerated, not just the modes snemu implements: the gates
/// below return the *effective* mode on refusal, and a reader checking what a reported
/// mode number means should find the answer here rather than in the spec PDF. Hence
/// the allow — `RDN`/`RUP`/`RMM` are referenced by tests and by that reader, not by
/// production code, which matches them as "anything else".
#[allow(dead_code, reason = "complete spec field enumeration; see module docs")]
pub(crate) mod rm {
    /// Round to nearest, ties to even — the only mode snemu implements, and the
    /// only one a Rust compiler's output ever asks for.
    pub const RNE: u32 = 0b000;
    /// Round towards zero.
    pub const RTZ: u32 = 0b001;
    /// Round down (towards −∞).
    pub const RDN: u32 = 0b010;
    /// Round up (towards +∞).
    pub const RUP: u32 = 0b011;
    /// Round to nearest, ties to max magnitude.
    pub const RMM: u32 = 0b100;
    /// Use the dynamic mode in `fcsr.frm`.
    pub const DYN: u32 = 0b111;
}

/// A rounding mode snemu can actually apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rounding {
    /// Round to nearest, ties to even — the IEEE default, and what the host FPU does.
    NearestEven,
    /// Round toward zero (truncate). **Not optional:** `rustc` emits
    /// `fcvt.w.d …, rtz` for every float→int cast, because Rust's cast semantics are
    /// truncation, so refusing this mode would halt snemu at the first `as i32` in
    /// real guest code. (Measured from `rustc --emit asm` for
    /// `riscv64gc-unknown-none-elf`; it corrects `docs/floating-point-design.md`,
    /// which claimed RNE + DYN covered everything a Rust compiler emits.)
    TowardZero,
}

/// The **effective** rounding mode — `frm` resolved in if the instruction said `DYN`.
/// Returned as a bare value so both gates below report what the guest actually asked
/// for rather than the `DYN` placeholder it asked with; naming `DYN` in a diagnostic
/// would send a reader looking in the wrong place.
fn effective_mode(rm: u32, frm: u64) -> u32 {
    if rm == rm::DYN {
        u32::try_from(frm & 0b111).unwrap_or(rm::DYN)
    } else {
        rm
    }
}

/// The rounding gate for **arithmetic** (`fadd`, `fmul`, `fsqrt`, …), which snemu
/// evaluates with the host FPU and therefore only in round-to-nearest-even. Any other
/// effective mode is refused, and the refusal must stay loud: rounding the host's way
/// regardless would yield a *plausible* number that flows downstream, so `snemu diff`
/// would surface a distant symptom long after the cause. Gaps the differential oracle
/// can't attribute have to shout — contrast `fflags`, left unaccrued precisely because
/// a guest reading those diverges immediately and attributably.
pub(crate) fn arithmetic_rounding(rm: u32, frm: u64) -> Result<(), u32> {
    let effective = effective_mode(rm, frm);
    if effective == rm::RNE { Ok(()) } else { Err(effective) }
}

/// The rounding gate for **float→int conversion**, where snemu does the rounding
/// itself and so can honour more than one mode. Both modes real code asks for are
/// supported; the rest are refused as loudly as in [`arithmetic_rounding`].
pub(crate) fn conversion_rounding(rm: u32, frm: u64) -> Result<Rounding, u32> {
    match effective_mode(rm, frm) {
        rm::RNE => Ok(Rounding::NearestEven),
        rm::RTZ => Ok(Rounding::TowardZero),
        other => Err(other),
    }
}

/// Round `value` per `mode`, leaving it as a float — the shared first half of every
/// float→int conversion, before the range clamp.
pub(crate) fn round_for_conversion(value: f64, mode: Rounding) -> f64 {
    match mode {
        Rounding::NearestEven => value.round_ties_even(),
        Rounding::TowardZero => value.trunc(),
    }
}

/// Convert `value` to a signed integer of `bits` width, by RISC-V's rules.
///
/// Two deliberate divergences from Rust's `as`, which is otherwise identical
/// (saturating since 1.45):
///
/// - **NaN becomes the maximum positive value**, where Rust's `as` gives 0. LLVM
///   emits an explicit `feq.d` NaN test around every cast specifically to paper over
///   this, which is the strongest available evidence for the hardware's behaviour.
/// - Rounding is applied first, per the instruction's mode, rather than always
///   truncating.
pub(crate) fn to_signed(value: f64, mode: Rounding, bits: u32) -> i64 {
    let max = if bits == 64 { i64::MAX } else { (1i64 << (bits - 1)) - 1 };
    let min = if bits == 64 { i64::MIN } else { -(1i64 << (bits - 1)) };
    if value.is_nan() {
        return max;
    }
    let rounded = round_for_conversion(value, mode);
    if rounded >= max as f64 {
        return max;
    }
    if rounded <= min as f64 {
        return min;
    }
    rounded as i64
}

/// Convert `value` to an unsigned integer of `bits` width. NaN becomes the maximum
/// value (all ones); anything negative floors at zero.
pub(crate) fn to_unsigned(value: f64, mode: Rounding, bits: u32) -> u64 {
    let max = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    if value.is_nan() {
        return max;
    }
    let rounded = round_for_conversion(value, mode);
    if rounded <= 0.0 {
        return 0;
    }
    if rounded >= max as f64 {
        return max;
    }
    rounded as u64
}

/// A double-precision result's bits, with a NaN replaced by the canonical one.
/// Non-NaN values pass through bit-for-bit — including the sign of zero, which a
/// comparison-based implementation would lose.
pub(crate) fn canonicalise_d(value: f64) -> u64 {
    if value.is_nan() { CANONICAL_NAN_D } else { value.to_bits() }
}

/// A single-precision result's bits, with a NaN replaced by the canonical one.
pub(crate) fn canonicalise_s(value: f32) -> u32 {
    if value.is_nan() { CANONICAL_NAN_S } else { value.to_bits() }
}

/// Read an FP register as a single. A register that is **not** properly NaN-boxed
/// does not hold a single at all, so it reads as canonical NaN rather than having
/// its low half reinterpreted — the architecture refuses to invent an `f32` from
/// the bottom of some `f64`, and the wrong answer here would be a plausible number.
pub(crate) fn unbox_single(bits: u64) -> f32 {
    if bits >> 32 == 0xffff_ffff {
        f32::from_bits(bits as u32)
    } else {
        f32::from_bits(CANONICAL_NAN_S)
    }
}

/// Write a single result into an FP register: canonicalise a NaN, then NaN-box.
/// Both rules apply — one does not override the other.
pub(crate) fn box_single(value: f32) -> u64 {
    nan_box(canonicalise_s(value))
}

/// `fmin`/`fmax` for doubles, by the *architecture's* rules rather than Rust's.
/// `is_max` selects `fmax`.
///
/// Two places this differs from the obvious implementation:
///
/// - **NaN.** Exactly one NaN operand → return the other (the opposite of ordinary
///   arithmetic, where a NaN poisons the result). Both NaN → canonical NaN.
/// - **Signed zero.** RISC-V orders `−0.0` below `+0.0`, but they *compare equal* —
///   so `if a < b { a } else { b }` returns whichever operand came second and is
///   wrong half the time. The sign bit has to be consulted directly.
pub(crate) fn min_max_d(a: f64, b: f64, is_max: bool) -> u64 {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => return CANONICAL_NAN_D,
        (true, false) => return b.to_bits(),
        (false, true) => return a.to_bits(),
        (false, false) => {}
    }
    if a == b {
        // Equal covers ±0.0, where the sign decides. `is_sign_negative` is the only
        // thing that can distinguish them.
        let take_negative = !is_max;
        let pick_a = a.is_sign_negative() == take_negative;
        return if pick_a { a.to_bits() } else { b.to_bits() };
    }
    let pick_a = if is_max { a > b } else { a < b };
    if pick_a { a.to_bits() } else { b.to_bits() }
}

/// `fmin`/`fmax` for singles — same rules, one width down.
pub(crate) fn min_max_s(a: f32, b: f32, is_max: bool) -> u64 {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => return nan_box(CANONICAL_NAN_S),
        (true, false) => return nan_box(b.to_bits()),
        (false, true) => return nan_box(a.to_bits()),
        (false, false) => {}
    }
    if a == b {
        let take_negative = !is_max;
        let pick_a = a.is_sign_negative() == take_negative;
        return nan_box(if pick_a { a.to_bits() } else { b.to_bits() });
    }
    let pick_a = if is_max { a > b } else { a < b };
    nan_box(if pick_a { a.to_bits() } else { b.to_bits() })
}

/// Sign injection: `rs1`'s magnitude with a sign derived from `rs2`. `mode` is the
/// instruction's funct3 — 0 = `fsgnj` (rs2's sign), 1 = `fsgnjn` (inverted),
/// 2 = `fsgnjx` (the XOR of both signs, which is how `abs` is spelled as
/// `fsgnjx rd, x, x`).
///
/// A **bit operation, not arithmetic**: the magnitude passes through untouched, so a
/// NaN payload survives and no rounding mode applies. `sign_bit` lets the same logic
/// serve both widths.
pub(crate) fn sign_inject(a: u64, b: u64, mode: u32, sign_bit: u64) -> Option<u64> {
    let sign = match mode {
        0 => b & sign_bit,
        1 => !b & sign_bit,
        2 => (a ^ b) & sign_bit,
        _ => return None,
    };
    Some((a & !sign_bit) | sign)
}

/// A double-precision comparison, by funct3: 0 = `fle`, 1 = `flt`, 2 = `feq`.
/// `None` for an unknown selector.
///
/// **NaN is unordered**, so every comparison involving one is false — including
/// `feq(nan, nan)`. Rust's operators already agree; this exists to name the rule and
/// to keep the selector mapping in one testable place.
pub(crate) fn compare(a: f64, b: f64, selector: u32) -> Option<bool> {
    match selector {
        0 => Some(a <= b),
        1 => Some(a < b),
        2 => Some(a == b),
        _ => None,
    }
}

/// `fclass`'s ten-bit one-hot classification of a double.
///
/// Bit 8 vs 9 — signalling vs quiet NaN — is a distinction *only* `fclass` can
/// observe: arithmetic canonicalises every NaN to a quiet one, so this is the one
/// instruction that can tell a guest what it was really handed.
pub(crate) fn classify_d(bits: u64) -> u64 {
    let value = f64::from_bits(bits);
    let negative = bits >> 63 == 1;
    if value.is_nan() {
        // The MSB of the significand is the quiet bit.
        let quiet = bits & (1 << 51) != 0;
        return if quiet { 1 << 9 } else { 1 << 8 };
    }
    if value.is_infinite() {
        return if negative { 1 << 0 } else { 1 << 7 };
    }
    if value == 0.0 {
        return if negative { 1 << 3 } else { 1 << 4 };
    }
    if value.is_subnormal() {
        return if negative { 1 << 2 } else { 1 << 5 };
    }
    if negative { 1 << 1 } else { 1 << 6 }
}

/// `fclass`'s ten-bit one-hot classification of a **single**.
///
/// The same ten classes as [`classify_d`], decided at single precision — which is
/// the whole point, and the reason this is not `classify_d(widen(bits))`:
///
/// - a subnormal `f32` widens to a perfectly **normal** `f64`, so the widening
///   version would report class 2/5 as 1/6 — wrong for exactly the values a guest
///   calls `fclass` to detect;
/// - widening a **signalling** NaN quiets it, and bits 8 vs 9 are the one
///   distinction `fclass` exists to expose.
///
/// A non-boxed register reads as the canonical NaN, per [`unbox_single`] — that is
/// the architecture's rule for a single-precision read of a register holding a
/// double, not a shortcut.
pub(crate) fn classify_s(bits: u64) -> u64 {
    let single = unbox_single(bits);
    let raw = single.to_bits();
    let value = f64::from(single);
    let negative = raw >> 31 == 1;

    if single.is_nan() {
        // The MSB of the significand is the quiet bit — bit 22 at this width.
        let quiet = raw & (1 << 22) != 0;
        return if quiet { 1 << 9 } else { 1 << 8 };
    }
    if single.is_infinite() {
        return if negative { 1 << 0 } else { 1 << 7 };
    }
    if value == 0.0 {
        return if negative { 1 << 3 } else { 1 << 4 };
    }
    if single.is_subnormal() {
        return if negative { 1 << 2 } else { 1 << 5 };
    }
    if negative { 1 << 1 } else { 1 << 6 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `fclass.s` classifies at **single** precision. The tempting implementation —
    /// widen to `f64` and reuse `classify_d` — is wrong twice, and both wrongs are
    /// silent: a subnormal single widens to a normal double, and a signalling NaN is
    /// quieted on the way. Those are precisely the two classes a guest calls
    /// `fclass` to find out about.
    #[test]
    fn classify_s_decides_at_single_precision_not_by_widening() {
        let boxed = |bits: u32| 0xffff_ffff_0000_0000 | u64::from(bits);

        // Smallest positive subnormal single: normal as a double.
        let subnormal = boxed(0x0000_0001);
        assert_eq!(classify_s(subnormal), 1 << 5, "a subnormal single is class 5");
        assert_ne!(
            classify_s(subnormal),
            classify_d(f64::from(f32::from_bits(0x0000_0001)).to_bits()),
            "widening would call this normal",
        );

        // Signalling NaN: quiet bit clear.
        let signalling = boxed(0x7f80_0001);
        assert_eq!(classify_s(signalling), 1 << 8, "signalling NaN is class 8");
        let quiet = boxed(0x7fc0_0000);
        assert_eq!(classify_s(quiet), 1 << 9, "quiet NaN is class 9");
    }

    #[test]
    fn classify_s_covers_the_ordinary_classes() {
        let boxed = |value: f32| 0xffff_ffff_0000_0000 | u64::from(value.to_bits());

        assert_eq!(classify_s(boxed(f32::NEG_INFINITY)), 1 << 0);
        assert_eq!(classify_s(boxed(-1.5)), 1 << 1);
        assert_eq!(classify_s(boxed(-0.0)), 1 << 3);
        assert_eq!(classify_s(boxed(0.0)), 1 << 4);
        assert_eq!(classify_s(boxed(1.5)), 1 << 6);
        assert_eq!(classify_s(boxed(f32::INFINITY)), 1 << 7);
    }

    /// A register that is not NaN-boxed holds a double, and reading it as a single
    /// yields the canonical NaN — so `fclass.s` must report a quiet NaN rather than
    /// classifying the low half as if it were a float.
    #[test]
    fn classify_s_of_an_unboxed_register_is_a_quiet_nan() {
        assert_eq!(classify_s(1.5f64.to_bits()), 1 << 9);
    }

    // ---- canonical NaN ----------------------------------------------------

    /// RISC-V generates a **canonical** NaN — quiet, positive, zero payload —
    /// whenever an operation produces a NaN. The host does the opposite: it
    /// propagates an operand's payload. So a NaN that arrives with a payload must
    /// come out clean, or snemu's arithmetic diverges from the architecture in a
    /// way that shows up as an inexplicable bit difference much later.
    #[test]
    fn arithmetic_producing_nan_yields_canonical_nan_not_a_propagated_payload() {
        // An sNaN with a distinctive payload, as an operand.
        let payload_nan = f64::from_bits(0x7ff0_0000_dead_beef);
        assert_eq!(
            canonicalise_d(payload_nan + 1.0),
            CANONICAL_NAN_D,
            "the operand's payload must not survive into the result",
        );
        // 0 * inf, inf - inf: NaN generated from non-NaN operands.
        assert_eq!(canonicalise_d(0.0 * f64::INFINITY), CANONICAL_NAN_D);
        assert_eq!(canonicalise_d(f64::INFINITY - f64::INFINITY), CANONICAL_NAN_D);
    }

    /// The single-precision canonical NaN is the same idea one width down.
    #[test]
    fn single_precision_canonical_nan_is_distinct_from_the_double_one() {
        let payload_nan = f32::from_bits(0x7f80_dead);
        assert_eq!(canonicalise_s(payload_nan + 1.0), CANONICAL_NAN_S);
        assert_eq!(CANONICAL_NAN_S, 0x7fc0_0000);
        assert_eq!(CANONICAL_NAN_D, 0x7ff8_0000_0000_0000);
    }

    /// A non-NaN result passes through **bit-for-bit**. Canonicalising must not
    /// touch ordinary values, and in particular must preserve the sign of zero —
    /// `-0.0` and `0.0` compare equal, so a sloppy implementation that round-trips
    /// through a comparison would silently lose the sign bit.
    #[test]
    fn canonicalising_leaves_ordinary_values_untouched_including_negative_zero() {
        assert_eq!(canonicalise_d(1.5), 1.5f64.to_bits());
        assert_eq!(canonicalise_d(-0.0), (-0.0f64).to_bits());
        assert_ne!(canonicalise_d(-0.0), canonicalise_d(0.0), "sign of zero survives");
        assert_eq!(canonicalise_d(f64::INFINITY), f64::INFINITY.to_bits());
    }

    // ---- NaN boxing -------------------------------------------------------

    /// A properly boxed single reads back as itself.
    #[test]
    fn a_boxed_single_unboxes_to_its_value() {
        let bits = box_single(3.5);
        assert_eq!(bits >> 32, 0xffff_ffff, "boxed");
        assert_eq!(unbox_single(bits), 3.5);
    }

    /// **An improperly boxed register reads as canonical NaN** when used as a
    /// single. This is how RV64D stops the low half of some `f64` from being
    /// silently reinterpreted as a valid `f32`: the value isn't a single, so the
    /// architecture refuses to invent one. Without this the wrong answer is a
    /// plausible number, which is the worst kind.
    #[test]
    fn an_unboxed_register_reads_as_canonical_nan_when_used_as_a_single() {
        // 1.0f64's bits: a perfectly good double, not a boxed single.
        let double_bits = 1.0f64.to_bits();
        assert_ne!(double_bits >> 32, 0xffff_ffff, "precondition: not boxed");
        assert_eq!(
            unbox_single(double_bits).to_bits(),
            CANONICAL_NAN_S,
            "an unboxed register is not a single — it reads as canonical NaN",
        );
    }

    /// A single result is boxed on the way back into a register, and a NaN result is
    /// canonicalised *and* boxed — the two rules compose rather than one winning.
    #[test]
    fn a_nan_single_result_is_both_canonicalised_and_boxed() {
        let bits = box_single(f32::from_bits(0x7f80_dead) + 1.0);
        assert_eq!(bits, nan_box(CANONICAL_NAN_S));
    }

    // ---- rounding modes ---------------------------------------------------

    /// Round-to-nearest-even is what a Rust compiler emits (it emits `DYN`, and
    /// `frm` resets to 0), and it is the only mode snemu implements. Both spellings
    /// must be accepted: an explicit `rm = RNE`, and `rm = DYN` while `frm == 0`.
    #[test]
    fn nearest_even_is_accepted_both_explicitly_and_dynamically() {
        assert_eq!(arithmetic_rounding(rm::RNE, 0), Ok(()));
        assert_eq!(arithmetic_rounding(rm::DYN, 0), Ok(()));
    }

    /// Any other mode **fails loudly**, naming itself, rather than being rounded
    /// the host's way. A wrong rounding mode produces a plausible number that flows
    /// downstream, so `snemu diff` would report a distant symptom long after the
    /// cause — that class of gap must shout, not whisper. Note `DYN` with a non-zero
    /// `frm` is exactly as unsupported as an explicit non-RNE mode: the mode that
    /// matters is the *effective* one.
    ///
    /// `RTZ` is refused *here* — arithmetic is evaluated by the host FPU, which only
    /// rounds to nearest-even — even though [`conversion_rounding`] accepts it. The
    /// two gates differ because snemu does a conversion's rounding itself and cannot
    /// do an addition's.
    #[test]
    fn any_other_arithmetic_rounding_mode_is_reported_by_its_effective_value() {
        for mode in [rm::RTZ, rm::RDN, rm::RUP, rm::RMM] {
            assert_eq!(arithmetic_rounding(mode, 0), Err(mode), "explicit mode {mode} must be refused");
            // ...and the same mode reached dynamically, through frm.
            assert_eq!(
                arithmetic_rounding(rm::DYN, u64::from(mode)),
                Err(mode),
                "DYN with frm={mode} is the same unsupported mode",
            );
        }
    }

    /// **Conversion accepts `rtz` as well as `rne`** — it has to, because `rustc`
    /// emits `fcvt.w.d …, rtz` for every `as i32`. Modes snemu genuinely cannot
    /// apply are still refused by effective value.
    #[test]
    fn conversion_rounding_accepts_the_two_modes_real_code_emits() {
        assert_eq!(conversion_rounding(rm::RNE, 0), Ok(Rounding::NearestEven));
        assert_eq!(conversion_rounding(rm::RTZ, 0), Ok(Rounding::TowardZero));
        assert_eq!(conversion_rounding(rm::DYN, 0), Ok(Rounding::NearestEven));
        assert_eq!(conversion_rounding(rm::DYN, u64::from(rm::RTZ)), Ok(Rounding::TowardZero));
        for mode in [rm::RDN, rm::RUP, rm::RMM, 0b101, 0b110] {
            assert_eq!(conversion_rounding(mode, 0), Err(mode));
        }
    }

    /// The rounding modes differ in their answers, so picking the wrong one is a
    /// silently wrong number rather than a crash: 1.5 truncates to 1 but rounds to 2,
    /// and 2.5 rounds *down* to 2 under ties-to-even.
    #[test]
    fn the_two_conversion_modes_give_different_answers() {
        assert_eq!(round_for_conversion(1.5, Rounding::TowardZero), 1.0);
        assert_eq!(round_for_conversion(1.5, Rounding::NearestEven), 2.0);
        assert_eq!(round_for_conversion(2.5, Rounding::NearestEven), 2.0);
        assert_eq!(round_for_conversion(-1.9, Rounding::TowardZero), -1.0);
    }

    /// NaN → maximum positive, for both signednesses and both widths. Rust's `as`
    /// gives 0 here, so this is one of the few places snemu must *not* simply defer
    /// to the host.
    #[test]
    fn nan_converts_to_the_maximum_positive_integer() {
        assert_eq!(to_signed(f64::NAN, Rounding::TowardZero, 64), i64::MAX);
        assert_eq!(to_signed(f64::NAN, Rounding::TowardZero, 32), i64::from(i32::MAX));
        assert_eq!(to_unsigned(f64::NAN, Rounding::TowardZero, 64), u64::MAX);
        assert_eq!(to_unsigned(f64::NAN, Rounding::TowardZero, 32), u64::from(u32::MAX));
    }

    /// Out-of-range values saturate at the destination width's limits — including the
    /// 32-bit variants, whose limits are *not* the host register's.
    #[test]
    fn out_of_range_conversions_saturate_at_the_destination_width() {
        assert_eq!(to_signed(1e300, Rounding::TowardZero, 32), i64::from(i32::MAX));
        assert_eq!(to_signed(-1e300, Rounding::TowardZero, 32), i64::from(i32::MIN));
        assert_eq!(to_unsigned(-1.0, Rounding::TowardZero, 64), 0, "negative floors at zero");
        assert_eq!(to_unsigned(1e300, Rounding::TowardZero, 32), u64::from(u32::MAX));
        assert_eq!(to_signed(f64::INFINITY, Rounding::TowardZero, 64), i64::MAX);
        assert_eq!(to_signed(f64::NEG_INFINITY, Rounding::TowardZero, 64), i64::MIN);
    }

    /// The reserved encodings (5 and 6) are refused too — they are not RNE, and
    /// treating "not a mode I know" as "probably nearest-even" is the silent-wrong
    /// answer this whole rule exists to prevent.
    #[test]
    fn reserved_rounding_encodings_are_refused() {
        assert_eq!(arithmetic_rounding(0b101, 0), Err(0b101));
        assert_eq!(arithmetic_rounding(0b110, 0), Err(0b110));
    }
}
