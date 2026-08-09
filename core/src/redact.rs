//! Removing sensitive spans from text, for two different readers.
//!
//! `redact_url` serves the log file. `redact_secrets` serves a model on the other side of a network
//! boundary (`egress.rs`). They share this module because they answer the same question — what in
//! this string must not be repeated — and keeping both here means the day a third caller needs one,
//! there is one place to look.
//!
//! Everything here is deterministic on purpose, and that is a design constraint rather than an
//! implementation detail. `egress.rs` is allowed to REFUSE when redaction cannot run, which is a
//! promise; a promise resting on a language model is not one, because "the model did not notice the
//! key" is indistinguishable from "there was no key". Detectors that are arithmetic and table
//! lookups fail in ways a test can pin down.

/// What a detector found, as a half-open byte range into the scanned string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Finding {
    pub start: usize,
    pub end: usize,
    /// The marker the span is replaced by, chosen so the reader still knows what KIND of thing was
    /// there. A model told `[IBAN]` can still answer "yes, they sent you an IBAN"; a model handed a
    /// blank cannot, and will guess.
    pub label: &'static str,
}

/// Replaces every deterministically-detectable secret in `input` with a labelled marker.
///
/// Overlapping findings are resolved by taking the earliest, then the longest — a PEM block that
/// happens to contain a base64 run must be redacted as the block, not sliced into pieces around its
/// interior.
pub(crate) fn redact_secrets(input: &str) -> String {
    let findings = scan_secrets(input);
    if findings.is_empty() {
        return input.to_owned();
    }

    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for finding in findings {
        if finding.start < cursor {
            continue;
        }
        out.push_str(&input[cursor..finding.start]);
        out.push_str(finding.label);
        cursor = finding.end;
    }
    out.push_str(&input[cursor..]);
    out
}

/// PURE: every secret-looking span in `input`, sorted by start and de-overlapped.
pub(crate) fn scan_secrets(input: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(pem_blocks(input));
    findings.extend(prefixed_tokens(input));
    findings.extend(json_web_tokens(input));
    findings.extend(checksummed_numbers(input));

    // Earliest first; on a tie the longest wins, so an enclosing block swallows what is inside it.
    findings.sort_by_key(|finding| (finding.start, std::cmp::Reverse(finding.end)));

    let mut kept: Vec<Finding> = Vec::with_capacity(findings.len());
    for finding in findings {
        if kept.last().is_some_and(|last| finding.start < last.end) {
            continue;
        }
        kept.push(finding);
    }
    kept
}

/// A private key pasted into a message, from its BEGIN line to its END line.
///
/// Matched as a whole block rather than by its base64 body: the body alone would also match the
/// generic high-entropy rule, but only the block carries the header that says what it is, and
/// redacting the body while leaving `-----BEGIN RSA PRIVATE KEY-----` in place tells a reader
/// exactly what they are missing without telling them anything useful.
fn pem_blocks(input: &str) -> Vec<Finding> {
    const BEGIN: &str = "-----BEGIN ";
    const END_MARK: &str = "-----END ";
    let mut findings = Vec::new();
    let mut from = 0;

    while let Some(relative) = input[from..].find(BEGIN) {
        let start = from + relative;
        // `continue` past this header rather than abandoning the scan. A malformed block early in a
        // message used to `break`, which silently gave up on every real key after it — the failure
        // mode where one bad input disarms the detector for the rest of the text.
        let Some(header_end) = input[start..]
            .find("-----\n")
            .or(input[start..].find("-----\r"))
        else {
            from = start + BEGIN.len();
            continue;
        };
        let header = &input[start + BEGIN.len()..start + header_end];
        if !header.contains("PRIVATE KEY") {
            from = start + BEGIN.len();
            continue;
        }

        // A truncated block — pasted without its footer, or cut by a length limit — is still a key,
        // and used to pass through whole. Falling back to the body's own extent covers it without
        // swallowing the rest of the message: the body ends where the base64 stops.
        let end = match input[start..].find(END_MARK) {
            Some(end_relative) => {
                let after_end = start + end_relative;
                input[after_end + END_MARK.len()..]
                    .find("-----")
                    .map_or(input.len(), |offset| {
                        after_end + END_MARK.len() + offset + "-----".len()
                    })
            }
            None => {
                // Past the `-----` that closes the header, not just up to it, or the marker lands
                // before the dashes and leaves them in the text.
                let body_start = start + header_end + "-----".len();
                body_start + base64_body_len(&input[body_start..])
            }
        };

        findings.push(Finding {
            start,
            end: end.min(input.len()),
            label: "[SECRET:private-key]",
        });
        from = end;
    }

    findings
}

