//! Single table of user-visible text (Anexo D, D.14). Every sentence the hub sends lives here,
//! in English like the shell (Q3 = C). Ported from the Telegram sidecar's `pipe/` and `notifier/`.

use crate::governance::budget::BudgetSnapshot;

/// Every user-visible sentence the hub sends.
// `Eq` is not derived: `BudgetSnapshot` carries `f64`.
#[derive(Debug, Clone, PartialEq)]
pub enum Text {
    Help,
    Working {
        turn: i64,
    },
    StillWorking,
    /// Final answer when the turn produced no stdout: failed / timeout / cancelled / interrupted.
    TurnEnded {
        turn: i64,
        status: String,
    },
    Queued {
        position: usize,
    },
    QueuePaused {
        waiting: usize,
    },
    NotSent {
        refusal: String,
    },
    KillEngaged,
    KillDisengaged,
    /// `/cancel` with no running turn. Exact text: "Nothing to stop.".
    NothingToCancel,
    Cancelled {
        turn: i64,
    },
    Budget(BudgetSnapshot),
    NoProposals,
    NoteUsage,
    Noted {
        id: i64,
    },
    NoteFailed,
    CaptureHint {
        jobs: Vec<i64>,
        reply_to: bool,
    },
    CaptureNeedsText,
    CaptureAnswered {
        job: i64,
    },
    AlreadyHandled,
    Approved {
        id: i64,
    },
    Rejected {
        id: i64,
    },
    /// Degraded buttons (§6.5).
    ReplyApproveReject {
        id: i64,
    },
    HeardCommand {
        text: String,
    },
    Unsupported {
        label: String,
    },
    MediaUnavailable {
        name: String,
    },
    Paired,
    PairInvalid,
    ProjectPrefix {
        project: String,
    },
    MailNotArmed,
}

fn usd(v: f64) -> String {
    format!("${v:.2}")
}

fn render_budget(b: &BudgetSnapshot) -> String {
    let limit = b.limit_usd.map_or_else(|| "no limit".to_string(), usd);
    let mut out = format!(
        "Budget ({}): {} spent of {}.",
        b.period,
        usd(b.window_spend_usd),
        limit
    );
    match b.hourly_limit_usd {
        Some(h) => out.push_str(&format!(
            " This hour: {} of {}.",
            usd(b.hourly_spend_usd),
            usd(h)
        )),
        None => out.push_str(&format!(" This hour: {}.", usd(b.hourly_spend_usd))),
    }
    if b.paused {
        out.push_str(" New runs are paused");
        if let Some(reason) = &b.reason {
            out.push_str(&format!(": {reason}"));
        }
        out.push('.');
    } else {
        out.push_str(" New runs are allowed.");
    }
    out
}

pub fn render(text: &Text) -> String {
    match text {
        Text::Help => "Commands:\n/help - this list\n/cancel - stop the running turn\n\
/kill - engage the kill switch\n/unkill - disengage it\n/budget - show the budget\n\
/proposals - list open proposals\napprove N - approve proposal N\nreject N - reject proposal N\n\
/projects [filter] - list projects\n/inbox - queued mail\n/note <text> - save a note\n\
/cap <N> <text> - answer capture request N"
            .to_string(),
        Text::Working { turn } => format!("Working on it (turn {turn})."),
        Text::StillWorking => {
            "Still working on it - I'll reply here when the turn finishes.".to_string()
        }
        Text::TurnEnded { turn, status } => format!("Turn {turn} ended: {status}."),
        Text::Queued { position } => format!("Queued (position {position})."),
        Text::QueuePaused { waiting } => {
            format!("The queue is paused; {waiting} message(s) waiting.")
        }
        Text::NotSent { refusal } => format!("Not sent: {refusal}."),
        Text::KillEngaged => "Kill switch ENGAGED".to_string(),
        Text::KillDisengaged => "Kill switch DISENGAGED".to_string(),
        Text::NothingToCancel => "Nothing to stop.".to_string(),
        Text::Cancelled { turn } => format!("Cancelled turn {turn}."),
        Text::Budget(b) => render_budget(b),
        Text::NoProposals => "No open proposals.".to_string(),
        Text::NoteUsage => "Usage: /note <text>".to_string(),
        Text::Noted { id } => format!("Noted (#{id})."),
        Text::NoteFailed => "Could not save the note.".to_string(),
        Text::CaptureHint { jobs, reply_to } => {
            let list = jobs
                .iter()
                .map(|j| format!("#{j}"))
                .collect::<Vec<_>>()
                .join(", ");
            if *reply_to {
                format!(
                    "Capture requests waiting: {list}. Use Reply on the request's message to answer."
                )
            } else {
                format!("Capture requests waiting: {list}. Answer with /cap <N> <text>.")
            }
        }
        Text::CaptureNeedsText => {
            "Capture requests take a typed reply - type your answer instead.".to_string()
        }
        Text::CaptureAnswered { job } => format!("Answered capture request #{job}."),
        Text::AlreadyHandled => "That one was already handled.".to_string(),
        Text::Approved { id } => format!("Approved #{id}."),
        Text::Rejected { id } => format!("Rejected #{id}."),
        Text::ReplyApproveReject { id } => format!("Reply approve {id} or reject {id}."),
        Text::HeardCommand { text } => format!(
            "I heard a command in that voice note and did not run it - type it instead: {text}"
        ),
        Text::Unsupported { label } => format!(
            "I can't read {label} here. I can only read text, voice notes, photos and documents."
        ),
        Text::MediaUnavailable { name } => format!("The attachment {name} is no longer available."),
        Text::Paired => "Paired. This conversation is now linked to your NucleOS.".to_string(),
        Text::PairInvalid => "That pairing code is invalid or expired.".to_string(),
        Text::ProjectPrefix { project } => format!("[{project}] "),
        Text::MailNotArmed => "Mail is not armed.".to_string(),
    }
}

