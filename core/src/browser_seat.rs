//! The person's seat in the agent's headless browser (spec browser-volante §4.3).
//!
//! Holds the in-memory state of that seat: the `seat_nonce` the shell must present with each input
//! (never persisted, never logged) and the "wheel returned" flag the UI is told about once.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use rand::RngExt;
use subtle::ConstantTimeEq;

/// Seat state kept in memory per session id.
#[derive(Debug, Default)]
pub struct SeatState {
    nonces: Mutex<HashMap<i64, String>>,
    returned: Mutex<HashSet<i64>>,
    panel_messages: Mutex<HashMap<i64, Vec<String>>>,
    returned_notes: Mutex<HashMap<i64, String>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SeatState {
    /// Mint a fresh nonce for a session, replacing any earlier one: 16 random bytes as lowercase hex.
    pub fn issue(&self, id: i64) -> String {
        let bytes = rand::rng().random::<[u8; 16]>();
        let nonce: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        locked(&self.nonces).insert(id, nonce.clone());
        nonce
    }

    /// Whether `presented` is the nonce issued for this session. False when none was issued or the
    /// input is empty; compared in constant time.
    pub fn matches(&self, id: i64, presented: &str) -> bool {
        if presented.is_empty() {
            return false;
        }
        match locked(&self.nonces).get(&id) {
            Some(issued) => issued.as_bytes().ct_eq(presented.as_bytes()).into(),
            None => false,
        }
    }

    /// Drop a session's nonce.
    pub fn forget(&self, id: i64) {
        locked(&self.nonces).remove(&id);
    }

    /// Note that the wheel came back from the person, to be announced once.
    pub fn mark_returned(&self, id: i64) {
        locked(&self.returned).insert(id);
    }

    /// Consume the "wheel returned" flag: true once per `mark_returned`.
    pub fn take_returned(&self, id: i64) -> bool {
        locked(&self.returned).remove(&id)
    }

    /// `mark_returned`, plus a note the person left with the wheel, told once alongside the flag.
    pub fn mark_returned_with_note(&self, id: i64, note: &str) {
        self.mark_returned(id);
        locked(&self.returned_notes).insert(id, note.to_string());
    }

    /// Consume the note left with the wheel: `Some` once per `mark_returned_with_note`.
    pub fn take_returned_note(&self, id: i64) -> Option<String> {
        locked(&self.returned_notes).remove(&id)
    }

    /// Queue text the person typed in the panel for a run that has no chat to speak to.
    pub fn push_panel_message(&self, id: i64, text: &str) {
        locked(&self.panel_messages)
            .entry(id)
            .or_default()
            .push(text.to_string());
    }

    /// Consume the queued panel messages, in the order they were typed.
    pub fn take_panel_messages(&self, id: i64) -> Vec<String> {
        locked(&self.panel_messages).remove(&id).unwrap_or_default()
    }
}

/// Whether the caller is a run rather than the owner's shell.
///
/// `browser_policy::Requester` is presence only and says nothing about who is calling, so the
/// check is the credential: every scope but the control token (and an admin API token) is a run's,
/// and the only agents holding the control token are orchestrator turns, whose MCP client always
/// sends the run-id header. A control-token holder that omits the header is not caught here; the
/// defence against that is presence (`take`) and the in-memory nonce (input and answer).
pub fn from_a_run(scope: &crate::auth::Scope, headers: &axum::http::HeaderMap) -> bool {
    use crate::auth::{ApiTokenLevel, Scope};
    !matches!(
        scope,
        Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin)
    ) || headers.contains_key(crate::daemon_client::RUN_ID_HEADER)
}

/// Why a seat operation was refused.
#[derive(Debug)]
pub enum SeatError {
    Disabled,
    Gone,
    RunRequester,
    OwnerAbsent,
    NotAgent,
    SeatUnavailable,
    NotPerson,
    BadNonce,
    Begin(crate::browser_client::BrowserError),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for SeatError {
    fn from(error: sqlx::Error) -> Self {
        SeatError::Db(error)
    }
}

/// A seat refusal as an HTTP answer: `{"error": code, "detail": ...}`.
pub fn seat_error(error: SeatError) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse as _;

