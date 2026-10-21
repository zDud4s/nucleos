//! v1 Telegram tests ported to the ingest, replies, routing and media; each is named `port_<original_snake_name>`.
//!
//! Covered elsewhere (no port): IsTerminal (`open_pass_*` in replies), HandleCallbackApprove
//! (`button_fresh_approve_token_approves_and_answers`), BuildAttachmentPrompt (`media` turn_input
//! tests), the notifier seed/state/kill/budget/feed rows (routing `seed`, `announce`, `alerts`
//! tests and `run_routing_loop_seed_announces_nothing_old`), the retry rows (outbox `ack` tests).

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;

use crate::channels::contract::{
    Conversation, ConversationKey, ConversationKind, EventBody, InMessage, InPart, InboundEvent,
    Sender,
};
use crate::channels::strings::{self, Text};
use crate::channels::{ingest, media, replies, routing, testkit};
use crate::state::AppState;
use crate::transcribe::FakeTranscriber;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap()
}

/// `(conversation, text)` of every outbox row, oldest first.
async fn sent(pool: &SqlitePool) -> Vec<(String, String)> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT conversation, payload FROM channel_outbox ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    rows.into_iter()
        .map(|(conversation, payload)| {
            let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
            let text = payload["parts"][0]["text"].as_str().unwrap_or("");
            (conversation, text.to_owned())
        })
        .collect()
}

/// A state whose `telegram` channel is launched and has a routing row.
async fn live_state() -> AppState {
    let state = testkit::state().await;
    {
        let mut conn = state.pool.acquire().await.unwrap();
        routing::seed(&mut conn, "telegram").await.unwrap();
    }
    state.channels.presence.set_launched("telegram", now());
    state
}

async fn running_turn(state: &AppState, chat_id: &str) -> i64 {
    let run: i64 = sqlx::query_scalar(
        "INSERT INTO runs (prompt, status, mode, chat_id, created_at) \
         VALUES ('p', 'running', 'assistant', ?, '2026-10-20T00:00:00Z') RETURNING id",
    )
    .bind(chat_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    replies::turn_started(state, run, chat_id).await;
    run
}

async fn finish(state: &AppState, run: i64, stdout: &str) {
    sqlx::query("UPDATE runs SET status = 'completed', stdout = ? WHERE id = ?")
        .bind(stdout)
        .bind(run)
        .execute(&state.pool)
        .await
        .unwrap();
}

#[test]
fn port_run_reply() {
    let ended = |status: &str| {
        strings::render(&Text::TurnEnded {
            turn: 1,
            status: status.to_owned(),
        })
    };
    assert_eq!(replies::final_text(1, "completed", Some("hello")), "hello");
    // A blank output says the turn ended instead of sending nothing.
    assert_eq!(
        replies::final_text(1, "completed", Some("  ")),
        ended("completed")
    );
    assert_eq!(replies::final_text(1, "completed", None), ended("completed"));
    // A failed turn answers with the status sentence, never with its partial output.
    assert_eq!(
        replies::final_text(1, "failed", Some("boom")),
        ended("failed")
    );
}

#[tokio::test]
async fn port_a_long_turn_still_gets_its_reply() {
    let state = live_state().await;
    let run = running_turn(&state, "telegram:77").await;

    // A long turn gets exactly one still-working notice...
    assert!(replies::fire_ping(&state, run, now()).await.unwrap());
    assert!(!replies::fire_ping(&state, run, now()).await.unwrap());
    // ...and, when it finishes, its reply.
    finish(&state, run, "late answer").await;
    let pass = replies::open_pass(&state, now()).await.unwrap();
    assert_eq!(pass.report.replied, 1);

    let texts: Vec<String> = sent(&state.pool).await.into_iter().map(|r| r.1).collect();
    assert_eq!(
        texts,
        vec![
            strings::render(&Text::Working { turn: run }),
            strings::render(&Text::StillWorking),
            "late answer".to_owned(),
        ]
    );
}

#[test]
fn port_chat_key() {
    // The key of a topic, of a group's General and of a positive id round-trips byte for byte.
    for reference in ["-1001234567890:7", "-1001234567890", "12345:3"] {
        let key = ConversationKey::new("telegram", reference);
        assert_eq!(key.chat_id(), format!("telegram:{reference}"));
        let back = ConversationKey::parse(&key.chat_id()).unwrap();
        assert_eq!((back.channel, back.reference.as_str()), ("telegram", reference));
    }
}

#[test]
fn port_a_key_that_cannot_be_read_is_refused_rather_than_guessed_at() {
    // The reference after the channel is opaque to the core (the adapter owns its grammar), so
    // `telegram:-100:abc` is not refused here; what must never happen is a guess at a channel.
    for key in ["", "abc", "-100", ":7", " -100", "nope:-100:7", "telegram:"] {
        assert!(ConversationKey::parse(key).is_none(), "{key:?}");
    }
}

#[tokio::test]
async fn port_the_tracker_keeps_one_turn_per_topic() {
    // Each topic has its own turn, and a reply goes back to the topic it came from, never to the
    // group itself.
    let state = live_state().await;
    let seven = running_turn(&state, "telegram:-100123:7").await;
    let nine = running_turn(&state, "telegram:-100123:9").await;
    finish(&state, seven, "answer seven").await;
    finish(&state, nine, "answer nine").await;

    let pass = replies::open_pass(&state, now()).await.unwrap();
    assert_eq!(pass.report.replied, 2);

    let rows = sent(&state.pool).await;
    assert!(rows.contains(&("-100123:7".to_owned(), "answer seven".to_owned())));
    assert!(rows.contains(&("-100123:9".to_owned(), "answer nine".to_owned())));
    assert!(rows.iter().all(|(conversation, _)| conversation != "-100123"));
}

#[tokio::test]
async fn port_start_turn_error() {
    let st = testkit::state().await;
    // A conversation pinned to the hosted model, with none configured: the turn is refused.
    let chat = crate::chats::create(&st.pool, crate::chats::Brain::OpenRouter, None)
        .await
        .unwrap();
    sqlx::query("UPDATE chats SET chat_id = 'telegram:refused-chat' WHERE chat_id = ?")
        .bind(&chat)
        .execute(&st.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
         conversation_kind, home, created_at) VALUES ('telegram', 'u1', 'refused-chat', 'private', 1, 't')",
    )
    .execute(&st.pool)
    .await
    .unwrap();
    let event = InboundEvent {
        event_id: "ev-refused".to_owned(),
        at: None,
        conversation: Conversation {
            reference: "refused-chat".to_owned(),
            kind: ConversationKind::Private,
            parent: None,
        },
        sender: Sender {
            reference: "u1".to_owned(),
            name: None,
        },
        body: EventBody::Message {
            message: InMessage {
                reference: "m1".to_owned(),
                reply_to: None,
                parts: vec![InPart::Text {
                    text: "hello".to_owned(),
                }],
            },
        },
    };

    let _ = ingest::ingest_event(&st, "telegram", event, now()).await;

    let rows = sent(&st.pool).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].1,
        strings::render(&Text::NotSent {
            refusal: crate::assistant::NO_HOSTED_MODEL.to_owned()
        })
    );
}

