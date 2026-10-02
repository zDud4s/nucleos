//! The four prompts a deliberation sends: the answer, the critique, the revision and the chairman's
//! synthesis.
//!
//! Pure on purpose: strings in, string out. What a seat may see in each phase is the whole of the
//! anonymity guarantee, so it is decided here, where a test can read it, and not at the call site.

use crate::council::formats::{Point, Role, Stance};

/// The sentence every critique prompt carries verbatim. A test runner that stands in for a seat
/// routes on it, so it is a constant rather than prose that might be reworded in one place only.
pub const CRITIQUE_MARKER: &str = "Critique each anonymous peer response below.";

/// The sentence every revision prompt carries verbatim. See [`CRITIQUE_MARKER`].
pub const REVISE_MARKER: &str =
    "Decide whether to revise your own answer in the light of these critiques.";

/// The sentence every chairman prompt carries verbatim. See [`CRITIQUE_MARKER`].
pub const CHAIRMAN_MARKER: &str = "You are the chairman: write the council's synthesis.";

/// The role paragraph followed by a blank line, or nothing for a seat without a role.
fn role_preamble(role: Option<Role>) -> String {
    match role {
        Some(role) => format!("{}\n\n", role.paragraph()),
        None => String::new(),
    }
}

/// The prompt a seat answers first: the owner's question, exactly as written, behind its role.
///
/// Without a role nothing is prepended — the property `stage1_prompt` had. A seat is an agent with
/// tools, and a preamble explaining that it is on a panel would change the answer being measured
/// into an answer about being measured. A role is the one exception, because the owner configured
/// it.
pub fn answer_prompt(question: &str, role: Option<Role>) -> String {
    format!("{}{question}", role_preamble(role))
}

/// The prompt a critiquing seat receives: its role, the question, the OTHER seats' answers under
/// their labels, and the Critique JSON contract.
///
/// `peers` is `(label, answer)` for the labels this seat may see — never its own. The caller
/// decides that list; this function only promises to show exactly it and nothing else.
pub fn critique_prompt(question: &str, role: Option<Role>, peers: &[(String, String)]) -> String {
    let mut prompt = format!(
        "{}Question:\n{question}\n\n{CRITIQUE_MARKER}\n\n",
        role_preamble(role)
    );
    if peers.is_empty() {
        // Said rather than left blank: a seat handed an empty section invents what belongs in it.
        prompt.push_str("No peer response is available to critique.\n\n");
    }
    for (label, answer) in peers {
        prompt.push_str(&format!("Response {label}:\n{}\n\n", answer.trim()));
    }
    let labels: Vec<&str> = peers.iter().map(|(l, _)| l.as_str()).collect();
    prompt.push_str(&format!(
        "For each response, list the claims that matter and take a stance on each: `agree`, \
         `disagree` or `unsure`, with a one-sentence why. Then rank the responses, best first; a \
         partial ranking is allowed when you cannot order some of them. Refer to responses by \
         label only ({labels}) and do not name, or guess at, who wrote any of them.\n\n\
         Reply with one JSON object in a ```json block, shaped like this:\n\
         ```json\n\
         {{\"reviews\": [{{\"label\": \"A\", \"points\": [{{\"claim\": \"...\", \"stance\": \"agree\", \
         \"why\": \"...\"}}]}}], \"ranking\": [\"A\"]}}\n\
         ```\n",
        labels = labels.join(", ")
    ));
    prompt
}

fn stance_word(stance: Stance) -> &'static str {
    match stance {
        Stance::Agree => "agree",
        Stance::Disagree => "disagree",
        Stance::Unsure => "unsure",
    }
}

