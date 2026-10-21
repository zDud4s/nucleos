//! `POST /browser/embedded/sessions/{id}/owner`: the shell tells the daemon who drives an embedded
//! session's webview. `human` marks the session in the registry first (so the agent is refused from
//! it even if the sidecar call then fails) and only then asks the sidecar to detach its CDP
//! connection; `agent` asks the sidecar to reattach first, stores the chain of urls visited while
//! detached on the session's row, and clears the mark only once all of that worked, so every failure
//! leaves the session on the person's side. The agent reply carries `chain` and `row_id` so the shell
//! can ask the keep question through `POST /browser/keep`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sqlx::SqlitePool;

use super::BrowserRuntime;
use super::embedded::{pillar_off, sidecar_error};
use crate::browser_client::BrowserError;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    Human,
    Agent,
}

#[derive(Debug, Deserialize)]
pub struct OwnerBody {
    pub owner: Owner,
}

/// Why an owner change did not happen.
#[derive(Debug)]
pub(super) enum OwnerError {
    NotEmbedded,
    Sidecar(BrowserError),
    Db(sqlx::Error),
}

/// What handing a session back to the agent found: the urls visited while the person drove, and the
/// `browser_sessions` row they were stored on (if the session has an open one).
#[derive(Debug)]
pub(super) struct Reattached {
    pub chain: Vec<String>,
    pub row_id: Option<i64>,
}

type Gates = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;

/// One gate per session id with a call in flight; an entry lives only while a call holds or awaits it.
fn gates() -> &'static Gates {
    static GATES: OnceLock<Gates> = OnceLock::new();
    GATES.get_or_init(Gates::default)
}

/// Held for a whole owner change, so two calls on one session never interleave.
struct OwnerGate {
    id: String,
    gate: Arc<tokio::sync::Mutex<()>>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl OwnerGate {
    async fn enter(id: &str) -> Self {
        let gate = gates()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id.to_string())
            .or_default()
            .clone();
        let guard = gate.clone().lock_owned().await;
        Self {
            id: id.to_string(),
            gate,
            guard: Some(guard),
        }
    }
}

impl Drop for OwnerGate {
    fn drop(&mut self) {
        self.guard.take();
        let mut map = gates().lock().unwrap_or_else(PoisonError::into_inner);
        // The map and this struct hold one handle each; any more is a call still waiting its turn.
        if Arc::strong_count(&self.gate) == 2 {
            map.remove(&self.id);
        }
    }
}

/// The open `browser_sessions` row of a sidecar session, with whether it is a project session
/// (the only kind whose chain can be recorded).
async fn open_row(pool: &SqlitePool, id: &str) -> Result<Option<(i64, bool)>, OwnerError> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, profile_kind FROM browser_sessions \
         WHERE sidecar_id = ? AND closed_at IS NULL LIMIT 1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(OwnerError::Db)?;
    Ok(row.map(|(row_id, kind)| (row_id, kind == "project")))
}

/// Flips the owner of an embedded session; idempotent in both directions. `Some` only for the agent.
/// Calls on one session run one at a time, so the flag always ends as the last call to finish left it.
pub(super) async fn set_owner(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    id: &str,
    owner: Owner,
) -> Result<Option<Reattached>, OwnerError> {
    let _gate = OwnerGate::enter(id).await;
    if !runtime.embedded.is_embedded(id) {
        // The registry forgot the session (shell silent, daemon restarted) but the sidecar may still
        // hold it detached. Hand it back to the agent only when its row is open and a project
        // session; the registry has nothing to unmark. Anything else is a clear 404.
        if owner == Owner::Agent
            && let Some((row_id, true)) = open_row(pool, id).await?
        {
            let chain = runtime
                .client
                .embedded_reattach(id)
                .await
                .map_err(OwnerError::Sidecar)?;
            if !chain.is_empty() {
                crate::browser::record_chain(pool, row_id, &chain)
                    .await
                    .map_err(OwnerError::Db)?;
            }
            return Ok(Some(Reattached {
                chain,
                row_id: Some(row_id),
            }));
        }
        return Err(OwnerError::NotEmbedded);
    }
    match owner {
        Owner::Human => {
            // Fail closed: refuse the agent from this record before the sidecar is told.
            if !runtime.embedded.set_human(id, true) {
                return Err(OwnerError::NotEmbedded);
            }
            runtime
                .client
                .embedded_detach(id)
                .await
                .map_err(OwnerError::Sidecar)?;
            Ok(None)
        }
        Owner::Agent => {
            let mut chain = runtime
                .client
                .embedded_reattach(id)
                .await
                .map_err(OwnerError::Sidecar)?;
            // Contract E4: a row that is not an open project session cannot hold a chain, so the
            // reply carries none and the shell asks no keep question.
            let row_id = match open_row(pool, id).await? {
                Some((row_id, true)) => Some(row_id),
                _ => {
                    chain.clear();
                    None
                }
            };
            if let (Some(row_id), false) = (row_id, chain.is_empty()) {
                // Before the flag is cleared: a database error leaves the session on the person's side.
                crate::browser::record_chain(pool, row_id, &chain)
                    .await
                    .map_err(OwnerError::Db)?;
            }
            if runtime.embedded.set_human(id, false) {
                Ok(Some(Reattached { chain, row_id }))
            } else {
                Err(OwnerError::NotEmbedded)
            }
        }
    }
}

