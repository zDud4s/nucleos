//! `/compact [focus]` from the command route (spec §7): the answer and refusals of a compaction
//! turn. The turn itself, its `chat_compactions` row and what a settled one does are
//! `assistant::compaction`; this file only maps its outcome onto the route's wire shape.
//!
//! Not queued, unlike a command that expands into a prompt: a compaction asked for while a turn
//! runs would summarise a conversation that is still moving, so busy is a 409.

use super::command::{Refusal, internal, send_refusal, slash_refusal};
use crate::assistant::compaction::{self, StartError};
use crate::assistant::slash::SlashError;
use crate::assistant::slash::parse::Parsed;
use crate::state::AppState;
use axum::Json;
use serde_json::{Value, json};

/// Starts the compaction turn of `chat_id`. The caller has already answered 404 for a chat that
/// does not exist: `compaction::start` does not check it. `typed` is the line as the composer held
/// it, which is what the transcript shows.
pub(super) async fn run(
    state: &AppState,
    chat_id: &str,
    parsed: &Parsed,
    typed: &str,
) -> Result<Json<Value>, Refusal> {
    let focus = Some(parsed.rest.trim()).filter(|focus| !focus.is_empty());
    match compaction::start(state, chat_id, typed, focus).await {
        Ok(turn_id) => Ok(Json(json!({ "kind": "prompt", "turn_id": turn_id }))),
        Err(StartError::Busy) => Err(slash_refusal(SlashError::Busy)),
        Err(StartError::NothingToCompact) => Err(slash_refusal(SlashError::NothingToCompact)),
        Err(StartError::Send(msg)) => Err(send_refusal(msg)),
        Err(StartError::Store(error)) => {
            tracing::warn!(%error, chat_id, "starting a compaction failed");
            Err(internal())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::execution::runner::{FakeCommandRunner, RunOutcome};
    use crate::http::build_router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    const PROMPT_MARK: &str = "You are compacting THIS conversation into a handoff document.";

    async fn request(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn command(state: &AppState, chat: &str, text: &str) -> (StatusCode, Value) {
        request(
            state,
            "POST",
            &format!("/assistant/chats/{chat}/command"),
            Some(json!({ "text": text })),
        )
        .await
    }

    async fn seed_chat(state: &AppState, id: &str) {
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', '2026-10-28T10:00:00Z')",
        )
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// A finished exchange of `chat`; returns its run id.
    async fn seed_exchange(state: &AppState, chat: &str, asked: &str, answered: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, stdout, created_at)
             VALUES (?, 'completed', 'assistant', ?, ?, '2026-10-28T10:00:00+00:00')
             RETURNING id",
        )
        .bind(asked)
        .bind(chat)
        .bind(answered)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    async fn seed_session(state: &AppState, chat: &str) {
        sqlx::query(
            "INSERT INTO assistant_sessions (chat_id, session_id, updated_at)
             VALUES (?, 's-old', '2026-10-28T10:00:00Z')",
        )
        .bind(chat)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn has_session(state: &AppState, chat: &str) -> bool {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assistant_sessions WHERE chat_id = ?)")
            .bind(chat)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    fn outcome(exit_code: i32, text: &str) -> RunOutcome {
        RunOutcome {
            exit_code,
            stdout: format!(r#"{{"type":"result","subtype":"success","result":"{text}"}}"#),
            stderr: String::new(),
            session_id: Some("s-compact".into()),
            cost_usd: Some(0.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
            model_requested: false,
        }
    }

    /// A state whose CLI runner answers with `canned`, and a cloud chat `chat` holding one answered
    /// exchange and a live session.
    async fn compactable(chat: &str, canned: RunOutcome) -> (AppState, Arc<FakeCommandRunner>) {
        let fake = Arc::new(FakeCommandRunner::default());
        *fake.canned.lock().unwrap() = Some(canned);
        let mut state = crate::http::tests::test_state().await;
        state.runner = fake.clone();
        seed_chat(&state, chat).await;
        seed_exchange(&state, chat, "old question", "OLD-ANSWER").await;
        seed_session(&state, chat).await;
        (state, fake)
    }

    /// The compaction row's status once it left `pending` and the turn's slot is free.
    async fn settled(state: &AppState, chat: &str, run_id: i64) -> String {
        let mut status = String::new();
        for _ in 0..500 {
            status = sqlx::query_scalar("SELECT status FROM chat_compactions WHERE run_id = ?")
                .bind(run_id)
                .fetch_optional(&state.pool)
                .await
                .unwrap()
                .unwrap_or_default();
            if status != "pending" && !status.is_empty() && !crate::assistant::is_busy(chat) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the compaction of run {run_id} never settled: {status:?}");
    }

    /// Starts `/compact` (with `text`) and waits for it to settle; returns the turn id.
    async fn compact(state: &AppState, chat: &str, text: &str) -> (i64, String) {
        let (status, body) = command(state, chat, text).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["kind"], "prompt", "{body}");
        let id = body["turn_id"].as_i64().expect("a turn id");
        (id, settled(state, chat, id).await)
    }

    async fn typed_of(state: &AppState, run_id: i64) -> String {
        sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn notices(state: &AppState, chat: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT body FROM chat_notices WHERE chat_id = ? AND kind = 'compaction' ORDER BY id",
        )
        .bind(chat)
        .fetch_all(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn compact_end_to_end_with_a_fake_engine() {
        let chat = "c-compact-e2e";
        let (state, fake) = compactable(chat, outcome(0, "HANDOFF-XYZ")).await;

        let (id, status) = compact(&state, chat, "/compact").await;

        assert_eq!(status, "done");
        assert_eq!(typed_of(&state, id).await, "/compact");
        let told = fake.last_prompt.lock().unwrap().clone().unwrap_or_default();
        assert!(told.contains(PROMPT_MARK), "{told}");
        let (summary, chars): (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT summary, summary_chars FROM chat_compactions WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(summary.as_deref(), Some("HANDOFF-XYZ"));
        assert_eq!(chars, Some(11));
        assert!(!has_session(&state, chat).await, "the session is forgotten");
        assert_eq!(
            notices(&state, chat).await,
            vec!["Compacted: the next turn starts from the handoff (~3 tokens).".to_string()]
        );

        *fake.canned.lock().unwrap() = Some(outcome(0, "fine"));
        crate::assistant::send_message(&state, chat, "what now?", crate::assistant::Origin::Shell)
            .await
            .unwrap();
        let mut next = String::new();
        for _ in 0..500 {
            next = fake.last_prompt.lock().unwrap().clone().unwrap_or_default();
            if next.contains("what now?") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(next.contains("HANDOFF-XYZ"), "{next}");
        assert!(!next.contains("OLD-ANSWER"), "{next}");
        assert!(!next.contains(PROMPT_MARK), "{next}");
    }

    #[tokio::test]
    async fn compact_with_focus_appends_it_and_keeps_typed() {
        let chat = "c-compact-focus";
        let (state, fake) = compactable(chat, outcome(0, "HANDOFF")).await;

        let (id, status) = compact(&state, chat, "  /compact keep the plan  ").await;

        assert_eq!(status, "done");
        assert_eq!(typed_of(&state, id).await, "/compact keep the plan");
        let told = fake.last_prompt.lock().unwrap().clone().unwrap_or_default();
        assert!(told.contains(PROMPT_MARK), "{told}");
        assert!(
            told.contains("Additional focus from the user: keep the plan"),
            "{told}"
        );
        let focus: Option<String> =
            sqlx::query_scalar("SELECT focus FROM chat_compactions WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(focus.as_deref(), Some("keep the plan"));
    }

    #[tokio::test]
    async fn compact_busy_409() {
        let chat = "c-compact-busy-409";
        let (state, _fake) = compactable(chat, outcome(0, "HANDOFF")).await;
        let _busy = crate::assistant::take_the_slot_for_testing(chat);

        let (status, body) = command(&state, chat, "/compact").await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "busy", "{body}");
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chat_compactions WHERE chat_id = ?")
                .bind(chat)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn compact_nothing_to_compact_400() {
        let state = crate::http::tests::test_state().await;
        let chat = "c-compact-nothing-400";
        seed_chat(&state, chat).await;

        // No turn at all.
        let (status, body) = command(&state, chat, "/compact").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "nothing_to_compact", "{body}");
        assert_eq!(
            body["detail"], "nothing to compact since the last /clear",
            "{body}"
        );

        // Only turns from before a `/clear`.
        seed_exchange(&state, chat, "q", "a").await;
        let (status, body) = command(&state, chat, "/clear").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = command(&state, chat, "/compact").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "nothing_to_compact", "{body}");
    }

    #[tokio::test]
    async fn compact_unknown_chat_404() {
        let state = crate::http::tests::test_state().await;

        let (status, body) = command(&state, "c-compact-no-such-chat", "/compact").await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "chat_not_found", "{body}");
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "no turn starts for a chat that does not exist");
    }

    #[tokio::test]
    async fn compact_failed_turn_keeps_the_session() {
        let chat = "c-compact-failed";
        // No `result` event in the stream: the CLI stopped without answering.
        let failed = RunOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: "the engine broke".into(),
            session_id: None,
            ..outcome(1, "")
        };
        let (state, _fake) = compactable(chat, failed).await;

        let (_, status) = compact(&state, chat, "/compact").await;

        assert_eq!(status, "failed");
        assert!(has_session(&state, chat).await, "the session stays");
        assert_eq!(
            notices(&state, chat).await,
            vec![crate::assistant::compaction::FAILED_NOTICE.to_string()]
        );
    }

    #[tokio::test]
    async fn transcript_marks_the_compaction_turn() {
        let chat = "c-compact-transcript";
        let (state, _fake) = compactable(chat, outcome(0, "HANDOFF")).await;
        let (id, _) = compact(&state, chat, "/compact").await;

        let (status, body) =
            request(&state, "GET", &format!("/assistant/chats/{chat}"), None).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let turns = body["turns"].as_array().expect("turns");
        assert_eq!(turns.len(), 2, "{body}");
        for turn in turns {
            let is_it = turn["id"].as_i64() == Some(id);
            assert_eq!(turn["compaction"], json!(is_it), "{turn}");
        }
    }

    #[tokio::test]
    async fn context_card_shows_the_compaction_in_force_and_null_after_clear() {
        let chat = "c-compact-context";
        let (state, _fake) = compactable(chat, outcome(0, "HANDOFF")).await;

        let (status, body) = command(&state, chat, "/context").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["card"]["data"]["compaction"], Value::Null, "{body}");

        let (id, _) = compact(&state, chat, "/compact").await;
        let (status, body) = command(&state, chat, "/context").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let shown = &body["card"]["data"]["compaction"];
        assert_eq!(shown["run_id"], id, "{body}");
        assert!(
            shown["at"].as_str().is_some_and(|at| !at.is_empty()),
            "{body}"
        );

        let (status, body) = command(&state, chat, "/clear").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = command(&state, chat, "/context").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["card"]["data"]["compaction"], Value::Null, "{body}");
    }
}
