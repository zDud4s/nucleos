//! Adapter ingest: `hello` and `events` (flow A, Anexo D D.6). The routes still answer 501 (RA owns
//! them); `ingest_event` is the whole of `/events` minus the HTTP.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{post, put};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sqlx::SqlitePool;

use super::commands::{self, Intent};
use super::contract::{
    self, ConversationKey, EventBody, InAction, InMessage, InPart, InboundEvent, IngestReply,
    OutPart,
};
use super::outbox::{self, Class, NewAction};
use super::strings::{self, Text};
use super::tokens::{self, Redeemed, TokenAction};
use super::{access, media, replies};
use crate::assistant::{Origin, Sent};
use crate::state::AppState;

/// Retention of `channel_events`: far above the redelivery window of any platform (section 5.3).
const EVENT_TTL: Duration = Duration::days(7);
/// A `processing` row older than this is a crashed ingest: the redelivered event runs again
/// (Q4 = A, decided by the owner). Younger rows are an ingest still in flight, so a redelivery
/// is a duplicate and must not run a second time.
const REPROCESS_AFTER: Duration = Duration::seconds(60);

/// Routes of D.6, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels/{channel}/hello", put(not_implemented))
        .route("/channels/{channel}/events", post(not_implemented))
}

/// Placeholder handler: the real one replaces it in the chunk that owns the route.
async fn not_implemented() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// Everything ingest knows about one inbound message once it is admitted, handed to commands.
pub struct Inbound<'a> {
    pub channel: &'static str,
    pub key: ConversationKey,
    pub conversation: &'a contract::Conversation,
    pub sender: &'a contract::Sender,
    pub message_ref: &'a str,
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Where an event ended up. `Accepted` covers every event that was understood, whatever it did.
enum Processed {
    Accepted,
    Dropped,
}

/// The whole of `/events` minus the HTTP. `channel` comes from the authenticated scope, never from
/// the body (section 7.3). Runs inside `door::uncancellable`: the adapter dropping the connection
/// must not leave a command half-done.
///
/// A database failure cannot be told to the adapter (the reply has three shapes and none is an
/// error): it is logged, the row stays `processing` so a redelivery after a minute runs again, and
/// the reply is `Accepted`.
pub async fn ingest_event(
    state: &AppState,
    channel: &'static str,
    event: InboundEvent,
    now: DateTime<Utc>,
) -> IngestReply {
    match claim(&state.pool, channel, &event.event_id, now).await {
        Ok(true) => {}
        Ok(false) => return IngestReply::Duplicate,
        Err(error) => {
            tracing::warn!(%error, channel, "ingest could not record the event");
            return IngestReply::Accepted;
        }
    }
    let (reply, outcome) = match process(state, channel, &event, now).await {
        Ok(Processed::Accepted) => (IngestReply::Accepted, "accepted"),
        Ok(Processed::Dropped) => (IngestReply::Dropped, "dropped"),
        Err(error) => {
            tracing::warn!(%error, channel, "ingest failed; the event stays processing");
            return IngestReply::Accepted;
        }
    };
    if let Err(error) = settle(&state.pool, channel, &event.event_id, outcome).await {
        tracing::warn!(%error, channel, "ingest could not settle the event");
    }
    reply
}

