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
    /// Budget alert: new runs just became paused. Carries no figures (H4: no `snapshot` read).
    BudgetPaused,
    /// Budget alert: new runs are allowed again.
    BudgetResumed,
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
    Feed {
        kind: String,
        summary: String,
    },
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
        Text::BudgetPaused => "Budget limit reached. New runs are paused.".to_string(),
        Text::BudgetResumed => "Budget is back within its limit. New runs are allowed.".to_string(),
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
        Text::Feed { kind, summary } => format!("{kind}: {summary}"),
    }
}

/// The exact phrase the owner sends the bot, `"/pair 482913"`; `access::MintedCode::command`.
pub fn pair_command(code: &str) -> String {
    format!("/pair {code}")
}

/// One proposal as the list shows it: id, kind, tool, then the reasoning and project when present.
fn format_proposal(p: &crate::proposals::Proposal) -> String {
    let mut out = format!(
        "proposal {} ({}): {}",
        p.id,
        p.kind,
        p.tool_name.as_deref().unwrap_or("?")
    );
    if !p.reasoning.is_empty() {
        out.push_str("\nwhy: ");
        out.push_str(&p.reasoning);
    }
    if let Some(project) = p.project_id.as_deref().filter(|s| !s.is_empty()) {
        out.push_str("\nproject: ");
        out.push_str(project);
    }
    out
}

