//! Chat commands (flow D, Anexo D D.8): the port of `shortcuts/router.go`. P0 holds the `Intent`
//! type; A2 owns the real routing and `execute`.
//!
//! `execute`, `answer_capture` and `capture_target` take `ingest::Inbound` and `contract::InMessage`
//! as annex D says. In A1 the placeholder `route` only ever yields `Pair` and `Chat`, and
//! `capture_target` finds nothing, so `execute` and `answer_capture` are unreachable until A2.

use super::strings::{self, Text};
use super::{contract, ingest, media, outbox};
use crate::state::AppState;

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

/// Runs a command and enqueues its answer in the same conversation, through service functions only
/// (no HTTP). C2a covers help, budget, proposals, projects and a refused voice command; the rest
/// arrive with C2b (mail, inbox, note) and C3 (kill, cancel, approve, reject), and C4 (cap).
pub async fn execute(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    intent: Intent,
) -> sqlx::Result<()> {
    let now = chrono::Utc::now();
    let reply = match intent {
        Intent::Help => strings::render(&Text::Help),
        Intent::Budget => {
            let snapshot = crate::governance::budget::snapshot(&state.pool, now).await?;
            strings::render(&Text::Budget(snapshot))
        }
        Intent::Proposals => {
            let pending = crate::proposals::list_pending(&state.pool).await?;
            strings::render_proposals(&pending)
        }
        Intent::Proj { filter } => {
            let roster = crate::governance::autopilot::project_roster(&state.pool).await?;
            strings::render_projects(&roster, filter.as_deref())
        }
        Intent::Refused(text) => strings::render(&Text::HeardCommand { text }),
        Intent::Pair(_) => {
            // `ingest` runs `/pair` through `access::pair` before it reaches here.
            tracing::error!("a pairing command reached commands::execute");
            return Ok(());
        }
        Intent::Mail => run_mail(state, now).await,
        Intent::Inbox => {
            let queue = crate::integrations::email::queue(&state.pool, INBOX_LIMIT).await?;
            strings::render_inbox(&queue)
        }
        Intent::Note(text) => run_note(state, at, &text).await,
        Intent::Kill { on } => {
            crate::governance::autopilot::set_kill_switch(&state.pool, on).await?;
            strings::render(&if on {
                Text::KillEngaged
            } else {
                Text::KillDisengaged
            })
        }
        // The only answer when nothing runs is `NothingToCancel`: no pause notice and no touch on
        // the queue, so a queue that was held stays held and a free one stays free. When a turn is
        // stopped, `stop_chat` owns the single pause notice; the reply here never repeats it.
        Intent::Cancel => match crate::assistant::stop_chat(state, &at.key.chat_id()).await {
            None => strings::render(&Text::NothingToCancel),
            Some((turn, _)) => strings::render(&Text::Cancelled { turn }),
        },
        Intent::Approve(id) => {
            let decided =
                crate::proposals::approve(state, id, crate::proposals::ApproveInput::default())
                    .await
                    .map(|_| ());
            decision_reply(decided, Text::Approved { id })
        }
        Intent::Reject(id) => {
            let decided = crate::proposals::reject(state, id).await;
            decision_reply(decided, Text::Rejected { id })
        }
        Intent::Cap { .. } | Intent::Chat(_) => todo!(),
    };
    say(state, &at.key, reply, now).await
}

/// How many queued mails `/inbox` reads: the same bound the HTTP door uses.
const INBOX_LIMIT: i64 = 200;

/// `/mail`: triage what is waiting. An unarmed pillar is an answer of its own.
async fn run_mail(state: &AppState, now: chrono::DateTime<chrono::Utc>) -> String {
    if !state.email.armed.load(std::sync::atomic::Ordering::Relaxed) {
        return strings::render(&Text::MailNotArmed);
    }
    match crate::governance::triage::triage_on_request(state, now).await {
        Ok(outcome) => strings::render_triage(&outcome),
        Err(crate::governance::triage::TriageError(status)) => strings::render(&Text::NotSent {
            refusal: format!("mail triage failed ({status})"),
        }),
    }
}

/// `/note <text>`: files the text as a note whose origin is this channel. The text never reaches a
/// log line or an error reply.
async fn run_note(state: &AppState, at: &ingest::Inbound<'_>, text: &str) -> String {
    if text.trim().is_empty() {
        return strings::render(&Text::NoteUsage);
    }
    let origin = crate::assistant::Origin::Channel(at.channel).as_wire();
    match crate::owner_notes::create(&state.pool, text, origin).await {
        Ok(id) => strings::render(&Text::Noted { id }),
        Err(error) => {
            tracing::warn!(channel = at.channel, %error, "a channel note could not be saved");
            strings::render(&Text::NoteFailed)
        }
    }
}

