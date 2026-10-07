//! Work a chat turn launched that can outlive it (spec 2026-10-04 §3 option C.2, §6 slice 5).
//!
//! Written at two points only: `record_turn` at a turn's terminal write, over its whole stdout, and
//! `apply_line` from the between-turns reader. A row only ever leaves `running`. This module owns
//! the `chat_tasks` SQL and nothing else — not who may act for a task (slice 6), not
//! pinning/reaping (`assistant.rs`), and never a price estimate.

use sqlx::SqlitePool;

/// The longest final text kept: the ceiling a tool answer already has (`runner::RESULT_LIMIT`).
const SUMMARY_LIMIT: usize = 2000;
/// The most rows one read returns; the newest are kept.
const READ_LIMIT: i64 = 500;

/// One task a chat turn launched.
#[derive(Debug, Clone, PartialEq, serde::Serialize, sqlx::FromRow)]
pub struct ChatTask {
    pub id: i64,
    pub chat_id: String,
    pub launched_by_run_id: i64,
    pub tool_use_id: String,
    pub task_id: Option<String>,
    /// `subagent`, `background_bash` or `background_agent`.
    pub kind: String,
    pub subagent_type: Option<String>,
    pub model: Option<String>,
    /// `running`, `completed`, `failed`, `stopped` or `orphaned`.
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub summary: Option<String>,
}

/// What one call of a turn says about a task it launched.
#[derive(Debug, PartialEq)]
struct Launch {
    tool_use_id: String,
    task_id: Option<String>,
    kind: &'static str,
    subagent_type: Option<String>,
    model: Option<String>,
    status: &'static str,
    started_at: Option<String>,
    finished_at: Option<String>,
    total_tokens: Option<i64>,
    summary: Option<String>,
}

fn cut(text: &str) -> String {
    text.chars().take(SUMMARY_LIMIT).collect()
}

/// PURE: the task a call launched, or `None` for a call that launches none (or carries no id).
fn launched(call: &crate::runner::ToolCall) -> Option<Launch> {
    let tool_use_id = call.id.clone()?;
    let spawns = matches!(call.name.as_str(), "Task" | "Agent");
    if !spawns && !call.background {
        return None;
    }
    let kind = match (spawns, call.background) {
        (true, true) => "background_agent",
        (true, false) => "subagent",
        _ => "background_bash",
    };
    let status = if call.background {
        match call.status.as_deref() {
            Some("completed") => "completed",
            Some("failed") => "failed",
            Some("killed") => "stopped",
            _ => "running",
        }
    } else if call.finished_at.is_some() {
        if call.result_failed { "failed" } else { "completed" }
    } else {
        // A foreground subagent whose turn ended before it answered ended with that turn.
        "stopped"
    };
    Some(Launch {
        tool_use_id,
        task_id: call.task_id.clone(),
        kind,
        subagent_type: call.subagent_type.clone(),
        model: call.model.clone(),
        status,
        started_at: call.started_at.clone(),
        finished_at: if status == "running" { None } else { call.finished_at.clone() },
        total_tokens: call.tokens.and_then(|t| i64::try_from(t).ok()),
        // A background call's answer only acknowledges the launch; its end event says the rest.
        summary: if call.background { None } else { call.result.as_deref().map(cut) },
    })
}

/// What one `task_*` stream line says about a task.
#[derive(Debug, PartialEq)]
struct TaskUpdate {
    task_id: String,
    tool_use_id: Option<String>,
    status: Option<&'static str>,
    finished_at: Option<String>,
    total_tokens: Option<i64>,
    summary: Option<String>,
}