/// How far a PEM body runs: consecutive lines made only of base64 characters.
///
/// Used only when the END line is missing, to bound a truncated block. Stopping at the first line
/// that is not base64 is what keeps a key with no footer from redacting the paragraph after it.
fn base64_body_len(input: &str) -> usize {
    /// PEM wraps at 64 characters, so a body line is long. The bound is what stops a sign-off being
    /// eaten: "Cumprimentos" and "Duarte" are punctuation-free single words and were being read as
    /// key material, which redacted the end of a message rather than the end of a key.
    const MIN_BODY_LINE: usize = 16;

    let mut consumed = 0;
    for line in input.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        // The header's own trailing newline is consumed before any body line is examined.
        if consumed == 0 && trimmed.is_empty() {
            consumed += line.len();
            continue;
        }

        let charset_ok = trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'));

        // Long OR encoded-looking, not long AND encoded-looking, and not long alone. Each of the
        // three was tried and two are wrong. Length alone eats a sign-off, because "Cumprimentos"
        // is a punctuation-free word. Length AND a digit breaks a real body, because a full-width
        // base64 line can be all letters. Length alone also stops at a body's LAST line, which is
        // short by construction — leaving the tail of a key in the text, which is the failure that
        // matters most here.
        let looks_encoded = trimmed
            .chars()
            .any(|c| c.is_ascii_digit() || matches!(c, '+' | '/' | '='));

        if !charset_ok || (trimmed.len() < MIN_BODY_LINE && !looks_encoded) {
            break;
        }
        consumed += line.len();
    }
    consumed
}

/// Credentials whose issuer stamped a recognisable prefix on them.
///
/// The prefix is what makes these safe to redact without a trigger word nearby: a string starting
/// `ghp_` followed by 36 token characters is a GitHub token or a deliberate imitation of one, and
/// neither belongs in a message crossing to a third party.
const PREFIXED: &[(&str, usize, &str)] = &[
    ("ghp_", 36, "[SECRET:github]"),
    ("gho_", 36, "[SECRET:github]"),
    ("ghu_", 36, "[SECRET:github]"),
    ("ghs_", 36, "[SECRET:github]"),
    ("ghr_", 36, "[SECRET:github]"),
    ("github_pat_", 22, "[SECRET:github]"),
    ("AKIA", 16, "[SECRET:aws]"),
    ("ASIA", 16, "[SECRET:aws]"),
    ("sk-ant-", 24, "[SECRET:anthropic]"),
    ("sk-", 20, "[SECRET:api-key]"),
    ("xoxb-", 10, "[SECRET:slack]"),
    ("xoxp-", 10, "[SECRET:slack]"),
    ("xoxa-", 10, "[SECRET:slack]"),
    ("xoxs-", 10, "[SECRET:slack]"),
    ("AIza", 35, "[SECRET:google]"),
];

fn prefixed_tokens(input: &str) -> Vec<Finding> {
    let mut findings = Vec::new();

    for (prefix, min_tail, label) in PREFIXED {
        let mut from = 0;
        while let Some(relative) = input[from..].find(prefix) {
            let start = from + relative;
            // A prefix in the middle of a longer word is not a token boundary: `basketball-sk-x`
            // must not arm the `sk-` rule.
            let boundary = start == 0
                || !input[..start]
                    .chars()
                    .next_back()
                    .is_some_and(is_token_character);
            let tail_start = start + prefix.len();
            let tail_len = input[tail_start..]
                .chars()
                .take_while(|character| is_token_character(*character))
                .map(char::len_utf8)
                .sum::<usize>();

            if boundary && tail_len >= *min_tail {
                findings.push(Finding {
                    start,
                    end: tail_start + tail_len,
                    label,
                });
            }
            from = tail_start;
        }
    }

    findings
}