/// The prompt a revising seat receives: its role, the question, its own answer UNLABELLED, the
/// critiques it received as "Reviewer 1..n", and the Revision JSON contract.
///
/// Deliberately absent: the peers' answers, their labels and any standing. A seat that saw where it
/// placed would revise towards the winner rather than towards the argument, and a label would tell
/// it which one it was — exactly what the critique phase withheld. A test holds the text below to
/// that, down to never using the word for an ordering.
#[allow(dead_code)] // Sent by the revise phase, council-deliberacao P7.
pub fn revise_prompt(
    question: &str,
    role: Option<Role>,
    own: &str,
    received: &[Vec<Point>],
) -> String {
    let mut prompt = format!(
        "{}Question:\n{question}\n\nYour own answer:\n{}\n\n{REVISE_MARKER}\n\n",
        role_preamble(role),
        own.trim()
    );
    if received.is_empty() {
        // Said rather than left blank, for the same reason as in `critique_prompt`.
        prompt.push_str("No reviewer commented on your answer.\n\n");
    }
    for (i, points) in received.iter().enumerate() {
        prompt.push_str(&format!("Reviewer {}:\n", i + 1));
        if points.is_empty() {
            prompt.push_str("- (no specific points)\n");
        }
        for p in points {
            prompt.push_str(&format!(
                "- {} [{}]: {}\n",
                p.claim.trim(),
                stance_word(p.stance),
                p.why.trim()
            ));
        }
        prompt.push('\n');
    }
    prompt.push_str(
        "If the critiques do not change your view, keep your answer: reply with `changed` false. \
         If they do, reply with `changed` true, say why, and give the revised answer in full — it \
         replaces what you wrote. Do not name, or guess at, who wrote any critique.\n\n\
         Reply with one JSON object in a ```json block, shaped like this:\n\
         ```json\n\
         {\"changed\": true, \"why\": \"...\", \"answer\": \"...\"}\n\
         ```\n",
    );
    prompt
}

/// One seat's answer as the chairman sees it: by index and real name, with its role.
#[derive(Debug, Clone, PartialEq)]
pub struct SeatBrief {
    pub seat_idx: usize,
    pub name: String,
    pub role: Option<Role>,
    pub answer: String,
}

/// How the critiques landed on one seat's answer: stance counts and the reasons given against it.
#[derive(Debug, Clone, PartialEq)]
pub struct SeatCritiques {
    pub seat_idx: usize,
    pub agree: usize,
    pub disagree: usize,
    pub unsure: usize,
    pub disagree_whys: Vec<String>,
}

/// Everything the chairman is shown. Built by the caller from the tally; this module only lays it
/// out.
#[derive(Debug, Clone, PartialEq)]
pub struct ChairmanInput {
    pub question: String,
    pub answers: Vec<SeatBrief>,
    pub critiques: Vec<SeatCritiques>,
    /// Already formatted, e.g. "1. Name (model) — 0.83, 4 votes".
    pub leaderboard_lines: Vec<String>,
    pub agreement_level: String,
    /// `(name, why)` for each seat that changed its answer in revision.
    pub changes: Vec<(String, String)>,
}