/// PURE (but for "now" when an end carries no clock): a `system`/`task_*` line, else `None`.
fn task_update(line: &str) -> Option<TaskUpdate> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("system") {
        return None;
    }
    let subtype = value.get("subtype").and_then(serde_json::Value::as_str)?;
    if !subtype.starts_with("task_") {
        return None;
    }
    let task_id = value.get("task_id").and_then(serde_json::Value::as_str)?.to_owned();
    let word = match subtype {
        "task_updated" => value.pointer("/patch/status"),
        "task_notification" => value.get("status"),
        _ => None,
    }
    .and_then(serde_json::Value::as_str);
    let status = match word {
        Some("completed") => Some("completed"),
        Some("failed") => Some("failed"),
        Some("killed" | "stopped") => Some("stopped"),
        _ => None,
    };
    let finished_at = status.map(|_| {
        value
            .pointer("/patch/end_time")
            .and_then(serde_json::Value::as_i64)
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|end| end.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
    });
    Some(TaskUpdate {
        task_id,
        tool_use_id: value
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        status,
        finished_at,
        total_tokens: value
            .pointer("/usage/total_tokens")
            .and_then(serde_json::Value::as_i64),
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(cut),
    })
}

/// Records every task a turn launched, then every task event in its stream. Best effort: a
/// failed write is logged and never fails the turn.
pub async fn record_turn(pool: &SqlitePool, chat_id: &str, run_id: i64, stdout: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    let calls = crate::runner::live_from_stream(stdout).did;
    for launch in calls.iter().filter_map(launched) {
        // A row already closed keeps its end: the upsert only rewrites a `running` row.
        let written = sqlx::query(
            "INSERT INTO chat_tasks (chat_id, launched_by_run_id, tool_use_id, task_id, kind,
                                     subagent_type, model, status, started_at, finished_at,
                                     total_tokens, summary)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (chat_id, tool_use_id) DO UPDATE SET
                 task_id = COALESCE(excluded.task_id, chat_tasks.task_id),
                 status = excluded.status,
                 finished_at = excluded.finished_at,
                 total_tokens = COALESCE(excluded.total_tokens, chat_tasks.total_tokens),
                 summary = COALESCE(excluded.summary, chat_tasks.summary)
             WHERE chat_tasks.status = 'running'",
        )
        .bind(chat_id)
        .bind(run_id)
        .bind(&launch.tool_use_id)
        .bind(&launch.task_id)
        .bind(launch.kind)
        .bind(&launch.subagent_type)
        .bind(&launch.model)
        .bind(launch.status)
        .bind(launch.started_at.as_deref().unwrap_or(&now))
        .bind(&launch.finished_at)
        .bind(launch.total_tokens)
        .bind(&launch.summary)
        .execute(pool)
        .await;
        if let Err(error) = written {
            tracing::warn!(chat_id = %chat_id, %error, "could not record a task a chat turn launched");
        }
    }
    for line in stdout.lines() {
        apply_line(pool, chat_id, line).await;
    }
}

/// Applies one stream line's task event to its row. Status and end move only out of `running`;
/// tokens and summary still land on a row a `task_updated` closed just before its notification.
/// SET reads the pre-update row (SQLite), so the order of the assignments does not matter.
pub async fn apply_line(pool: &SqlitePool, chat_id: &str, line: &str) {
    let Some(update) = task_update(line) else {
        return;
    };
    let written = sqlx::query(
        "UPDATE chat_tasks
            SET task_id = COALESCE(task_id, ?),
                finished_at = CASE WHEN status = 'running' AND ? IS NOT NULL
                                   THEN COALESCE(finished_at, ?) ELSE finished_at END,
                status = CASE WHEN status = 'running' THEN COALESCE(?, status) ELSE status END,
                total_tokens = COALESCE(?, total_tokens),
                summary = COALESCE(?, summary)
          WHERE chat_id = ? AND status != 'orphaned' AND (task_id = ? OR tool_use_id = ?)",
    )
    .bind(&update.task_id)
    .bind(update.status)
    .bind(&update.finished_at)
    .bind(update.status)
    .bind(update.total_tokens)
    .bind(&update.summary)
    .bind(chat_id)
    .bind(&update.task_id)
    .bind(&update.tool_use_id)
    .execute(pool)
    .await;
    if let Err(error) = written {
        tracing::warn!(chat_id = %chat_id, %error, "could not record a chat task's event");
    }
}