fn is_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

/// A JSON Web Token: three base64url segments separated by dots, the first decoding to a JSON
/// object header. Only the `eyJ` opening is checked, which is `{"` in base64url — enough to
/// separate a token from three dot-separated words.
fn json_web_tokens(input: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut from = 0;

    while let Some(relative) = input[from..].find("eyJ") {
        let start = from + relative;
        let boundary = start == 0
            || !input[..start]
                .chars()
                .next_back()
                .is_some_and(is_token_character);
        let raw: usize = input[start..]
            .chars()
            .take_while(|character| is_token_character(*character) || *character == '.')
            .map(char::len_utf8)
            .sum();
        // Trailing dots belong to the sentence, not the token. Without this, a JWT ending a
        // sentence split into four segments, failed the three-segment test, and was published
        // whole — the detector defeated by a full stop.
        let candidate = input[start..start + raw].trim_end_matches('.');
        let run = candidate.len();
        let segments: Vec<&str> = candidate.split('.').collect();

        if boundary
            && segments.len() == 3
            && segments.iter().all(|segment| segment.len() >= 8)
            && segments
                .iter()
                .all(|segment| segment.chars().all(is_token_character))
        {
            findings.push(Finding {
                start,
                end: start + run,
                label: "[SECRET:jwt]",
            });
        }
        from = start + 3;
    }

    findings
}

/// Numbers that carry their own proof of being what they look like.
///
/// A check digit is what separates these from every other long number in a message. An order
/// reference and an IBAN are both digit runs; only one of them survives mod-97. That is why these
/// are enforced rather than merely observed: the false-positive rate is not a matter of judgement.
fn checksummed_numbers(input: &str) -> Vec<Finding> {
    /// An IBAN caps at 34 characters, which is four more groups than the longest real grouping.
    /// Bounding the search is what keeps a paragraph from being considered as one enormous number.
    const MAX_GROUPS: usize = 9;

    let groups = alphanumeric_groups(input);
    let mut findings: Vec<Finding> = Vec::new();

    for (index, (start, _)) in groups.iter().enumerate() {
        // Longest first: `4111 1111 1111 1111` must be found as one card, not as a prefix of it.
        let mut best: Option<Finding> = None;
        for count in (1..=MAX_GROUPS.min(groups.len() - index)).rev() {
            // Groups must be consecutive and separated by exactly one space; anything else — a
            // newline, a comma, two spaces — ends the candidate, because real groupings do not
            // straddle punctuation.
            if !single_spaced(input, &groups[index..index + count]) {
                continue;
            }
            let end = groups[index + count - 1].1;
            let compact: String = input[*start..end].chars().filter(|c| *c != ' ').collect();
            if compact.len() > 34 {
                continue;
            }

            let Some(label) = classify_number(&compact) else {
                continue;
            };
            // A check digit is necessary and, for two of these three, not sufficient. An IBAN
            // carries its own anchor — two letters and two check digits in fixed positions — so
            // mod-97 identifies it. Luhn and the NIF's mod-11 do not: roughly one 13-digit
            // millisecond timestamp in ten passes Luhn, and about one nine-digit integer in eleven
            // passes mod-11, so a byte count or an epoch was being replaced by `[CARD]` or `[NIF]`
            // in text on its way to a model trying to reason about it. Corrupting the numbers a
            // reader needs is a worse failure than missing a card written with no context, so those
            // two now need a word nearby that says what they are.
            if label != "[IBAN]" && !trigger_precedes(input, *start) {
                continue;
            }
            best = Some(Finding {
                start: *start,
                end,
                label,
            });
            break;
        }
        if let Some(finding) = best {
            findings.push(finding);
        }
    }

    findings
}