    let (status, code, detail) = match error {
        SeatError::Disabled => (StatusCode::CONFLICT, "pillar_off", String::new()),
        SeatError::Gone => (StatusCode::NOT_FOUND, "no_session", String::new()),
        SeatError::RunRequester => (StatusCode::FORBIDDEN, "run_requester", String::new()),
        SeatError::OwnerAbsent => (StatusCode::FORBIDDEN, "owner_absent", String::new()),
        SeatError::NotAgent => (StatusCode::CONFLICT, "not_agent", String::new()),
        SeatError::SeatUnavailable => (StatusCode::CONFLICT, "seat_unavailable", String::new()),
        SeatError::NotPerson => (StatusCode::CONFLICT, "not_person", String::new()),
        SeatError::BadNonce => (StatusCode::FORBIDDEN, "bad_nonce", String::new()),
        SeatError::Begin(error) => (StatusCode::BAD_GATEWAY, "begin_failed", error.to_string()),
        SeatError::Db(error) => (StatusCode::INTERNAL_SERVER_ERROR, "db", error.to_string()),
    };
    (
        status,
        axum::Json(serde_json::json!({ "error": code, "detail": detail })),
    )
        .into_response()
}

/// The person takes the wheel in the shell. The row moves first (so a second caller finds it
/// already human), then the sidecar is asked to begin the person stretch; if that fails the row
/// lands in `delivery-failed`, never back with the agent. Returns the nonce the seat's input needs.
/// A session already human/shell just gets a fresh nonce (the previous one stops matching), with no
/// row change and no sidecar call.
pub async fn take(
    state: &crate::state::AppState,
    id: i64,
    from_run: bool,
) -> Result<String, SeatError> {
    use crate::browser::{self, mode};

    if !state.browser.enabled {
        return Err(SeatError::Disabled);
    }
    if from_run {
        return Err(SeatError::RunRequester);
    }
    if !crate::attention::owner_is_present(&state.pool, chrono::Utc::now()).await {
        return Err(SeatError::OwnerAbsent);
    }
    let row = browser::live_session(state, id)
        .await
        .ok_or(SeatError::Gone)?;
    // A shell seat that is already the person's: hand out a fresh nonce, touching nothing else.
    // The nonce lives only in memory, so a daemon restart, a shell reload, or an approval from a
    // caller that discards it (Telegram approves with no body) would otherwise leave a shell seat
    // nobody can drive. Re-issuing replaces the old nonce. Human/window and every other non-agent
    // mode still fall through to `NotAgent`.
    if row.mode == mode::HUMAN && row.seat.as_deref() == Some("shell") {
        return Ok(state.browser.seats.issue(id));
    }
    if row.mode != mode::AGENT {
        return Err(SeatError::NotAgent);
    }
    if !row.shell_eligible {
        return Err(SeatError::SeatUnavailable);
    }
    let moved = browser::set_mode_seat(
        &state.pool,
        &state.browser.modes,
        id,
        mode::AGENT,
        mode::HUMAN,
        Some("shell"),
    )
    .await?;
    if !moved {
        return Err(SeatError::NotAgent);
    }
    if let Err(error) = state.browser.client.begin_person(&row.sidecar_id).await {
        let _ = browser::set_mode(
            &state.pool,
            &state.browser.modes,
            id,
            mode::HUMAN,
            mode::DELIVERY_FAILED,
        )
        .await;
        return Err(SeatError::Begin(error));
    }
    Ok(state.browser.seats.issue(id))
}

/// `POST /browser/sessions/{id}/take`.
pub async fn post_take(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;

    let from_run = from_a_run(&scope, &headers);
    match take(&state, id, from_run).await {
        Ok(nonce) => axum::Json(serde_json::json!({ "seat_nonce": nonce })).into_response(),
        Err(error) => seat_error(error),
    }
}

/// Body limit for `/input`: a batch of pointer and key events is tiny.
pub const INPUT_BODY_LIMIT: usize = 64 * 1024;

/// Body limit for `/answer`: a file-chooser answer carries file bytes (base64), so it is large.
pub const ANSWER_BODY_LIMIT: usize = 14 * 1024 * 1024;

/// `POST /browser/sessions/{id}/input` body.
#[derive(Debug, serde::Deserialize)]
pub struct InputBody {
    #[serde(default)]
    pub seat_nonce: String,
    pub events: serde_json::Value,
}

/// `POST /browser/sessions/{id}/answer` body.
#[derive(Debug, serde::Deserialize)]
pub struct AnswerBody {
    #[serde(default)]
    pub seat_nonce: String,
    pub prompt: String,
    pub answer: serde_json::Value,
}

