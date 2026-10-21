//! The normalised model the núcleo hands the shell. No type here names a harness (spec §3).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::limits::LINE_CHARS;
use crate::governance::redact::redact_secrets;

pub type Timestamp = DateTime<Utc>;
pub type NodeId = String;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeKey {
    pub harness: String,
    pub native_id: String,
}

impl NodeKey {
    pub fn new(harness: &str, native_id: &str) -> Self {
        Self {
            harness: harness.to_owned(),
            native_id: native_id.to_owned(),
        }
    }

    pub fn id(&self) -> NodeId {
        format!("{}:{}", self.harness, self.native_id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Busy,
    Idle,
    Done,
    Lost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub stop: bool,
    pub open_run: bool,
    pub full_transcript: bool,
    pub pin: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetNode {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub root_id: NodeId,
    pub depth: u32,
    pub harness: String,
    pub model: Option<String>,
    pub role: Option<String>,
    pub label: String,
    pub state: NodeState,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub pinned: bool,
    pub done_omitted: u32,
    pub project: Option<String>,
    pub run_id: Option<i64>,
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub node_id: NodeId,
    pub last_activity_at: Timestamp,
    pub context_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    Prompt,
    Text,
    Tool,
    Result,
    Error,
    Status,
    Final,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermLine {
    pub kind: TermKind,
    pub text: String,
}

/// Whitespace runs and newlines become one space and the ends are trimmed (contract amendment 7).
/// `TermLine::new` and the adapters' transcript builders both use it, so the uncut transcript text
/// and the cut terminal line collapse identically.
pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The one cleaning order for any short text that leaves an adapter (contract amendment 18):
/// collapse whitespace, REDACT, then cut to `max_chars` characters on a char boundary, ending in
/// '…' when cut. Redacting before cutting means a token straddling the cut can never survive
/// half-redacted. `TermLine::new` is this with `LINE_CHARS`; adapters build every
/// `Sighting.label` through it. The hub redacts again (amendment 10) as defence in depth.
pub fn clean_label(s: &str, max_chars: usize) -> String {
    // collapse -> redact -> cut (contract amendment 18).
    let redacted = redact_secrets(&collapse_ws(s));
    if redacted.chars().count() > max_chars {
        // One character is spent on the ellipsis, so the result is never longer than the limit.
        let mut cut: String = redacted.chars().take(max_chars.saturating_sub(1)).collect();
        cut.truncate(cut.trim_end().len());
        cut.push('…');
        cut
    } else {
        redacted
    }
}

impl TermLine {
    /// `clean_label(text, LINE_CHARS)`. Adapters ALWAYS build lines through this.
    pub fn new(kind: TermKind, text: impl AsRef<str>) -> Self {
        Self {
            kind,
            text: clean_label(text.as_ref(), LINE_CHARS),
        }
    }
}

/// One entry of the full transcript (side panel). `text` is the uncut summary line (whitespace
/// collapsed, no 200-char cut); `detail` is the full tool input / tool result, when there is one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub kind: TermKind,
    pub text: String,
    pub detail: Option<String>,
}

/// Opaque to the shell; adapters use a byte offset.
pub type Cursor = String;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPage {
    /// Oldest first.
    pub entries: Vec<TranscriptEntry>,
    /// `None` means nothing earlier.
    pub before: Option<Cursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_runs_and_newlines_collapse_to_one_space() {
        let line = TermLine::new(TermKind::Text, "  a \n\t b\r\n\r\n  c  ");
        assert_eq!(line.text, "a b c");
        assert_eq!(line.kind, TermKind::Text);
    }

    #[test]
    fn a_line_of_exactly_the_limit_is_not_cut() {
        let text = "a".repeat(LINE_CHARS);
        assert_eq!(TermLine::new(TermKind::Text, &text).text, text);
    }

    #[test]
    fn a_longer_line_is_cut_to_the_limit_and_ends_in_an_ellipsis() {
        let line = TermLine::new(TermKind::Text, "a".repeat(LINE_CHARS + 1));
        assert_eq!(line.text.chars().count(), LINE_CHARS);
        assert!(line.text.ends_with('…'));
        assert!(line.text.starts_with("aaaa"));
    }

    #[test]
    fn the_cut_lands_on_a_char_boundary_for_multibyte_text() {
        // 'é' is two bytes and '😀' four: a byte cut at 200 would panic or split one.
        for ch in ['é', '😀', '日'] {
            let line = TermLine::new(TermKind::Tool, ch.to_string().repeat(300));
            assert_eq!(line.text.chars().count(), LINE_CHARS, "{ch}");
            assert!(line.text.ends_with('…'));
        }
    }

    #[test]
    fn a_space_just_before_the_cut_does_not_leave_a_dangling_gap() {
        let line = TermLine::new(
            TermKind::Text,
            format!("{} {}", "a".repeat(LINE_CHARS - 2), "b".repeat(50)),
        );
        assert_eq!(line.text, format!("{}…", "a".repeat(LINE_CHARS - 2)));
    }

    #[test]
    fn collapse_happens_before_the_cut() {
        // 300 raw characters that are 149 after collapsing: not cut.
        let raw = "ab \n\n\n".repeat(50);
        assert_eq!(TermLine::new(TermKind::Text, raw).text.chars().count(), 149);
    }

    #[test]
    fn collapse_ws_joins_the_words_with_single_spaces_and_trims() {
        assert_eq!(collapse_ws("  a \n\t b\r\n\r\n  c  "), "a b c");
        assert_eq!(collapse_ws("one"), "one");
        assert_eq!(collapse_ws(" \n\t "), "");
        assert_eq!(collapse_ws(""), "");
        // Not cut: the 200-character limit belongs to `TermLine::new` alone.
        let long = "word ".repeat(100);
        assert_eq!(collapse_ws(&long).chars().count(), 499);
    }

    #[test]
    fn a_token_straddling_the_cut_is_redacted_whole() {
        // The token starts at character 190 and would end at 230: cutting first would leave
        // `ghp_` plus a few characters, which no redaction pattern recognises any more.
        let token = format!("ghp_{}", "A1b2C3d4E5".repeat(4).get(..36).unwrap());
        let raw = format!("{} {token} tail", "a".repeat(189));
        let line = TermLine::new(TermKind::Text, &raw);
        assert!(!line.text.contains("ghp_"), "{}", line.text);
        assert!(!line.text.contains(&token[4..14]), "{}", line.text);
        assert!(line.text.chars().count() <= LINE_CHARS);
        assert!(clean_label(&raw, LINE_CHARS).starts_with("aaaa"));
    }

    #[test]
    fn a_short_token_is_replaced_by_its_placeholder() {
        let token = format!("ghp_{}", "x".repeat(36));
        let line = TermLine::new(TermKind::Text, format!("use {token} now"));
        assert_eq!(line.text, "use [SECRET:github] now");
    }

    #[test]
    fn clean_label_collapses_redacts_and_cuts_to_its_own_limit() {
        assert_eq!(clean_label("  a \n b  ", 10), "a b");
        let cut = clean_label(&"é".repeat(50), 20);
        assert_eq!(cut.chars().count(), 20);
        assert!(cut.ends_with('…'));
        let token = format!("ghp_{}", "x".repeat(36));
        assert_eq!(clean_label(&format!("fix {token}"), 80), "fix [SECRET:github]");
    }

    #[test]
    fn node_keys_are_harness_colon_native_id() {
        assert_eq!(NodeKey::new("claude-code", "abc").id(), "claude-code:abc");
    }

    #[test]
    fn the_wire_spelling_is_snake_case() {
        let line = TermLine::new(TermKind::Final, "done");
        assert_eq!(
            serde_json::to_string(&line).unwrap(),
            r#"{"kind":"final","text":"done"}"#
        );
        assert_eq!(serde_json::to_string(&NodeState::Busy).unwrap(), r#""busy""#);
        assert_eq!(serde_json::to_string(&NodeState::Lost).unwrap(), r#""lost""#);
    }
}
