//! Chat commands (flow D, Anexo D D.8): the port of `shortcuts/router.go`. P0 holds the `Intent`
//! type; A2 owns the real routing and `execute`.
//!
//! `execute`, `answer_capture` and `capture_target` take `ingest::Inbound` and `contract::InMessage`,
//! which do not exist yet; the chunk that adds those types adds the three functions too.

/// What a message asks for. Port of `shortcuts.Intent` plus the channel-only commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Help,
    Kill {
        on: bool,
    },
    Cancel,
    Budget,
    Proposals,
    Proj {
        filter: Option<String>,
    },
    Mail,
    Inbox,
    /// Empty = bare `/note`, answered with the usage line.
    Note(String),
    /// Recognised by `route`, but handled by `ingest` via `access::pair` before `admit`.
    Pair(String),
    Cap {
        job: i64,
        text: String,
    },
    /// `approve N` / `reject N`, and the degraded form of buttons.
    Approve(i64),
    Reject(i64),
    /// A command heard in a voice note: said back, never run.
    Refused(String),
    /// Not a command: goes to the turn.
    Chat(String),
}

/// Typed text. Pure. Commands match case-insensitively and in English only; their argument keeps
/// its own case. What matches nothing is `Chat` with the text unchanged.
pub fn route(text: &str) -> Intent {
    let trimmed = text.trim();
    let lowered = trimmed.to_lowercase();

    match lowered.as_str() {
        "/start" | "/help" => return Intent::Help,
        "/kill on" => return Intent::Kill { on: true },
        "/kill off" => return Intent::Kill { on: false },
        "/cancel" => return Intent::Cancel,
        "/budget" => return Intent::Budget,
        "/proposals" => return Intent::Proposals,
        "/proj" | "/projects" => return Intent::Proj { filter: None },
        "/mail" => return Intent::Mail,
        "/inbox" => return Intent::Inbox,
        _ => {}
    }

    // Prefix checks run against `trimmed`, never `lowered`: lowercasing can change byte length.
    if has_prefix_fold(trimmed, "/kill") {
        return Intent::Help;
    }
    if let Some(arg) = note_command(trimmed) {
        return Intent::Note(arg);
    }
    if has_prefix_fold(trimmed, "/proj ") {
        let arg = trimmed["/proj ".len()..].trim();
        return Intent::Proj {
            filter: Some(arg.to_string()),
        };
    }
    if let Some(code) = word_argument(trimmed, "/pair") {
        if !code.is_empty() {
            return Intent::Pair(code.to_string());
        }
    }
    if let Some(rest) = word_argument(trimmed, "/cap") {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let job = parts.next().and_then(|n| n.parse::<i64>().ok());
        let body = parts.next().map(str::trim).unwrap_or("");
        return match job {
            Some(job) if !body.is_empty() => Intent::Cap {
                job,
                text: body.to_string(),
            },
            // A malformed `/cap` is answered with the usage, like a malformed `/kill`.
            _ => Intent::Help,
        };
    }
    if let Some(intent) = approval(&lowered) {
        return intent;
    }

    Intent::Chat(text.to_string())
}

/// Transcribed text. A transcript is a guess over a noisy channel with no confirmation step, so
/// one that lands on a command is refused out loud (`Refused` carries the text as heard); anything
/// else is what voice notes are for and goes to the turn unchanged.
pub fn route_transcript(text: &str) -> Intent {
    match route(text) {
        Intent::Chat(_) => Intent::Chat(text.to_string()),
        _ => Intent::Refused(text.to_string()),
    }
}

/// `approve N` / `reject N`, already lowercased. Exactly two words, the second a whole number.
fn approval(lowered: &str) -> Option<Intent> {
    let mut words = lowered.split_whitespace();
    let verb = words.next()?;
    let n = words.next()?.parse::<i64>().ok()?;
    if words.next().is_some() {
        return None;
    }
    match verb {
        "approve" => Some(Intent::Approve(n)),
        "reject" => Some(Intent::Reject(n)),
        _ => None,
    }
}

/// `command` alone or followed by whitespace (case-insensitive): the trimmed rest, else `None`.
fn word_argument<'a>(trimmed: &'a str, command: &str) -> Option<&'a str> {
    if !has_prefix_fold(trimmed, command) {
        return None;
    }
    let rest = &trimmed[command.len()..];
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// Reads `/note` and `/note@SomeBot`, alone or followed by any whitespace (a newline included).
/// What it misses falls through as chat, and a note is the one text that must never go there.
fn note_command(trimmed: &str) -> Option<String> {
    const COMMAND: &str = "/note";
    if !has_prefix_fold(trimmed, COMMAND) {
        return None;
    }
    let mut rest = &trimmed[COMMAND.len()..];
    if rest.starts_with('@') {
        match rest.find(char::is_whitespace) {
            None => return Some(String::new()),
            Some(end) => rest = &rest[end..],
        }
    }
    if rest.is_empty() {
        return Some(String::new());
    }
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim().to_string())
}

