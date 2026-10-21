//! v1 Telegram tests ported to the chat commands, access policy and strings; each is named `port_<original_snake_name>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use nucleos_base::notify_policy::{Policy, Rule};

use super::access::{self, DropReason, Verdict};
use super::commands::{self, Intent};
use super::contract::{
    Conversation, ConversationKey, ConversationKind, EventBody, InAction, InMessage, InPart,
    InboundEvent, IngestReply, Sender,
};
use super::ingest::{self, Inbound};
use super::media::TurnInput;
use super::registry::ChannelConfig;
use super::routing::{self, Wanted};
use super::strings::{self, Text};
use super::tokens::{self, TokenAction};
use super::{ChannelsRuntime, testkit};
use crate::state::AppState;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 20, 10, 0, 0).unwrap()
}

fn rule(selector: &str, enabled: bool) -> Rule {
    Rule {
        selector: selector.to_owned(),
        enabled,
    }
}

// ---------------------------------------------------------------------------------------------
// Commands run through `commands::execute` on the `fake` channel.
// ---------------------------------------------------------------------------------------------

fn conversation() -> Conversation {
    Conversation {
        reference: "c1".to_string(),
        kind: ConversationKind::Private,
        parent: None,
    }
}

fn sender() -> Sender {
    Sender {
        reference: "u1".to_string(),
        name: None,
    }
}

fn inbound<'a>(conversation: &'a Conversation, sender: &'a Sender) -> Inbound<'a> {
    Inbound {
        channel: "fake",
        key: ConversationKey::new("fake", "c1"),
        conversation,
        sender,
        message_ref: "m1",
    }
}

async fn run(state: &AppState, intent: Intent) {
    let (c, s) = (conversation(), sender());
    commands::execute(state, &inbound(&c, &s), intent)
        .await
        .unwrap();
}

/// The text of every queued outbox row that carries one, oldest first.
async fn answers(state: &AppState) -> Vec<String> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT payload FROM channel_outbox ORDER BY id")
        .fetch_all(&state.pool)
        .await
        .unwrap();
    rows.iter()
        .filter_map(|payload| {
            let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
            payload["parts"][0]["text"].as_str().map(str::to_owned)
        })
        .collect()
}

async fn count(state: &AppState, table: &str) -> i64 {
    // Test-only: `table` is always a literal table name from this file.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

/// What a log line is allowed to carry: collected here so a test can look for text in it.
#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;

    fn make_writer(&'a self) -> LogBuf {
        self.clone()
    }
}

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Routes every tracing event of this thread into the returned buffer until the guard drops.
fn capture_log() -> (LogBuf, tracing::subscriber::DefaultGuard) {
    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    (buf, tracing::subscriber::set_default(subscriber))
}

// ---------------------------------------------------------------------------------------------
// pipe_test.go: strings and commands
// ---------------------------------------------------------------------------------------------

fn budget(paused: bool, reason: Option<&str>) -> crate::governance::budget::BudgetSnapshot {
    crate::governance::budget::BudgetSnapshot {
        limit_usd: Some(50.0),
        period: "month".into(),
        hourly_limit_usd: None,
        per_run_reserve_usd: 0.5,
        time_cost_per_hour_usd: 0.0,
        window_spend_usd: 12.5,
        hourly_spend_usd: 0.25,
        paused,
        reason: reason.map(Into::into),
        relay_chain_limit_usd: 5.0,
    }
}

#[test]
fn port_format_budget() {
    let paused = strings::render(&Text::Budget(budget(true, Some("cap"))));
    assert!(
        paused.contains("paused") && paused.contains("cap"),
        "{paused}"
    );
    let active = strings::render(&Text::Budget(budget(false, None)));
    assert!(active.contains("allowed"), "{active}");
    assert!(!active.contains("paused"), "{active}");
}

#[tokio::test]
async fn port_proj_shortcut_is_deterministic_no_llm() {
    let state = testkit::state().await;
    for project in ["a", "b"] {
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'off')")
            .bind(project)
            .execute(&state.pool)
            .await
            .unwrap();
    }
    run(&state, Intent::Proj { filter: None }).await;
    let said = answers(&state).await;
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("**a**") && said[0].contains("**b**"),
        "{said:?}"
    );
    // A shortcut never goes through the model: no turn was recorded for it.
    assert_eq!(count(&state, "runs").await, 0);
}

