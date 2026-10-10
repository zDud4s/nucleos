//! Spec browser-com-painel §4.3: who owns a browsing session, whether that owner is still there,
//! and how text typed in the browser panel reaches it.

use sqlx::SqlitePool;

use crate::assistant::Origin;
use crate::browser::{self, SessionRow, mode};
use crate::browser_wheel::{self, SeatChoice, WheelError};
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
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
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

/// Push one message into the panel of a visible session. Best effort: the panel is a view, and a
/// sidecar that is gone or a panel that is closed must not fail the transition that caused the push.
pub async fn panel_push(state: &AppState, sidecar_id: &str, message: serde_json::Value) {
    if let Err(error) = state.browser.client.panel_push(sidecar_id, &message).await {
        tracing::debug!(%error, "the browser panel did not take a push");
    }
}

/// A `notice` line in the panel's conversation.
async fn push_notice(state: &AppState, sidecar_id: &str, text: &str) {
    panel_push(
        state,
        sidecar_id,
        serde_json::json!({
            "v": 1,
            "kind": "message",
            "role": "notice",
            "text": text,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
    .await;
}

const OWNER_FINISHED: &str = "the agent that opened this has finished";

/// The session as it is now, if it is still open.
async fn open_row(state: &AppState, id: i64) -> Result<Option<SessionRow>, WheelError> {
    Ok(browser::session_row(&state.pool, id)
        .await?
        .filter(|row| row.closed_at.is_none()))
}

/// Tell a live owner what the person did, in the form its kind can hear: a chat gets a turn, a run
/// with no chat gets the note on the wheel it is told about with its next snapshot.
async fn announce_return(state: &AppState, row: &SessionRow, text: &str) {
    let owner = owner_of(&state.pool, row).await;
    if !owner_alive(&state.pool, &owner).await {
        return;
    }
    match owner {
        Owner::Chat(_) => {
            if let Err(error) = deliver(state, row, text).await {
                tracing::warn!(
                    session = row.id,
                    %error,
                    "the owner was not told the wheel came back"
                );
            }
        }
        Owner::Run(_) => state.browser.seats.mark_returned_with_note(row.id, text),
        Owner::None => {}
    }
}

/// The person hands the wheel back from the panel (spec browser-com-painel §4.2).
///
/// Idempotent: a row that is no longer the person's is left alone and nothing is delivered. With the
/// owner gone the session closes as a plain return. Otherwise the stretch ends, the chain is
/// recorded and the session goes back to the agent; hosts the project does not admit yet are put to
/// the person first, and the note waits for that answer ([`on_keep`]).
pub async fn give_back_from_panel(
    state: &AppState,
    row: &SessionRow,
    note: Option<&str>,
) -> Result<(), WheelError> {
    let Some(row) = open_row(state, row.id).await? else {
        return Ok(());
    };
    if row.mode != mode::HUMAN {
        return Ok(());
    }
    let session_id = row.id;
    let now = chrono::Utc::now().to_rfc3339();
    let owner = owner_of(&state.pool, &row).await;

    if !owner_alive(&state.pool, &owner).await {
        // Nobody is left to hand it to: a plain return. The chain is kept so the person can still
        // answer the keep question about a login that has already happened.
        push_notice(state, &row.sidecar_id, OWNER_FINISHED).await;
        let chain = state
            .browser
            .client
            .end_person(&row.sidecar_id)
            .await
            .map(|returned| returned.chain)
            .unwrap_or_default();
        browser::record_chain(&state.pool, session_id, &chain).await?;
        let _ = browser::close_with_reason(
            &state.pool,
            &state.browser,
            session_id,
            "wheel-returned",
            &now,
        )
        .await;
        state.browser.seats.forget(session_id);
        return Ok(());
    }

    let returned = match state.browser.client.end_person(&row.sidecar_id).await {
        Ok(returned) => returned,
        Err(error) => {
            // The fence state is unknown, so the session must not go back to the agent.
            let _ = browser::close_with_reason(
                &state.pool,
                &state.browser,
                session_id,
                "fence-not-restored",
                &now,
            )
            .await;
            state.browser.seats.forget(session_id);
            return Err(WheelError::Sidecar(error));
        }
    };
    browser::record_chain(&state.pool, session_id, &returned.chain).await?;
    let moved = browser::set_mode_seat(
        &state.pool,
        &state.browser.modes,
        session_id,
        mode::HUMAN,
        mode::AGENT,
        None,
    )
    .await?;
    state.browser.seats.forget(session_id);
    if !moved {
        return Ok(());
    }

    let text = match note.map(str::trim).filter(|note| !note.is_empty()) {
        Some(note) => format!("the person handed the wheel back: {note}"),
        None => "the person handed the wheel back".to_string(),
    };
    let project = row.project_id.clone().unwrap_or_default();
    let admitted = browser::admitted_origins(&state.pool, &project).await?;
    let hosts: Vec<String> = crate::browser_policy::granted_origins(&returned.chain)
        .into_iter()
        .filter(|origin| !admitted.read.contains(origin))
        .collect();

    if hosts.is_empty() {
        // Nothing new to keep: settle the question so the chain is not left open, and speak now.
        let _ = browser_wheel::keep(state, session_id, false, false).await;
        announce_return(state, &row, &text).await;
    } else {
        state.browser.seats.hold_return(session_id, &text);
        panel_push(
            state,
            &row.sidecar_id,
            serde_json::json!({ "v": 1, "kind": "ask_keep", "hosts": hosts }),
        )
        .await;
    }
    Ok(())
}

/// The person's answer to the keep question asked by the panel. The grant is made FIRST, and only
/// then does the owner hear the note that waited for it; a second answer finds nothing waiting.
pub async fn on_keep(
    state: &AppState,
    row: &SessionRow,
    keep: bool,
    writable: bool,
) -> Result<Vec<String>, WheelError> {
    let granted = browser_wheel::keep(state, row.id, keep, writable).await;
    if let Some(text) = state.browser.seats.take_held_return(row.id) {
        announce_return(state, row, &text).await;
    }
    granted
}

/// The person takes the wheel from the panel: an asked-for wheel is approved by the very act, and a
/// session the agent is driving simply changes hands. If the person's stretch cannot begin the agent
/// keeps the wheel and nobody is told. With the owner gone the session stays the person's until they
/// close the window, and the panel says why nobody answers.
pub async fn take_from_panel(state: &AppState, row: &SessionRow) -> Result<(), WheelError> {
    let Some(row) = open_row(state, row.id).await? else {
        return Ok(());
    };
    if row.mode == mode::WHEEL_REQUESTED {
        if let Some(proposal) = row.proposal_id {
            let approved = crate::proposals::transition(
                &state.pool,
                proposal,
                "approved",
                "wheel accepted from the browser panel",
            )
            .await?;
            if !approved {
                return Err(WheelError::WrongState(
                    "this proposal has already been decided".to_string(),
                ));
            }
        }
        browser_wheel::accept_with_seat(state, row.id, Some(SeatChoice::Panel)).await?;
    } else if row.mode == mode::AGENT {
        if !browser::set_mode_seat(
            &state.pool,
            &state.browser.modes,
            row.id,
            mode::AGENT,
            mode::HUMAN,
            Some("window"),
        )
        .await?
        {
            return Ok(());
        }
        if let Err(error) = state.browser.client.begin_person(&row.sidecar_id).await {
            let _ = browser::set_mode_seat(
                &state.pool,
                &state.browser.modes,
                row.id,
                mode::HUMAN,
                mode::AGENT,
                None,
            )
            .await;
            return Err(WheelError::Sidecar(error));
        }
    } else {
        return Ok(());
    }

    let owner = owner_of(&state.pool, &row).await;
    if owner_alive(&state.pool, &owner).await {
        if let Err(error) = deliver(state, &row, "the person took the wheel").await {
            tracing::warn!(
                session = row.id,
                %error,
                "the owner was not told the wheel was taken"
            );
        }
    } else if owner != Owner::None {
        push_notice(state, &row.sidecar_id, OWNER_FINISHED).await;
    }
    Ok(())
}

/// The person closed the browser window: the row closes with its own reason and a live owner hears
/// it. A dead owner is not spoken to.
pub async fn on_person_closed(state: &AppState, row: &SessionRow) -> Result<(), WheelError> {
    let Some(row) = open_row(state, row.id).await? else {
        return Ok(());
    };
    let now = chrono::Utc::now().to_rfc3339();
    browser::close_session_row(&state.pool, row.id, "person-closed", &now).await?;
    state.browser.seats.forget(row.id);
    let _ = state.browser.seats.take_held_return(row.id);
    let owner = owner_of(&state.pool, &row).await;
    if owner_alive(&state.pool, &owner).await
        && let Err(error) = deliver(state, &row, "the person closed the browser").await
    {
        tracing::warn!(
            session = row.id,
            %error,
            "the owner was not told the browser was closed"
        );
    }
    Ok(())
}

/// Start reading the panel of a visible session, and mirroring its owning chat into it. One task per
/// session: a second start while one is running does nothing.
pub fn start(state: AppState, row_id: i64) {
    if !state.browser.seats.claim_pump(row_id) {
        return;
    }
    tokio::spawn(async move {
        tokio::select! {
            _ = pump(&state, row_id) => {}
            _ = mirror(&state, row_id) => {}
        }
        state.browser.seats.release_pump(row_id);
    });
}

/// Read the sidecar's panel events until the end record or the stream failing. No reconnect: the
/// session is over or the sidecar is gone.
async fn pump(state: &AppState, row_id: i64) {
    let Ok(Some(row)) = open_row(state, row_id).await else {
        return;
    };
    let mut response = match state.browser.client.panel_events(&row.sidecar_id).await {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!(session = row_id, %error, "the panel event stream did not open");
            return;
        }
    };
    let mut reader = crate::browser_live::RecordReader::default();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) | Err(_) => return,
        };
        for record in reader.push(&chunk) {
            match record.kind {
                b'N' => on_event(state, row_id, &record.body).await,
                b'E' => {
                    let reason = serde_json::from_slice::<serde_json::Value>(&record.body)
                        .ok()
                        .and_then(|body| body["reason"].as_str().map(str::to_string));
                    if reason.as_deref() == Some("person-closed")
                        && let Ok(Some(row)) = open_row(state, row_id).await
                        && let Err(error) = on_person_closed(state, &row).await
                    {
                        tracing::warn!(session = row_id, %error, "closing the session failed");
                    }
                    return;
                }
                _ => {}
            }
        }
        if reader.is_broken() {
            return;
        }
    }
}

