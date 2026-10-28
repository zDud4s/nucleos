//! Releasing what an embedded session holds in the sidecar when it ends.
//!
//! The sidecar's pool holds a small number of sessions (three). An embedded session takes a slot at
//! attach time, and the shell's tab closing frees nothing there, so every end of an embedded session
//! has to ask the sidecar to `close` it too: the daemon closing it (`browser_close`, a policy
//! close), the shell reporting the tab gone (`POST .../sessions/{id}/closed`), and an attach whose
//! result nobody received. Every call is best effort: the sidecar may have restarted and forgotten
//! the session already, and that must never keep a row open or fail a close. A shell registration
//! that lapses also ends its embedded sessions: their rows are closed with `shell-lapsed`.

use sqlx::SqlitePool;

use super::{BrowserRuntime, close_row};
use crate::browser_client::{BrowserClient, BrowserError};

/// The reason a row carries when the shell reports its tab closed (by the person or by a crash).
pub const TAB_CLOSED: &str = "tab-closed";

/// The reason a row carries when its shell registration lapsed.
pub const SHELL_LAPSED: &str = "shell-lapsed";

/// A registration lapsed: the sidecar is told to forget the project, and the still-open rows of
/// its embedded sessions are closed here with reason `shell-lapsed`. Best effort per row.
pub(super) async fn close_lapsed_rows(pool: &SqlitePool, sessions: &[String]) {
    let now = chrono::Utc::now().to_rfc3339();
    for session_id in sessions {
        let result = sqlx::query(
            "UPDATE browser_sessions SET closed_at = ?, closed_reason = ? \
             WHERE sidecar_id = ? AND closed_at IS NULL",
        )
        .bind(&now)
        .bind(SHELL_LAPSED)
        .bind(session_id)
        .execute(pool)
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, session_id, "could not close the row of a lapsed embedded session");
        }
    }
}

/// Best effort: frees the sidecar's slot for `sidecar_id`. A session it no longer knows is fine.
pub(super) async fn release_sidecar_slot(client: &BrowserClient, sidecar_id: &str) {
    match client.close(sidecar_id).await {
        Ok(()) | Err(BrowserError::NoSuchSession(_)) => {}
        Err(error) => {
            tracing::warn!(%error, sidecar_id, "could not free the sidecar slot of an embedded session");
        }
    }
}

/// The shell reports a tab closed: forget it, close its still-open row with reason `tab-closed` and
/// free the sidecar's slot. A session the daemon closed first (row already closed, registry entry
/// gone) is a no-op beyond the registry.
pub(super) async fn tab_closed(pool: &SqlitePool, runtime: &BrowserRuntime, session_id: &str) {
    let known = runtime.embedded.is_embedded(session_id);
    runtime.embedded.session_closed(session_id);
    let open_row = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM browser_sessions WHERE sidecar_id = ? AND closed_at IS NULL \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await;
    let open_row = match open_row {
        Ok(row) => row,
        Err(error) => {
            tracing::warn!(%error, session_id, "could not look up the row of a closed embedded tab");
            None
        }
    };
    // Only a session this daemon knew as embedded (or whose row is still open) owns a slot; a stray
    // id from a stale shell must not make the sidecar close somebody else's session.
    if let Some(id) = open_row {
        let now = chrono::Utc::now().to_rfc3339();
        if let Err(error) = close_row(pool, id, TAB_CLOSED, &now).await {
            tracing::warn!(%error, session_id, "could not close the row of a closed embedded tab");
        }
    }
    if known || open_row.is_some() {
        release_sidecar_slot(&runtime.client, session_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::close_lapsed_rows;
    use crate::browser_client::Session;
    use crate::storage::TempDb;

    async fn insert_open_row(pool: &sqlx::SqlitePool, sidecar_id: &str) {
        sqlx::query(
            "INSERT INTO browser_sessions \
               (sidecar_id, project_id, profile_kind, profile_id, requested_url, final_url, \
                rule, mode, opened_at, closed_at) \
             VALUES (?, 'acme', 'project', 'acme', 'https://a.example.net/', '', \
                'rule', 'agent', '2026-01-01T00:00:00Z', NULL)",
        )
        .bind(sidecar_id)
        .execute(pool)
        .await
        .expect("insert");
    }

    async fn state_of(pool: &sqlx::SqlitePool, sidecar_id: &str) -> (Option<String>, Option<String>) {
        sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT closed_at, closed_reason FROM browser_sessions WHERE sidecar_id = ?",
        )
        .bind(sidecar_id)
        .fetch_one(pool)
        .await
        .expect("row")
    }

    #[tokio::test]
    async fn lapsed_sessions_have_their_open_rows_closed() {
        let db = TempDb::new().await;
        insert_open_row(&db.pool, "s1").await;
        insert_open_row(&db.pool, "s2").await;
        close_lapsed_rows(&db.pool, &["s1".to_string()]).await;
        let (closed_at, reason) = state_of(&db.pool, "s1").await;
        assert!(closed_at.is_some());
        assert_eq!(reason.as_deref(), Some("shell-lapsed"));
        assert_eq!(state_of(&db.pool, "s2").await, (None, None));
        // A second call must not touch the already closed row.
        close_lapsed_rows(&db.pool, &["s1".to_string()]).await;
        assert_eq!(state_of(&db.pool, "s1").await, (closed_at, reason));
        db.close().await;
    }

    fn session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            mode: "agent".to_string(),
            requested_url: "https://x.test/a".to_string(),
            final_url: "https://x.test/a".to_string(),
            title: String::new(),
            refusal: None,
            still_loading: false,
            status: 200,
        }
    }

    /// The ack stopped listening (its HTTP call was dropped) while the attach was still running: the
    /// session is forgotten and reported as discarded, for the caller to free in the sidecar.
    #[tokio::test]
    async fn a_session_nobody_received_is_reported_discarded_and_forgotten() {
        use crate::browser::embedded::{OpenEnd, Registry};
        let registry = Registry::new();
        registry.register("p", "http://127.0.0.1:7");
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let attach_gate = gate.clone();
        let opener = registry.clone();
        let open = tokio::spawn(async move {
            opener
                .open_ended("p", "https://x.test/a", |_, _, _| async move {
                    attach_gate.notified().await;
                    Ok(session("late"))
                })
                .await
        });
        let request = loop {
            if let Some(request) = registry.poll().pop() {
                break request;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        // The ack is dropped while the attach is blocked on the gate.
        let dropped = tokio::time::timeout(
            std::time::Duration::from_millis(60),
            registry.ack(&request.request_id, "n0nce"),
        )
        .await;
        assert!(dropped.is_err(), "the attach is still running");
        gate.notify_one();
        match open.await.unwrap() {
            OpenEnd::Discarded(id) => assert_eq!(id, "late"),
            _ => panic!("expected a discarded session"),
        }
        assert!(!registry.is_embedded("late"));
    }
}
