//! Spec browser-com-painel §4.3: who owns a browsing session, whether that owner is still there,
//! and how text typed in the browser panel reaches it.

use sqlx::SqlitePool;

use crate::assistant::Origin;
use crate::browser::SessionRow;
use crate::state::AppState;

/// Who a browsing session answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// The conversation whose turn opened the session.
    Chat(String),
    /// A run with no chat of its own: text for it waits on the seat.
    Run(i64),
    /// A session no run opened.
    None,
}

/// The owner of a session: the chat of the run that opened it, else that run, else nobody.
pub async fn owner_of(pool: &SqlitePool, row: &SessionRow) -> Owner {
    let Some(run_id) = row.run_id else {
        return Owner::None;
    };
    let chat: Option<Option<String>> = sqlx::query_scalar("SELECT chat_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .unwrap_or(None);
    match chat.flatten().filter(|chat| !chat.is_empty()) {
        Some(chat) => Owner::Chat(chat),
        None => Owner::Run(run_id),
    }
}

/// Whether the owner is still there to hear: an unarchived chat, or a run still going.
pub async fn owner_alive(pool: &SqlitePool, owner: &Owner) -> bool {
    match owner {
        Owner::Chat(chat) => {
            let archived: Option<Option<String>> =
                sqlx::query_scalar("SELECT archived_at FROM chats WHERE chat_id = ?")
                    .bind(chat)
                    .fetch_optional(pool)
                    .await
                    .unwrap_or(None);
            matches!(archived, Some(None))
        }
        Owner::Run(run_id) => {
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                    .bind(run_id)
                    .fetch_optional(pool)
                    .await
                    .unwrap_or(None);
            status.as_deref() == Some("running")
        }
        Owner::None => false,
    }
}

/// Hand panel text to the session's owner. A session with no owner is an error: nobody heard it.
pub async fn deliver(state: &AppState, row: &SessionRow, text: &str) -> Result<(), String> {
    match owner_of(&state.pool, row).await {
        Owner::Chat(chat) => {
            let sent = format!(
                "[browser panel · session {} · {}] {text}",
                row.id, row.final_url
            );
            crate::assistant::say_now_from(state, &chat, &sent, Origin::BrowserPanel)
                .await
                .map(|_| ())
        }
        Owner::Run(_) => {
            state.browser.seats.push_panel_message(row.id, text);
            Ok(())
        }
        Owner::None => Err("this session has no owner to hear it".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{BrowserRuntime, SessionBody, SessionRow};
    use crate::browser_client::BrowserClient;
    use crate::state::AppState;
    use crate::storage::TempDb;

    /// A browser runtime whose sidecar answers `/snapshot` with a page and everything else with 204.
    async fn snapshot_runtime() -> BrowserRuntime {
        use axum::extract::Path;
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let app = axum::Router::new().route(
            "/{verb}",
            post(
                |Path(verb): Path<String>, _body: axum::body::Bytes| async move {
                    if verb == "snapshot" {
                        return axum::Json(serde_json::json!({
                            "session_id": "s1",
                            "url": "https://jira.example.org/login",
                        }))
                        .into_response();
                    }
                    axum::http::StatusCode::NO_CONTENT.into_response()
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        BrowserRuntime {
            enabled: true,
            client: BrowserClient::new(&address.to_string(), "tok".into()),
            modes: Default::default(),
            seats: Default::default(),
        }
    }

    fn state_over(pool: sqlx::SqlitePool, browser: BrowserRuntime) -> AppState {
        AppState {
            token: crate::auth::Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: std::sync::Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(browser),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// A conversation with no directory of its own, so its turns keep no process.
    async fn a_chat(pool: &sqlx::SqlitePool, chat_id: &str) {
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(chat_id)
            .bind("2026-01-01T00:00:00Z")
            .execute(pool)
            .await
            .unwrap();
    }

    /// Run 7, in the given status, belonging to `chat_id` when there is one.
    async fn run_seven(pool: &sqlx::SqlitePool, status: &str, chat_id: Option<&str>) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, chat_id, created_at) \
             VALUES (7, 'browse', ?, 'assistant', ?, '2026-08-16T10:00:00Z')",
        )
        .bind(status)
        .bind(chat_id)
        .execute(pool)
        .await
        .expect("insert run");
    }

    /// One open session opened by run 7 (or by no run), as the row the panel code receives.
    async fn session_of(pool: &sqlx::SqlitePool, run_id: Option<i64>) -> SessionRow {
        let id = sqlx::query(
            "INSERT INTO browser_sessions \
                (sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url, \
                 final_url, rule, mode, opened_at) \
             VALUES ('s1', ?, 'acme', 'project', 'acme', 'https://jira.example.org/login', \
                     'https://jira.example.org/login', 'project-site', 'agent', \
                     '2026-08-16T10:00:00Z')",
        )
        .bind(run_id)
        .execute(pool)
        .await
        .expect("insert session")
        .last_insert_rowid();
        crate::browser::session_row(pool, id)
            .await
            .expect("query")
            .expect("the row")
    }

    /// A chat turn that opens a browser is a run carrying the chat's id, and the panel's text goes
    /// back to that chat; a run with no chat is its own owner; a session nobody opened has none.
    #[tokio::test]
    async fn the_owner_of_a_session_opened_by_a_chat_turn_is_the_chat() {
        let db = TempDb::new().await;
        a_chat(&db.pool, "panel-chat").await;
        run_seven(&db.pool, "running", Some("panel-chat")).await;
        let row = session_of(&db.pool, Some(7)).await;

        let owner = owner_of(&db.pool, &row).await;
        assert!(
            matches!(&owner, Owner::Chat(chat) if chat == "panel-chat"),
            "{owner:?}"
        );
        assert!(
            owner_alive(&db.pool, &owner).await,
            "an unarchived chat is there"
        );

        sqlx::query("UPDATE chats SET archived_at = '2026-08-17T00:00:00Z' WHERE chat_id = ?")
            .bind("panel-chat")
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(
            !owner_alive(&db.pool, &owner).await,
            "an archived chat is gone"
        );

        let orphan = session_of(&db.pool, None).await;
        assert!(matches!(owner_of(&db.pool, &orphan).await, Owner::None));
        db.close().await;
    }

    /// Spec §4.3: text for a chat owner goes through `say_now_from` as the panel, and says which
    /// session and page it was typed about.
    #[tokio::test]
    async fn a_panel_say_is_delivered_through_say_now_with_the_browser_panel_origin() {
        let db = TempDb::new().await;
        let state = state_over(db.pool.clone(), BrowserRuntime::disabled());
        a_chat(&db.pool, "panel-say").await;
        run_seven(&db.pool, "completed", Some("panel-say")).await;
        let row = session_of(&db.pool, Some(7)).await;

        deliver(&state, &row, "hello from the panel")
            .await
            .expect("delivered");

        let sent = format!(
            "[browser panel · session {} · https://jira.example.org/login] hello from the panel",
            row.id
        );
        let (origin, prompt): (Option<String>, String) = sqlx::query_as(
            "SELECT origin, prompt FROM runs WHERE chat_id = ? AND origin = 'browser-panel'",
        )
        .bind("panel-say")
        .fetch_one(&db.pool)
        .await
        .expect("a turn recorded as the panel");
        assert_eq!(origin.as_deref(), Some("browser-panel"));
        assert!(prompt.contains(&sent), "{prompt}");
        db.close().await;
    }

    /// A run with no chat has nobody to speak to: the panel's messages wait on the seat and the
    /// agent's next snapshot carries them, once, in order.
    #[tokio::test]
    async fn a_run_without_a_chat_accumulates_panel_messages_and_gets_them_once() {
        let db = TempDb::new().await;
        let state = state_over(db.pool.clone(), snapshot_runtime().await);
        run_seven(&db.pool, "running", None).await;
        let row = session_of(&db.pool, Some(7)).await;
        let owner = owner_of(&db.pool, &row).await;
        assert!(matches!(owner, Owner::Run(7)), "{owner:?}");
        assert!(owner_alive(&db.pool, &owner).await);

        deliver(&state, &row, "first").await.expect("first");
        deliver(&state, &row, "second").await.expect("second");

        let ask = |state: AppState| {
            let id = row.id;
            async move {
                let response = crate::browser::post_snapshot(
                    axum::extract::State(state),
                    axum::Json(SessionBody {
                        session_id: id,
                        changes_only: false,
                        text_from: 0,
                        controls_from: 0,
                        find: String::new(),
                    }),
                )
                .await;
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body");
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
            }
        };

        let first = ask(state.clone()).await;
        assert_eq!(
            first["panel_messages"],
            serde_json::json!(["first", "second"]),
            "{first}"
        );
        assert_eq!(first["session_id"], "s1", "the snapshot itself is intact");

        let second = ask(state.clone()).await;
        assert!(second.get("panel_messages").is_none(), "{second}");
        db.close().await;
    }
}