/// The checks `input` and `answer` share: pillar on, not a run, a live row, the person driving
/// from the shell, and the nonce. Returns the sidecar session id.
async fn guard_seat(
    state: &crate::state::AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    id: i64,
    nonce: &str,
) -> Result<String, SeatError> {
    use crate::browser::{self, mode};

    if !state.browser.enabled {
        return Err(SeatError::Disabled);
    }
    if from_a_run(scope, headers) {
        return Err(SeatError::RunRequester);
    }
    let row = browser::live_session(state, id)
        .await
        .ok_or(SeatError::Gone)?;
    if row.mode != mode::HUMAN || row.seat.as_deref() != Some("shell") {
        return Err(SeatError::NotPerson);
    }
    if !state.browser.seats.matches(id, nonce) {
        return Err(SeatError::BadNonce);
    }
    Ok(row.sidecar_id)
}

/// A sidecar answer relayed as it came: its status and its JSON body.
fn relayed(relayed: crate::browser_client::Relayed) -> axum::response::Response {
    use axum::response::IntoResponse as _;

    let status = axum::http::StatusCode::from_u16(relayed.status)
        .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        relayed.body,
    )
        .into_response()
}

/// `POST /browser/sessions/{id}/input`: forward the person's events; the nonce stays here.
pub async fn post_input(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(body): axum::Json<InputBody>,
) -> axum::response::Response {
    let session = match guard_seat(&state, &scope, &headers, id, &body.seat_nonce).await {
        Ok(session) => session,
        Err(error) => return seat_error(error),
    };
    match state
        .browser
        .client
        .person_input(&session, &body.events)
        .await
    {
        Ok(answer) => relayed(answer),
        Err(error) => crate::browser::browser_error(error),
    }
}

