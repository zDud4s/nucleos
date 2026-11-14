//! The chat-turn doors: the checks `require_token` makes, against the database, before a chat
//! turn's run key may reach the browser routes or start a linked browser agent. The fixed scope
//! tables in the parent know nothing of them.

use super::Scope;

/// The chat turn a browser request was admitted for. `require_token` inserts it as a request
/// extension, and `post_open` reads it to record who owns the session it opens.
#[derive(Debug, Clone)]
pub struct ChatBrowser {
    pub run_id: i64,
    pub chat_id: String,
}

/// The session routes a chat turn's run key may drive (owner decision of 2026-10-15: a chat's key
/// drives the browser). POST only. The fixed scope tables stay pure and know nothing of this: the
/// door is decided in `require_token` by `chat_browser_admits`, which reads the database, because
/// whether a turn may touch a session depends on who owns that session.
pub(super) const CHAT_BROWSER_ROUTES: &[&str] = &[
    "/browser/open",
    "/browser/snapshot",
    "/browser/act",
    "/browser/look",
    "/browser/screenshot",
    "/browser/handoff",
    "/browser/close",
];

/// The most a browser request body may be to be read ahead of the handler.
pub(super) const CHAT_BROWSER_BODY_LIMIT: usize = 1024 * 1024;

#[derive(serde::Deserialize)]
struct SessionRef {
    session_id: i64,
}

/// Whether a run key belongs to a chat turn that may use this browser route. `/browser/open` is
/// open to any chat turn; every other route needs a session its own chat (or its own run) owns.
/// None for every other scope and for every error.
pub(super) async fn chat_browser_admits(
    pool: &sqlx::SqlitePool,
    scope: &Scope,
    route: &str,
    body: &[u8],
) -> Option<ChatBrowser> {
    let Scope::Run(id) = scope else {
        return None;
    };
    let chat_id: String =
        sqlx::query_scalar::<_, Option<String>>("SELECT chat_id FROM runs WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten()
            .filter(|chat| !chat.is_empty())?;
    let turn = ChatBrowser {
        run_id: *id,
        chat_id,
    };
    if route == "/browser/open" {
        return Some(turn);
    }
    let session: SessionRef = serde_json::from_slice(body).ok()?;
    let row = match crate::browser::session_row(pool, session.session_id).await {
        Ok(Some(row)) => row,
        _ => return None,
    };
    match crate::browser::panel::owner_of(pool, &row).await {
        crate::browser::panel::Owner::Chat(chat) if chat == turn.chat_id => Some(turn),
        crate::browser::panel::Owner::Run(run) if run == *id => Some(turn),
        _ => None,
    }
}

/// The one assistant route a chat turn's key may reach (plan browser-agent-linked, D2): starting a
/// linked browser agent. POST only. Like the chat-browser door, it is decided in `require_token`
/// and the fixed scope tables know nothing of it; the handler still re-derives the sender from
/// `RUN_ID_HEADER` and `relay::admit` still decides whether the task may travel.
pub(super) const CHAT_BROWSER_AGENT_ROUTE: &str = "/assistant/browser-agents";

/// Whether this request may start a browser agent through the chat-turn door: the bearer is a
/// conversation's key (`chat:` family, never a run's own `runs.token`), and it resolved to a run
/// that belongs to a conversation and is running now — between turns a `chat:` key can resolve to
/// a background task's launcher, which is not a chat turn. False for every other scope and for
/// every error.
///
/// The `chat:` key is NOT out of the turn's reach: `assistant::chat_key` hands it to a rooted
/// turn's CLI as `NUCLEOS_DAEMON_TOKEN`, so a turn with a shell can read it and call this route directly,
/// skipping the MCP tool's classification. `post_browser_agent` therefore refuses a sender that
/// has read untrusted text itself; this door only decides who may knock.
pub(super) async fn chat_browser_agent_admits(
    pool: &sqlx::SqlitePool,
    presented: &str,
    scope: &Scope,
) -> bool {
    if !presented.starts_with("chat:") {
        return false;
    }
    let Scope::Run(id) = scope else {
        return false;
    };
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT chat_id FROM runs WHERE id = ? AND status = 'running'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten()
    .is_some_and(|chat| !chat.is_empty())
}