/// One message the panel sent: what the person said or did there.
async fn on_event(state: &AppState, row_id: i64, body: &[u8]) {
    let Ok(event) = serde_json::from_slice::<serde_json::Value>(body) else {
        return;
    };
    let Ok(Some(row)) = open_row(state, row_id).await else {
        return;
    };
    let failure = match event["kind"].as_str().unwrap_or_default() {
        "say" => {
            let text = event["text"].as_str().unwrap_or_default();
            let ok = deliver(state, &row, text).await.is_ok();
            panel_push(
                state,
                &row.sidecar_id,
                serde_json::json!({
                    "v": 1,
                    "kind": "delivery",
                    "id": event["id"].clone(),
                    "ok": ok,
                }),
            )
            .await;
            None
        }
        "take_wheel" => take_from_panel(state, &row).await.err(),
        "give_back" => give_back_from_panel(state, &row, event["note"].as_str())
            .await
            .err(),
        "keep" => on_keep(
            state,
            &row,
            event["keep"].as_bool().unwrap_or(false),
            event["writable"].as_bool().unwrap_or(false),
        )
        .await
        .err(),
        _ => None,
    };
    if let Some(error) = failure {
        push_notice(state, &row.sidecar_id, &error.to_string()).await;
    }
}

/// How far the mirror has read: the last run whose turn was fully mirrored, the last run whose prompt
/// was, and the last said-now row. `Default` is the beginning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    run: i64,
    person: i64,
    said: i64,
}

