//! Capture requests: when a job went wrong, the distiller asks the owner, once, what only they
//! know about it, and holds that job's queue rows until the answer, a dismissal or the deadline.
//! Design: .ai/specs/2026-10-07-pedidos-captura-design.md.
//!
//! The question is a fixed template (no model, spec P5) and is redacted before it is written. An
//! answer is known here only as a note id: composing the note is http.rs's job (spec 5.3).

use crate::distill::Cause;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

pub const STATE_OPEN: &str = "open";
pub const STATE_ANSWERED: &str = "answered";
pub const STATE_DISMISSED: &str = "dismissed";
pub const STATE_EXPIRED: &str = "expired";

/// The feed kind a new request is announced with; the Telegram sidecar sends it without a prefix.
pub const FEED_KIND: &str = "capture_requested";

/// The causes that ask the owner (spec P3): only when something went wrong.
pub const ASKING_CAUSES: [Cause; 3] =
    [Cause::JobFailed, Cause::RunExhausted, Cause::ReviewBlocking];

/// One queue row's contribution to a request: which row, which cause, and its line of fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub row: i64,
    pub cause: String,
    pub fact: String,
}

/// What the first and the last line of the question say.
pub struct Header<'a> {
    pub project: &'a str,
    pub job_id: i64,
    /// The deadline as the owner reads it, local `HH:MM`.
    pub until: &'a str,
}

/// The one timestamp format of this table: fixed width, so `deadline > now` as text is time order.
pub fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn cause_label(cause: &str) -> &'static str {
    match cause {
        "job_failed" => "o job falhou",
        "run_exhausted" => "o gate esgotou as tentativas",
        "review_blocking" => "a review bloqueou",
        _ => "algo correu mal",
    }
}

/// The question, from its header and its facts, in the order the facts arrived.
pub fn render(header: &Header, facts: &[Fact]) -> String {
    let mut labels: Vec<&str> = Vec::new();
    for f in facts {
        let label = cause_label(&f.cause);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    let mut out = format!(
        "🧠 {} · job #{} · {}\n",
        header.project,
        header.job_id,
        labels.join(", ")
    );
    for f in facts {
        out.push_str(&f.fact);
        out.push('\n');
    }
    out.push_str("Há alguma coisa que só tu saibas sobre isto?\n");
    out.push_str(&format!(
        "(até às {}; depois o destilador avança) #cap{}",
        header.until, header.job_id
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(row: i64, cause: &str, line: &str) -> Fact {
        Fact {
            row,
            cause: cause.to_owned(),
            fact: line.to_owned(),
        }
    }

    #[test]
    fn the_question_is_one_fixed_template() {
        let header = Header {
            project: "web",
            job_id: 7,
            until: "14:30",
        };
        let cases = [
            (
                vec![fact(1, "job_failed", "o job terminou em `failed`")],
                "🧠 web · job #7 · o job falhou\n\
                 o job terminou em `failed`\n\
                 Há alguma coisa que só tu saibas sobre isto?\n\
                 (até às 14:30; depois o destilador avança) #cap7",
            ),
            (
                vec![
                    fact(
                        1,
                        "run_exhausted",
                        "o item 2 «build» esgotou 3 tentativas no gate: `E0425`",
                    ),
                    fact(4, "review_blocking", "a review bloqueou o job (run #9)"),
                ],
                "🧠 web · job #7 · o gate esgotou as tentativas, a review bloqueou\n\
                 o item 2 «build» esgotou 3 tentativas no gate: `E0425`\n\
                 a review bloqueou o job (run #9)\n\
                 Há alguma coisa que só tu saibas sobre isto?\n\
                 (até às 14:30; depois o destilador avança) #cap7",
            ),
            (
                vec![fact(1, "run_exhausted", "a"), fact(2, "run_exhausted", "b")],
                "🧠 web · job #7 · o gate esgotou as tentativas\na\nb\n\
                 Há alguma coisa que só tu saibas sobre isto?\n\
                 (até às 14:30; depois o destilador avança) #cap7",
            ),
        ];
        for (facts, want) in cases {
            assert_eq!(render(&header, &facts), want);
        }
    }

    #[test]
    fn a_stamp_has_a_fixed_width_so_text_order_is_time_order() {
        let t = chrono::DateTime::parse_from_rfc3339("2026-10-07T10:00:00.123456+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(stamp(t), "2026-10-07T10:00:00+00:00");
        assert!(stamp(t) < stamp(t + chrono::Duration::seconds(1)));
    }

    #[test]
    fn only_failure_causes_ask() {
        use crate::distill::Cause;
        assert_eq!(
            ASKING_CAUSES,
            [Cause::JobFailed, Cause::RunExhausted, Cause::ReviewBlocking]
        );
    }
}