fn a_proposal(project: Option<&str>) -> crate::proposals::Proposal {
    crate::proposals::Proposal {
        id: 4,
        kind: "action-approval".into(),
        status: "pending".into(),
        run_id: None,
        session_id: None,
        project_id: project.map(Into::into),
        tool_name: Some("send_email".into()),
        reasoning: "this turn has read third-party content and can no longer act".into(),
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

#[test]
fn port_a_proposal_without_a_project_does_not_invent_one() {
    let got = strings::render_proposals(&[a_proposal(None)]);
    assert!(!got.contains("project"), "{got}");
    assert!(
        !got.contains("nil") && !got.contains("null") && !got.contains("None"),
        "{got}"
    );
    // The counterpart: a proposal that has one names it.
    let named = strings::render_proposals(&[a_proposal(Some("nucleos"))]);
    assert!(named.contains("project: nucleos"), "{named}");
}

/// Adapted: a busy conversation no longer refuses (the message is queued), so the "gesture" a
/// refusal names is the queue position; a real refusal is said as a sentence, never a status code.
#[test]
fn port_a_turn_refused_says_what_undoes_it() {
    let refused = strings::render(&Text::NotSent {
        refusal: crate::assistant::NO_LOCAL_MODEL.to_string(),
    });
    assert!(refused.contains("local model"), "{refused}");
    assert!(!refused.contains("status code"), "{refused}");
    let busy = strings::render(&Text::Queued { position: 2 });
    assert!(busy.contains("Queued") && busy.contains('2'), "{busy}");
}

#[test]
fn port_a_turn_that_failed_for_no_stated_reason_still_says_so() {
    let got = strings::render(&Text::NotSent {
        refusal: "perform request: connection refused".to_string(),
    });
    assert!(got.contains("connection refused"), "{got}");
    // No remedy is invented for a failure that stated none.
    assert!(
        !got.contains("/cancel") && !got.contains("/resume"),
        "{got}"
    );
    // A busy turn does name the way out.
    let busy = strings::render(&Text::NotSent {
        refusal: crate::assistant::TURN_IN_PROGRESS.to_string(),
    });
    assert!(busy.contains("/cancel"), "{busy}");
}

// ---------------------------------------------------------------------------------------------
// Access: the senders that may command the bot.
// ---------------------------------------------------------------------------------------------

async fn owner(state: &AppState, sender: &str, conversation: &str, kind: &str) {
    sqlx::query(
        "INSERT INTO channel_identities \
         (channel, sender_ref, conversation_ref, conversation_kind, home, created_at) \
         VALUES ('telegram', ?, ?, ?, 0, '2026-10-20T10:00:00Z')",
    )
    .bind(sender)
    .bind(conversation)
    .bind(kind)
    .execute(&state.pool)
    .await
    .unwrap();
}

async fn verdict(state: &AppState, chat: &str, kind: ConversationKind, user: &str) -> Verdict {
    let conversation = Conversation {
        reference: chat.to_string(),
        kind,
        parent: None,
    };
    let sender = Sender {
        reference: user.to_string(),
        name: None,
    };
    access::admit(state, "telegram", &conversation, &sender)
        .await
        .unwrap()
}

#[tokio::test]
async fn port_config_is_allowed() {
    let state = testkit::state().await;
    // Nobody paired allows nobody.
    assert_eq!(
        verdict(&state, "42", ConversationKind::Private, "0").await,
        Verdict::Drop(DropReason::UnknownSender)
    );
    owner(&state, "42", "42", "private").await;
    assert_eq!(
        verdict(&state, "42", ConversationKind::Private, "42").await,
        Verdict::Admit
    );
    // The owner in a conversation that was never paired is denied.
    assert_eq!(
        verdict(&state, "7", ConversationKind::Private, "42").await,
        Verdict::Drop(DropReason::ConversationNotAllowed)
    );
}

#[tokio::test]
async fn port_only_the_configured_sender_can_command_the_bot() {
    let state = testkit::state().await;
    let group = "-100200300";
    // A group with two listed members.
    owner(&state, "7", group, "group").await;
    owner(&state, "9", group, "group").await;
    // A private chat whose owner is somebody else than the chat id.
    owner(&state, "7", "42", "private").await;

    let unknown = Verdict::Drop(DropReason::UnknownSender);
    assert_eq!(
        verdict(&state, group, ConversationKind::Group, "9").await,
        Verdict::Admit
    );
    assert_eq!(
        verdict(&state, group, ConversationKind::Group, "7").await,
        Verdict::Admit
    );
    // A member who is not on the list.
    assert_eq!(
        verdict(&state, group, ConversationKind::Group, "8").await,
        unknown
    );
    // No sender attributed.
    assert_eq!(
        verdict(&state, "42", ConversationKind::Private, "").await,
        unknown
    );
    // The allowlist overrides the private-chat shortcut: the chat id is not a sender.
    assert_eq!(
        verdict(&state, "42", ConversationKind::Private, "42").await,
        unknown
    );
}

fn event(id: &str, who: &str, kind: ConversationKind, chat: &str, body: EventBody) -> InboundEvent {
    InboundEvent {
        event_id: id.to_string(),
        at: None,
        conversation: Conversation {
            reference: chat.to_string(),
            kind,
            parent: None,
        },
        sender: Sender {
            reference: who.to_string(),
            name: None,
        },
        body,
    }
}

fn typed(reply_to: Option<&str>, text: &str) -> EventBody {
    EventBody::Message {
        message: InMessage {
            reference: "m1".to_string(),
            reply_to: reply_to.map(str::to_string),
            parts: vec![InPart::Text {
                text: text.to_string(),
            }],
        },
    }
}

async fn send(
    state: &AppState,
    id: &str,
    who: &str,
    kind: ConversationKind,
    chat: &str,
    body: EventBody,
) -> IngestReply {
    ingest::ingest_event(state, "telegram", event(id, who, kind, chat, body), now()).await
}

#[tokio::test]
async fn port_only_an_authorised_sender_reaches_the_daemon() {
    let state = testkit::state().await;
    let group = "-100200300";
    owner(&state, "7", group, "group").await;
    let engaged = || crate::governance::autopilot::kill_switch_engaged(&state.pool);

    let reply = send(
        &state,
        "e1",
        "8",
        ConversationKind::Group,
        group,
        typed(None, "/kill on"),
    )
    .await;
    assert_eq!(reply, IngestReply::Dropped);
    assert!(
        !engaged().await.unwrap(),
        "an unlisted member engaged the kill switch"
    );

    let reply = send(
        &state,
        "e2",
        "7",
        ConversationKind::Group,
        group,
        typed(None, "/kill on"),
    )
    .await;
    assert_eq!(reply, IngestReply::Accepted);
    assert!(engaged().await.unwrap());
}

async fn pending_proposal(state: &AppState) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('action-approval', 'pending', NULL, NULL, NULL, 'Bash', 'why', NULL, ?, NULL)
         RETURNING id",
    )
    .bind(now().to_rfc3339())
    .fetch_one(&state.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn port_an_unauthorised_press_cannot_approve_a_proposal() {
    let state = testkit::state().await;
    let group = "-100200300";
    owner(&state, "7", group, "group").await;
    let id = pending_proposal(&state).await;
    let key = ConversationKey::new("telegram", group.to_string());
    let mut conn = state.pool.acquire().await.unwrap();
    let token = tokens::issue(&mut conn, &key, TokenAction::Approve, id, now())
        .await
        .unwrap();
    drop(conn);

    let press = EventBody::Action {
        action: InAction {
            reference: "q".to_string(),
            message_ref: "990".to_string(),
            token,
        },
    };
    let reply = send(&state, "e1", "8", ConversationKind::Group, group, press).await;
    assert_eq!(reply, IngestReply::Dropped);
    let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
}

// ---------------------------------------------------------------------------------------------
// /note
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn port_note_from_an_unauthorised_chat_is_ignored() {
    let state = testkit::state().await;
    owner(&state, "42", "42", "private").await;
    let reply = send(
        &state,
        "e1",
        "99",
        ConversationKind::Private,
        "99",
        typed(None, "/note buy milk"),
    )
    .await;
    assert_eq!(reply, IngestReply::Dropped);
    assert_eq!(count(&state, "owner_notes").await, 0);
}

#[tokio::test]
async fn port_note_text_never_reaches_the_log() {
    const SECRET: &str = "the-secret-diary-entry";
    let (log, _guard) = capture_log();

    let state = testkit::state().await;
    run(&state, Intent::Note(SECRET.to_string())).await;
    assert_eq!(count(&state, "owner_notes").await, 1);

    // The failing path: a store that cannot take the note. If the drop is refused the second half
    // simply repeats the first, which the assertion below tolerates.
    let _ = sqlx::query("DROP TABLE owner_notes")
        .execute(&state.pool)
        .await;
    run(&state, Intent::Note(SECRET.to_string())).await;

    assert!(
        !log.text().contains(SECRET),
        "the log carries the note text: {}",
        log.text()
    );
}

// ---------------------------------------------------------------------------------------------
// pipe/capture_test.go
// ---------------------------------------------------------------------------------------------

async fn open_request(state: &AppState, job: i64) {
    sqlx::query(
        "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, \
         deadline, created_at) VALUES (?, 'p', '[]', 'what happened?', 'open', \
         '2099-01-01T00:00:00Z', '2026-10-20T00:00:00Z')",
    )
    .bind(job)
    .execute(&state.pool)
    .await
    .unwrap();
}

/// A delivered outbox row of `telegram:<conversation>` carrying platform ref `platform_ref`.
async fn delivered(state: &AppState, conversation: &str, platform_ref: &str, subject: &str) {
    sqlx::query(
        "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
         subject, state, due_at, platform_refs, attempts, created_at, settled_at) \
         VALUES ('telegram', ?, 'ordered', 'send', '{}', 'turn', ?, 'done', 't', ?, 0, 't', 's')",
    )
    .bind(conversation)
    .bind(subject)
    .bind(format!("[\"{platform_ref}\"]"))
    .execute(&state.pool)
    .await
    .unwrap();
}

async fn request_state(state: &AppState, job: i64) -> String {
    sqlx::query_scalar("SELECT state FROM capture_requests WHERE job_id = ?")
        .bind(job)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn port_a_reply_to_a_capture_request_answers_it_without_a_turn() {
    let state = testkit::state().await;
    owner(&state, "u1", "c1", "private").await;
    open_request(&state, 7).await;
    delivered(&state, "c1", "p9", "capture:7").await;

    let body = typed(Some("p9"), "it was the VPN");
    let reply = send(&state, "e1", "u1", ConversationKind::Private, "c1", body).await;
    assert_eq!(reply, IngestReply::Accepted);

    assert_eq!(request_state(&state, 7).await, "answered");
    assert_eq!(
        answers(&state).await,
        vec![strings::render(&Text::CaptureAnswered { job: 7 })]
    );
    assert_eq!(
        count(&state, "runs").await,
        0,
        "a capture answer started a turn"
    );
    let note: String = sqlx::query_scalar("SELECT note_text FROM owner_notes")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(note, "it was the VPN");
}

#[tokio::test]
async fn port_a_late_answer_says_the_distiller_moved_on() {
    // The late answer keeps its note and says the question has moved on; it releases nothing.
    let state = testkit::state().await;
    open_request(&state, 7).await;
    let (c, s) = (conversation(), sender());
    let at = inbound(&c, &s);
    for text in ["first", "late"] {
        let input = TurnInput {
            text: text.to_string(),
            ..Default::default()
        };
        commands::answer_capture(&state, &at, 7, &input)
            .await
            .unwrap();
    }
    assert_eq!(request_state(&state, 7).await, "answered");
    assert_eq!(count(&state, "owner_notes").await, 2);
    let said = answers(&state).await;
    let late = strings::render(&Text::CaptureLate { note: 2 });
    assert!(late.contains("moved on"), "{late}");
    assert_eq!(
        said,
        vec![strings::render(&Text::CaptureAnswered { job: 7 }), late]
    );
}

#[tokio::test]
async fn port_a_reply_without_the_mark_or_not_to_a_bot_is_an_ordinary_turn() {
    let state = testkit::state().await;
    owner(&state, "u1", "c1", "private").await;
    open_request(&state, 7).await;
    // A reply to a delivered message that is not a capture request.
    delivered(&state, "c1", "p8", "proposal:3").await;

    let body = typed(Some("p8"), "hello");
    let reply = send(&state, "e1", "u1", ConversationKind::Private, "c1", body).await;
    assert_eq!(reply, IngestReply::Accepted);
    assert_eq!(request_state(&state, 7).await, "open");
    assert_eq!(count(&state, "owner_notes").await, 0);
    let turns: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = 'telegram:c1'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(turns, 1);
}

#[tokio::test]
async fn port_a_reply_from_an_unauthorised_sender_never_answers() {
    let state = testkit::state().await;
    owner(&state, "u1", "c1", "private").await;
    open_request(&state, 7).await;
    delivered(&state, "c1", "p9", "capture:7").await;

    let body = typed(Some("p9"), "x");
    let reply = send(&state, "e1", "5", ConversationKind::Private, "c1", body).await;
    assert_eq!(reply, IngestReply::Dropped);
    assert_eq!(request_state(&state, 7).await, "open");
    assert_eq!(count(&state, "owner_notes").await, 0);
}

#[tokio::test]
async fn port_a_capture_answer_never_reaches_the_log() {
    const SECRET: &str = "the-secret-capture-answer";
    let (log, _guard) = capture_log();
    let state = testkit::state().await;
    let (c, s) = (conversation(), sender());
    let at = inbound(&c, &s);
    let input = TurnInput {
        text: SECRET.to_string(),
        ..Default::default()
    };
    // A request that exists, then one that does not (the failing path).
    open_request(&state, 7).await;
    commands::answer_capture(&state, &at, 7, &input)
        .await
        .unwrap();
    commands::answer_capture(&state, &at, 99, &input)
        .await
        .unwrap();
    assert!(
        !log.text().contains(SECRET),
        "the log carries the answer: {}",
        log.text()
    );
}

// ---------------------------------------------------------------------------------------------
// pipe/mail_test.go
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn port_inbox_shortcut_spends_nothing() {
    let state = testkit::state().await;
    for (uid, subject, class, summary) in [
        (1, "production is down", None, None),
        (2, "notes", Some("info"), Some("no action")),
    ] {
        sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, subject,
                                 received_at, ingested_at, direction, triage_class, triage_summary)
             VALUES (?, 'INBOX', 1, ?, 'rui@example.com', ?,
                     '2026-07-28T09:00:00Z', '2026-07-28T09:00:00Z', 'inbound', ?, ?)",
        )
        .bind(format!("<m{uid}@x>"))
        .bind(uid)
        .bind(subject)
        .bind(class)
        .bind(summary)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    run(&state, Intent::Inbox).await;
    let said = answers(&state).await;
    assert_eq!(said.len(), 1, "{said:?}");
    let text = &said[0];
    assert!(text.contains("Waiting to be read (1)"), "{text}");
    assert!(text.contains("production is down"), "{text}");
    assert!(
        text.contains("[info]") && text.contains("no action"),
        "{text}"
    );
    // Looking costs nothing: no triage run was started.
    assert_eq!(count(&state, "runs").await, 0);
}

#[tokio::test]
async fn port_inbox_with_nothing_says_so() {
    let state = testkit::state().await;
    run(&state, Intent::Inbox).await;
    assert_eq!(
        answers(&state).await,
        vec!["Nothing in the mailbox yet.".to_string()]
    );
}

// ---------------------------------------------------------------------------------------------
// Routing: the feed, its policy and plain text (pipe rendering / notifier policy tests)
// ---------------------------------------------------------------------------------------------

const HOME: &str = "-100";

/// A state whose primary channel is `telegram`, configured, with a home and a seeded routing row.
async fn routed() -> AppState {
    let mut state = testkit::state().await;
    let mut configs = HashMap::new();
    configs.insert("telegram", ChannelConfig::default());
    state.channels = Arc::new(ChannelsRuntime::with_configs(configs));
    state
        .channels
        .presence
        .set_secrets_present("telegram", Vec::new());
    sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
         conversation_kind, home, created_at) VALUES ('telegram', '42', ?, 'group', 1, 't')",
    )
    .bind(HOME)
    .execute(&state.pool)
    .await
    .unwrap();
    let mut conn = state.pool.acquire().await.unwrap();
    routing::seed(&mut conn, "telegram").await.unwrap();
    drop(conn);
    state
}