fn has_prefix_fold(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(s: &str) -> Intent {
        Intent::Chat(s.to_string())
    }

    #[test]
    fn route_recognises_only_pair() {
        assert_eq!(route("/pair 123456"), Intent::Pair("123456".to_string()));
        assert_eq!(route("/pair"), chat("/pair"));
        assert_eq!(route("/pairing x"), chat("/pairing x"));
        assert_eq!(route("hello"), chat("hello"));
        assert_eq!(
            route_transcript("/pair 1"),
            Intent::Refused("/pair 1".to_string())
        );
    }

    #[test]
    fn route_help_and_slash_forms() {
        assert_eq!(route("/start"), Intent::Help);
        assert_eq!(route("  /help  "), Intent::Help);
        assert_eq!(route("/HELP"), Intent::Help);
        assert_eq!(route("/kill on"), Intent::Kill { on: true });
        assert_eq!(route("/KILL on"), Intent::Kill { on: true });
        assert_eq!(route("/Kill Off"), Intent::Kill { on: false });
        assert_eq!(route("/kill"), Intent::Help);
        assert_eq!(route("/kill maybe"), Intent::Help);
        assert_eq!(route("/Cancel"), Intent::Cancel);
        assert_eq!(route("/budget"), Intent::Budget);
        assert_eq!(route("/Proposals"), Intent::Proposals);
        assert_eq!(route("/proj"), Intent::Proj { filter: None });
        assert_eq!(route("/PROJECTS"), Intent::Proj { filter: None });
        assert_eq!(
            route("/proj nucleos"),
            Intent::Proj {
                filter: Some("nucleos".to_string())
            }
        );
        assert_eq!(
            route("/Proj NucleOS"),
            Intent::Proj {
                filter: Some("NucleOS".to_string())
            }
        );
        assert_eq!(route("/Mail"), Intent::Mail);
        assert_eq!(route("/INBOX"), Intent::Inbox);
        assert_eq!(route("/unknown"), chat("/unknown"));
        for former in ["/assunto carros", "/assuntos", "/pausa", "/retomar", "/fim"] {
            assert_eq!(route(former), chat(former));
        }
    }

    #[test]
    fn route_approve_and_reject_commands() {
        assert_eq!(route("approve 12"), Intent::Approve(12));
        assert_eq!(route("  Approve   7 "), Intent::Approve(7));
        assert_eq!(route("reject 3"), Intent::Reject(3));
        assert_eq!(route("REJECT 3"), Intent::Reject(3));
        assert_eq!(route("approve"), chat("approve"));
        assert_eq!(route("approve x"), chat("approve x"));
        assert_eq!(route("approve 1 2"), chat("approve 1 2"));
        assert_eq!(route("please approve 12"), chat("please approve 12"));
    }

    #[test]
    fn route_portuguese_approve_and_reject_are_plain_chat() {
        for text in ["aprovar 12", "rejeitar 12", "aprova 12", "rejeita 12"] {
            assert_eq!(route(text), chat(text));
        }
    }

    #[test]
    fn route_cap_note_and_pair() {
        assert_eq!(
            route("/cap 4 yes, go ahead"),
            Intent::Cap {
                job: 4,
                text: "yes, go ahead".to_string()
            }
        );
        assert_eq!(
            route("/CAP 4\nline one\nline two"),
            Intent::Cap {
                job: 4,
                text: "line one\nline two".to_string()
            }
        );
        assert_eq!(route("/cap"), Intent::Help);
        assert_eq!(route("/cap x hi"), Intent::Help);
        assert_eq!(route("/cap 4"), Intent::Help);
        assert_eq!(route("/pair 123456"), Intent::Pair("123456".to_string()));
        assert_eq!(route("/PAIR  ab12 "), Intent::Pair("ab12".to_string()));

        let note = |s: &str| Intent::Note(s.to_string());
        assert_eq!(route("/note buy milk"), note("buy milk"));
        assert_eq!(route("/NOTE  Buy Milk  "), note("Buy Milk"));
        assert_eq!(route("/Note a /kill b"), note("a /kill b"));
        assert_eq!(route("/note"), note(""));
        assert_eq!(route("  /Note  "), note(""));
        assert_eq!(
            route("/note\nline one\nline two"),
            note("line one\nline two")
        );
        assert_eq!(route("/note\tbuy milk"), note("buy milk"));
        assert_eq!(route("/note@NucleosBot buy milk"), note("buy milk"));
        assert_eq!(route("/note@NucleosBot\nbuy milk"), note("buy milk"));
        assert_eq!(route("/note@NucleosBot"), note(""));
        assert_eq!(route("/notes for the agent"), chat("/notes for the agent"));
    }

    #[test]
    fn route_transcript_never_a_command() {
        for text in [
            "/kill off",
            "/KILL OFF",
            "/note buy milk",
            "approve 12",
            "/cap 1 yes",
        ] {
            assert_eq!(route_transcript(text), Intent::Refused(text.to_string()));
        }
        assert_eq!(
            route_transcript("cancel my last turn please"),
            chat("cancel my last turn please")
        );
    }

    #[test]
    fn route_plain_text_is_chat() {
        assert_eq!(route("  ask the agent  "), chat("  ask the agent  "));
        assert_eq!(route(""), chat(""));
        assert_eq!(route("héllo wörld"), chat("héllo wörld"));
        assert_eq!(route("ß"), chat("ß"));
    }
}