/// The prompt the chairman receives, with the Synthesis JSON contract.
///
/// The anonymity ends here on purpose: the chairman is writing the answer and needs to know that
/// two agreeing responses came from two different models rather than one model asked twice.
///
/// `retry_error` is the reason the previous synthesis was refused. It is APPENDED to the exact
/// first-attempt prompt, so the retry is the same question plus the one fact that changed.
pub fn chairman_prompt(input: &ChairmanInput, retry_error: Option<&str>) -> String {
    let mut prompt = format!(
        "{CHAIRMAN_MARKER}\n\nQuestion:\n{}\n\nCouncil answers, by seat:\n",
        input.question
    );
    if input.answers.is_empty() {
        // Said rather than left blank. A chairman handed an empty section writes a synthesis of
        // nothing and presents it as an answer; one told that every seat failed reports that.
        prompt.push_str("No seat produced a valid answer.\n");
    }
    for a in &input.answers {
        let role = a
            .role
            .map(|r| format!(", {}", r.as_str()))
            .unwrap_or_default();
        prompt.push_str(&format!(
            "\nSeat {} — {}{role}:\n{}\n",
            a.seat_idx,
            a.name,
            a.answer.trim()
        ));
    }

    prompt.push_str("\nHow the critiques landed, per seat:\n");
    if input.critiques.is_empty() {
        prompt.push_str("No critique could be read.\n");
    }
    for c in &input.critiques {
        prompt.push_str(&format!(
            "Seat {}: {} agree, {} disagree, {} unsure\n",
            c.seat_idx, c.agree, c.disagree, c.unsure
        ));
        for why in &c.disagree_whys {
            prompt.push_str(&format!("  - against: {}\n", why.trim()));
        }
    }

    prompt.push_str("\nLeaderboard:\n");
    if input.leaderboard_lines.is_empty() {
        prompt.push_str("No ranking could be read out of the critiques.\n");
    }
    for line in &input.leaderboard_lines {
        prompt.push_str(line);
        prompt.push('\n');
    }

    prompt.push_str(&format!("\nAgreement level: {}\n", input.agreement_level));

    prompt.push_str("\nAnswers changed in revision:\n");
    if input.changes.is_empty() {
        prompt.push_str("No seat changed its answer.\n");
    }
    for (name, why) in &input.changes {
        prompt.push_str(&format!("{name}: {}\n", why.trim()));
    }

    let seats: Vec<String> = input
        .answers
        .iter()
        .map(|a| a.seat_idx.to_string())
        .collect();
    prompt.push_str(&format!(
        "\nWrite one final answer. Refer to seats by index only; the valid seat indexes are: [{}]. \
         A minority view is required unless agreement is strong or insufficient — state it even \
         when you disagree with it. `confidence.level` is one of `strong`, `moderate`, `weak` or \
         `insufficient`.\n\n\
         Reply with one JSON object in a ```json block, shaped like this:\n\
         ```json\n\
         {{\"answer\": \"...\", \"consensus\": [\"...\"], \"disagreements\": [{{\"topic\": \"...\", \
         \"positions\": [{{\"seats\": [0], \"view\": \"...\"}}]}}], \"minority\": \"...\", \
         \"confidence\": {{\"level\": \"moderate\", \"why\": \"...\"}}, \"open_questions\": [\"...\"]}}\n\
         ```\n",
        seats.join(", ")
    ));

    if let Some(err) = retry_error {
        prompt.push_str(&format!(
            "\nYour previous synthesis was refused: {err}\n\
             Fix that and reply again with the full JSON object.\n"
        ));
    }
    prompt
}