/// `POST /browser/embedded/sessions/{id}/owner`.
pub async fn post_owner(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(body): Json<OwnerBody>,
) -> Response {
    if !state.browser.enabled {
        return pillar_off();
    }
    match set_owner(&state.pool, &state.browser, &session_id, body.owner).await {
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Ok(Some(back)) => Json(serde_json::json!({
            "chain": back.chain,
            "row_id": back.row_id,
        }))
        .into_response(),
        Err(OwnerError::NotEmbedded) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "refusal": "no_such_session" })),
        )
            .into_response(),
        Err(OwnerError::Sidecar(error)) => sidecar_error(error),
        Err(OwnerError::Db(error)) => {
            tracing::error!(%error, "recording the chain visited while detached failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::browser_client::BrowserClient;
    use crate::storage::TempDb;

    type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

    async fn insert_row(pool: &SqlitePool) -> i64 {
        insert_row_with(pool, "s1", "project", false).await
    }

    async fn insert_row_with(pool: &SqlitePool, sidecar_id: &str, kind: &str, closed: bool) -> i64 {
        sqlx::query(
            "INSERT INTO browser_sessions \
               (sidecar_id, project_id, profile_kind, profile_id, requested_url, final_url, \
                rule, mode, opened_at, closed_at) \
             VALUES (?, 'acme', ?, 'acme', 'https://a.example.net/', '', \
                'rule', 'agent', '2026-01-01T00:00:00Z', ?)",
        )
        .bind(sidecar_id)
        .bind(kind)
        .bind(closed.then_some("2026-01-01T00:00:05Z"))
        .execute(pool)
        .await
        .expect("insert")
        .last_insert_rowid()
    }

    /// A sidecar recording every embedded verb; `reattach` answers 502 when `fail_reattach` is set and
    /// otherwise returns `chain`.
    async fn stub(fail_reattach: bool, chain: Vec<&'static str>) -> (BrowserRuntime, Seen) {
        stub_slow(fail_reattach, chain, 0).await
    }

    /// Like `stub`, with `reattach` answering `delay_ms` late.
    async fn stub_slow(
        fail_reattach: bool,
        chain: Vec<&'static str>,
        delay_ms: u64,
    ) -> (BrowserRuntime, Seen) {
        use axum::routing::post;

        let seen: Seen = Arc::default();
        let recorder = seen.clone();
        let chain = serde_json::json!(chain);
        let app = axum::Router::new().route(
            "/embedded/{verb}",
            post(
                move |Path(verb): Path<String>, Json(body): Json<serde_json::Value>| {
                    let recorder = recorder.clone();
                    let chain = chain.clone();
                    async move {
                        let failing = fail_reattach && verb == "reattach";
                        recorder
                            .lock()
                            .unwrap()
                            .push(serde_json::json!({"verb": verb, "body": body}));
                        if failing {
                            return (
                                StatusCode::BAD_GATEWAY,
                                Json(serde_json::json!({ "error": "boom" })),
                            )
                                .into_response();
                        }
                        if verb == "reattach" {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            return Json(serde_json::json!({ "chain": chain })).into_response();
                        }
                        Json(serde_json::json!({})).into_response()
                    }
                },
            ),
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
            embedded: Default::default(),
            embedded_forced: Default::default(),
        };
        runtime.embedded.adopt_for_test("s1", "p");
        (runtime, seen)
    }

    fn verbs(seen: &Seen) -> Vec<(String, String)> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|call| {
                (
                    call["verb"].as_str().unwrap_or_default().to_string(),
                    call["body"]["session_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn the_body_parses_lowercase_owners_only() {
        let human: OwnerBody = serde_json::from_str(r#"{"owner":"human"}"#).unwrap();
        assert_eq!(human.owner, Owner::Human);
        let agent: OwnerBody = serde_json::from_str(r#"{"owner":"agent"}"#).unwrap();
        assert_eq!(agent.owner, Owner::Agent);
        assert!(serde_json::from_str::<OwnerBody>(r#"{"owner":"Human"}"#).is_err());
    }

    #[test]
    fn the_pillar_off_refusal_is_a_409() {
        assert_eq!(pillar_off().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn human_marks_the_session_and_detaches_the_sidecar() {
        let db = TempDb::new().await;
        let (runtime, seen) = stub(false, vec![]).await;
        let first = set_owner(&db.pool, &runtime, "s1", Owner::Human).await;
        assert!(matches!(first, Ok(None)), "{first:?}");
        set_owner(&db.pool, &runtime, "s1", Owner::Human)
            .await
            .expect("idempotent");
        assert!(runtime.embedded.is_human("s1"));
        assert_eq!(
            verbs(&seen),
            vec![
                ("detach".to_string(), "s1".to_string()),
                ("detach".to_string(), "s1".to_string())
            ]
        );
        db.close().await;
    }

    #[tokio::test]
    async fn agent_reattaches_then_clears_the_flag() {
        let db = TempDb::new().await;
        let (runtime, seen) = stub(false, vec![]).await;
        runtime.embedded.set_human("s1", true);
        set_owner(&db.pool, &runtime, "s1", Owner::Agent)
            .await
            .expect("ok");
        assert!(!runtime.embedded.is_human("s1"));
        assert_eq!(
            verbs(&seen),
            vec![("reattach".to_string(), "s1".to_string())]
        );
        db.close().await;
    }

    #[tokio::test]
    async fn agent_reattach_records_the_chain_on_the_row() {
        let db = TempDb::new().await;
        let id = insert_row(&db.pool).await;
        let urls = ["https://login.example.net/", "https://app.example.net/"];
        let (runtime, _seen) = stub(false, urls.to_vec()).await;
        runtime.embedded.set_human("s1", true);
        let back = set_owner(&db.pool, &runtime, "s1", Owner::Agent)
            .await
            .expect("ok")
            .expect("agent reply");
        assert_eq!(back.chain, urls.map(String::from).to_vec());
        assert_eq!(back.row_id, Some(id));
        assert!(!runtime.embedded.is_human("s1"));

        let row = crate::browser::session_row(&db.pool, id)
            .await
            .expect("query")
            .expect("row");
        let stored = row.chain.expect("chain recorded");
        for url in urls {
            assert!(format!("{stored:?}").contains(url), "{stored:?}");
        }
        assert!(row.chain_decided_at.is_none());

        // The keep question can then be answered from the stored chain.
        let taken = crate::browser::take_chain(&db.pool, id, "2026-01-01T00:00:01Z")
            .await
            .expect("take")
            .expect("chain to ask about");
        let granted =
            crate::browser::grant(&db.pool, "acme", &taken, false, "2026-01-01T00:00:01Z")
                .await
                .expect("grant");
        assert!(!granted.is_empty(), "{granted:?}");
        db.close().await;
    }

    #[tokio::test]
    async fn an_empty_chain_records_nothing() {
        let db = TempDb::new().await;
        let id = insert_row(&db.pool).await;
        let (runtime, _seen) = stub(false, vec![]).await;
        let back = set_owner(&db.pool, &runtime, "s1", Owner::Agent)
            .await
            .expect("ok")
            .expect("agent reply");
        assert!(back.chain.is_empty());
        assert_eq!(back.row_id, Some(id));
        let row = crate::browser::session_row(&db.pool, id)
            .await
            .expect("query")
            .expect("row");
        assert!(row.chain.is_none());
        db.close().await;
    }

    #[tokio::test]
    async fn a_failed_reattach_keeps_the_flag_set() {
        let db = TempDb::new().await;
        let id = insert_row(&db.pool).await;
        let (runtime, _seen) = stub(true, vec!["https://a.example.net/"]).await;
        runtime.embedded.set_human("s1", true);
        let outcome = set_owner(&db.pool, &runtime, "s1", Owner::Agent).await;
        assert!(
            matches!(outcome, Err(OwnerError::Sidecar(_))),
            "{outcome:?}"
        );
        assert!(runtime.embedded.is_human("s1"));
        let row = crate::browser::session_row(&db.pool, id)
            .await
            .expect("query")
            .expect("row");
        assert!(row.chain.is_none());
        db.close().await;
    }

    #[tokio::test]
    async fn a_slow_agent_reattach_cannot_overwrite_a_later_human_call() {
        let db = TempDb::new().await;
        let (runtime, seen) = stub_slow(false, vec![], 300).await;
        let runtime = Arc::new(runtime);
        // An id of its own: the gate map is shared with the tests running beside this one.
        runtime.embedded.adopt_for_test("sg", "p");
        runtime.embedded.set_human("sg", true);
        let agent = {
            let (pool, runtime) = (db.pool.clone(), runtime.clone());
            tokio::spawn(async move { set_owner(&pool, &runtime, "sg", Owner::Agent).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        set_owner(&db.pool, &runtime, "sg", Owner::Human)
            .await
            .expect("human");
        agent.await.unwrap().expect("agent");
        // The person's call came last, so the session ends on the person's side.
        assert!(runtime.embedded.is_human("sg"));
        assert_eq!(
            verbs(&seen),
            vec![
                ("reattach".to_string(), "sg".to_string()),
                ("detach".to_string(), "sg".to_string())
            ]
        );
        assert!(
            !gates().lock().unwrap().contains_key("sg"),
            "the per-session gate is cleaned up"
        );
        db.close().await;
    }

    #[tokio::test]
    async fn an_agent_call_on_a_forgotten_session_reattaches_through_its_open_project_row() {
        let db = TempDb::new().await;
        let id = insert_row_with(&db.pool, "s9", "project", false).await;
        let (runtime, seen) = stub(false, vec!["https://x.example.net/"]).await;
        let back = set_owner(&db.pool, &runtime, "s9", Owner::Agent)
            .await
            .expect("ok")
            .expect("agent reply");
        assert_eq!(back.chain, vec!["https://x.example.net/".to_string()]);
        assert_eq!(back.row_id, Some(id));
        assert_eq!(
            verbs(&seen),
            vec![("reattach".to_string(), "s9".to_string())]
        );
        assert!(!runtime.embedded.is_embedded("s9"));
        db.close().await;
    }

    #[tokio::test]
    async fn a_forgotten_session_without_an_open_project_row_is_a_404() {
        let db = TempDb::new().await;
        insert_row_with(&db.pool, "closed", "project", true).await;
        insert_row_with(&db.pool, "eph", "ephemeral", false).await;
        let (runtime, seen) = stub(false, vec![]).await;
        for ghost in ["closed", "eph", "nothing"] {
            let outcome = set_owner(&db.pool, &runtime, ghost, Owner::Agent).await;
            assert!(
                matches!(outcome, Err(OwnerError::NotEmbedded)),
                "{ghost}: {outcome:?}"
            );
        }
        let human = set_owner(&db.pool, &runtime, "closed", Owner::Human).await;
        assert!(matches!(human, Err(OwnerError::NotEmbedded)), "{human:?}");
        assert!(verbs(&seen).is_empty());
        db.close().await;
    }

    #[tokio::test]
    async fn a_row_that_is_not_an_open_project_session_replies_an_empty_chain() {
        let db = TempDb::new().await;
        let (runtime, _seen) = stub(false, vec!["https://a.example.net/"]).await;
        for (kind, closed) in [("ephemeral", false), ("project", true)] {
            sqlx::query("DELETE FROM browser_sessions")
                .execute(&db.pool)
                .await
                .unwrap();
            let id = insert_row_with(&db.pool, "s1", kind, closed).await;
            runtime.embedded.set_human("s1", true);
            let back = set_owner(&db.pool, &runtime, "s1", Owner::Agent)
                .await
                .expect("ok")
                .expect("agent reply");
            assert!(back.chain.is_empty(), "{kind}/{closed}: {:?}", back.chain);
            assert_eq!(back.row_id, None);
            assert!(!runtime.embedded.is_human("s1"));
            let unset: i64 =
                sqlx::query_scalar("SELECT chain IS NULL FROM browser_sessions WHERE id = ?")
                    .bind(id)
                    .fetch_one(&db.pool)
                    .await
                    .unwrap();
            assert_eq!(unset, 1);
        }
        db.close().await;
    }

    #[tokio::test]
    async fn an_unknown_session_is_refused_without_calling_the_sidecar() {
        let db = TempDb::new().await;
        let (runtime, seen) = stub(false, vec![]).await;
        for owner in [Owner::Human, Owner::Agent] {
            let outcome = set_owner(&db.pool, &runtime, "ghost", owner).await;
            assert!(
                matches!(outcome, Err(OwnerError::NotEmbedded)),
                "{outcome:?}"
            );
        }
        assert!(verbs(&seen).is_empty());
        assert!(!runtime.embedded.is_human("ghost"));
        db.close().await;
    }
}
