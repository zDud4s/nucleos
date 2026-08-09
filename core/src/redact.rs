/// Removes credential-bearing and caller-controlled parts from an absolute URL before logging it.
///
/// Takes a string that IS a URL, not prose containing one: given `dial imaps://u:p@host failed` it
/// reads the scheme as `dial imaps`, rejects it for the space, and hands the line back unchanged.
/// `sidecar.rs`, its first caller, therefore splits a line into whitespace-separated tokens and
/// applies this to each. Any future caller with a whole log line to clean must do the same.
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
    use super::redact_url;
    use std::fs;
    use std::path::Path;

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
