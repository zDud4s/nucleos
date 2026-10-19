//! Chat commands (flow D, Anexo D D.8): the port of `shortcuts/router.go`. P0 holds the `Intent`
//! type and the placeholder router; A2 owns the real routing and `execute`.
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

/// Typed text. Pure. Placeholder: only `/pair <code>` is recognised, everything else is chat (A2).
pub fn route(text: &str) -> Intent {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("/pair") {
        let starts_word = rest.is_empty() || rest.starts_with(char::is_whitespace);
        let code = rest.trim();
        if starts_word && !code.is_empty() {
            return Intent::Pair(code.to_string());
        }
    }
    Intent::Chat(text.to_string())
}

/// Transcribed text: never a command. Placeholder that A2 replaces.
pub fn route_transcript(text: &str) -> Intent {
    Intent::Chat(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_recognises_only_pair() {
        assert_eq!(route("/pair 123456"), Intent::Pair("123456".to_string()));
        assert_eq!(route("/pair"), Intent::Chat("/pair".to_string()));
        assert_eq!(route("/pairing x"), Intent::Chat("/pairing x".to_string()));
        assert_eq!(route("hello"), Intent::Chat("hello".to_string()));
        assert_eq!(
            route_transcript("/pair 1"),
            Intent::Chat("/pair 1".to_string())
        );
    }
}
