use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// Extract the assistant's final text from `claude -p --output-format stream-json` output.
/// Each line is a JSON object; the final `{"type":"result", "result":"<text>", ...}` event holds
/// the reply. Returns the last non-empty `result` string, or None if there is no such event
/// (caller falls back to the raw stream so nothing is silently lost).
fn extract_reply(stdout: &str) -> Option<String> {
    let mut reply: Option<String> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
            && v.get("type").and_then(|t| t.as_str()) == Some("result")
            && let Some(text) = v.get("result").and_then(|r| r.as_str())
            && !text.trim().is_empty()
        {
            reply = Some(text.to_string());
        }
    }
    reply
}

static BUSY_CHATS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Owns a chat's one turn slot for as long as it is held, releasing it on every way out.
///
/// A guard rather than a matching pair of calls, because there is no point in the turn's life where
/// a trailing statement is reliable. Cancelling a run calls `abort()`
/// (`runs::finalize_termination`), which drops the task's future mid-await; abandoning the HTTP
/// request that started the turn drops that future the same way. Anything written after an `.await`
/// then never runs, and a chat left behind in `BUSY_CHATS` rejects every later message with 409
/// until the daemon restarts — "the bot stopped answering" points nowhere near either cause.
struct ChatSlot {
    chat_id: String,
}

impl ChatSlot {
    /// Claims the chat, or returns None if a turn is already in flight for it.
    fn acquire(chat_id: &str) -> Option<Self> {
        BUSY_CHATS
            .lock()
            .unwrap()
            .insert(chat_id.to_string())
            .then(|| Self {
                chat_id: chat_id.to_string(),
            })
    }
}

impl Drop for ChatSlot {
    fn drop(&mut self) {
        BUSY_CHATS.lock().unwrap().remove(&self.chat_id);
    }
}

/// Owns a turn's chat slot and its temp MCP config for the length of the turn, releasing both when
/// it ends — by completing, by failing, or by being aborted.
struct TurnGuard {
    slot: ChatSlot,
    mcp_path: std::path::PathBuf,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        // `slot` releases the chat by being dropped with the rest of this struct.
        let _ = std::fs::remove_file(&self.mcp_path);
    }
}

pub async fn get_session(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    let session_id: Option<Option<String>> =
        sqlx::query_scalar("SELECT session_id FROM assistant_sessions WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(pool)
            .await?;

    Ok(session_id.flatten())
}

pub async fn upsert_session(
    pool: &SqlitePool,
    chat_id: &str,
    session_id: &str,
    updated_at: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO assistant_sessions (chat_id, session_id, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(chat_id) DO UPDATE SET
             session_id = excluded.session_id,
             updated_at = excluded.updated_at",
    )
    .bind(chat_id)
    .bind(session_id)
    .bind(updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

pub fn build_mcp_config(exe_path: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": ["--mcp-tools"]
            }
        }
    })
}

pub async fn send_message(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
) -> Result<i64, String> {
    // Held from here on: every early return, error, and dropped future below releases the chat by
    // dropping this, which is why none of them needs a cleanup statement of its own.
    let slot = ChatSlot::acquire(chat_id)
        .ok_or("a turn is already in progress for this chat".to_string())?;

    let resume = get_session(&state.pool, chat_id)
        .await
        .map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    let config = build_mcp_config(&exe);
    let mcp_path = std::env::temp_dir().join(format!("nucleos-mcp-{chat_id}.json"));
    let config_bytes = serde_json::to_vec(&config).map_err(|e| e.to_string())?;
    std::fs::write(&mcp_path, config_bytes).map_err(|e| e.to_string())?;

    let id = sqlx::query(
        "INSERT INTO runs (prompt, status, mode, created_at) VALUES (?, 'running', 'assistant', ?)",
    )
    .bind(text)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await
    .map_err(|e| e.to_string())?
    .last_insert_rowid();

    spawn_assistant_turn(state, id, slot, text.to_string(), resume, mcp_path);
    Ok(id)
}