/// The `/proposals` reply: one block per open proposal (the hub attaches the buttons separately).
pub fn render_proposals(list: &[crate::proposals::Proposal]) -> String {
    if list.is_empty() {
        return render(&Text::NoProposals);
    }
    list.iter()
        .map(format_proposal)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The `/projects [filter]` reply; the filter matches the project id case-insensitively as a substring.
pub fn render_projects(
    list: &[crate::governance::autopilot::ProjectSummary],
    filter: Option<&str>,
) -> String {
    let filter = filter.map(str::trim).filter(|f| !f.is_empty());
    let needle = filter.map(str::to_lowercase);
    let records: Vec<String> = list
        .iter()
        .filter(|p| {
            needle
                .as_deref()
                .is_none_or(|n| p.project_id.to_lowercase().contains(n))
        })
        .map(|p| {
            let root = p
                .project_root
                .as_deref()
                .filter(|r| !r.is_empty())
                .unwrap_or("-");
            format!(
                "**{}**\nMode: {} · Pending: {}\nRoot: {}",
                p.project_id,
                p.mode.as_db_str(),
                p.pending,
                root
            )
        })
        .collect();
    if records.is_empty() {
        return match filter {
            Some(f) => format!("No project matches \"{f}\"."),
            None => "No projects registered in NucleOS.".to_string(),
        };
    }
    format!("Registered projects:\n\n{}", records.join("\n\n"))
}

/// The `/inbox` reply: unread mail first, then up to ten already-triaged messages.
pub fn render_inbox(list: &[crate::integrations::email::QueuedEmail]) -> String {
    if list.is_empty() {
        return "Nothing in the mailbox yet.".to_string();
    }
    let mut waiting: Vec<String> = Vec::new();
    let mut judged: Vec<String> = Vec::new();
    for mail in list {
        let who = mail
            .from_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(if mail.from_addr.is_empty() {
                "(unknown sender)"
            } else {
                mail.from_addr.as_str()
            });
        match mail.triage_class.as_deref().filter(|c| !c.is_empty()) {
            None => waiting.push(format!(
                "· {who} — {}",
                mail.subject.as_deref().unwrap_or("(no subject)")
            )),
            Some(class) => {
                if judged.len() < 10 {
                    let summary = mail
                        .triage_summary
                        .as_deref()
                        .or(mail.subject.as_deref())
                        .unwrap_or("");
                    judged.push(format!("[{class}] {who} — {summary}"));
                }
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    if !waiting.is_empty() {
        out.push(format!(
            "Waiting to be read ({}) — send /mail to triage:",
            waiting.len()
        ));
        out.extend(waiting);
    }
    if !judged.is_empty() {
        out.push(String::new());
        out.push("Already read:".to_string());
        out.extend(judged);
    }
    out.join("\n")
}

/// The `/mail` reply: what the pass started, not verdicts (those arrive through the feed).
pub fn render_triage(outcome: &crate::governance::triage::TriageOutcome) -> String {
    if outcome.run_id.is_none() {
        let reason = outcome
            .reason
            .as_deref()
            .filter(|r| !r.is_empty())
            .unwrap_or("nothing to do");
        return format!("No triage started: {reason}");
    }
    format!(
        "Reading {} message(s) — the verdicts arrive here in a few minutes",
        outcome.queued
    )
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
            Text::BudgetPaused,
            Text::BudgetResumed,
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
            Text::Feed {
                kind: "job_done".into(),
                summary: "built".into(),
            },
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

    fn proposal(id: i64, kind: &str, tool: &str, why: &str) -> crate::proposals::Proposal {
        crate::proposals::Proposal {
            id,
            kind: kind.into(),
            status: "pending".into(),
            run_id: None,
            session_id: None,
            project_id: None,
            tool_name: Some(tool.into()),
            reasoning: why.into(),
            tool_input: None,
            read_from: None,
            created_at: "2026-10-20T00:00:00Z".into(),
            decided_at: None,
            job_id: None,
            run_stage: None,
            item_ordinal: None,
            item_description: None,
            run_status: None,
            run_cwd: None,
            run_prompt: None,
            run_recent: None,
            team_run_id: None,
            team_name: None,
            team_request: None,
            asked_by: None,
            asked_for: None,
            job_low_task: None,
            job_high_task: None,
        }
    }

    fn project(id: &str, pending: i64) -> crate::governance::autopilot::ProjectSummary {
        crate::governance::autopilot::ProjectSummary {
            project_id: id.into(),
            mode: crate::governance::autopilot::Mode::Shadow,
            project_root: Some("C:/p".into()),
            pending,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: None,
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }
    }

    fn mail(
        from_name: Option<&str>,
        subject: &str,
        class: Option<&str>,
        summary: Option<&str>,
    ) -> crate::integrations::email::QueuedEmail {
        crate::integrations::email::QueuedEmail {
            id: 1,
            from_addr: "a@example.com".into(),
            from_name: from_name.map(Into::into),
            subject: Some(subject.into()),
            received_at: "2026-10-20T00:00:00Z".into(),
            triage_class: class.map(Into::into),
            triage_summary: summary.map(Into::into),
            triaged_at: None,
            has_attachments: 0,
            sender_verdict: None,
        }
    }

    #[test]
    fn render_proposals_lists_id_kind_and_summary() {
        let s = render_proposals(&[
            proposal(12, "action_approval", "Bash", "needs a shell"),
            proposal(13, "recruit", "Hire", "more hands"),
        ]);
        assert!(s.contains("12") && s.contains("action_approval") && s.contains("Bash"));
        assert!(s.contains("needs a shell") && s.contains("13") && s.contains("recruit"));
        assert_eq!(render_proposals(&[]), "No open proposals.");
    }

    #[test]
    fn render_projects_filters_case_insensitively() {
        let list = [project("Alpha", 2), project("beta", 0)];
        let s = render_projects(&list, Some("ALP"));
        assert!(s.contains("Alpha") && !s.contains("beta"), "{s}");
        assert!(s.contains("Pending: 2"), "{s}");
        let all = render_projects(&list, None);
        assert!(all.contains("Alpha") && all.contains("beta"), "{all}");
        assert!(render_projects(&list, Some("zzz")).contains("No project matches"));
    }

    #[test]
    fn render_inbox_shows_sender_subject_and_class() {
        let s = render_inbox(&[
            mail(Some("Ana"), "Invoice", None, None),
            mail(None, "Hello", Some("urgent"), Some("asks for a call")),
        ]);
        assert!(s.contains("Ana") && s.contains("Invoice"), "{s}");
        assert!(s.contains("[urgent]") && s.contains("a@example.com"), "{s}");
        assert!(s.contains("asks for a call"), "{s}");
        assert_eq!(render_inbox(&[]), "Nothing in the mailbox yet.");
    }

    #[test]
    fn render_triage_reports_queued_and_reason() {
        use crate::governance::triage::TriageOutcome;
        let started = render_triage(&TriageOutcome {
            queued: 3,
            run_id: Some(9),
            reason: None,
        });
        assert!(started.contains('3'), "{started}");
        let idle = render_triage(&TriageOutcome {
            queued: 0,
            run_id: None,
            reason: Some("mail is not armed".into()),
        });
        assert!(idle.contains("mail is not armed"), "{idle}");
    }

    #[test]
    fn pair_command_has_the_exact_phrase() {
        assert_eq!(pair_command("482913"), "/pair 482913");
    }

    #[test]
    fn help_text_names_approve_and_reject_in_english_only() {
        let s = render(&Text::Help);
        assert!(s.contains("approve N") && s.contains("reject N"), "{s}");
        assert!(!s.contains("aprovar") && !s.contains("rejeitar"), "{s}");
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
