//! Redaction pipeline for text shown to the judge.

use super::*;

/// What replaces a value the judge's state must not carry.
pub(super) const JUDGE_MARKER: &str = "[REDACTED]";

/// Key-name fragments (lowercase, `-` folded to `_`) that mark an assignment's value as a secret.
pub(super) const SECRET_KEY_PARTS: [&str; 11] = [
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "api_key",
    "apikey",
    "auth",
    "credential",
    "private_key",
    "access_key",
];

/// Authorization schemes that precede the opaque credential.
pub(super) const AUTH_SCHEMES: [&str; 7] = [
    "bearer",
    "basic",
    "token",
    "digest",
    "negotiate",
    "ntlm",
    "apikey",
];

/// Spec A D9 (`2026-09-26-autopilot-modo-juiz-design.md`) asks for the state to be redacted.
/// `redact_secrets` recognises issuer-shaped tokens only, and this is data leaving the machine, so
/// the judge adds the shapes a shell line carries secrets in (named assignments, authorization
/// headers, URL credentials) and the owner's home path. A false positive costs the judge some
/// context, never a leak.
pub fn redact_for_judge(text: &str) -> String {
    let home = crate::commands::home();
    redact_for_judge_with_home(text, home.as_deref())
}

/// `redact_for_judge` with the home directory handed in, so a test does not depend on the machine.
pub(super) fn redact_for_judge_with_home(text: &str, home: Option<&Path>) -> String {
    let text = crate::governance::redact::redact_secrets(text);
    let text = match home {
        Some(home) => redact_home(&text, home),
        None => text,
    };
    let text = redact_url_userinfo(&text);
    let text = redact_auth_headers(&text);
    redact_assignments(&text)
}

pub(super) fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// A quote at `at` as it reads after any level of escaping: a run of backslashes, then `"` or `'`.
/// Returns the run's length (backslashes plus the quote) and the quote.
///
/// The tool input reaches the judge JSON-serialized (`clean_tool_input`), so a shell `"` arrives
/// as `\"`, and one inside a double-quoted shell string as `\\\"`. Reading only a bare `"` as a
/// quote let `{\"api_key\": \"v\"}` and `KEY=\"v\"` through with the value intact.
pub(super) fn quote_at(chars: &[char], at: usize) -> Option<(usize, char)> {
    let mut end = at;
    while chars.get(end) == Some(&'\\') {
        end += 1;
    }
    match chars.get(end) {
        Some(&quote) if quote == '"' || quote == '\'' => Some((end + 1 - at, quote)),
        _ => None,
    }
}

/// The end of an unquoted value: whitespace, a quote (escaped or not), or a delimiter that closes
/// it in JSON/shell.
pub(super) fn bare_value_end(chars: &[char], from: usize) -> usize {
    let mut end = from;
    while end < chars.len()
        && !chars[end].is_whitespace()
        && !matches!(chars[end], '"' | '\'' | ',' | ';' | '}' | ')' | '&')
        && !(chars[end] == '\\' && quote_at(chars, end).is_some())
    {
        end += 1;
    }
    end
}

/// The end (exclusive) of the value starting at `from`: a quoted string including its quotes, or a
/// bare run.
///
/// A quote opened as `\"` closes at a `"` behind exactly as many backslashes or fewer; a quote
/// behind more is escaped one level deeper and belongs to the value. Unclosed, the value runs to the
/// end of the text: redacting too much costs the judge context, too little leaks.
pub(super) fn value_end(chars: &[char], from: usize) -> usize {
    let Some((open, quote)) = quote_at(chars, from) else {
        return bare_value_end(chars, from);
    };
    let slashes = open - 1;
    let body = from + open;
    for (end, &character) in chars.iter().enumerate().skip(body) {
        if character != quote {
            continue;
        }
        let mut run = end;
        while run > body && chars[run - 1] == '\\' {
            run -= 1;
        }
        if end - run <= slashes {
            return end + 1;
        }
    }
    chars.len()
}

pub(super) fn skip_ws(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    at
}

