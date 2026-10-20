//! Routing `browser_open` / `browser_close` to the shell's embedded browser (phase 2).
//!
//! Kept out of `browser.rs` for its size. The headless path stays the default: this only takes an
//! open when the platform supports embedding AND the shell holds a live registration for the
//! project, and every failure here returns `None` so the caller falls through to the headless path
//! unchanged. A session never migrates between the two.

use std::sync::atomic::Ordering;

use super::*;

impl BrowserRuntime {
    /// Whether this build can host an embedded browser: Windows (WebView2), or a test that forced it.
    pub fn embedded_supported(&self) -> bool {
        cfg!(windows) || self.embedded_forced.load(Ordering::Relaxed)
    }

    /// Test override for [`Self::embedded_supported`], so any OS can exercise the routing.
    #[cfg(any(test, feature = "testkit"))]
    pub fn force_embedded(&self, on: bool) {
        self.embedded_forced.store(on, Ordering::Relaxed);
    }
}

/// Try to open `ask.url` in the shell's embedded browser. `Some(row)` when the shell attached
/// within the registry's bound; `None` when embedding does not apply or did not happen (no
/// registration, timeout, redirect off the list, row update failed), and the caller then opens
/// headless on the same row. `placement` is the project's real one, the same value `prepare` used.
pub(super) async fn try_open(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    row_id: i64,
    ask: Ask<'_>,
    sites: &Allowed,
    placement: &Placement,
) -> Option<SessionRow> {
    if !runtime.embedded_supported() || !runtime.embedded.has_live(ask.project_id) {
        return None;
    }
    let session = runtime
        .embedded
        .open(&runtime.client, ask.project_id, ask.url, placement)
        .await?;
    // The same redirect trap as the headless path: the fence refused a document that landed off the
    // list, so the shell's tab holds nothing. An embedded tab cannot be re-profiled, so it is closed
    // and the caller reopens through the headless path, which downgrades to a throwaway.
    let after = browser_policy::decide(
        ask.url,
        &session.final_url,
        ask.surface,
        ask.requester,
        &sites.read,
    );
    if matches!(after.outcome, Outcome::Open(Profile::Ephemeral)) {
        runtime.embedded.queue_close(&session.id);
        return None;
    }
    match finish_session(pool, row_id, placement, &session).await {
        Ok(row) => Some(row),
        Err(error) => {
            tracing::warn!(%error, "could not record an embedded browser session");
            runtime.embedded.queue_close(&session.id);
            None
        }
    }
}

/// Close an embedded session: queue the `close` request for the shell. `false` when the session is
/// not embedded, and the caller closes it through the sidecar as before.
pub(super) fn queue_close(runtime: &BrowserRuntime, sidecar_id: &str) -> bool {
    runtime.embedded.is_embedded(sidecar_id) && runtime.embedded.queue_close(sidecar_id)
}