/// `POST /browser/sessions/{id}/answer`: forward the person's answer to a page prompt.
pub async fn post_answer(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
    axum::Json(body): axum::Json<AnswerBody>,
) -> axum::response::Response {
    let session = match guard_seat(&state, &scope, &headers, id, &body.seat_nonce).await {
        Ok(session) => session,
        Err(error) => return seat_error(error),
    };
    match state
        .browser
        .client
        .answer(&session, &body.prompt, &body.answer)
        .await
    {
        Ok(answer) => relayed(answer),
        Err(error) => crate::browser::browser_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A nonce is the proof the shell holds the seat: it matches only the one issued for that
    /// session, only until forgotten, and never when none was issued or the input is empty.
    #[test]
    fn volante_nonce_matches_once_issued_and_is_forgotten() {
        let seats = SeatState::default();
        assert!(!seats.matches(1, "anything"), "nothing issued yet");

        let nonce = seats.issue(1);
        assert_eq!(nonce.len(), 32);
        assert!(
            nonce
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert!(seats.matches(1, &nonce));
        assert!(!seats.matches(1, ""), "an empty input never matches");
        assert!(!seats.matches(1, "wrong"));
        assert!(!seats.matches(2, &nonce), "a nonce belongs to its session");

        let replaced = seats.issue(1);
        assert_ne!(replaced, nonce);
        assert!(
            !seats.matches(1, &nonce),
            "a new nonce replaces the old one"
        );
        assert!(seats.matches(1, &replaced));

        seats.forget(1);
        assert!(!seats.matches(1, &replaced));
    }

    /// "Wheel returned" is announced to the UI once: taking it consumes it.
    #[test]
    fn volante_wheel_returned_is_taken_once() {
        let seats = SeatState::default();
        assert!(!seats.take_returned(7));
        seats.mark_returned(7);
        assert!(seats.take_returned(7));
        assert!(!seats.take_returned(7), "the second take finds nothing");
    }

    use crate::browser::mode;
    use crate::browser_client::BrowserClient;
    use crate::state::AppState;
    use crate::storage::TempDb;

    type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A sidecar that serves `/person/{verb}`, `/input` and `/answer`, recording `<verb> <body>` for
    /// every call. `begin` answers 200 `{}`, or 500 when `fail_begin` is set. `/input` answers 202
    /// `{"accepted": <n events>}`; `/answer` answers 404 `{"error":"no_prompt"}` when the prompt is
    /// "gone" and 200 `{"answered":true}` otherwise.
    async fn stub_sidecar(
        fail_begin: bool,
    ) -> (std::sync::Arc<crate::browser::BrowserRuntime>, Seen) {
        use axum::extract::Path;
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let seen: Seen = Default::default();
        let recorder = seen.clone();
        let input_recorder = seen.clone();
        let answer_recorder = seen.clone();
        let app = axum::Router::new()
            .route(
                "/person/{verb}",
                post(move |Path(verb): Path<String>, body: axum::body::Bytes| {
                    let recorder = recorder.clone();
                    async move {
                        recorder
                            .lock()
                            .unwrap()
                            .push(format!("{verb} {}", String::from_utf8_lossy(&body)));
                        if verb == "begin" && fail_begin {
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "no display")
                                .into_response();
                        }
                        axum::Json(serde_json::json!({})).into_response()
                    }
                }),
            )
            .route(
                "/input",
                post(move |body: axum::body::Bytes| {
                    let recorder = input_recorder.clone();
                    async move {
                        recorder
                            .lock()
                            .unwrap()
                            .push(format!("input {}", String::from_utf8_lossy(&body)));
                        let parsed: serde_json::Value =
                            serde_json::from_slice(&body).unwrap_or_default();
                        let accepted = parsed["events"].as_array().map_or(0, Vec::len);
                        (
                            axum::http::StatusCode::ACCEPTED,
                            axum::Json(serde_json::json!({ "accepted": accepted })),
                        )
                            .into_response()
                    }
                }),
            )
            .route(
                "/answer",
                post(move |body: axum::body::Bytes| {
                    let recorder = answer_recorder.clone();
                    async move {
                        recorder
                            .lock()
                            .unwrap()
                            .push(format!("answer {}", String::from_utf8_lossy(&body)));
                        let parsed: serde_json::Value =
                            serde_json::from_slice(&body).unwrap_or_default();
                        if parsed["prompt"] == "gone" {
                            return (
                                axum::http::StatusCode::NOT_FOUND,
                                axum::Json(serde_json::json!({ "error": "no_prompt" })),
                            )
                                .into_response();
                        }
                        axum::Json(serde_json::json!({ "answered": true })).into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            std::sync::Arc::new(crate::browser::BrowserRuntime {
                enabled: true,
                client: BrowserClient::new(&address.to_string(), "tok".into()),
                modes: Default::default(),
                seats: Default::default(),
            }),
            seen,
        )
    }

    async fn seated(fail_begin: bool) -> (TempDb, AppState, Seen) {
        let db = TempDb::new().await;
        let (browser, seen) = stub_sidecar(fail_begin).await;
        let state = AppState {
            token: crate::auth::Token("test-token".into()),
            pool: db.pool.clone(),
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
            browser,
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };
        (db, state, seen)
    }

    /// One open agent-mode row for project `acme` on the sidecar id `s1`.
    async fn row(state: &AppState, profile_kind: &str, profile_id: &str, sidecar_id: &str) -> i64 {
        sqlx::query(
            "INSERT INTO browser_sessions \
                (sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url, \
                 final_url, rule, mode, opened_at) \
             VALUES (?, 1, 'acme', ?, ?, 'https://example.org/', 'https://example.org/', \
                     'project-site', 'agent', '2026-08-16T10:00:00Z')",
        )
        .bind(sidecar_id)
        .bind(profile_kind)
        .bind(profile_id)
        .execute(&state.pool)
        .await
        .expect("insert")
        .last_insert_rowid()
    }

    async fn present(state: &AppState) {
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Global,
            chrono::Utc::now(),
        )
        .await
        .expect("heartbeat");
    }

    async fn mode_of(state: &AppState, id: i64) -> String {
        crate::browser::session_row(&state.pool, id)
            .await
            .expect("row")
            .expect("exists")
            .mode
    }

    /// The seat is the owner's: with nobody present the take is refused, nothing moves and the
    /// sidecar is never called.
    #[tokio::test]
    async fn volante_take_requires_the_owner_present() {
        let (_db, state, seen) = seated(false).await;
        let id = row(&state, "project", "acme", "s1").await;

        let taken = take(&state, id, false).await;
        assert!(matches!(taken, Err(SeatError::OwnerAbsent)), "{taken:?}");
        assert_eq!(mode_of(&state, id).await, mode::AGENT);
        assert!(seen.lock().unwrap().is_empty());
    }

    /// Only the agent's own session can be taken: a missing row is gone, and one a person already
    /// drives (or that was already requested) is not the agent's any more.
    #[tokio::test]
    async fn volante_take_only_from_agent() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;

        let missing = take(&state, 9999, false).await;
        assert!(matches!(missing, Err(SeatError::Gone)), "{missing:?}");

        let id = row(&state, "project", "acme", "s1").await;
        sqlx::query("UPDATE browser_sessions SET mode = 'human' WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        let taken = take(&state, id, false).await;
        assert!(matches!(taken, Err(SeatError::NotAgent)), "{taken:?}");
        assert_eq!(mode_of(&state, id).await, mode::HUMAN);
        assert!(seen.lock().unwrap().is_empty());
    }

    /// The shell seat reuses the project's own browser, so a throwaway profile and a profile shared
    /// by two open sessions are both refused, before any write.
    #[tokio::test]
    async fn volante_take_refuses_a_throwaway_and_a_shared_profile() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;

        let throwaway = row(&state, "ephemeral", "t1", "s1").await;
        let refused = take(&state, throwaway, false).await;
        assert!(
            matches!(refused, Err(SeatError::SeatUnavailable)),
            "{refused:?}"
        );
        assert_eq!(mode_of(&state, throwaway).await, mode::AGENT);

        let first = row(&state, "project", "acme", "s2").await;
        let second = row(&state, "project", "acme", "s3").await;
        for id in [first, second] {
            let refused = take(&state, id, false).await;
            assert!(
                matches!(refused, Err(SeatError::SeatUnavailable)),
                "{refused:?}"
            );
            assert_eq!(mode_of(&state, id).await, mode::AGENT);
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    /// A run must never take the seat: the check is the scope plus the run-id header, and a refused
    /// take reaches neither the database nor the sidecar.
    #[tokio::test]
    async fn volante_take_refuses_a_run_requester() {
        use crate::auth::{ApiTokenLevel, Scope};
        use axum::http::{HeaderMap, HeaderValue};

        let none = HeaderMap::new();
        assert!(from_a_run(&Scope::Run(7), &none));
        assert!(!from_a_run(&Scope::Control, &none));
        let mut with_run = HeaderMap::new();
        with_run.insert(
            crate::daemon_client::RUN_ID_HEADER,
            HeaderValue::from_static("7"),
        );
        assert!(from_a_run(&Scope::Control, &with_run));
        assert!(!from_a_run(&Scope::ApiToken(ApiTokenLevel::Admin), &none));

        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;
        let taken = take(&state, id, true).await;
        assert!(matches!(taken, Err(SeatError::RunRequester)), "{taken:?}");
        assert_eq!(mode_of(&state, id).await, mode::AGENT);
        assert!(seen.lock().unwrap().is_empty());
    }

    /// The happy path writes the row first (human on the shell seat), then asks the sidecar to begin
    /// the person stretch, and hands back a nonce the seat then accepts.
    #[tokio::test]
    async fn volante_take_moves_the_row_then_begins_person_and_returns_a_nonce() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;

        let nonce = take(&state, id, false).await.expect("take");
        assert_eq!(nonce.len(), 32);
        assert!(state.browser.seats.matches(id, &nonce));

        let stored = crate::browser::session_row(&state.pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.mode, mode::HUMAN);
        assert_eq!(stored.seat.as_deref(), Some("shell"));

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].starts_with("begin "), "{calls:?}");
        assert!(calls[0].contains("s1"), "{calls:?}");
    }

    /// When the sidecar cannot begin, the row is already human, so it lands in delivery-failed
    /// (never back to the agent) and no nonce is left behind.
    #[tokio::test]
    async fn volante_take_with_failed_begin_person_is_delivery_failed() {
        let (_db, state, _seen) = seated(true).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;

        let taken = take(&state, id, false).await;
        assert!(matches!(taken, Err(SeatError::Begin(_))), "{taken:?}");
        assert_eq!(mode_of(&state, id).await, mode::DELIVERY_FAILED);
        assert!(!state.browser.seats.matches(id, "anything"));
    }

    /// Puts a row in human mode on the given seat, the way a take or an approval would leave it.
    async fn hand_to_person(state: &AppState, id: i64, seat: &str) {
        sqlx::query("UPDATE browser_sessions SET mode = 'human', seat = ? WHERE id = ?")
            .bind(seat)
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    /// A shell that lost its nonce (a reload) takes again: the person already holds the seat, so
    /// the row stays human/shell, a NEW nonce replaces the old one, and the sidecar hears nothing.
    #[tokio::test]
    async fn volante_take_reissues_the_nonce_for_a_shell_seat() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;
        hand_to_person(&state, id, "shell").await;
        let old = state.browser.seats.issue(id);

        let fresh = take(&state, id, false).await.expect("reissue");
        assert_eq!(fresh.len(), 32);
        assert_ne!(fresh, old);
        assert!(!state.browser.seats.matches(id, &old));
        assert!(state.browser.seats.matches(id, &fresh));

        let stored = crate::browser::session_row(&state.pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.mode, mode::HUMAN);
        assert_eq!(stored.seat.as_deref(), Some("shell"));
        assert!(seen.lock().unwrap().is_empty(), "no second person/begin");
    }

    /// A person on the real window has no shell seat to re-enter: take still refuses it as not
    /// the agent's, and nothing moves.
    #[tokio::test]
    async fn volante_take_still_refuses_a_window_seat() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;
        hand_to_person(&state, id, "window").await;

        let taken = take(&state, id, false).await;
        assert!(matches!(taken, Err(SeatError::NotAgent)), "{taken:?}");
        assert_eq!(mode_of(&state, id).await, mode::HUMAN);
        assert!(seen.lock().unwrap().is_empty());
    }

    /// The reissue path is the owner's too: a run requester is refused there exactly as it is on
    /// the first take, and the nonce already issued is left alone.
    #[tokio::test]
    async fn volante_take_reissue_refuses_a_run_requester() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;
        hand_to_person(&state, id, "shell").await;
        let old = state.browser.seats.issue(id);

        let taken = take(&state, id, true).await;
        assert!(matches!(taken, Err(SeatError::RunRequester)), "{taken:?}");
        assert!(state.browser.seats.matches(id, &old));
        assert_eq!(mode_of(&state, id).await, mode::HUMAN);
        assert!(seen.lock().unwrap().is_empty());
    }

    /// Calls `post_input` the way the router would, as the owner's shell (control token, no run id).
    async fn call_input(
        state: &AppState,
        id: i64,
        nonce: &str,
        events: serde_json::Value,
    ) -> axum::response::Response {
        post_input(
            axum::extract::State(state.clone()),
            axum::Extension(crate::auth::Scope::Control),
            axum::http::HeaderMap::new(),
            axum::extract::Path(id),
            axum::Json(InputBody {
                seat_nonce: nonce.to_string(),
                events,
            }),
        )
        .await
    }

    /// Calls `post_answer` the way the router would, as the owner's shell.
    async fn call_answer(
        state: &AppState,
        id: i64,
        nonce: &str,
        prompt: &str,
        answer: serde_json::Value,
    ) -> axum::response::Response {
        post_answer(
            axum::extract::State(state.clone()),
            axum::Extension(crate::auth::Scope::Control),
            axum::http::HeaderMap::new(),
            axum::extract::Path(id),
            axum::Json(AnswerBody {
                seat_nonce: nonce.to_string(),
                prompt: prompt.to_string(),
                answer,
            }),
        )
        .await
    }

    async fn body_json(response: axum::response::Response) -> (u16, serde_json::Value) {
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    /// Input needs the seat's nonce: a wrong or missing one is refused with `bad_nonce`, and so is
    /// a run's call even with the right nonce. Nothing reaches the sidecar but the earlier `begin`.
    #[tokio::test]
    async fn volante_input_refuses_a_wrong_or_missing_nonce() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;
        let nonce = take(&state, id, false).await.expect("take");
        let events = serde_json::json!([{"t": "click", "x": 1, "y": 2}]);

        for presented in ["", "wrong"] {
            let (status, body) =
                body_json(call_input(&state, id, presented, events.clone()).await).await;
            assert_eq!(status, 403, "{presented:?}");
            assert_eq!(body["error"], "bad_nonce", "{presented:?}");
        }

        let mut with_run = axum::http::HeaderMap::new();
        with_run.insert(
            crate::daemon_client::RUN_ID_HEADER,
            axum::http::HeaderValue::from_static("7"),
        );
        let from_run = post_input(
            axum::extract::State(state.clone()),
            axum::Extension(crate::auth::Scope::Control),
            with_run,
            axum::extract::Path(id),
            axum::Json(InputBody {
                seat_nonce: nonce,
                events,
            }),
        )
        .await;
        assert_eq!(
            from_run.status().as_u16(),
            403,
            "a run never drives the seat"
        );

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "only begin reached the sidecar: {calls:?}");
        assert!(calls[0].starts_with("begin "), "{calls:?}");
    }

    /// Input and answer are the person's alone: an agent-mode row, a human row on the window seat
    /// and a missing row are all refused, even holding a nonce, and the sidecar is never called.
    #[tokio::test]
    async fn volante_input_and_answer_refuse_outside_person_mode() {
        let (_db, state, seen) = seated(false).await;
        let id = row(&state, "project", "acme", "s1").await;
        let nonce = state.browser.seats.issue(id);
        let events = serde_json::json!([]);

        let (status, body) = body_json(call_input(&state, id, &nonce, events.clone()).await).await;
        assert_eq!(
            (status, &body["error"]),
            (409, &serde_json::json!("not_person"))
        );
        let (status, body) =
            body_json(call_answer(&state, id, &nonce, "p1", serde_json::json!(true)).await).await;
        assert_eq!(
            (status, &body["error"]),
            (409, &serde_json::json!("not_person"))
        );

        sqlx::query("UPDATE browser_sessions SET mode = 'human', seat = 'window' WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        let (status, body) = body_json(call_input(&state, id, &nonce, events.clone()).await).await;
        assert_eq!(
            (status, &body["error"]),
            (409, &serde_json::json!("not_person"))
        );
        let (status, body) =
            body_json(call_answer(&state, id, &nonce, "p1", serde_json::json!(true)).await).await;
        assert_eq!(
            (status, &body["error"]),
            (409, &serde_json::json!("not_person"))
        );

        let (status, body) = body_json(call_input(&state, 9999, &nonce, events).await).await;
        assert_eq!(
            (status, &body["error"]),
            (404, &serde_json::json!("no_session"))
        );

        assert!(
            seen.lock().unwrap().is_empty(),
            "the sidecar was never called"
        );
    }

    /// With the right nonce the events go down as `{session, events}` and the sidecar's own status
    /// and body come back, JSON-typed. The nonce never goes to the sidecar.
    #[tokio::test]
    async fn volante_input_forwards_events_without_the_nonce() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;
        let nonce = take(&state, id, false).await.expect("take");
        let events = serde_json::json!([{"t": "click", "x": 1, "y": 2}, {"t": "key", "k": "a"}]);

        let response = call_input(&state, id, &nonce, events.clone()).await;
        assert_eq!(response.status().as_u16(), 202);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let (_, body) = body_json(response).await;
        assert_eq!(body, serde_json::json!({"accepted": 2}));

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "begin then input: {calls:?}");
        let sent = calls[1].strip_prefix("input ").expect("an input call");
        let sent: serde_json::Value = serde_json::from_str(sent).expect("a JSON body");
        assert_eq!(sent, serde_json::json!({"session": "s1", "events": events}));
        assert!(!calls[1].contains(&nonce), "the nonce stays in the daemon");
    }

    /// An answer is relayed verbatim, the sidecar's refusal included, and forwarded without the nonce.
    #[tokio::test]
    async fn volante_answer_relays_the_sidecar_refusal() {
        let (_db, state, seen) = seated(false).await;
        present(&state).await;
        let id = row(&state, "project", "acme", "s1").await;
        let nonce = take(&state, id, false).await.expect("take");

        let (status, body) =
            body_json(call_answer(&state, id, &nonce, "gone", serde_json::json!("yes")).await)
                .await;
        assert_eq!(status, 404);
        assert_eq!(body, serde_json::json!({"error": "no_prompt"}));

        let response = call_answer(&state, id, &nonce, "dialog-1", serde_json::json!("yes")).await;
        assert_eq!(response.status().as_u16(), 200);
        let (_, body) = body_json(response).await;
        assert_eq!(body, serde_json::json!({"answered": true}));

        let (status, body) =
            body_json(call_answer(&state, id, "wrong", "dialog-1", serde_json::json!("yes")).await)
                .await;
        assert_eq!(
            (status, &body["error"]),
            (403, &serde_json::json!("bad_nonce"))
        );

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 3, "begin and two answers: {calls:?}");
        let sent = calls[2].strip_prefix("answer ").expect("an answer call");
        let sent: serde_json::Value = serde_json::from_str(sent).expect("a JSON body");
        assert_eq!(
            sent,
            serde_json::json!({"session": "s1", "prompt": "dialog-1", "answer": "yes"})
        );
        assert!(!calls[2].contains(&nonce), "the nonce stays in the daemon");
    }
}
