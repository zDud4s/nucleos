//! Every JSON shape a seat or the chairman returns during a deliberation, and the markdown a
//! synthesis is shown as.
//!
//! Pure on purpose: text in, value or reason out. A model's output is untrusted input, so each
//! parser refuses what it cannot vouch for instead of repairing it — a guessed repair would put
//! words in a seat's mouth that the seat never said.

// Not wired into the council's run loop yet; a later packet of council-deliberacao calls these and
// removes this line. Unconditional because some items are exercised by no test of their own, so
// they are dead under `cfg(test)` as well.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The part a seat plays in a deliberation. A closed set: a role outside it is a typo, and a
/// typo that silently became "no role" would run a council nobody configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Proposer,
    Skeptic,
    DevilsAdvocate,
    FactChecker,
}

impl Role {
    pub const ALL: [Role; 4] = [
        Role::Proposer,
        Role::Skeptic,
        Role::DevilsAdvocate,
        Role::FactChecker,
    ];

    /// The wire name, identical to the serde one.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Proposer => "proposer",
            Role::Skeptic => "skeptic",
            Role::DevilsAdvocate => "devils_advocate",
            Role::FactChecker => "fact_checker",
        }
    }

    /// Exact and case-sensitive: the names come from a config file, not from a person typing.
    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// The fixed instruction a seat in this role is given.
    pub fn paragraph(self) -> &'static str {
        match self {
            Role::Proposer => {
                "You are the proposer. Put forward the strongest concrete answer you can and defend its choices."
            }
            Role::Skeptic => {
                "You are the skeptic. Look for the assumption, risk or missing case that would make the obvious answer fail."
            }
            Role::DevilsAdvocate => {
                "You are the devil's advocate. Argue for the option the other seats are likely to dismiss, as well as it can be argued."
            }
            Role::FactChecker => {
                "You are the fact checker. Verify the claims that can be verified and say plainly which ones cannot."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stance {
    Agree,
    Disagree,
    Unsure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub claim: String,
    pub stance: Stance,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub label: String,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Critique {
    pub reviews: Vec<Review>,
    pub ranking: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    #[serde(default)]
    pub answer: Option<String>,
    pub changed: bool,
    #[serde(default)]
    pub why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerPayload {
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub seats: Vec<usize>,
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Disagreement {
    pub topic: String,
    pub positions: Vec<Position>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Confidence {
    pub level: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Synthesis {
    pub answer: String,
    pub consensus: Vec<String>,
    pub disagreements: Vec<Disagreement>,
    pub minority: Option<String>,
    pub confidence: Confidence,
    pub open_questions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

/// The JSON a reply carries: the last fenced ```json block, else the whole text, trimmed.
/// The last block and not the first because a model that thinks aloud puts its examples first and
/// its verdict last. No repair — prose around an unfenced object stays and the parse fails.
pub fn extract_json(text: &str) -> &str {
    const OPEN: &str = "```json";
    if let Some(start) = text.rfind(OPEN) {
        let body = &text[start + OPEN.len()..];
        if let Some(end) = body.find("```") {
            return body[..end].trim();
        }
    }
    text.trim()
}

fn blank(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(|s| s.trim().is_empty())
}

/// A critique, with everything about a label the seat was never shown thrown away — the same rule
/// `council::parse_rankings` applies, since a ballot for an answer nobody saw is made up.
/// A label ranked twice is refused outright rather than deduplicated: which of the two positions
/// the seat meant is not something to guess.
pub fn parse_critique(text: &str, shown: &[String]) -> Result<Critique, String> {
    let mut critique: Critique = serde_json::from_str(extract_json(text))
        .map_err(|e| format!("critique did not parse: {e}"))?;
    let mut seen = BTreeSet::new();
    for label in &critique.ranking {
        if !seen.insert(label.as_str()) {
            return Err(format!("critique ranks `{label}` more than once"));
        }
    }
    critique.ranking.retain(|l| shown.contains(l));
    critique.reviews.retain(|r| shown.contains(&r.label));
    Ok(critique)
}

/// A revision. Claiming a change requires saying what changed and why; an unchanged revision's
/// answer is dropped, because the seat's earlier answer is the one that stands.
pub fn parse_revision(text: &str) -> Result<Revision, String> {
    let mut revision: Revision = serde_json::from_str(extract_json(text))
        .map_err(|e| format!("revision did not parse: {e}"))?;
    if !revision.changed {
        revision.answer = None;
        return Ok(revision);
    }
    if blank(&revision.why) {
        return Err("revision claims a change without saying why".to_string());
    }
    if blank(&revision.answer) {
        return Err("revision claims a change without an answer".to_string());
    }
    Ok(revision)
}

pub fn parse_synthesis(text: &str) -> Result<Synthesis, String> {
    serde_json::from_str(extract_json(text)).map_err(|e| format!("synthesis did not parse: {e}"))
}

/// Whether a synthesis can be trusted to be shown. `level` is the confidence the chairman
/// reported; a dissent may be absent only when the council was unanimous (`strong`) or had too
/// little to go on (`insufficient`) — anywhere in between, a missing minority is a hidden one.
pub fn validate_synthesis(
    s: &Synthesis,
    seats: &BTreeSet<usize>,
    level: &str,
) -> Result<(), String> {
    if s.answer.trim().is_empty() {
        return Err("synthesis has an empty answer".to_string());
    }
    for d in &s.disagreements {
        for p in &d.positions {
            if let Some(seat) = p.seats.iter().find(|i| !seats.contains(i)) {
                return Err(format!(
                    "synthesis names seat {seat}, which is not on the council"
                ));
            }
        }
    }
    let exempt = matches!(level, "strong" | "insufficient");
    if !exempt && blank(&s.minority) {
        return Err(format!(
            "synthesis at confidence `{level}` must state the minority view"
        ));
    }
    Ok(())
}

/// What the chairman's raw text becomes when it will not parse: shown as it came, with the reason
/// it could not be structured, instead of dropped.
pub fn degraded(raw: &str, reason: &str) -> Synthesis {
    Synthesis {
        answer: raw.to_string(),
        consensus: vec![],
        disagreements: vec![],
        minority: None,
        confidence: Confidence {
            level: String::new(),
            why: String::new(),
        },
        open_questions: vec![],
        degraded_reason: Some(reason.to_string()),
    }
}

/// The synthesis as markdown. Empty sections are omitted rather than printed as bare headings.
pub fn compose_markdown(s: &Synthesis, name_of: &dyn Fn(usize) -> String) -> String {
    let mut out = s.answer.trim().to_string();
    if !s.consensus.is_empty() {
        out.push_str("\n\n## Consensus\n");
        for c in &s.consensus {
            out.push_str(&format!("\n- {c}"));
        }
    }
    if !s.disagreements.is_empty() {
        out.push_str("\n\n## Disagreements\n");
        for d in &s.disagreements {
            out.push_str(&format!("\n- **{}**", d.topic));
            for p in &d.positions {
                let names: Vec<String> = p.seats.iter().map(|i| name_of(*i)).collect();
                out.push_str(&format!("\n  - {}: {}", names.join(", "), p.view));
            }
        }
    }
    if let Some(m) = s.minority.as_deref().filter(|m| !m.trim().is_empty()) {
        out.push_str(&format!("\n\n## Minority\n\n{m}"));
    }
    if !s.confidence.level.trim().is_empty() {
        out.push_str(&format!(
            "\n\n## Confidence\n\n{}: {}",
            s.confidence.level, s.confidence.why
        ));
    }
    if !s.open_questions.is_empty() {
        out.push_str("\n\n## Open questions\n");
        for q in &s.open_questions {
            out.push_str(&format!("\n- {q}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn seats(ids: &[usize]) -> BTreeSet<usize> {
        ids.iter().copied().collect()
    }

    fn labels(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    /// A synthesis JSON with the level and minority as the only knobs the validation rules read.
    fn synthesis_json(level: &str, minority: Option<&str>) -> String {
        let minority = match minority {
            Some(m) => format!("\"{m}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{"answer":"Use SQLite.","consensus":["one writer"],"disagreements":[{{"topic":"wal","positions":[{{"seats":[0,1],"view":"on"}},{{"seats":[2],"view":"off"}}]}}],"minority":{minority},"confidence":{{"level":"{level}","why":"seats converged"}},"open_questions":["backup cadence?"]}}"#
        )
    }

    #[test]
    fn role_is_a_closed_set() {
        assert_eq!(Role::ALL.len(), 4);
        for role in Role::ALL {
            assert_eq!(Role::parse(role.as_str()), Some(role));
            assert!(!role.paragraph().trim().is_empty());
        }
        assert_eq!(Role::parse("skeptic"), Some(Role::Skeptic));
        assert_eq!(Role::parse("devils_advocate"), Some(Role::DevilsAdvocate));
        assert_eq!(Role::parse("fact_checker"), Some(Role::FactChecker));
        assert_eq!(Role::parse("proposer"), Some(Role::Proposer));
        assert_eq!(Role::parse("judge"), None);
        assert_eq!(Role::parse(""), None);
        assert_eq!(Role::parse("Skeptic"), None);
        assert_eq!(
            serde_json::to_string(&Role::DevilsAdvocate).unwrap(),
            "\"devils_advocate\""
        );
    }

    #[test]
    fn extract_json_takes_the_last_fenced_block_or_the_whole_text() {
        let two = "intro\n```json\n{\"a\":1}\n```\nthen\n```json\n{\"a\":2}\n```\ntail";
        assert_eq!(extract_json(two).trim(), "{\"a\":2}");
        let bare = "  {\"a\":3}  \n";
        assert_eq!(extract_json(bare), "{\"a\":3}");
        // No repair: prose around an unfenced object stays, and the parse fails downstream.
        let prose = "here you go {\"a\":4}";
        assert_eq!(extract_json(prose), prose);
    }

    #[test]
    fn critique_parses_and_drops_unseen_labels() {
        let text = r#"```json
{"reviews":[
  {"label":"A","points":[{"claim":"c","stance":"agree","why":"w"}]},
  {"label":"Z","points":[{"claim":"c","stance":"disagree","why":"w"}]}
 ],
 "ranking":["B","Z","A"]}
```"#;
        let c = parse_critique(text, &labels(&["A", "B"])).unwrap();
        assert_eq!(c.ranking, labels(&["B", "A"]));
        assert_eq!(c.reviews.len(), 1);
        assert_eq!(c.reviews[0].label, "A");
        assert_eq!(c.reviews[0].points[0].stance, Stance::Agree);
    }

    #[test]
    fn critique_with_repeated_ranking_labels_is_invalid() {
        let text = r#"{"reviews":[],"ranking":["A","B","A"]}"#;
        assert!(parse_critique(text, &labels(&["A", "B"])).is_err());
    }

    #[test]
    fn critique_that_does_not_parse_is_invalid() {
        assert!(parse_critique("not json at all", &labels(&["A"])).is_err());
        assert!(parse_critique(r#"{"reviews":[]}"#, &labels(&["A"])).is_err());
        let bad_stance = r#"{"reviews":[{"label":"A","points":[{"claim":"c","stance":"maybe","why":"w"}]}],"ranking":["A"]}"#;
        assert!(parse_critique(bad_stance, &labels(&["A"])).is_err());
    }

    #[test]
    fn revision_changed_without_why_is_invalid() {
        assert!(parse_revision(r#"{"answer":"new","changed":true}"#).is_err());
        assert!(parse_revision(r#"{"answer":"new","changed":true,"why":"   "}"#).is_err());
        assert!(parse_revision(r#"{"answer":"  ","changed":true,"why":"fixed"}"#).is_err());
        assert!(parse_revision(r#"{"changed":true,"why":"fixed"}"#).is_err());
        assert!(parse_revision("nonsense").is_err());
        let ok = parse_revision(r#"{"answer":"new","changed":true,"why":"fixed"}"#).unwrap();
        assert!(ok.changed);
        assert_eq!(ok.answer.as_deref(), Some("new"));
    }

    #[test]
    fn revision_unchanged_ignores_its_answer() {
        let r = parse_revision(r#"{"answer":"stray text","changed":false}"#).unwrap();
        assert!(!r.changed);
        assert_eq!(r.answer, None);
        // An unchanged revision needs neither an answer nor a reason.
        let r = parse_revision(r#"{"changed":false}"#).unwrap();
        assert!(!r.changed);
        assert_eq!(r.answer, None);
    }

    #[test]
    fn synthesis_requires_minority_unless_strong_or_insufficient() {
        let all = seats(&[0, 1, 2]);
        for level in ["medium", "low", "weak"] {
            let s = parse_synthesis(&synthesis_json(level, None)).unwrap();
            assert!(validate_synthesis(&s, &all, level).is_err(), "{level}");
            let s = parse_synthesis(&synthesis_json(level, Some("  "))).unwrap();
            assert!(
                validate_synthesis(&s, &all, level).is_err(),
                "{level} blank"
            );
            let s = parse_synthesis(&synthesis_json(level, Some("seat 2 dissents"))).unwrap();
            assert!(validate_synthesis(&s, &all, level).is_ok(), "{level} with");
        }
        for level in ["strong", "insufficient"] {
            let s = parse_synthesis(&synthesis_json(level, None)).unwrap();
            assert!(validate_synthesis(&s, &all, level).is_ok(), "{level}");
        }
    }

    #[test]
    fn synthesis_refuses_unknown_seats_and_empty_answer() {
        let s = parse_synthesis(&synthesis_json("strong", None)).unwrap();
        // Seat 2 is named in a position but is not on the council.
        assert!(validate_synthesis(&s, &seats(&[0, 1]), "strong").is_err());
        assert!(validate_synthesis(&s, &seats(&[0, 1, 2]), "strong").is_ok());
        let mut empty = s.clone();
        empty.answer = "   ".to_string();
        assert!(validate_synthesis(&empty, &seats(&[0, 1, 2]), "strong").is_err());
        assert!(parse_synthesis("garbage").is_err());
    }

    #[test]
    fn synthesis_markdown_composes_every_section() {
        let s = parse_synthesis(&synthesis_json("medium", Some("seat 2 dissents"))).unwrap();
        let md = compose_markdown(&s, &|i| format!("Seat-{i}"));
        assert!(md.contains("Use SQLite."));
        for heading in ["Consensus", "Disagreements", "Minority", "Confidence"] {
            assert!(md.contains(heading), "missing {heading}");
        }
        assert!(md.contains("one writer"));
        assert!(md.contains("Seat-0"));
        assert!(md.contains("Seat-1"));
        assert!(md.contains("Seat-2"));
        assert!(md.contains("seat 2 dissents"));
        assert!(md.contains("seats converged"));

        // Empty sections are omitted, not printed as bare headings.
        let bare = Synthesis {
            answer: "Just this.".to_string(),
            consensus: vec![],
            disagreements: vec![],
            minority: None,
            confidence: Confidence {
                level: "strong".to_string(),
                why: "unanimous".to_string(),
            },
            open_questions: vec![],
            degraded_reason: None,
        };
        let md = compose_markdown(&bare, &|i| i.to_string());
        assert!(md.contains("Just this."));
        assert!(!md.contains("Consensus"));
        assert!(!md.contains("Disagreements"));
        assert!(!md.contains("Minority"));
    }

    #[test]
    fn degraded_synthesis_keeps_the_raw_text_and_reason() {
        let d = degraded("the chairman rambled", "synthesis did not parse");
        assert_eq!(d.answer, "the chairman rambled");
        assert_eq!(
            d.degraded_reason.as_deref(),
            Some("synthesis did not parse")
        );
        assert!(d.consensus.is_empty());
        assert!(d.disagreements.is_empty());
        assert!(d.minority.is_none());
        assert!(d.open_questions.is_empty());
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("degraded_reason"));
        // A healthy synthesis does not carry the field at all.
        let healthy = parse_synthesis(&synthesis_json("strong", None)).unwrap();
        assert!(
            !serde_json::to_string(&healthy)
                .unwrap()
                .contains("degraded_reason")
        );
    }
}