async fn feed_line(state: &AppState, kind: &str, summary: &str) {
    nucleos_base::feed::append(&state.pool, None, kind, summary, None, None)
        .await
        .unwrap();
}

fn wanted(feed: bool, proposals: bool, kill: bool) -> Wanted {
    Wanted {
        feed,
        proposals,
        kill,
        ..Wanted::default()
    }
}

#[tokio::test]
async fn port_feed_notifications_are_sent_as_plain_text() {
    let state = routed().await;
    feed_line(&state, "mail_triaged", "<b>bold</b> & <script>x</script>").await;
    routing::route_once(&state, now(), wanted(true, false, false))
        .await
        .unwrap();

    let payload: String = sqlx::query_scalar("SELECT payload FROM channel_outbox")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    // A neutral text part carrying the markup literally: the adapter, not the hub, decides how
    // to render it, and a model-written summary is never promoted to markup.
    assert_eq!(payload["parts"][0]["type"], "text");
    assert_eq!(
        payload["parts"][0]["text"],
        "mail_triaged: <b>bold</b> & <script>x</script>"
    );
    assert_eq!(
        strings::render(&Text::Feed {
            kind: "k".into(),
            summary: "<i>s</i>".into()
        }),
        "k: <i>s</i>"
    );
}

#[tokio::test]
async fn port_governance_is_never_silenced() {
    let state = routed().await;
    let muted = Policy {
        families: vec![rule("job_", false)],
        kinds: vec![],
    };
    routing::put_channel_policy(&state.pool, "telegram", &muted, now())
        .await
        .unwrap();
    feed_line(&state, "job_failed", "a job failed").await;
    crate::proposals::create_action_approval(&state.pool, 1, None, None, "git push", "why", None)
        .await
        .unwrap();
    crate::governance::autopilot::set_kill_switch(&state.pool, true)
        .await
        .unwrap();

    routing::route_once(&state, now(), wanted(true, true, true))
        .await
        .unwrap();

    let joined = answers(&state).await.join("\n---\n");
    assert!(
        !joined.contains("a job failed"),
        "the silenced line was sent: {joined}"
    );
    for wanted in ["git push", "Kill switch ENGAGED"] {
        assert!(
            joined.contains(wanted),
            "governance went quiet, {wanted:?} missing: {joined}"
        );
    }
}