fn panel_message(role: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "v": 1,
        "kind": "message",
        "role": role,
        "text": text,
        "ts": chrono::Utc::now().to_rfc3339(),
    })
}

/// The person's words without the `[browser panel · session N · url] ` prefix `deliver` put on them.
fn strip_panel_prefix(text: &str) -> &str {
    match text.strip_prefix("[browser panel") {
        Some(rest) => rest.split_once("] ").map_or(text, |(_, words)| words),
        None => text,
    }
}

/// What is new in the owning chat since `cursor`, as panel messages, and the cursor after them. Pure
/// over the pool. A turn still going yields its prompt now and its reply when it completes.
pub async fn mirror_step(
    pool: &SqlitePool,
    chat_id: &str,
    cursor: Cursor,
) -> (Vec<serde_json::Value>, Cursor) {
    let mut out = Vec::new();
    let mut next = cursor;

    let runs: Vec<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, COALESCE(prompt, ''), COALESCE(status, ''), stdout FROM runs
         WHERE chat_id = ? AND mode = 'assistant' AND id > ? ORDER BY id",
    )
    .bind(chat_id)
    .bind(cursor.run)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, prompt, status, stdout) in runs {
        if id > next.person {
            out.push(panel_message("person", strip_panel_prefix(&prompt)));
            next.person = id;
        }
        if matches!(status.as_str(), "running" | "queued" | "pending") {
            break;
        }
        if status == "completed"
            && let Some(reply) = stdout.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            out.push(panel_message("agent", reply));
        }
        next.run = id;
    }

    let said: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, text FROM chat_said_now WHERE chat_id = ? AND id > ? ORDER BY id",
    )
    .bind(chat_id)
    .bind(cursor.said)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, text) in said {
        out.push(panel_message("person", strip_panel_prefix(&text)));
        next.said = id;
    }
    (out, next)
}