/// Step 1, dedup: `true` when this call owns the event. A `processing` row older than
/// `REPROCESS_AFTER` is taken over by compare-and-set on its old stamp, so two redeliveries cannot
/// both run it.
async fn claim(
    pool: &SqlitePool,
    channel: &str,
    event_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO channel_events (channel, event_id, received_at, outcome) \
         VALUES (?, ?, ?, 'processing')",
    )
    .bind(channel)
    .bind(event_id)
    .bind(stamp(now))
    .execute(pool)
    .await?;
    if inserted.rows_affected() == 1 {
        return Ok(true);
    }
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT outcome, received_at FROM channel_events WHERE channel = ? AND event_id = ?",
    )
    .bind(channel)
    .bind(event_id)
    .fetch_optional(pool)
    .await?;
    let Some((outcome, received_at)) = row else {
        return Ok(false);
    };
    if outcome != "processing" {
        return Ok(false);
    }
    let stale = parse_time(&received_at).is_none_or(|at| now - at > REPROCESS_AFTER);
    if !stale {
        return Ok(false);
    }
    let taken = sqlx::query(
        "UPDATE channel_events SET received_at = ? \
         WHERE channel = ? AND event_id = ? AND outcome = 'processing' AND received_at = ?",
    )
    .bind(stamp(now))
    .bind(channel)
    .bind(event_id)
    .bind(received_at)
    .execute(pool)
    .await?;
    Ok(taken.rows_affected() == 1)
}

