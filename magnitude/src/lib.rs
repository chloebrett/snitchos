//! Human-scale integers: `1.2B` ⇄ `1_200_000_000`.
//!
//! Large counts are error-prone to read and to type as bare digit strings.
//! [`parse`] reads a decimal with an optional `K`/`M`/`B` magnitude suffix;
//! [`format`] renders one back the same way, so a printed number can be pasted
//! straight back into a flag that parses one.
//!
//! Suffixes are decimal (1K = 1000), not binary — this is for counting things,
//! not sizing memory.

/// Magnitude suffixes, largest first (the order [`format`] searches).
const SUFFIXES: [(char, u64); 3] = [('B', 1_000_000_000), ('M', 1_000_000), ('K', 1_000)];

/// Parse a count: bare digits (`1200000000`, `1_200_000_000`) or a decimal with
/// a `K`/`M`/`B` suffix (`400M`, `1.2B`). Case-insensitive. Fractions must land
/// on a whole unit — `1.5M` is 1_500_000, but `1.0000001M` is an error rather
/// than a silent round.
pub fn parse(text: &str) -> Result<u64, String> {
    let reject = || format!("invalid number `{text}` (expected e.g. `400M`, `1.2B`, `1200000000`)");

    let (digits, scale) = match SUFFIXES
        .iter()
        .find(|(suffix, _)| text.to_ascii_uppercase().ends_with(*suffix))
    {
        Some((_, scale)) => (&text[..text.len() - 1], *scale),
        None => (text, 1),
    };

    let digits = digits.replace('_', "");
    let (whole, fraction) = match digits.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (digits.as_str(), ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(reject());
    }
    if !fraction.is_empty() && (scale == 1 || !fraction.bytes().all(|b| b.is_ascii_digit())) {
        return Err(reject());
    }

    let whole: u64 = whole.parse().map_err(|_| reject())?;
    let units = whole.checked_mul(scale).ok_or_else(reject)?;

    // A fraction is `fraction / 10^len` of `scale`; exact only when its digits
    // divide evenly into the scale.
    let divisor = 10_u64.checked_pow(u32::try_from(fraction.len()).map_err(|_| reject())?);
    let extra = match (fraction.is_empty(), divisor) {
        (true, _) => 0,
        (false, Some(divisor)) if scale % divisor == 0 => {
            fraction.parse::<u64>().map_err(|_| reject())? * (scale / divisor)
        }
        _ => return Err(format!("`{text}` is finer than one whole unit")),
    };

    units.checked_add(extra).ok_or_else(reject)
}

/// Render a count the way [`parse`] reads one: `1_200_000_000` → `1.2B`.
/// Remainders show at most two decimals, so this is lossy for arbitrary values —
/// it is for reports, not for round-tripping an exact number.
pub fn format(value: u64) -> String {
    let Some(&(suffix, scale)) = SUFFIXES.iter().find(|(_, scale)| value >= *scale) else {
        return value.to_string();
    };

    let scaled = value as f64 / scale as f64;
    // Two decimals can round up to the next magnitude (999_999 → `1000K`); carry
    // into the bigger suffix instead.
    if scaled >= 999.995 && scale < SUFFIXES[0].1 {
        return format(scale * 1000);
    }

    let rendered = format!("{scaled:.2}");
    let trimmed = rendered.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::{format, parse};

    #[test]
    fn plain_digits_pass_through() {
        assert_eq!(parse("1200000000"), Ok(1_200_000_000));
    }

    #[test]
    fn underscores_separate_digit_groups() {
        assert_eq!(parse("1_200_000_000"), Ok(1_200_000_000));
    }

    #[test]
    fn suffixes_scale_by_magnitude() {
        assert_eq!(parse("400K"), Ok(400_000));
        assert_eq!(parse("400M"), Ok(400_000_000));
        assert_eq!(parse("2B"), Ok(2_000_000_000));
    }

    #[test]
    fn suffixes_are_case_insensitive() {
        assert_eq!(parse("400m"), Ok(400_000_000));
        assert_eq!(parse("1b"), Ok(1_000_000_000));
    }

    #[test]
    fn fractional_values_scale_to_whole_units() {
        assert_eq!(parse("1.2B"), Ok(1_200_000_000));
        assert_eq!(parse("1.5M"), Ok(1_500_000));
        assert_eq!(parse("0.25M"), Ok(250_000));
    }

    #[test]
    fn a_fraction_finer_than_one_unit_is_rejected() {
        assert!(parse("1.0000001M").is_err());
    }

    #[test]
    fn a_bare_fraction_needs_a_suffix() {
        assert!(parse("1.2").is_err());
    }

    #[test]
    fn junk_is_rejected() {
        assert!(parse("").is_err());
        assert!(parse("M").is_err());
        assert!(parse("12X").is_err());
        assert!(parse("1.2.3M").is_err());
        assert!(parse("-5M").is_err());
    }

    #[test]
    fn overflow_is_rejected_rather_than_wrapping() {
        assert!(parse("99999999999B").is_err());
    }

    #[test]
    fn small_counts_render_bare() {
        assert_eq!(format(0), "0");
        assert_eq!(format(912), "912");
    }

    #[test]
    fn whole_multiples_render_without_a_fraction() {
        assert_eq!(format(1_000), "1K");
        assert_eq!(format(400_000_000), "400M");
        assert_eq!(format(2_000_000_000), "2B");
    }

    #[test]
    fn remainders_render_to_two_decimals() {
        assert_eq!(format(1_200_000_000), "1.2B");
        assert_eq!(format(1_234_567), "1.23M");
        assert_eq!(format(1_500), "1.5K");
    }

    #[test]
    fn rounding_up_to_the_next_magnitude_carries_the_suffix() {
        assert_eq!(format(999_999), "1M");
    }

    #[test]
    fn rendering_a_round_value_round_trips_through_parse() {
        for value in [400_000_000_u64, 1_200_000_000, 1_500, 912] {
            assert_eq!(parse(&format(value)), Ok(value));
        }
    }
}