/// The reply to an approve or reject: `done` when it went through, "already handled" when somebody
/// decided first or the proposal is gone, the refusal sentence otherwise.
fn decision_reply(result: Result<(), crate::proposals::DecideError>, done: Text) -> String {
    use crate::proposals::DecideError;
    match result {
        Ok(()) => strings::render(&done),
        Err(DecideError::AlreadyDecided | DecideError::NotFound) => {
            strings::render(&Text::AlreadyHandled)
        }
        Err(error) => {
            let refusal = match error {
                DecideError::Conflict(message)
                | DecideError::Invalid(message)
                | DecideError::BadRequest(message)
                | DecideError::BadGateway(message)
                | DecideError::Status(_, message)
                    if !message.is_empty() =>
                {
                    message
                }
                _ => "the decision could not be recorded".to_string(),
            };
            strings::render(&Text::NotSent { refusal })
        }
    }
}

/// Queues a plain text answer in the conversation the command came from and wakes its channel.
async fn say(
    state: &AppState,
    key: &contract::ConversationKey,
    text: String,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<()> {
    let action = outbox::NewAction::Send {
        reply_to: None,
        parts: vec![contract::OutPart::Text { text }],
        buttons: None,
        silent: false,
    };
    let mut conn = state.pool.acquire().await?;
    outbox::enqueue(&mut conn, key, action, outbox::Class::Command, None, now).await?;
    drop(conn);
    outbox::wake(state, key.channel);
    Ok(())
}

/// Which capture request a message answers, if any. Placeholder: A1 has no capture requests on the
/// outbox yet, so nothing is ever a capture answer; A2 replaces the body.
pub async fn capture_target(
    _state: &AppState,
    _at: &ingest::Inbound<'_>,
    _message: &contract::InMessage,
) -> sqlx::Result<Option<i64>> {
    Ok(None)
}

/// Answers a capture with typed text only. A2: reachable only through a `Some` from
/// `capture_target`, which A1 never returns.
pub async fn answer_capture(
    _state: &AppState,
    _at: &ingest::Inbound<'_>,
    _job: i64,
    _input: &media::TurnInput,
) -> sqlx::Result<()> {
    todo!()
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

    fn conversation() -> contract::Conversation {
        contract::Conversation {
            reference: "c1".to_string(),
            kind: contract::ConversationKind::Private,
            parent: None,
        }
    }

    fn sender() -> contract::Sender {
        contract::Sender {
            reference: "u1".to_string(),
            name: None,
        }
    }

    /// Runs `intent` as if it came from conversation `fake:c1`.
    async fn run(state: &AppState, intent: Intent) {
        let (conversation, sender) = (conversation(), sender());
        let at = ingest::Inbound {
            channel: "fake",
            key: contract::ConversationKey::new("fake", "c1"),
            conversation: &conversation,
            sender: &sender,
            message_ref: "m1",
        };
        execute(state, &at, intent).await.unwrap();
    }

    /// Every queued answer as (conversation, class, text), oldest first.
    async fn queued(state: &AppState) -> Vec<(String, String, String)> {
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT conversation, class, payload FROM channel_outbox ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(conversation, class, payload)| {
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
                let text = payload["parts"][0]["text"].as_str().unwrap().to_string();
                (conversation, class, text)
            })
            .collect()
    }

    async fn the_one_answer(state: &AppState) -> String {
        let mut all = queued(state).await;
        assert_eq!(all.len(), 1, "{all:?}");
        let (conversation, class, text) = all.remove(0);
        assert_eq!(conversation, "c1");
        assert_eq!(class, "command");
        text
    }

    #[tokio::test]
    async fn execute_help_enqueues_the_help_text() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Help).await;
        assert_eq!(the_one_answer(&state).await, strings::render(&Text::Help));
    }

    #[tokio::test]
    async fn execute_budget_enqueues_the_snapshot() {
        let state = crate::channels::testkit::state().await;
        let snapshot = crate::governance::budget::snapshot(&state.pool, chrono::Utc::now())
            .await
            .unwrap();
        run(&state, Intent::Budget).await;
        let answer = the_one_answer(&state).await;
        assert_eq!(answer, strings::render(&Text::Budget(snapshot)));
        assert!(answer.starts_with("Budget ("), "{answer}");
    }

    #[tokio::test]
    async fn execute_proposals_empty_says_none() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Proposals).await;
        assert_eq!(the_one_answer(&state).await, "No open proposals.");
    }

    #[tokio::test]
    async fn execute_proposals_lists_the_pending_ones() {
        let state = crate::channels::testkit::state().await;
        // `list_pending` is the approvals queue: only `action-approval` rows count, so a
        // contact-merge proposal would (rightly) not be listed.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO proposals
             (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
             VALUES ('action-approval', 'pending', NULL, NULL, NULL, 'Bash', 'same person', NULL, ?, NULL)
             RETURNING id",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        run(&state, Intent::Proposals).await;
        let answer = the_one_answer(&state).await;
        assert!(
            answer.contains(&format!("proposal {id} (action-approval): Bash")),
            "{answer}"
        );
        assert!(answer.contains("same person"), "{answer}");
    }

    #[tokio::test]
    async fn execute_proj_filter_applies() {
        let state = crate::channels::testkit::state().await;
        for project in ["alpha-app", "beta-tool"] {
            sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'off')")
                .bind(project)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        run(
            &state,
            Intent::Proj {
                filter: Some("ALPHA".to_string()),
            },
        )
        .await;
        let answer = the_one_answer(&state).await;
        assert!(answer.contains("alpha-app"), "{answer}");
        assert!(!answer.contains("beta-tool"), "{answer}");
    }

    #[tokio::test]
    async fn execute_refused_enqueues_the_heard_command_text() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Refused("kill off".to_string())).await;
        assert_eq!(
            the_one_answer(&state).await,
            strings::render(&Text::HeardCommand {
                text: "kill off".to_string()
            })
        );
    }

    #[tokio::test]
    async fn execute_mail_not_armed_says_so() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Mail).await;
        assert_eq!(the_one_answer(&state).await, "Mail is not armed.");
    }

    #[tokio::test]
    async fn execute_inbox_lists_queued_mail() {
        let state = crate::channels::testkit::state().await;
        sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, subject,
                                 received_at, ingested_at, direction)
             VALUES ('<i1@x>', 'INBOX', 1, 1, 'rita@example.com', 'July invoice',
                     '2026-07-28T09:00:00Z', '2026-07-28T09:00:00Z', 'inbound')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        run(&state, Intent::Inbox).await;
        let answer = the_one_answer(&state).await;
        assert!(answer.contains("rita@example.com"), "{answer}");
        assert!(answer.contains("July invoice"), "{answer}");
    }

    #[tokio::test]
    async fn execute_note_creates_with_channel_origin() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Note("buy milk".to_string())).await;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT note_text, origin FROM owner_notes")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![("buy milk".to_string(), "channel:fake".to_string())]
        );
        let answer = the_one_answer(&state).await;
        assert!(answer.starts_with("Noted (#"), "{answer}");
    }

    #[tokio::test]
    async fn execute_note_bare_answers_usage() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Note(String::new())).await;
        assert_eq!(the_one_answer(&state).await, "Usage: /note <text>");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owner_notes")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn execute_kill_on_off_sets_switch_and_answers() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Kill { on: true }).await;
        assert!(
            crate::governance::autopilot::kill_switch_engaged(&state.pool)
                .await
                .unwrap()
        );
        run(&state, Intent::Kill { on: false }).await;
        assert!(
            !crate::governance::autopilot::kill_switch_engaged(&state.pool)
                .await
                .unwrap()
        );
        let texts: Vec<String> = queued(&state).await.into_iter().map(|q| q.2).collect();
        assert_eq!(texts, vec!["Kill switch ENGAGED", "Kill switch DISENGAGED"]);
    }

    #[tokio::test]
    async fn execute_cancel_nothing_running_leaves_the_queue_hold_as_it_was() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Cancel).await;
        // The only message is the exact text, with no pause notice beside it. The hold itself is
        // private to the assistant module, where the `stop_chat` test asserts it stays as it was.
        assert_eq!(the_one_answer(&state).await, "Nothing to stop.");
    }

    #[tokio::test]
    async fn execute_cancel_stops_turn_and_does_not_repeat_pause_text() {
        let state = crate::channels::testkit::state().await;
        let turn: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('slow', 'running', 'assistant', 'fake:c1', '2026-10-20T00:00:00Z')
             RETURNING id",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        // No process is registered for the chat, so the stop takes the kill path, which needs a handle.
        let task = tokio::spawn(tokio::time::sleep(std::time::Duration::from_secs(60)));
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(turn, task.abort_handle());

        run(&state, Intent::Cancel).await;

        let answer = the_one_answer(&state).await;
        assert_eq!(answer, format!("Cancelled turn {turn}."));
        assert!(!answer.to_lowercase().contains("paused"), "{answer}");
        task.abort();
    }

    async fn a_proposal(state: &AppState, status: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO proposals
             (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
             VALUES ('action-approval', ?, NULL, NULL, NULL, 'Bash', 'why', NULL, ?, NULL)
             RETURNING id",
        )
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn execute_approve_maps_already_decided() {
        let state = crate::channels::testkit::state().await;
        let decided = a_proposal(&state, "approved").await;
        run(&state, Intent::Approve(decided)).await;
        run(&state, Intent::Approve(987_654)).await;
        let texts: Vec<String> = queued(&state).await.into_iter().map(|q| q.2).collect();
        assert_eq!(
            texts,
            vec![
                "That one was already handled.",
                "That one was already handled."
            ]
        );
    }

    #[tokio::test]
    async fn execute_reject_resolves_a_pending_proposal() {
        let state = crate::channels::testkit::state().await;
        let id = a_proposal(&state, "pending").await;
        run(&state, Intent::Reject(id)).await;
        assert_eq!(the_one_answer(&state).await, format!("Rejected #{id}."));
        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "rejected");
        // A second reject finds it decided.
        run(&state, Intent::Reject(id)).await;
        let last = queued(&state).await.pop().unwrap().2;
        assert_eq!(last, "That one was already handled.");
    }
}