async fn settle(
    pool: &SqlitePool,
    channel: &str,
    event_id: &str,
    outcome: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE channel_events SET outcome = ? WHERE channel = ? AND event_id = ?")
        .bind(outcome)
        .bind(channel)
        .bind(event_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The text of a message made only of text parts; `None` as soon as any part is not text.
fn typed_text(parts: &[InPart]) -> Option<String> {
    let mut lines = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            InPart::Text { text } => lines.push(text.as_str()),
            _ => return None,
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// A pairing code is exactly six digits; anything else is not worth an answer.
fn is_pair_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

async fn process(
    state: &AppState,
    channel: &'static str,
    event: &InboundEvent,
    now: DateTime<Utc>,
) -> sqlx::Result<Processed> {
    let key = ConversationKey::new(channel, event.conversation.reference.clone());

    // Step 2: `/pair <code>` runs BEFORE `admit`, because the sender is unknown until it works.
    if let EventBody::Message { message } = &event.body
        && let Some(text) = typed_text(&message.parts)
        && let Intent::Pair(code) = commands::route(&text)
    {
        let outcome = access::pair(
            state,
            channel,
            &code,
            &event.conversation,
            &event.sender,
            now,
        )
        .await?;
        if outcome == access::PairOutcome::Refused && is_pair_code(&code) {
            say(state, &key, Text::PairInvalid, now).await?;
        }
        return Ok(Processed::Accepted);
    }

    // Step 3: access.
    if let access::Verdict::Drop(_) =
        access::admit(state, channel, &event.conversation, &event.sender).await?
    {
        return Ok(Processed::Dropped);
    }

    match &event.body {
        EventBody::Action { action } => on_action(state, &key, action, now).await?,
        EventBody::Message { message } => on_message(state, &key, event, message, now).await?,
    }
    Ok(Processed::Accepted)
}

/// Queues a plain text reply in the conversation and wakes the channel.
async fn say(
    state: &AppState,
    key: &ConversationKey,
    text: Text,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let action = NewAction::Send {
        reply_to: None,
        parts: vec![OutPart::Text {
            text: strings::render(&text),
        }],
        buttons: None,
        silent: false,
    };
    enqueue_and_wake(state, key, action, now).await
}

async fn enqueue_and_wake(
    state: &AppState,
    key: &ConversationKey,
    action: NewAction,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let mut conn = state.pool.acquire().await?;
    outbox::enqueue(&mut conn, key, action, Class::Command, None, now).await?;
    drop(conn);
    outbox::wake(state, key.channel);
    Ok(())
}

/// Why a button press could not be decided. I1 replaces `decide_button` with
/// `proposals::approve/reject`; whatever it can fail with (`Conflict`, `Invalid`, `BadRequest`,
/// `BadGateway`, `Internal`) lands in the one generic `Err(_)` arm of `on_action` (R12).
enum ButtonError {
    Unavailable,
}

/// A1: the proposal service functions do not exist yet, so a fresh button is never decided.
async fn decide_button(
    _state: &AppState,
    _action: TokenAction,
    _proposal_id: i64,
) -> Result<Text, ButtonError> {
    Err(ButtonError::Unavailable)
}

/// Step 4: a pressed button. The answer goes to the callback itself (`answer_action`).
async fn on_action(
    state: &AppState,
    key: &ConversationKey,
    action: &InAction,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let text = match tokens::redeem(&state.pool, key, &action.token, now).await? {
        Redeemed::Stale => Text::AlreadyHandled,
        Redeemed::Fresh {
            action: kind,
            proposal_id,
        } => match decide_button(state, kind, proposal_id).await {
            Ok(text) => text,
            Err(_) => Text::NotSent {
                refusal: "approving from a button is not available yet".to_string(),
            },
        },
    };
    let answer = NewAction::AnswerAction {
        action_ref: action.reference.clone(),
        text: strings::render(&text),
    };
    enqueue_and_wake(state, key, answer, now).await
}

/// Step 5: a message from an admitted owner becomes a command, a capture answer or a turn.
async fn on_message(
    state: &AppState,
    key: &ConversationKey,
    event: &InboundEvent,
    message: &InMessage,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let channel = key.channel;
    let input = media::to_turn_input(state, channel, &message.parts).await;
    for label in &input.refused {
        say(
            state,
            key,
            Text::Unsupported {
                label: label.clone(),
            },
            now,
        )
        .await?;
    }
    let inbound = Inbound {
        channel,
        key: key.clone(),
        conversation: &event.conversation,
        sender: &event.sender,
        message_ref: &message.reference,
    };
    // A1 placeholder: always `None`, so `answer_capture` (A2) is not reached.
    if let Some(job) = commands::capture_target(state, &inbound, message).await? {
        return commands::answer_capture(state, &inbound, job, &input).await;
    }
    if input.text.is_empty() && input.images.is_empty() {
        return Ok(());
    }

    // Only typed text can be a command; a transcript never is (`route_transcript` says it back).
    let intent = if input.transcribed {
        commands::route_transcript(&input.text)
    } else if input.non_text {
        Intent::Chat(input.text.clone())
    } else {
        commands::route(&input.text)
    };
    match intent {
        Intent::Chat(_) => {}
        // Handled before `admit` for typed text; a transcript never yields it.
        Intent::Pair(_) => return Ok(()),
        // A2: until then the placeholder router yields nothing else.
        other => return commands::execute(state, &inbound, other).await,
    }

    let chat_id = key.chat_id();
    replies::clear_queue_pause(&state.pool, &chat_id).await?;
    match crate::assistant::send_or_queue(
        state,
        &chat_id,
        &input.text,
        &input.images,
        Origin::Channel(channel),
    )
    .await
    {
        Ok(Sent::Turn(_)) => Ok(()),
        Ok(Sent::Queued) => {
            let position = crate::chats::queued(&state.pool, &chat_id).await?.len();
            say(state, key, Text::Queued { position }, now).await
        }
        Err(refusal) => say(state, key, Text::NotSent { refusal }, now).await,
    }
}

/// TTL of `channel_events` (7 days). Called by `run_housekeeping`.
pub async fn prune(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let cutoff = stamp(now - EVENT_TTL);
    let done = sqlx::query("DELETE FROM channel_events WHERE received_at < ?")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::channels::contract::{Conversation, ConversationKind, Sender};
    use crate::channels::testkit::state;

    const NOW: &str = "2026-10-20T10:00:00Z";

    fn now() -> DateTime<Utc> {
        NOW.parse().unwrap()
    }

    fn event(id: &str, sender: &str, conversation: &str, parts: Vec<InPart>) -> InboundEvent {
        InboundEvent {
            event_id: id.to_string(),
            at: None,
            conversation: Conversation {
                reference: conversation.to_string(),
                kind: ConversationKind::Private,
                parent: None,
            },
            sender: Sender {
                reference: sender.to_string(),
                name: None,
            },
            body: EventBody::Message {
                message: InMessage {
                    reference: "m1".to_string(),
                    reply_to: None,
                    parts,
                },
            },
        }
    }

    fn text_event(id: &str, sender: &str, conversation: &str, text: &str) -> InboundEvent {
        event(
            id,
            sender,
            conversation,
            vec![InPart::Text {
                text: text.to_string(),
            }],
        )
    }

    async fn owner(pool: &SqlitePool, sender: &str, conversation: &str) {
        sqlx::query(
            "INSERT INTO channel_identities \
             (channel, sender_ref, conversation_ref, conversation_kind, home, created_at) \
             VALUES ('telegram', ?, ?, 'private', 1, ?)",
        )
        .bind(sender)
        .bind(conversation)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn outcome_of(pool: &SqlitePool, event_id: &str) -> String {
        sqlx::query_scalar("SELECT outcome FROM channel_events WHERE event_id = ?")
            .bind(event_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn outbox_rows(pool: &SqlitePool) -> Vec<(String, serde_json::Value)> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT action, payload FROM channel_outbox ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(action, payload)| (action, serde_json::from_str(&payload).unwrap()))
            .collect()
    }

    /// The text of a `send` row.
    fn sent_text(payload: &serde_json::Value) -> String {
        payload["parts"][0]["text"].as_str().unwrap().to_string()
    }

    async fn run_count(pool: &SqlitePool, chat_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn state_with_root() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(dir.path()).unwrap();
        let mut st = state().await;
        st.files_root = Some(root);
        (st, dir)
    }

    #[tokio::test]
    async fn a_new_event_is_accepted_and_recorded() {
        let st = state().await;
        owner(&st.pool, "u1", "accepted-chat").await;
        let reply = ingest_event(
            &st,
            "telegram",
            text_event("ev-new", "u1", "accepted-chat", "hello"),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        assert_eq!(outcome_of(&st.pool, "ev-new").await, "accepted");
    }

    #[tokio::test]
    async fn the_same_event_id_is_a_duplicate_with_no_second_effect() {
        let st = state().await;
        owner(&st.pool, "u1", "dup-chat").await;
        let first = ingest_event(
            &st,
            "telegram",
            text_event("ev-dup", "u1", "dup-chat", "hello"),
            now(),
        )
        .await;
        let second = ingest_event(
            &st,
            "telegram",
            text_event("ev-dup", "u1", "dup-chat", "hello"),
            now(),
        )
        .await;
        assert_eq!(first, IngestReply::Accepted);
        assert_eq!(second, IngestReply::Duplicate);
        assert_eq!(run_count(&st.pool, "telegram:dup-chat").await, 1);
        assert!(
            crate::chats::queued(&st.pool, "telegram:dup-chat")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_processing_event_older_than_a_minute_is_reprocessed() {
        let st = state().await;
        owner(&st.pool, "u1", "stale-chat").await;
        for (id, age) in [("ev-old", 61), ("ev-young", 10)] {
            sqlx::query(
                "INSERT INTO channel_events (channel, event_id, received_at, outcome) \
                 VALUES ('telegram', ?, ?, 'processing')",
            )
            .bind(id)
            .bind(stamp(now() - Duration::seconds(age)))
            .execute(&st.pool)
            .await
            .unwrap();
        }
        let old = ingest_event(
            &st,
            "telegram",
            text_event("ev-old", "u1", "stale-chat", "hello"),
            now(),
        )
        .await;
        let young = ingest_event(
            &st,
            "telegram",
            text_event("ev-young", "u1", "stale-chat", "hello"),
            now(),
        )
        .await;
        assert_eq!(old, IngestReply::Accepted);
        assert_eq!(outcome_of(&st.pool, "ev-old").await, "accepted");
        assert_eq!(young, IngestReply::Duplicate);
        assert_eq!(outcome_of(&st.pool, "ev-young").await, "processing");
    }

    #[tokio::test]
    async fn an_unknown_sender_is_dropped_and_recorded_as_dropped() {
        let st = state().await;
        let reply = ingest_event(
            &st,
            "telegram",
            text_event("ev-stranger", "stranger", "drop-chat", "hello"),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Dropped);
        assert_eq!(outcome_of(&st.pool, "ev-stranger").await, "dropped");
        assert_eq!(run_count(&st.pool, "telegram:drop-chat").await, 0);
        assert!(outbox_rows(&st.pool).await.is_empty());
    }

    #[tokio::test]
    async fn pair_from_an_unknown_sender_with_a_valid_code_pairs_and_confirms() {
        let st = state().await;
        let minted = access::mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let reply = ingest_event(
            &st,
            "telegram",
            text_event("ev-pair", "newbie", "pair-chat", &minted.command),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        assert_eq!(outcome_of(&st.pool, "ev-pair").await, "accepted");
        let paired: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM channel_identities WHERE sender_ref = 'newbie' \
             AND conversation_ref = 'pair-chat'",
        )
        .fetch_one(&st.pool)
        .await
        .unwrap();
        assert_eq!(paired, 1);
        let rows = outbox_rows(&st.pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(sent_text(&rows[0].1), strings::render(&Text::Paired));
    }

    #[tokio::test]
    async fn pair_with_a_wrong_six_digit_code_answers_invalid_and_counts() {
        let st = state().await;
        let minted = access::mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let wrong = if minted.code == "000000" {
            "000001"
        } else {
            "000000"
        };
        let reply = ingest_event(
            &st,
            "telegram",
            text_event(
                "ev-wrong",
                "newbie",
                "wrong-chat",
                &format!("/pair {wrong}"),
            ),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        assert_eq!(outcome_of(&st.pool, "ev-wrong").await, "accepted");
        let rows = outbox_rows(&st.pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(sent_text(&rows[0].1), strings::render(&Text::PairInvalid));
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM channel_pairing_codes")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(attempts, 1);
        let identities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_identities")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(identities, 0);
    }

    #[tokio::test]
    async fn pair_with_garbage_is_silent() {
        let st = state().await;
        access::mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let reply = ingest_event(
            &st,
            "telegram",
            text_event("ev-garbage", "newbie", "garbage-chat", "/pair not-a-code"),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        assert_eq!(outcome_of(&st.pool, "ev-garbage").await, "accepted");
        assert!(outbox_rows(&st.pool).await.is_empty());
    }

    #[tokio::test]
    async fn an_admitted_text_becomes_a_turn_with_origin_channel() {
        let st = state().await;
        owner(&st.pool, "u1", "turn-chat").await;
        let reply = ingest_event(
            &st,
            "telegram",
            text_event("ev-turn", "u1", "turn-chat", "what is next?"),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        let runs: Vec<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT origin, chat_id FROM runs WHERE chat_id = 'telegram:turn-chat'")
                .fetch_all(&st.pool)
                .await
                .unwrap();
        assert_eq!(
            runs,
            vec![(
                Some("channel:telegram".to_string()),
                Some("telegram:turn-chat".to_string())
            )]
        );
    }

    #[tokio::test]
    async fn a_busy_conversation_queues_and_says_the_position() {
        let mut st = state().await;
        // A slow runner keeps the first turn running while the second message arrives.
        st.runner = Arc::new(crate::execution::runner::FakeCommandRunner {
            delay: std::sync::Mutex::new(Some(std::time::Duration::from_secs(30))),
            ..Default::default()
        });
        owner(&st.pool, "u1", "busy-chat").await;
        crate::assistant::send_message(
            &st,
            "telegram:busy-chat",
            "first",
            Origin::Channel("telegram"),
        )
        .await
        .unwrap();
        let reply = ingest_event(
            &st,
            "telegram",
            text_event("ev-busy", "u1", "busy-chat", "second"),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        let waiting = crate::chats::queued(&st.pool, "telegram:busy-chat")
            .await
            .unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].text, "second");
        let rows = outbox_rows(&st.pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            sent_text(&rows[0].1),
            strings::render(&Text::Queued { position: 1 })
        );
    }

    #[tokio::test]
    async fn a_stale_button_token_answers_already_handled() {
        let st = state().await;
        owner(&st.pool, "u1", "button-chat").await;
        let mut stale = text_event("ev-button", "u1", "button-chat", "unused");
        stale.body = EventBody::Action {
            action: InAction {
                reference: "551".to_string(),
                message_ref: "990".to_string(),
                token: "tk_unknown".to_string(),
            },
        };
        let reply = ingest_event(&st, "telegram", stale, now()).await;
        assert_eq!(reply, IngestReply::Accepted);
        let rows = outbox_rows(&st.pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "answer_action");
        assert_eq!(rows[0].1["action_ref"], "551");
        assert_eq!(
            rows[0].1["text"],
            strings::render(&Text::AlreadyHandled).as_str()
        );
        assert_eq!(run_count(&st.pool, "telegram:button-chat").await, 0);
    }

    #[tokio::test]
    async fn a_voice_message_never_runs_a_command() {
        let (mut st, _dir) = state_with_root().await;
        let minted = access::mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        // The owner "says" the pairing command; heard text is never routed as one.
        st.voice = Arc::new(crate::integrations::voice::VoiceRuntime {
            transcriber: Some(Arc::new(crate::transcribe::FakeTranscriber::returning(
                &minted.command,
            ))),
            ..Default::default()
        });
        owner(&st.pool, "u1", "voice-chat").await;
        let audio = media::store(&st, "telegram", "audio/ogg", b"ogg", now())
            .await
            .unwrap();
        let reply = ingest_event(
            &st,
            "telegram",
            event(
                "ev-voice",
                "u1",
                "voice-chat",
                vec![InPart::Audio {
                    media: audio,
                    mime: "audio/ogg".to_string(),
                    duration_ms: Some(1000),
                }],
            ),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        let paired: Option<String> =
            sqlx::query_scalar("SELECT paired_at FROM channel_pairing_codes")
                .fetch_one(&st.pool)
                .await
                .unwrap();
        assert_eq!(paired, None, "the heard /pair must not run");
        // It went to the turn as plain words instead.
        assert_eq!(run_count(&st.pool, "telegram:voice-chat").await, 1);
    }

    #[tokio::test]
    async fn a_refused_part_gets_the_unsupported_reply() {
        let st = state().await;
        owner(&st.pool, "u1", "sticker-chat").await;
        let reply = ingest_event(
            &st,
            "telegram",
            event(
                "ev-sticker",
                "u1",
                "sticker-chat",
                vec![InPart::Unsupported {
                    label: "sticker".to_string(),
                }],
            ),
            now(),
        )
        .await;
        assert_eq!(reply, IngestReply::Accepted);
        let rows = outbox_rows(&st.pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            sent_text(&rows[0].1),
            strings::render(&Text::Unsupported {
                label: "sticker".to_string()
            })
        );
        assert_eq!(run_count(&st.pool, "telegram:sticker-chat").await, 0);
    }

    #[tokio::test]
    async fn prune_removes_events_older_than_seven_days() {
        let st = state().await;
        for (id, age_days) in [("ev-ancient", 8), ("ev-recent", 6)] {
            sqlx::query(
                "INSERT INTO channel_events (channel, event_id, received_at, outcome) \
                 VALUES ('telegram', ?, ?, 'accepted')",
            )
            .bind(id)
            .bind(stamp(now() - Duration::days(age_days)))
            .execute(&st.pool)
            .await
            .unwrap();
        }
        assert_eq!(prune(&st.pool, now()).await.unwrap(), 1);
        let left: Vec<String> =
            sqlx::query_scalar("SELECT event_id FROM channel_events ORDER BY event_id")
                .fetch_all(&st.pool)
                .await
                .unwrap();
        assert_eq!(left, vec!["ev-recent".to_string()]);
    }
}