/// Words that say a number is a card or a taxpayer id rather than a measurement.
///
/// Portuguese and English, matching the trigger list the entropy rule uses, and with the same
/// known gap: a number announced in a third language is not caught. That is a miss, and a miss is
/// the direction this detector is allowed to fail in — the shadow pass is what will measure whether
/// it happens enough to matter.
const NUMBER_TRIGGERS: &[&str] = &[
    "card",
    "cartao",
    "cartão",
    // Portuguese plurals that are not the singular plus one character, so the rule in
    // `is_trigger_word` cannot reach them.
    "cartoes",
    "cartões",
    "visa",
    "mastercard",
    "amex",
    "nif",
    "contribuinte",
    "vat",
    "iban",
    "conta",
    "account",
    "number",
    "numero",
    "número",
];

/// How far back a trigger word may sit. Wide enough for "the card number ends ...", short enough
/// that a word in the previous sentence does not vouch for a number in this one.
const TRIGGER_WINDOW_CHARS: usize = 40;

/// Whether one of `NUMBER_TRIGGERS` appears as a WORD just before `start`.
///
/// Whole words, not substrings, and the difference is the whole rule rather than a nicety.
/// "contains" ends in "conta", "discarded" contains "card", and "private" contains "vat" — so a
/// substring test re-armed the exact corruption this gate exists to prevent, and did it on
/// sentences as ordinary as "the message contains 100000002 bytes".
fn trigger_precedes(input: &str, start: usize) -> bool {
    let before = &input[..start];
    let window_start = before
        .char_indices()
        .rev()
        .take(TRIGGER_WINDOW_CHARS)
        .last()
        .map_or(0, |(index, _)| index);

    before[window_start..]
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .any(is_trigger_word)
}

/// Whether one word is a trigger, allowing one trailing character for a plural.
///
/// Exact equality was the first correction and it was too tight: "cards", "contas", "accounts",
/// "numbers" and "IBANs" all stopped counting, so a labelled card written in the plural went out
/// unredacted. A bare prefix test is too loose in the other direction — "contains" starts with
/// "conta", which is how the substring version let a byte count be read as a taxpayer id.
///
/// One character is the whole difference between the two, and it is enough for the plural in both
/// languages while excluding every longer word that happens to begin with a trigger.
fn is_trigger_word(word: &str) -> bool {
    NUMBER_TRIGGERS.iter().any(|trigger| {
        word.len() >= trigger.len()
            && word.len() <= trigger.len() + 1
            && word.starts_with(trigger)
    })
}

/// PURE: what a compacted, space-free run of characters proves itself to be, if anything.
fn classify_number(compact: &str) -> Option<&'static str> {
    if is_iban(compact) {
        return Some("[IBAN]");
    }
    if !compact.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let digits: Vec<u32> = compact
        .chars()
        .filter_map(|c| c.to_digit(10))
        .collect::<Vec<_>>();
    if (13..=19).contains(&digits.len()) && luhn(&digits) {
        return Some("[CARD]");
    }
    if digits.len() == 9 && is_portuguese_nif(&digits) {
        return Some("[NIF]");
    }
    None
}

/// Every maximal run of ASCII alphanumerics, as half-open byte ranges.
fn alphanumeric_groups(input: &str) -> Vec<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut groups = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        groups.push((start, index));
    }

    groups
}

/// Whether consecutive groups are joined by exactly one space each.
fn single_spaced(input: &str, groups: &[(usize, usize)]) -> bool {
    groups
        .windows(2)
        .all(|pair| &input[pair[0].1..pair[1].0] == " ")
}