/// The mirror task: only a chat-owned session has a conversation to mirror. Ticks every second and
/// ends when the row closes.
async fn mirror(state: &AppState, row_id: i64) {
    let Ok(Some(row)) = open_row(state, row_id).await else {
        return;
    };
    let Owner::Chat(chat) = owner_of(&state.pool, &row).await else {
        std::future::pending::<()>().await;
        return;
    };
    let before = row.run_id.map_or(0, |run| run - 1);
    let said: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM chat_said_now WHERE chat_id = ?")
            .bind(&chat)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
    let mut cursor = Cursor {
        run: before,
        person: before,
        said,
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let Ok(Some(row)) = open_row(state, row_id).await else {
            return;
        };
        let (messages, next) = mirror_step(&state.pool, &chat, cursor).await;
        cursor = next;
        for message in messages {
            panel_push(state, &row.sidecar_id, message).await;
        }
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

    fn record_wire(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![kind];
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    /// A sidecar whose `/panel/events` answers a fixed byte stream and whose `/panel/push` keeps
    /// every message it is given; everything else is 204.
    async fn panel_sidecar(
        stream: Vec<u8>,
    ) -> (
        BrowserRuntime,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let pushed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = pushed.clone();
        let app = axum::Router::new()
            .route(
                "/panel/events",
                post(move || {
                    let stream = stream.clone();
                    async move { stream.into_response() }
                }),
            )
            .route(
                "/panel/push",
                post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let kept = kept.clone();
                    async move {
                        kept.lock().unwrap().push(body["message"].clone());
                        axum::http::StatusCode::NO_CONTENT.into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let runtime = BrowserRuntime {
            enabled: true,
            client: BrowserClient::new(&address.to_string(), "tok".into()),
            modes: Default::default(),
            seats: Default::default(),
        };
        (runtime, pushed)
    }

    /// The pump reads the sidecar's panel events through `RecordReader`: a `say` reaches the owner
    /// and is acknowledged with a `delivery` push, and nothing after the end record is dispatched.
    #[tokio::test]
    async fn the_panel_pump_dispatches_events_until_the_end_record() {
        let db = TempDb::new().await;
        a_chat(&db.pool, "pump-chat").await;
        run_seven(&db.pool, "completed", Some("pump-chat")).await;
        let row = session_of(&db.pool, Some(7)).await;

        let mut stream = record_wire(
            b'N',
            br#"{"kind":"say","id":11,"text":"hello from the panel"}"#,
        );
        stream.extend(record_wire(b'E', br#"{"reason":"closed"}"#));
        stream.extend(record_wire(
            b'N',
            br#"{"kind":"say","id":12,"text":"after the end"}"#,
        ));
        let (runtime, pushed) = panel_sidecar(stream).await;
        let state = state_over(db.pool.clone(), runtime);

        start(state.clone(), row.id);

        let mut delivered = false;
        for _ in 0..100 {
            delivered = pushed.lock().unwrap().iter().any(|message| {
                message["kind"] == "delivery" && message["id"] == 11 && message["ok"] == true
            });
            if delivered {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(delivered, "{:?}", pushed.lock().unwrap());

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !pushed
                .lock()
                .unwrap()
                .iter()
                .any(|message| message["id"] == 12),
            "nothing after the end record is dispatched"
        );
        let turns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE chat_id = 'pump-chat' AND origin = 'browser-panel'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(turns, 1, "only the say before the end reached the chat");
        db.close().await;
    }

    async fn a_turn(pool: &sqlx::SqlitePool, id: i64, chat: &str, prompt: &str, stdout: &str) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, chat_id, stdout, created_at)              VALUES (?, ?, 'completed', 'assistant', ?, ?, '2026-08-16T10:00:00Z')",
        )
        .bind(id)
        .bind(prompt)
        .bind(chat)
        .bind(stdout)
        .execute(pool)
        .await
        .expect("insert turn");
    }

    /// The mirror pushes each new turn of the owning chat once: the person's words with the panel
    /// prefix stripped, then the agent's reply; a said-now row is the person again.
    #[tokio::test]
    async fn the_mirror_step_yields_new_turns_of_the_owning_chat_once() {
        let db = TempDb::new().await;
        a_chat(&db.pool, "mirror-chat").await;
        a_turn(
            &db.pool,
            7,
            "mirror-chat",
            "[browser panel · session 1 · https://jira.example.org/login] look at it",
            "I looked",
        )
        .await;

        let (first, cursor) = mirror_step(&db.pool, "mirror-chat", Cursor::default()).await;
        let pairs: Vec<(String, String)> = first
            .iter()
            .map(|m| {
                (
                    m["role"].as_str().unwrap_or_default().to_string(),
                    m["text"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("person".to_string(), "look at it".to_string()),
                ("agent".to_string(), "I looked".to_string()),
            ],
            "{first:?}"
        );

        let (again, cursor) = mirror_step(&db.pool, "mirror-chat", cursor).await;
        assert!(again.is_empty(), "a turn is mirrored once: {again:?}");

        sqlx::query(
            "INSERT INTO chat_said_now (chat_id, run_id, text, origin, created_at)              VALUES ('mirror-chat', 7, 'one more thing', 'shell', '2026-08-16T10:01:00Z')",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        a_turn(&db.pool, 9, "mirror-chat", "next question", "next answer").await;
        a_turn(&db.pool, 10, "other-chat", "not mine", "not mine either").await;

        let (later, cursor) = mirror_step(&db.pool, "mirror-chat", cursor).await;
        let texts: Vec<&str> = later.iter().filter_map(|m| m["text"].as_str()).collect();
        assert!(texts.contains(&"one more thing"), "{later:?}");
        assert!(texts.contains(&"next question"), "{later:?}");
        assert!(texts.contains(&"next answer"), "{later:?}");
        assert!(!texts.iter().any(|t| t.contains("not mine")), "{later:?}");

        let (done, _) = mirror_step(&db.pool, "mirror-chat", cursor).await;
        assert!(done.is_empty(), "{done:?}");
        db.close().await;
    }
}