/// Startup: every task still `running` was cut by the restart (spec §4.2 item 5).
pub async fn orphan_running(pool: &SqlitePool) -> sqlx::Result<u64> {
    let done = sqlx::query(
        "UPDATE chat_tasks SET status = 'orphaned', finished_at = COALESCE(finished_at, ?)
          WHERE status = 'running'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// A kept process ended: every task its turns launched and that is still `running` ended with it.
pub async fn stop_launched_by(
    pool: &SqlitePool,
    chat_id: &str,
    run_ids: &[i64],
) -> sqlx::Result<u64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut closed = 0;
    for run_id in run_ids {
        closed += sqlx::query(
            "UPDATE chat_tasks SET status = 'stopped', finished_at = COALESCE(finished_at, ?)
              WHERE chat_id = ? AND launched_by_run_id = ? AND status = 'running'",
        )
        .bind(&now)
        .bind(chat_id)
        .bind(run_id)
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(closed)
}

/// A chat's tasks, oldest first (at most `READ_LIMIT`, the newest).
pub async fn for_chat(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Vec<ChatTask>> {
    sqlx::query_as::<_, ChatTask>(
        "SELECT * FROM (
             SELECT id, chat_id, launched_by_run_id, tool_use_id, task_id, kind, subagent_type,
                    model, status, started_at, finished_at, total_tokens, cost_usd, summary
               FROM chat_tasks WHERE chat_id = ? ORDER BY id DESC LIMIT ?
         ) ORDER BY id",
    )
    .bind(chat_id)
    .bind(READ_LIMIT)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

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

    async fn row(pool: &SqlitePool, call: &str) -> ChatTask {
        for_chat(pool, "chat-1")
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.tool_use_id == call)
            .expect("the call has a row")
    }

    /// A finished foreground subagent, a background `Bash` and a background `Agent`.
    const LAUNCHES: &str = concat!(
        r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_task","name":"Task","input":{"description":"Find callers","prompt":"p","subagent_type":"Explore","model":"haiku"}}]}}"#,
        "\n",
        r#"{"type":"user","parent_tool_use_id":null,"timestamp":"2026-10-06T10:00:05.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_task","content":"the callers are in parser.rs"}]},"tool_use_result":{"status":"completed","totalTokens":4242}}"#,
        "\n",
        r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"sleep 20","run_in_background":true}}]}}"#,
        "\n",
        r#"{"type":"system","subtype":"task_started","task_id":"b1","tool_use_id":"toolu_bg","description":"sleep 20","task_type":"local_bash","is_backgrounded":true}"#,
        "\n",
        r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_agent","name":"Agent","input":{"description":"Review","prompt":"p","subagent_type":"reviewer","run_in_background":true}}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","result":"launched"}"#
    );

    const BG_UPDATED: &str = r#"{"type":"system","subtype":"task_updated","task_id":"b1","patch":{"status":"completed","end_time":1791021626000}}"#;
    const BG_NOTIFIED: &str = r#"{"type":"system","subtype":"task_notification","task_id":"b1","tool_use_id":"toolu_bg","status":"completed","summary":"(exit code 0)","usage":{"total_tokens":321}}"#;

    #[tokio::test]
    async fn a_launch_in_a_turn_is_a_running_row() {
        let pool = test_pool().await;

        record_turn(&pool, "chat-1", 7, LAUNCHES).await;

        assert_eq!(for_chat(&pool, "chat-1").await.unwrap().len(), 3);
        let task = row(&pool, "toolu_task").await;
        assert_eq!(task.kind, "subagent");
        assert_eq!(task.status, "completed");
        assert_eq!(task.total_tokens, Some(4242));
        assert_eq!(task.summary.as_deref(), Some("the callers are in parser.rs"));
        assert_eq!(task.subagent_type.as_deref(), Some("Explore"));
        assert_eq!(task.model.as_deref(), Some("haiku"));
        assert_eq!(task.launched_by_run_id, 7);
        assert_eq!(task.finished_at.as_deref(), Some("2026-10-06T10:00:05.000Z"));
        let bash = row(&pool, "toolu_bg").await;
        assert_eq!(bash.kind, "background_bash");
        assert_eq!(bash.status, "running");
        assert_eq!(bash.task_id.as_deref(), Some("b1"));
        assert_eq!(bash.finished_at, None);
        let agent = row(&pool, "toolu_agent").await;
        assert_eq!(agent.kind, "background_agent");
        assert_eq!(agent.status, "running");
        assert_eq!(agent.subagent_type.as_deref(), Some("reviewer"));
    }

    #[tokio::test]
    async fn a_notification_closes_the_row_with_its_status_tokens_and_summary() {
        let pool = test_pool().await;
        record_turn(&pool, "chat-1", 7, LAUNCHES).await;

        apply_line(&pool, "chat-1", BG_UPDATED).await;
        apply_line(&pool, "chat-1", BG_NOTIFIED).await;

        let bash = row(&pool, "toolu_bg").await;
        assert_eq!(bash.status, "completed");
        assert_eq!(bash.total_tokens, Some(321));
        assert_eq!(bash.summary.as_deref(), Some("(exit code 0)"));
        let end = chrono::DateTime::from_timestamp_millis(1791021626000)
            .unwrap()
            .to_rfc3339();
        assert_eq!(bash.finished_at.as_deref(), Some(end.as_str()));

        apply_line(
            &pool,
            "chat-1",
            r#"{"type":"system","subtype":"task_notification","task_id":"a9","tool_use_id":"toolu_agent","status":"stopped"}"#,
        )
        .await;
        let agent = row(&pool, "toolu_agent").await;
        assert_eq!(agent.status, "stopped");
        assert_eq!(agent.task_id.as_deref(), Some("a9"));
        assert!(agent.finished_at.is_some());
    }

    #[tokio::test]
    async fn a_closed_row_is_never_reopened_by_a_replay() {
        let pool = test_pool().await;
        record_turn(&pool, "chat-1", 7, LAUNCHES).await;
        apply_line(&pool, "chat-1", BG_NOTIFIED).await;

        record_turn(&pool, "chat-1", 7, LAUNCHES).await;
        apply_line(
            &pool,
            "chat-1",
            r#"{"type":"system","subtype":"task_updated","task_id":"b1","patch":{"status":"running"}}"#,
        )
        .await;

        let bash = row(&pool, "toolu_bg").await;
        assert_eq!(bash.status, "completed");
        assert_eq!(bash.total_tokens, Some(321));
        assert_eq!(for_chat(&pool, "chat-1").await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_restart_orphans_what_was_still_running() {
        let pool = test_pool().await;
        record_turn(&pool, "chat-1", 7, LAUNCHES).await;

        assert_eq!(orphan_running(&pool).await.unwrap(), 2);

        for call in ["toolu_bg", "toolu_agent"] {
            let task = row(&pool, call).await;
            assert_eq!(task.status, "orphaned", "{call}");
            assert!(task.finished_at.is_some(), "{call}");
        }
        assert_eq!(row(&pool, "toolu_task").await.status, "completed");
    }

    #[tokio::test]
    async fn stopping_a_process_closes_only_the_tasks_it_launched() {
        let pool = test_pool().await;
        record_turn(&pool, "chat-1", 7, LAUNCHES).await;
        sqlx::query(
            "INSERT INTO chat_tasks (chat_id, launched_by_run_id, tool_use_id, kind, status, started_at)
             VALUES ('chat-1', 8, 'toolu_other', 'background_bash', 'running', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(stop_launched_by(&pool, "chat-1", &[7]).await.unwrap(), 2);

        for call in ["toolu_bg", "toolu_agent"] {
            let task = row(&pool, call).await;
            assert_eq!(task.status, "stopped", "{call}");
            assert!(task.finished_at.is_some(), "{call}");
        }
        assert_eq!(row(&pool, "toolu_task").await.status, "completed");
        assert_eq!(row(&pool, "toolu_other").await.status, "running");

        assert_eq!(stop_launched_by(&pool, "chat-2", &[8]).await.unwrap(), 0);
        assert_eq!(row(&pool, "toolu_other").await.status, "running");
    }
}