/// The exact phrase the owner sends the bot, `"/pair 482913"`; `access::MintedCode::command`.
pub fn pair_command(code: &str) -> String {
    format!("/pair {code}")
}

/// Body is A2's (S1); only A2 calls it.
pub fn render_proposals(list: &[crate::proposals::Proposal]) -> String {
    let _ = list;
    todo!("A2/S1")
}

/// Body is A2's (S1); only A2 calls it.
pub fn render_projects(
    list: &[crate::governance::autopilot::ProjectSummary],
    filter: Option<&str>,
) -> String {
    let _ = (list, filter);
    todo!("A2/S1")
}

/// Body is A2's (S1); only A2 calls it.
pub fn render_inbox(list: &[crate::integrations::email::QueuedEmail]) -> String {
    let _ = list;
    todo!("A2/S1")
}

/// Body is A2's (S1); only A2 calls it.
pub fn render_triage(outcome: &crate::governance::triage::TriageOutcome) -> String {
    let _ = outcome;
    todo!("A2/S1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> BudgetSnapshot {
        BudgetSnapshot {
            limit_usd: Some(50.0),
            period: "month".into(),
            hourly_limit_usd: None,
            per_run_reserve_usd: 0.5,
            time_cost_per_hour_usd: 0.0,
            window_spend_usd: 12.5,
            hourly_spend_usd: 0.25,
            paused: false,
            reason: None,
            relay_chain_limit_usd: 5.0,
        }
    }

    #[test]
    fn pair_command_is_the_slash_phrase() {
        assert_eq!(pair_command("482913"), "/pair 482913");
    }

    #[test]
    fn every_text_variant_renders_non_empty_english() {
        let all = vec![
            Text::Help,
            Text::Working { turn: 3 },
            Text::StillWorking,
            Text::TurnEnded {
                turn: 3,
                status: "failed".into(),
            },
            Text::Queued { position: 2 },
            Text::QueuePaused { waiting: 4 },
            Text::NotSent {
                refusal: "kill switch".into(),
            },
            Text::KillEngaged,
            Text::KillDisengaged,
            Text::NothingToCancel,
            Text::Cancelled { turn: 3 },
            Text::Budget(budget()),
            Text::NoProposals,
            Text::NoteUsage,
            Text::Noted { id: 7 },
            Text::NoteFailed,
            Text::CaptureHint {
                jobs: vec![1, 2],
                reply_to: true,
            },
            Text::CaptureHint {
                jobs: vec![1, 2],
                reply_to: false,
            },
            Text::CaptureNeedsText,
            Text::CaptureAnswered { job: 1 },
            Text::AlreadyHandled,
            Text::Approved { id: 12 },
            Text::Rejected { id: 12 },
            Text::ReplyApproveReject { id: 12 },
            Text::HeardCommand {
                text: "/kill".into(),
            },
            Text::Unsupported {
                label: "a sticker".into(),
            },
            Text::MediaUnavailable {
                name: "a.png".into(),
            },
            Text::Paired,
            Text::PairInvalid,
            Text::ProjectPrefix {
                project: "alpha".into(),
            },
            Text::MailNotArmed,
        ];
        for t in &all {
            let s = render(t);
            assert!(!s.trim().is_empty(), "{t:?} rendered empty");
            for pt in ["aprovar", "rejeitar", "Projetos", "Esta conversa"] {
                assert!(!s.contains(pt), "{t:?} is not English: {s}");
            }
        }
    }

    #[test]
    fn capture_hint_without_reply_mentions_the_cap_command() {
        let s = render(&Text::CaptureHint {
            jobs: vec![4],
            reply_to: false,
        });
        assert!(s.contains("/cap <N>"), "{s}");
    }

    #[test]
    fn degraded_buttons_use_english_approve_and_reject() {
        let s = render(&Text::ReplyApproveReject { id: 12 });
        assert!(s.contains("approve 12") && s.contains("reject 12"), "{s}");
        assert!(!s.contains("aprovar") && !s.contains("rejeitar"), "{s}");
    }

    #[test]
    fn project_prefix_is_bracketed_with_a_trailing_space() {
        assert_eq!(
            render(&Text::ProjectPrefix {
                project: "alpha".into()
            }),
            "[alpha] "
        );
    }

    #[test]
    fn nothing_to_cancel_is_exact() {
        assert_eq!(render(&Text::NothingToCancel), "Nothing to stop.");
    }

    #[test]
    fn a_paused_budget_says_why() {
        let mut b = budget();
        b.paused = true;
        b.reason = Some("monthly limit reached".into());
        let s = render(&Text::Budget(b));
        assert!(
            s.contains("paused") && s.contains("monthly limit reached"),
            "{s}"
        );
    }
}