/// ISO 13616 mod-97: move the first four characters to the end, map letters to two-digit numbers,
/// and the whole thing must be congruent to 1 modulo 97.
fn is_iban(compact: &str) -> bool {
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let upper = compact.to_ascii_uppercase();
    let mut chars = upper.chars();
    let country: String = chars.by_ref().take(2).collect();
    if !country.chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if !upper[2..4].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if !upper[4..].chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }

    let rearranged: String = format!("{}{}", &upper[4..], &upper[..4]);
    let mut remainder: u32 = 0;
    for character in rearranged.chars() {
        let value = if character.is_ascii_digit() {
            character.to_digit(10).expect("checked ascii digit")
        } else {
            character as u32 - 'A' as u32 + 10
        };
        remainder = if value >= 10 {
            (remainder * 100 + value) % 97
        } else {
            (remainder * 10 + value) % 97
        };
    }
    remainder == 1
}

fn luhn(digits: &[u32]) -> bool {
    let mut sum = 0;
    for (index, digit) in digits.iter().rev().enumerate() {
        let mut value = *digit;
        if index % 2 == 1 {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
    }
    sum % 10 == 0
}

/// Portuguese NIF: nine digits, weighted 9..2, check digit closing the sum to a multiple of 11.
///
/// The leading digit is also constrained — a NIF starts 1, 2, 3, 5, 6, 8 or 9 — which matters here
/// because nine-digit runs are common and mod-11 alone lets roughly one in eleven of them through.
fn is_portuguese_nif(digits: &[u32]) -> bool {
    if digits.len() != 9 || !matches!(digits[0], 1 | 2 | 3 | 5 | 6 | 8 | 9) {
        return false;
    }
    let sum: u32 = digits[..8]
        .iter()
        .enumerate()
        .map(|(index, digit)| digit * (9 - index as u32))
        .sum();
    let remainder = sum % 11;
    let expected = if remainder < 2 { 0 } else { 11 - remainder };
    digits[8] == expected
}

/// Removes credential-bearing and caller-controlled parts from an absolute URL before logging it.
///
/// Future `email.rs` and `triage.rs` logging call sites that need to name a credentialed URL must
/// use this helper first; it exists before those callers so their safe shape is already available.
#[allow(
    dead_code,
    reason = "email.rs and triage.rs do not yet log credentialed URLs, but must use this helper when they do"
)]
pub(crate) fn redact_url(input: &str) -> String {
    let Some(scheme_end) = input.find("://") else {
        return strip_userinfo_from_non_url(input).unwrap_or_else(|| input.to_owned());
    };

    let scheme = &input[..scheme_end];
    if !is_url_scheme(scheme) {
        return strip_userinfo_from_non_url(input).unwrap_or_else(|| input.to_owned());
    }

    let remainder = &input[scheme_end + 3..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return strip_userinfo_from_non_url(input).unwrap_or_else(|| input.to_owned());
    }

    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.is_empty() {
        return strip_userinfo_from_non_url(input).unwrap_or_else(|| input.to_owned());
    }

    let tail = &remainder[authority_end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    format!("{scheme}://{host}{}", &tail[..path_end])
}

fn is_url_scheme(scheme: &str) -> bool {
    let mut chars = scheme.bytes();
    matches!(chars.next(), Some(byte) if byte.is_ascii_alphabetic())
        && chars.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn strip_userinfo_from_non_url(input: &str) -> Option<String> {
    let (userinfo, remainder) = input.rsplit_once('@')?;
    if userinfo.is_empty()
        || remainder.is_empty()
        || userinfo.bytes().any(|byte| byte.is_ascii_whitespace())
        || remainder.bytes().any(|byte| byte.is_ascii_whitespace())
        || remainder.contains("://")
    {
        return None;
    }

    Some(remainder.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{redact_secrets, redact_url};
    use std::fs;
    use std::path::Path;

    #[test]
    fn issuer_prefixed_credentials_are_replaced_by_a_labelled_marker() {
        for (input, marker) in [
            (
                "the token is ghp_abcdefghijklmnopqrstuvwxyz0123456789 ok",
                "[SECRET:github]",
            ),
            ("AKIAIOSFODNN7EXAMPLE", "[SECRET:aws]"),
            ("key sk-ant-api03-abcdefghijklmnopqrstuvwxyz", "[SECRET:anthropic]"),
            ("AIzaSyD-abcdefghijklmnopqrstuvwxyz01234", "[SECRET:google]"),
        ] {
            let redacted = redact_secrets(input);
            assert!(
                redacted.contains(marker),
                "{input:?} was not marked; got {redacted:?}"
            );
        }
    }

    /// A prefix inside a longer word is not a token, and the boundary check is the only thing
    /// standing between this rule and every message containing the letters `sk-`.
    #[test]
    fn a_prefix_inside_a_word_is_left_alone() {
        let input = "basketball-sk-not-a-key-just-words-here-really";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn a_private_key_block_is_redacted_whole() {
        let input = "here:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow-fake-body\n-----END RSA PRIVATE KEY-----\nbye";
        let redacted = redact_secrets(input);
        assert!(redacted.contains("[SECRET:private-key]"), "{redacted:?}");
        assert!(!redacted.contains("MIIEow"), "{redacted:?}");
        assert!(redacted.ends_with("bye"), "{redacted:?}");
    }

    #[test]
    fn a_json_web_token_is_redacted_and_three_dotted_words_are_not() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r";
        assert!(redact_secrets(jwt).contains("[SECRET:jwt]"));
        let prose = "read.the.manual";
        assert_eq!(redact_secrets(prose), prose);
    }

    /// The check digit is the whole argument for enforcing these rather than merely observing them:
    /// a reference number and an IBAN are both digit runs, and only one survives mod-97.
    #[test]
    fn numbers_that_prove_themselves_are_redacted_and_lookalikes_are_not() {
        assert!(redact_secrets("IBAN GB82 WEST 1234 5698 7654 32 please").contains("[IBAN]"));
        assert!(redact_secrets("DE89370400440532013000").contains("[IBAN]"));
        // One digit changed: same shape, fails mod-97, must survive untouched.
        let broken = "GB82 WEST 1234 5698 7654 33";
        assert_eq!(redact_secrets(broken), broken);

        assert!(redact_secrets("card 4111 1111 1111 1111 exp").contains("[CARD]"));
        assert!(redact_secrets("NIF 123456789").contains("[NIF]"));
        let not_a_nif = "123456788";
        assert_eq!(redact_secrets(not_a_nif), not_a_nif);
    }

    /// The false positives that would make this unusable in practice. A commit sha and a phone
    /// number are both long alphanumeric runs, and neither is a secret.
    #[test]
    fn ordinary_long_runs_are_not_mistaken_for_secrets() {
        for input in [
            "commit 9f2b1c4e8a7d6f5b3c2a1e0d9f8b7a6c5d4e3f21 landed",
            "call me on +351 912 345 678 tomorrow",
            "order 2026-08-08-000123456 shipped",
            "a plain sentence with no secrets at all",
        ] {
            assert_eq!(redact_secrets(input), input, "{input:?} was altered");
        }
    }

    /// The false positives the first version of this test was too kind to find. A check digit is
    /// necessary and not sufficient: roughly one 13-digit millisecond timestamp in ten passes Luhn,
    /// and about one nine-digit integer in eleven passes the NIF's mod-11. Replacing a byte count
    /// with `[NIF]` corrupts the number a model was asked to reason about, which is worse than
    /// missing a card nobody labelled.
    #[test]
    fn measurements_that_happen_to_pass_a_checksum_survive() {
        for input in [
            "run finished at 1786000000003 ms",
            "processed 100000002 bytes",
            "elapsed 1786000000003",
            "id 100000002 completed",
        ] {
            assert_eq!(
                redact_secrets(input),
                input,
                "{input:?} was redacted with nothing saying what the number is"
            );
        }
    }

    /// The other half: with a word nearby saying what it is, the same digits are redacted. Without
    /// this the fix above would be indistinguishable from deleting the rule.
    #[test]
    fn a_labelled_card_or_nif_is_still_redacted() {
        assert!(redact_secrets("card 4111 1111 1111 1111").contains("[CARD]"));
        assert!(redact_secrets("o NIF dele é 123456789").contains("[NIF]"));
        assert!(redact_secrets("Número de contribuinte: 123456789").contains("[NIF]"));
        // An IBAN carries its own anchor — country code and check digits — so it needs no word.
        assert!(redact_secrets("GB82 WEST 1234 5698 7654 32").contains("[IBAN]"));
    }

    /// A full stop is not part of a token. The detector used to be defeated by one.
    #[test]
    fn a_jwt_ending_a_sentence_is_still_redacted() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r";
        let redacted = redact_secrets(&format!("the token is {jwt}."));
        assert!(redacted.contains("[SECRET:jwt]"), "{redacted:?}");
        assert!(redacted.ends_with('.'), "{redacted:?}");
        assert!(!redacted.contains("dBjftJeZ"), "{redacted:?}");
    }

    /// A key pasted without its footer is still a key, and used to pass through whole.
    #[test]
    fn a_private_key_block_with_no_end_line_is_still_redacted() {
        let input = "here it is:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\nbG9uZ2Jhc2U2NA==\n\nand that is all";
        let redacted = redact_secrets(input);

        assert!(redacted.contains("[SECRET:private-key]"), "{redacted:?}");
        assert!(!redacted.contains("MIIEowIBAAKCAQEA"), "{redacted:?}");
        // Bounded by where the base64 stops, so the sentence after it survives.
        assert!(redacted.contains("and that is all"), "{redacted:?}");
    }

    /// The bound has to survive a sign-off, not just a blank line. "Cumprimentos" and "Duarte" are
    /// punctuation-free single words, which an earlier version read as key material — redacting the
    /// end of the message along with the end of the key.
    #[test]
    fn a_footerless_key_does_not_swallow_the_sign_off() {
        let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA0000\nObrigado\nCumprimentos\nDuarte";
        let redacted = redact_secrets(input);

        assert!(!redacted.contains("MIIEowIBAAKCAQEA"), "{redacted:?}");
        assert!(redacted.contains("Obrigado"), "{redacted:?}");
        assert!(redacted.ends_with("Duarte"), "{redacted:?}");
    }

    /// A plural still names the number. Exact word matching was the first correction and it was too
    /// tight the other way: "cards", "contas", "accounts" and "IBANs" all stopped counting, so a
    /// labelled card written in the plural went out unredacted.
    #[test]
    fn a_trigger_in_the_plural_still_vouches_for_a_number() {
        for input in [
            "please check these cards 4111 1111 1111 1111",
            "os NIFs 123456789 e outro",
            "accounts 100000002 and more",
        ] {
            assert_ne!(
                redact_secrets(input),
                input,
                "{input:?} names the number in the plural and was not redacted"
            );
        }
    }

    /// The tail of a key is still key material. A minimum line length alone stopped at a body's
    /// last line, which is short by construction, and left it in the text.
    #[test]
    fn the_short_last_line_of_a_key_body_is_redacted_with_the_rest() {
        let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAaaaa\nAbCdEf12==\n\nObrigado";
        let redacted = redact_secrets(input);

        assert!(!redacted.contains("AbCdEf12"), "{redacted:?}");
        assert!(redacted.ends_with("Obrigado"), "{redacted:?}");
    }

    /// The substring bug this gate had at first: "contains" ends in "conta", "discarded" contains
    /// "card", "private" contains "vat". Each re-armed the corruption the gate exists to prevent,
    /// on sentences as ordinary as these.
    #[test]
    fn a_trigger_inside_a_longer_word_does_not_vouch_for_a_number() {
        for input in [
            "the message contains 100000002 bytes",
            "discarded 1786000000003 rows",
            "private buffer 100000002 wide",
            "the account was 100000002",
        ] {
            let redacted = redact_secrets(input);
            let vouched = input.to_lowercase().split_whitespace().any(|word| {
                super::NUMBER_TRIGGERS
                    .contains(&word.trim_matches(|c: char| !c.is_alphanumeric()))
            });
            if vouched {
                assert_ne!(
                    redacted, input,
                    "{input:?} names the number and was not redacted"
                );
            } else {
                assert_eq!(redacted, input, "{input:?} was redacted by a substring match");
            }
        }
    }

    /// A malformed block used to `break`, abandoning the scan — so one bad header disarmed the
    /// detector for every real key after it.
    #[test]
    fn a_malformed_header_does_not_disarm_the_rest_of_the_scan() {
        let input = "-----BEGIN NOT A KEY\nthen: ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        assert!(redact_secrets(input).contains("[SECRET:github]"));
    }

    #[test]
    fn text_without_secrets_is_returned_unchanged() {
        let input = "Bom dia, envio em anexo o relatório de julho. Cumprimentos.";
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn redacts_credentialed_and_caller_controlled_url_parts() {
        for (input, secret) in [
            (
                "imap://user:password@mail.example.test:993/INBOX",
                "password",
            ),
            (
                "https://api.example.test/items?token=secret-token",
                "secret-token",
            ),
            (
                "https://example.test/page#private-section",
                "private-section",
            ),
            (
                "https://user:password@example.test:8443/path?token=secret-token#private-section",
                "password",
            ),
        ] {
            let redacted = redact_url(input);
            assert!(
                !redacted.contains(secret),
                "{input:?} leaked {secret:?} as {redacted:?}"
            );
        }
    }

    #[test]
    fn keeps_safe_url_parts_and_leaves_non_urls_unmistakably_non_urls() {
        assert_eq!(
            redact_url("https://example.test:8443/path/to/resource"),
            "https://example.test:8443/path/to/resource"
        );
        assert_eq!(redact_url("mail.example.test"), "mail.example.test");
        assert_eq!(redact_url("not a URL"), "not a URL");
        assert_eq!(
            redact_url("user:password@mail.example.test"),
            "mail.example.test"
        );
    }

    #[test]
    fn no_tracing_call_interpolates_mail_content() {
        let source_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let forbidden = ["body_excerpt", "body_text", "subject"];

        for entry in fs::read_dir(source_dir).expect("core source directory must be readable") {
            let entry = entry.expect("core source entry must be readable");
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
                || path.file_name().and_then(|name| name.to_str()) == Some("redact.rs")
            {
                continue;
            }

            let source = fs::read_to_string(&path).expect("core source file must be readable");
            if let Some((line, identifier)) = logging_interpolation(&source, &forbidden) {
                panic!(
                    "{}:{line} interpolates {identifier} in a tracing log call; redact mail content before logging",
                    path.display()
                );
            }
        }
    }

    fn logging_interpolation<'a>(
        source: &str,
        forbidden: &'a [&'a str],
    ) -> Option<(usize, &'a str)> {
        let lines: Vec<_> = source.lines().collect();
        let mut line_index = 0;

        while line_index < lines.len() {
            let line = lines[line_index];
            let Some(macro_start) = tracing_macro_start(line) else {
                line_index += 1;
                continue;
            };

            let mut depth = 0_i32;
            let mut started = false;
            for (offset, candidate) in lines[line_index..].iter().enumerate() {
                for character in candidate[if offset == 0 { macro_start } else { 0 }..].chars() {
                    if character == '(' {
                        depth += 1;
                        started = true;
                    } else if character == ')' {
                        depth -= 1;
                    }
                }

                if let Some(identifier) = forbidden
                    .iter()
                    .copied()
                    .find(|identifier| candidate.contains(identifier))
                {
                    return Some((line_index + offset + 1, identifier));
                }

                if started && depth == 0 {
                    break;
                }
            }

            line_index += 1;
        }

        None
    }

    fn tracing_macro_start(line: &str) -> Option<usize> {
        [
            "tracing::info!(",
            "tracing::warn!(",
            "tracing::error!(",
            "tracing::debug!(",
            "tracing::trace!(",
        ]
        .iter()
        .filter_map(|marker| line.find(marker))
        .min()
    }
}
