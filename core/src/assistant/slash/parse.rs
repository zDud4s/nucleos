//! Turning typed text into a slash invocation.
//!
//! Pure: the text goes in, a name and its arguments come out. Whether the name means anything is
//! the registry's question, which is why `/etc/x` and `//help` are told apart here by their SHAPE
//! alone: a path has a second slash in its first word, an escaped slash has one at the start.

use super::SlashError;

/// A typed `/name arguments` line, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The name as typed, case kept. The registry compares without it.
    pub name: String,
    /// Everything after the name, trimmed, quotes and inner line breaks kept. This is what
    /// `$ARGUMENTS` stands for.
    pub rest: String,
    /// `rest` split the way a simple shell would, quotes removed. This is what `$1..$9` read.
    pub argv: Vec<String>,
}

/// Whether `name` is `[A-Za-z0-9][A-Za-z0-9_.:-]*`.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

/// `Ok(None)` when the text is not a command at all: plain text, a lone slash, `//text`, or a
/// path such as `/etc/hosts`. Only an unclosed quote in the arguments is an error.
pub fn parse(text: &str) -> Result<Option<Parsed>, SlashError> {
    let Some(after) = text.trim_start().strip_prefix('/') else {
        return Ok(None);
    };
    let end = after.find(char::is_whitespace).unwrap_or(after.len());
    let name = &after[..end];
    if !is_name(name) {
        return Ok(None);
    }
    let rest = after[end..].trim();
    let argv = split(rest).map_err(|detail| SlashError::InvalidArgument {
        usage: format!("/{name} [arguments]"),
        detail,
    })?;
    Ok(Some(Parsed {
        name: name.to_string(),
        rest: rest.to_string(),
        argv,
    }))
}

/// Splits `rest` into arguments: whitespace separates, `"` and `'` group, and a backslash escapes
/// the quote that follows it. Any other backslash is literal, so a Windows path survives.
fn split(rest: &str) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    // A quoted empty string is an argument although it adds no characters.
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', _) if matches!(chars.peek(), Some('"' | '\'')) => {
                current.push(chars.next().unwrap_or(c));
                started = true;
            }
            (q, Some(open)) if q == open => quote = None,
            (q @ ('"' | '\''), None) => {
                quote = Some(q);
                started = true;
            }
            (c, None) if c.is_whitespace() => {
                if started {
                    argv.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (c, _) => {
                current.push(c);
                started = true;
            }
        }
    }
    if let Some(open) = quote {
        return Err(format!("a {open} was opened and never closed"));
    }
    if started {
        argv.push(current);
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::slash::SlashError;

    /// Parses `text` and expects a command.
    fn command(text: &str) -> Parsed {
        parse(text)
            .expect("text should parse")
            .expect("text should be a command")
    }

    #[test]
    fn parse_name_and_args() {
        let parsed = command("/review 12 now");
        assert_eq!(parsed.name, "review");
        assert_eq!(parsed.rest, "12 now");
        assert_eq!(parsed.argv, vec!["12".to_string(), "now".to_string()]);

        // No arguments: empty rest, empty argv; leading whitespace before the slash is ignored.
        let bare = command("  /help");
        assert_eq!(bare.name, "help");
        assert_eq!(bare.rest, "");
        assert!(bare.argv.is_empty());

        // The name ends at a line break too, and the rest is trimmed but keeps inner line breaks.
        let multi = command("/note first line\nsecond line  \n");
        assert_eq!(multi.name, "note");
        assert_eq!(multi.rest, "first line\nsecond line");
        assert_eq!(
            multi.argv,
            vec![
                "first".to_string(),
                "line".to_string(),
                "second".to_string(),
                "line".to_string()
            ]
        );

        let broken = command("/note\nbody");
        assert_eq!(broken.name, "note");
        assert_eq!(broken.rest, "body");

        // The case is kept as typed; the registry compares without it.
        assert_eq!(command("/Help").name, "Help");
    }

    #[test]
    fn parse_quotes_and_backslash() {
        let parsed = command(r#"/x "a b" 'c d' plain"#);
        assert_eq!(
            parsed.argv,
            vec!["a b".to_string(), "c d".to_string(), "plain".to_string()]
        );
        // `rest` is the raw text, quotes included.
        assert_eq!(parsed.rest, r#""a b" 'c d' plain"#);

        // A backslash escapes the next quote, inside quotes and outside them.
        let escaped = command(r#"/x "a \"b\" c" e\"f"#);
        assert_eq!(
            escaped.argv,
            vec![r#"a "b" c"#.to_string(), r#"e"f"#.to_string()]
        );

        // The other quote kind is literal inside a quoted argument.
        let mixed = command(r#"/x "it's fine""#);
        assert_eq!(mixed.argv, vec!["it's fine".to_string()]);
    }

    #[test]
    fn parse_unclosed_quote_is_invalid_argument() {
        for text in [r#"/x "abc"#, "/x 'abc", r#"/x ok "a b"#] {
            match parse(text) {
                Err(SlashError::InvalidArgument { .. }) => {}
                other => panic!("{text:?} should be an invalid argument, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_double_slash_is_not_a_command() {
        assert!(parse("//help").unwrap().is_none());
        assert!(parse("  //help me").unwrap().is_none());
        // Plain text and a lone slash are not commands either.
        assert!(parse("hello /help").unwrap().is_none());
        assert!(parse("/").unwrap().is_none());
        assert!(parse("").unwrap().is_none());
    }

    #[test]
    fn parse_path_is_not_a_command() {
        assert!(parse("/etc/x y").unwrap().is_none());
        assert!(parse("/etc/hosts is empty").unwrap().is_none());
        assert!(parse("/usr/bin").unwrap().is_none());
    }

    #[test]
    fn parse_name_charset() {
        // `[A-Za-z0-9][A-Za-z0-9_.:-]*`, ended by whitespace or the end of the text.
        for name in ["a", "A1", "ops:deploy", "a_b.c-d", "plugin:thing", "9lives"] {
            let parsed = command(&format!("/{name} x"));
            assert_eq!(parsed.name, name);
        }
        for text in [
            "/-x", "/_x", "/.x", "/:x", "/a!b", "/a/b", "/a@b c", "/é", "/ x",
        ] {
            assert!(
                parse(text).unwrap().is_none(),
                "{text:?} must not be a command"
            );
        }
    }
}
