//! Node `timeout` parsing (M4 C3).
//!
//! A command node may carry a wall-clock ceiling written as one positive integer and one unit
//! suffix: `90s`, `30m`, `2h`. Anything richer (`1h30m`, `1.5h`) is refused rather than guessed at:
//! a ceiling that parses to something other than what the author meant is worse than a refusal at
//! install time.

use std::time::Duration;

/// The longest ceiling a node may ask for. Equal to `job::COMMISSIONED_JOB_LIFETIME`: a node that
/// could outlive the job around it would be a ceiling that never fires.
pub const MAX_NODE_TIMEOUT: Duration = Duration::from_secs(12 * 3600);

/// `30m`, `2h`, `90s`: ONE unit suffix `s|m|h` after a positive integer.
///
/// Refuses `""`, `"30"` (no unit), `"0m"`, `"1.5h"`, `"1h30m"`, `"-5m"` and anything over
/// [`MAX_NODE_TIMEOUT`].
pub fn parse(src: &str) -> Result<Duration, String> {
    let text = src.trim();
    if text.is_empty() {
        return Err("a timeout is empty; write it like `30m`, `2h` or `90s`".to_string());
    }
    let unit = text.chars().last().unwrap_or(' ');
    let digits = &text[..text.len() - unit.len_utf8()];
    let multiplier: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => {
            return Err(format!(
                "timeout `{text}` has no unit; write it like `30m`, `2h` or `90s`"
            ));
        }
    };
    // Digits only: this rules out a sign, a fraction and a second unit (`1h30m` leaves `1h30`).
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "timeout `{text}` must be one positive whole number and one unit (`s`, `m` or `h`), like `30m`"
        ));
    }
    let amount: u64 = digits
        .parse()
        .map_err(|_| format!("timeout `{text}` is too large"))?;
    if amount == 0 {
        return Err(format!(
            "timeout `{text}` is zero; a ceiling must be positive"
        ));
    }
    let seconds = amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("timeout `{text}` is too large"))?;
    let parsed = Duration::from_secs(seconds);
    if parsed > MAX_NODE_TIMEOUT {
        return Err(format!(
            "timeout `{text}` is over the longest a node may ask for (12h)"
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirty_minutes_two_hours_ninety_seconds_parse() {
        assert_eq!(parse("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse("12h"), Ok(MAX_NODE_TIMEOUT));
    }

    #[test]
    fn a_bare_number_has_no_unit() {
        let error = parse("30").unwrap_err();
        assert!(error.contains("unit"), "{error}");
        assert!(parse("").is_err());
    }

    #[test]
    fn zero_negative_fractional_and_compound_are_refused() {
        for bad in ["0m", "0s", "-5m", "1.5h", "1h30m", "m", "+5m", "5 m", "5d"] {
            assert!(parse(bad).is_err(), "`{bad}` must be refused");
        }
    }

    #[test]
    fn over_twelve_hours_is_refused() {
        assert!(parse("13h").is_err());
        assert!(parse("721m").is_err());
        assert!(parse("43201s").is_err());
        assert!(parse("99999999999999999999h").is_err());
    }
}