fn spawn_assistant_turn(
    state: &crate::state::AppState,
    id: i64,
    slot: ChatSlot,
    text: String,
    resume: Option<String>,
    mcp_path: std::path::PathBuf,
) {
    let pool = state.pool.clone();
    let runner = state.runner.clone();
    let run_timeout = state.run_timeout;
    let env = crate::runs::run_env(state, id);
    // Built HERE, outside the task, and captured by the async block. A task aborted before its first
    // poll drops its captured state without ever running a line of the body, so a guard constructed
    // inside would simply never exist — and a `/cancel` racing a fresh message hits exactly that.
    let turn = TurnGuard { slot, mcp_path };

    crate::runs::spawn_registered(state, id, async move {
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        {
            let pool = pool.clone();
            let chat_id = turn.slot.chat_id.clone();
            tokio::spawn(async move {
                if let Some(session_id) = session_rx.recv().await {
                    let _ = sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
                        .bind(&session_id)
                        .bind(id)
                        .execute(&pool)
                        .await;
                    let _ = upsert_session(
                        &pool,
                        &chat_id,
                        &session_id,
                        &chrono::Utc::now().to_rfc3339(),
                    )
                    .await;
                }
            });
        }

        let result = tokio::time::timeout(
            run_timeout,
            runner.run_prompt(
                &text,
                &env,
                None,
                false,
                resume.as_deref(),
                Some(turn.mcp_path.as_path()),
                session_tx,
            ),
        )
        .await;
        let completed_at = chrono::Utc::now().to_rfc3339();

        match result {
            Ok(Ok(o)) => {
                let reply = extract_reply(&o.stdout).unwrap_or_else(|| o.stdout.clone());
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'completed', exit_code = ?, stdout = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, completed_at = ? WHERE id = ?",
                )
                .bind(o.exit_code)
                .bind(&reply)
                .bind(&o.stderr)
                .bind(&o.session_id)
                .bind(o.cost_usd)
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
                if let Some(session_id) = o.session_id.as_deref() {
                    let _ =
                        upsert_session(&pool, &turn.slot.chat_id, session_id, &completed_at).await;
                }
            }
            Ok(Err(e)) => {
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ? WHERE id = ?",
                )
                .bind(e.to_string())
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
            }
            Err(_) => {
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'timed_out', completed_at = ? WHERE id = ?",
                )
                .bind(&completed_at)
                .bind(id)
                .execute(&pool)
                .await;
            }
        }

        // No cleanup here on purpose: `turn` drops it, on every path including an abort.
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::FakeCommandRunner;
    use crate::state::AppState;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn extract_reply_pulls_the_result_text() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}
{"type":"result","subtype":"success","result":"Here are your projects: alpha, beta.","total_cost_usd":0.08}"#;

        assert_eq!(
            extract_reply(stdout),
            Some("Here are your projects: alpha, beta.".to_string())
        );
    }

    #[test]
    fn extract_reply_returns_none_without_result_event() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}"#;

        assert_eq!(extract_reply(stdout), None);
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn test_state() -> AppState {
        AppState {
            token: Token("t".into()),
            pool: test_pool().await,
            runner: Arc::new(FakeCommandRunner::default()),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    #[tokio::test]
    async fn unknown_chat_has_no_session() {
        let pool = test_pool().await;

        assert_eq!(get_session(&pool, "unknown").await.unwrap(), None);
    }

    #[tokio::test]
    async fn upsert_stores_session() {
        let pool = test_pool().await;

        upsert_session(&pool, "chat-1", "session-1", "2026-07-21T10:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, "chat-1").await.unwrap(),
            Some("session-1".to_string())
        );
    }

    #[tokio::test]
    async fn upsert_updates_existing_session() {
        let pool = test_pool().await;

        upsert_session(&pool, "chat-1", "session-1", "2026-07-21T10:00:00Z")
            .await
            .unwrap();
        upsert_session(&pool, "chat-1", "session-2", "2026-07-21T11:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, "chat-1").await.unwrap(),
            Some("session-2".to_string())
        );
    }

    #[test]
    fn builds_mcp_config() {
        let config = build_mcp_config("C:/x/nucleos-core.exe");

        assert_eq!(config["mcpServers"]["nucleos"]["type"], "stdio");
        assert_eq!(
            config["mcpServers"]["nucleos"]["command"],
            "C:/x/nucleos-core.exe"
        );
        assert_eq!(
            config["mcpServers"]["nucleos"]["args"],
            serde_json::json!(["--mcp-tools"])
        );
    }

    #[test]
    fn only_one_turn_can_be_in_flight_per_chat() {
        let chat_id = "busy-test-chat";

        let slot = ChatSlot::acquire(chat_id).expect("the chat starts free");
        assert!(ChatSlot::acquire(chat_id).is_none());
        drop(slot);
        assert!(ChatSlot::acquire(chat_id).is_some());
    }

    /// The chat is marked busy synchronously, but the work that follows — reading the session,
    /// writing the MCP config, inserting the run row — spans awaits, and the Telegram sidecar's HTTP
    /// client can give up inside that span. A cancelled request drops this future exactly the way
    /// `abort()` drops a turn's, so a chat slot released only by a trailing statement is never
    /// released at all. That is the same 409-forever symptom as a cancelled turn, reached through a
    /// different door: the bot simply stops answering until the daemon restarts.
    #[tokio::test]
    async fn a_dropped_message_request_frees_the_chat() {
        use std::future::Future;

        let state = test_state().await;
        let chat_id = "assistant-dropped-request-chat";

        let mut request = Box::pin(send_message(&state, chat_id, "hello"));

        // One poll is all it takes to claim the chat; the future then parks on the session lookup.
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            request.as_mut().poll(&mut context).is_pending(),
            "the request ran to completion before it could be dropped"
        );
        assert!(
            BUSY_CHATS.lock().unwrap().contains(chat_id),
            "the turn should have claimed the chat"
        );
        drop(request);

        assert!(
            !BUSY_CHATS.lock().unwrap().contains(chat_id),
            "an abandoned request must not leave the chat busy forever"
        );
    }

    #[tokio::test]
    async fn send_message_creates_assistant_run_and_upserts_session() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let chat_id = "assistant-send-test-chat";

        let id = send_message(&state, chat_id, "hello").await.unwrap();
        let mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, "assistant");

        let mut status = String::new();
        let mut session = None;
        for _ in 0..50 {
            status = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if status == "completed" {
                session = get_session(&pool, chat_id).await.unwrap();
                if session.is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(status, "completed");
        assert_eq!(session, Some("fake-session-id".to_string()));
    }

    #[tokio::test]
    async fn cancelling_a_turn_frees_the_chat_and_removes_its_mcp_config() {
        let mut state = test_state().await;
        // A slow runner keeps the turn parked on an await, which is where a real `/cancel` lands.
        let runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_secs(30))),
            ..Default::default()
        });
        state.runner = runner.clone();
        let chat_id = "assistant-cancel-test-chat";
        let mcp_path = std::env::temp_dir().join(format!("nucleos-mcp-{chat_id}.json"));

        let id = send_message(&state, chat_id, "take your time")
            .await
            .unwrap();
        // Wait until the CLI is actually under way; cancelling a turn still queued would exercise a
        // different (and easier) path than the one a user hits.
        for _ in 0..50 {
            if *runner.calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            *runner.calls.lock().unwrap(),
            1,
            "the turn should have started"
        );

        assert!(
            crate::runs::finalize_termination(&state, id, "cancelled").await,
            "the turn should still have been in flight"
        );

        // `finalize_termination` aborts the task, which drops its future mid-await. Cleanup that
        // lives in trailing statements never runs — and a chat left in BUSY_CHATS rejects every
        // later message with 409 until the daemon restarts, which is indistinguishable from the bot
        // having died.
        for _ in 0..50 {
            if !mcp_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !mcp_path.exists(),
            "the temp mcp config should not outlive a cancelled turn"
        );
        assert!(
            ChatSlot::acquire(chat_id).is_some(),
            "a cancelled turn must free the chat for the next message"
        );
    }
}
