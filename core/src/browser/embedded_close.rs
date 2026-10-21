//! Releasing what an embedded session holds in the sidecar when it ends.
//!
//! The sidecar's pool holds a small number of sessions (three). An embedded session takes a slot at
//! attach time, and the shell's tab closing frees nothing there, so every end of an embedded session
//! has to ask the sidecar to `close` it too: the daemon closing it (`browser_close`, a policy
//! close), the shell reporting the tab gone (`POST .../sessions/{id}/closed`), and an attach whose
//! result nobody received. Every call is best effort: the sidecar may have restarted and forgotten
//! the session already, and that must never keep a row open or fail a close.

use sqlx::SqlitePool;

use super::{BrowserRuntime, close_row};
use crate::browser_client::{BrowserClient, BrowserError};

/// The reason a row carries when the shell reports its tab closed (by the person or by a crash).
pub const TAB_CLOSED: &str = "tab-closed";

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
    use crate::browser_client::Session;

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
