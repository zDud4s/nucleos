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
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("type").and_then(|t| t.as_str()) == Some("result") {
                if let Some(text) = v.get("result").and_then(|r| r.as_str()) {
                    if !text.trim().is_empty() {
                        reply = Some(text.to_string());
                    }
                }
            }
        }
    }
    reply
}

static BUSY_CHATS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn try_begin_turn(chat_id: &str) -> bool {
    BUSY_CHATS.lock().unwrap().insert(chat_id.to_string())
}

fn end_turn(chat_id: &str) {
    BUSY_CHATS.lock().unwrap().remove(chat_id);
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
    if !try_begin_turn(chat_id) {
        return Err("a turn is already in progress for this chat".into());
    }

    let result = async {
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

        spawn_assistant_turn(
            state,
            id,
            chat_id.to_string(),
            text.to_string(),
            resume,
            mcp_path,
        );
        Ok(id)
    }
    .await;

    if result.is_err() {
        end_turn(chat_id);
    }
    result
}

fn spawn_assistant_turn(
    state: &crate::state::AppState,
    id: i64,
    chat_id: String,
    text: String,
    resume: Option<String>,
    mcp_path: std::path::PathBuf,
) {
    let pool = state.pool.clone();
    let runner = state.runner.clone();
    let run_timeout = state.run_timeout;
    let env = crate::runs::run_env(state, id);

    crate::runs::spawn_registered(state, id, async move {
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        {
            let pool = pool.clone();
            let chat_id = chat_id.clone();
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
                Some(mcp_path.as_path()),
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
                    let _ = upsert_session(&pool, &chat_id, session_id, &completed_at).await;
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

        end_turn(&chat_id);
        let _ = std::fs::remove_file(&mcp_path);
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

        assert!(try_begin_turn(chat_id));
        assert!(!try_begin_turn(chat_id));
        end_turn(chat_id);
        assert!(try_begin_turn(chat_id));
        end_turn(chat_id);
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
}
