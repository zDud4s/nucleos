//! Failure signature generation and normalisation helpers.

use super::*;

/// The stable identity and short human label of one failed gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureSignature {
    pub fingerprint: String,
    pub headline: String,
}

/// Turns the volatile output of a failed gate into the failure it describes.
pub fn failure_signature(output: &str) -> Option<FailureSignature> {
    if output.trim().is_empty() {
        return None;
    }

    let normalised: Vec<String> = output
        .lines()
        .map(normalise_line)
        .filter(|line| !line.is_empty())
        .collect();
    if normalised.is_empty() {
        return None;
    }

    let mut decisive = Vec::new();
    for line in &normalised {
        let lower = line.to_lowercase();
        if [
            "test result:",
            "running ",
            "finished ",
            "compiling ",
            "…[output truncated",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            continue;
        }
        if [
            "error",
            "failed",
            "panicked",
            "denied",
            "acesso negado",
            "cannot",
            "could not",
            "not found",
            "assert",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            decisive.push(line.as_str());
            if decisive.len() == 3 {
                break;
            }
        }
    }
    if decisive.is_empty() {
        decisive.push(normalised.last().expect("non-empty output has one line"));
    }

    let joined = decisive.join("\n");
    Some(FailureSignature {
        fingerprint: hex16(&Sha256::digest(joined.as_bytes())),
        headline: clip(decisive[0], 160),
    })
}

pub(super) fn normalise_line(line: &str) -> String {
    let line = replace_worktree_paths(line);
    let line = replace_numbered_ids(&line);
    let line = replace_line_columns(&line);
    let line = replace_durations(&line);
    collapse_horizontal_space(&line)
}

pub(super) fn replace_worktree_paths(line: &str) -> String {
    let mut normalised = line.to_owned();
    let mut cursor = 0;
    loop {
        let bytes = normalised.as_bytes();
        let mut found = None;
        let mut at = cursor;
        while at < bytes.len() {
            let numbered = bytes[at..].starts_with(b"run-") || bytes[at..].starts_with(b"job-");
            if numbered && at > 0 && matches!(bytes[at - 1], b'/' | b'\\') {
                let mut end = at + 4;
                let digit_start = end;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > digit_start && end < bytes.len() && matches!(bytes[end], b'/' | b'\\') {
                    found = Some((at, end));
                    break;
                }
            }
            at += 1;
        }
        let Some((segment, end)) = found else {
            break;
        };

        let mut start = segment;
        while start > 0 && !is_path_token_delimiter(normalised.as_bytes()[start - 1]) {
            start -= 1;
        }
        normalised.replace_range(start..end, "<worktree>");
        let replacement_end = start + "<worktree>".len();
        let mut token_end = replacement_end;
        while token_end < normalised.len()
            && !is_path_token_delimiter(normalised.as_bytes()[token_end])
        {
            token_end += 1;
        }
        let canonical_suffix = normalised[replacement_end..token_end].replace('\\', "/");
        normalised.replace_range(replacement_end..token_end, &canonical_suffix);
        cursor = replacement_end + canonical_suffix.len();
    }
    normalised
}

pub(super) fn is_path_token_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(byte, b'"' | b'\'' | b'(' | b')' | b'<' | b'>' | b'=' | b',')
}

pub(super) fn replace_numbered_ids(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalised = String::with_capacity(line.len());
    let mut copied_until = 0;
    let mut at = 0;
    while at < bytes.len() {
        let label = if bytes[at..].starts_with(b"run-") {
            Some("run-<n>")
        } else if bytes[at..].starts_with(b"job-") {
            Some("job-<n>")
        } else {
            None
        };
        let Some(label) = label else {
            at += 1;
            continue;
        };
        let mut end = at + 4;
        let digit_start = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == digit_start {
            at += 4;
            continue;
        }
        normalised.push_str(&line[copied_until..at]);
        normalised.push_str(label);
        copied_until = end;
        at = end;
    }
    normalised.push_str(&line[copied_until..]);
    normalised
}

pub(super) fn replace_line_columns(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalised = String::with_capacity(line.len());
    let mut copied_until = 0;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b':' {
            at += 1;
            continue;
        }
        let mut middle = at + 1;
        while middle < bytes.len() && bytes[middle].is_ascii_digit() {
            middle += 1;
        }
        if middle == at + 1 || middle == bytes.len() || bytes[middle] != b':' {
            at += 1;
            continue;
        }
        let mut end = middle + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == middle + 1 {
            at += 1;
            continue;
        }
        normalised.push_str(&line[copied_until..at]);
        normalised.push_str(":<l>:<c>");
        copied_until = end;
        at = end;
    }
    normalised.push_str(&line[copied_until..]);
    normalised
}

pub(super) fn replace_durations(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalised = String::with_capacity(line.len());
    let mut copied_until = 0;
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let mut end = at;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end < bytes.len()
            && bytes[end] == b'.'
            && end + 1 < bytes.len()
            && bytes[end + 1].is_ascii_digit()
        {
            end += 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
        }
        let suffix_end = if bytes[end..].starts_with(b"ms") {
            end + 2
        } else if bytes[end..].starts_with(b"s") {
            end + 1
        } else {
            at = end;
            continue;
        };
        if line[suffix_end..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
        {
            at = suffix_end;
            continue;
        }
        normalised.push_str(&line[copied_until..at]);
        normalised.push_str("<t>");
        copied_until = suffix_end;
        at = suffix_end;
    }
    normalised.push_str(&line[copied_until..]);
    normalised
}

pub(super) fn collapse_horizontal_space(line: &str) -> String {
    let mut normalised = String::with_capacity(line.len());
    let mut separating = false;
    for character in line.trim().chars() {
        if matches!(character, ' ' | '\t') {
            separating = !normalised.is_empty();
        } else {
            if separating {
                normalised.push(' ');
                separating = false;
            }
            normalised.push(character);
        }
    }
    normalised
}

pub(super) fn hex16(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(16);
    for byte in bytes.iter().take(8) {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}