// Tests for the four phase prompts. Written first (RED): the implementation goes above this
// module and may not edit it, so the contract below is the one GREEN has to meet.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::council::formats::{Point, Role, Stance};

    fn point(claim: &str, stance: Stance, why: &str) -> Point {
        Point {
            claim: claim.into(),
            stance,
            why: why.into(),
        }
    }

    fn peers() -> Vec<(String, String)> {
        vec![
            ("A".into(), "peer answer alpha".into()),
            ("B".into(), "peer answer beta".into()),
        ]
    }

    fn chairman_input() -> ChairmanInput {
        ChairmanInput {
            question: "Which database?".into(),
            answers: vec![
                SeatBrief {
                    seat_idx: 0,
                    name: "Claude Opus".into(),
                    role: Some(Role::Proposer),
                    answer: "use sqlite".into(),
                },
                SeatBrief {
                    seat_idx: 1,
                    name: "Codex Mini".into(),
                    role: None,
                    answer: "use postgres".into(),
                },
            ],
            critiques: vec![
                SeatCritiques {
                    seat_idx: 0,
                    agree: 1,
                    disagree: 2,
                    unsure: 0,
                    disagree_whys: vec!["no concurrent writers".into()],
                },
                SeatCritiques {
                    seat_idx: 1,
                    agree: 3,
                    disagree: 0,
                    unsure: 1,
                    disagree_whys: vec![],
                },
            ],
            leaderboard_lines: vec!["1. Claude Opus (opus) — 0.83, 4 votes".into()],
            agreement_level: "weak".into(),
            changes: vec![("Codex Mini".into(), "conceded the write-load point".into())],
        }
    }

    #[test]
    fn prompts_carry_the_role_in_every_phase() {
        let role = Role::Skeptic;
        let p = role.paragraph();
        let own = "my own answer";
        let received = vec![vec![point("c", Stance::Disagree, "because")]];
        assert!(answer_prompt("Q?", Some(role)).contains(p));
        assert!(critique_prompt("Q?", Some(role), &peers()).contains(p));
        assert!(revise_prompt("Q?", Some(role), own, &received).contains(p));
    }

    #[test]
    fn a_seat_without_role_gets_the_bare_question() {
        assert_eq!(answer_prompt("What is 2+2?", None), "What is 2+2?");
        let with = answer_prompt("What is 2+2?", Some(Role::Proposer));
        assert_eq!(
            with,
            format!("{}\n\nWhat is 2+2?", Role::Proposer.paragraph())
        );
    }

    #[test]
    fn critique_prompt_never_shows_the_readers_own_answer() {
        // The caller passes only the OTHER seats' answers; the prompt must show exactly those,
        // labelled, and nothing that could be the reader's own text.
        let p = critique_prompt("Q?", None, &peers());
        assert!(p.contains("Response A:"));
        assert!(p.contains("Response B:"));
        assert!(p.contains("peer answer alpha"));
        assert!(p.contains("peer answer beta"));
        assert!(!p.contains("Response C:"));
        assert!(p.contains(CRITIQUE_MARKER));
        assert!(p.contains("```json"));
    }

    #[test]
    fn only_the_chairman_sees_real_names() {
        let input = chairman_input();
        let chairman = chairman_prompt(&input, None);
        assert!(chairman.contains("Claude Opus"));
        assert!(chairman.contains("Codex Mini"));
        assert!(chairman.contains("1. Claude Opus (opus) — 0.83, 4 votes"));
        assert!(chairman.contains(CHAIRMAN_MARKER));
        assert!(chairman.contains("minority"));

        for p in [
            critique_prompt("Q?", None, &peers()),
            revise_prompt("Q?", None, "mine", &[vec![point("c", Stance::Agree, "w")]]),
        ] {
            assert!(!p.contains("Claude Opus"));
            assert!(!p.contains("Codex Mini"));
        }
    }

    #[test]
    fn revise_prompt_carries_no_leaderboard() {
        let received = vec![
            vec![point("claim one", Stance::Disagree, "reason one")],
            vec![point("claim two", Stance::Unsure, "reason two")],
        ];
        let p = revise_prompt("Q?", Some(Role::Proposer), "my answer text", &received);
        assert!(p.contains(REVISE_MARKER));
        assert!(p.contains("my answer text"));
        assert!(p.contains("Reviewer 1"));
        assert!(p.contains("Reviewer 2"));
        assert!(p.contains("claim one"));
        assert!(p.contains("reason two"));
        let lower = p.to_lowercase();
        assert!(!lower.contains("leaderboard"));
        assert!(!lower.contains("rank"));
        assert!(!p.contains("Response A"));
        assert!(!p.contains("Response B"));
        assert!(p.contains("changed"));
    }

    #[test]
    fn chairman_retry_appends_the_validation_error() {
        let input = chairman_input();
        let first = chairman_prompt(&input, None);
        let retry = chairman_prompt(&input, Some("seat 7 does not exist"));
        assert!(!first.contains("seat 7 does not exist"));
        assert!(retry.contains("seat 7 does not exist"));
        assert!(retry.starts_with(&first));
        // The marker routes the test runner; it must be a single stable sentence in each prompt.
        assert_ne!(CRITIQUE_MARKER, REVISE_MARKER);
        assert_ne!(REVISE_MARKER, CHAIRMAN_MARKER);
        assert_ne!(CRITIQUE_MARKER, CHAIRMAN_MARKER);
    }
}