async fn media_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = crate::files::ensure_root(dir.path()).unwrap();
    let mut state = testkit::state().await;
    state.files_root = Some(root);
    (state, dir)
}

/// Stores one audio part and runs it through `to_turn_input` with `transcriber` armed.
async fn hear(transcriber: &Arc<FakeTranscriber>) -> media::TurnInput {
    let (mut state, _dir) = media_state().await;
    state.voice = Arc::new(crate::integrations::voice::VoiceRuntime {
        transcriber: Some(transcriber.clone()),
        ..Default::default()
    });
    let id = media::store(&state, "fake", "audio/ogg", b"OggS fake opus", now())
        .await
        .unwrap();
    let parts = [InPart::Audio {
        media: id,
        mime: "audio/ogg".to_owned(),
        duration_ms: Some(4000),
    }];
    media::to_turn_input(&state, "fake", &parts).await
}

#[tokio::test]
async fn port_voice_goes_through_the_daemon_when_the_pillar_is_armed() {
    let transcriber = Arc::new(FakeTranscriber::returning("the nucleo owns the DB."));
    let input = hear(&transcriber).await;
    assert_eq!(input.text, "the nucleo owns the DB.");
    assert!(input.transcribed && input.refused.is_empty());
    assert_eq!(transcriber.calls(), 1);
}

#[tokio::test]
async fn port_a_failing_daemon_is_reported_rather_than_retried_locally() {
    let transcriber = Arc::new(FakeTranscriber::failing(
        std::io::ErrorKind::Other,
        "transcriber exited with 1",
    ));
    let input = hear(&transcriber).await;
    assert_eq!(input.refused, vec!["audio".to_owned()]);
    assert!(input.text.is_empty() && !input.transcribed);
    assert_eq!(transcriber.calls(), 1, "one chance, no second attempt");
}

#[tokio::test]
async fn port_silence_from_the_daemon_says_nothing_was_heard() {
    // v1 said "nothing was heard"; the core refuses the audio part like any unusable one.
    let transcriber = Arc::new(FakeTranscriber::returning("   "));
    let input = hear(&transcriber).await;
    assert_eq!(input.refused, vec!["audio".to_owned()]);
    assert!(input.text.is_empty() && !input.transcribed);
}