/// `KEY=v`, `KEY: v`, `"key": "v"`, `$env:KEY = 'v'` and `--key v` for a secret-named key.
pub(super) fn redact_assignments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if !is_key_char(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let key_start = i;
        let mut key_end = i;
        while key_end < chars.len() && is_key_char(chars[key_end]) {
            key_end += 1;
        }
        let key: String = chars[i..key_end].iter().collect();
        out.push_str(&key);
        i = key_end;
        let folded = key.to_ascii_lowercase().replace('-', "_");
        // `[SECRET:github]` is a label `redact_secrets` already put there, not a key.
        if key_start > 0 && chars[key_start - 1] == '[' {
            continue;
        }
        // Headers are the next pass's business, and it keeps the scheme word readable.
        if folded.contains("authorization")
            || !SECRET_KEY_PARTS.iter().any(|part| folded.contains(part))
        {
            continue;
        }
        let mut at = i;
        if let Some((quote, _)) = quote_at(&chars, at) {
            at += quote;
        }
        let value_start = match chars.get(at) {
            Some('=' | ':') => Some(skip_ws(&chars, at + 1)),
            Some(c) if c.is_whitespace() => {
                let next = skip_ws(&chars, at);
                match chars.get(next) {
                    Some('=') => Some(skip_ws(&chars, next + 1)),
                    // `--token x`: a flag takes its value as the next word.
                    Some(c) if key.starts_with("--") && *c != '-' => Some(next),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(start) = value_start else { continue };
        let end = value_end(&chars, start);
        if end == start {
            continue;
        }
        out.extend(&chars[i..start]);
        out.push_str(JUDGE_MARKER);
        i = end;
    }
    out
}

/// `Authorization: Bearer x`, `Authorization: Basic x`, `-H "Authorization: token x"`: the scheme
/// stays, the credential goes.
pub(super) fn redact_auth_headers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let name: Vec<char> = "authorization".chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if lower[i..].starts_with(&name) {
            let mut at = i + name.len();
            if let Some((quote, _)) = quote_at(&chars, at) {
                at += quote;
            }
            at = skip_ws(&chars, at);
            if matches!(chars.get(at), Some(':' | '=')) {
                let mut word_start = skip_ws(&chars, at + 1);
                // `{\"Authorization\": \"Bearer x\"}`: the value's own opening quote, escaped.
                if let Some((quote, _)) = quote_at(&chars, word_start) {
                    word_start += quote;
                }
                let word_end = bare_value_end(&chars, word_start);
                let word: String = chars[word_start..word_end].iter().collect();
                let secret_start = if AUTH_SCHEMES.contains(&word.to_ascii_lowercase().as_str()) {
                    skip_ws(&chars, word_end)
                } else {
                    word_start
                };
                let secret_end = bare_value_end(&chars, secret_start);
                if secret_end > secret_start {
                    out.extend(&chars[i..secret_start]);
                    out.push_str(JUDGE_MARKER);
                    i = secret_end;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `https://user:pass@host/` becomes `https://[REDACTED]@host/`; a bare `user@` carries no secret.
pub(super) fn redact_url_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(found) = rest.find("://") {
        let authority_start = found + 3;
        out.push_str(&rest[..authority_start]);
        rest = &rest[authority_start..];
        let authority_end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\''))
            .unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        match authority.rfind('@') {
            Some(at) if authority[..at].contains(':') => {
                out.push_str(JUDGE_MARKER);
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        rest = &rest[authority_end..];
    }
    out.push_str(rest);
    out
}

/// The home directory, in any spelling (`C:\Users\X`, `C:/Users/X`, `/c/Users/X`, JSON-escaped
/// backslashes, any case), becomes `~`. A longer name sharing the prefix is left alone.
pub(super) fn redact_home(text: &str, home: &Path) -> String {
    let raw = home.to_string_lossy().replace('\\', "/");
    let raw = raw.trim_end_matches('/').to_ascii_lowercase();
    if raw.chars().count() < 3 {
        return text.to_owned();
    }
    let mut needles = vec![raw.chars().collect::<Vec<char>>()];
    let bytes = raw.as_bytes();
    if bytes.len() > 3 && bytes[1] == b':' && bytes[2] == b'/' && bytes[0].is_ascii_alphabetic() {
        let msys = format!("/{}/{}", bytes[0] as char, &raw[3..]);
        needles.push(msys.chars().collect());
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = needles.iter().find_map(|n| match_path(&chars, i, n))
            && !chars.get(end).is_some_and(|c| is_key_char(*c))
        {
            out.push('~');
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Matches `needle` (lowercase, `/` separators) at `at`, where `/` also accepts a run of `\`.
pub(super) fn match_path(chars: &[char], at: usize, needle: &[char]) -> Option<usize> {
    let mut j = at;
    for &c in needle {
        if c == '/' {
            match chars.get(j) {
                Some('/') => j += 1,
                Some('\\') => {
                    while chars.get(j) == Some(&'\\') {
                        j += 1;
                    }
                }
                _ => return None,
            }
        } else if chars.get(j)?.to_ascii_lowercase() == c {
            j += 1;
        } else {
            return None;
        }
    }
    Some(j)
}