#[tokio::test]
async fn port_reenabling_a_family_replays_nothing() {
    let state = routed().await;
    let muted = Policy {
        families: vec![rule("job_", false)],
        kinds: vec![],
    };
    routing::put_channel_policy(&state.pool, "telegram", &muted, now())
        .await
        .unwrap();
    feed_line(&state, "job_failed", "missed while off").await;
    routing::route_once(&state, now(), wanted(true, false, false))
        .await
        .unwrap();
    assert!(answers(&state).await.is_empty());

    let on = Policy {
        families: vec![rule("job_", true)],
        kinds: vec![],
    };
    routing::put_channel_policy(&state.pool, "telegram", &on, now())
        .await
        .unwrap();
    feed_line(&state, "job_failed", "arrived while on").await;
    routing::route_once(&state, now(), wanted(true, false, false))
        .await
        .unwrap();

    // The muted line moved the cursor with it: only what came after is announced.
    assert_eq!(
        answers(&state).await,
        vec!["job_failed: arrived while on".to_string()]
    );
}

#[tokio::test]
async fn port_the_notifier_reads_the_policy_only_when_there_are_new_feed_items() {
    let state = routed().await;
    // A policy store that cannot be read: the pass is fine as long as it never has to look.
    let dropped = sqlx::query("DROP TABLE channel_notify_policy")
        .execute(&state.pool)
        .await
        .is_ok();
    routing::route_once(&state, now(), wanted(true, false, false))
        .await
        .expect("an idle feed read the policy");
    if dropped {
        feed_line(&state, "run", "fresh").await;
        let outcome = routing::route_once(&state, now(), wanted(true, false, false)).await;
        assert!(outcome.is_err(), "a new feed item did not read the policy");
    }
}

#[test]
fn port_allows_resolution() {
    let policy = Policy {
        families: vec![
            rule("job_", false),
            rule("job_item_", true),
            rule("worktree_", true),
        ],
        kinds: vec![rule("job_failed", true), rule("worktree_gc", false)],
    };
    for (kind, want) in [
        ("job_failed", true),
        ("worktree_gc", false),
        ("job_started", false),
        ("job_item_done", true),
        ("worktree_pruned", true),
        ("council_opened", true),
        ("nightly_job_started", true),
    ] {
        assert_eq!(routing::passes(&policy, kind), want, "{kind}");
    }

    // The same tie-break with the rules in the other order: "longest wins" and "last wins" only
    // disagree here.
    let reversed = Policy {
        families: vec![rule("job_item_", true), rule("job_", false)],
        kinds: vec![],
    };
    assert!(routing::passes(&reversed, "job_item_done"));
    assert!(!routing::passes(&reversed, "job_started"));
}
