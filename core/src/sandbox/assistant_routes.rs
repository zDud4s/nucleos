//! The route table of `Scope::Assistant`, kept beside the sandbox door that hands the key out.

use axum::http::Method;

/// Mints the `Assistant` key of a conversation: the same stored secret as its `chat:` key, under
/// the `assistant:` prefix, so it is rotated together with it.
pub async fn mint_assistant_token(
    pool: &sqlx::SqlitePool,
    chat_id: &str,
) -> Result<String, sqlx::Error> {
    let key = crate::auth::mint_chat_token(pool, chat_id).await?;
    Ok(key.replacen("chat:", "assistant:", 1))
}

/// What an orchestrator turn's own key reaches (`Scope::Assistant`): the routes its MCP tools call,
/// and nothing that approves, lands, disengages the kill switch or edits configuration.
///
/// Written out in full rather than as `READ_ONLY_ROUTES` plus extras, so the corpus test in
/// `mcp_tools` and the walk in `auth::tests` read one list. The three tools that need the control
/// token — `approve_proposal`, `reject_proposal` and `set_kill` — are deliberately absent: their
/// routes answer 403 to this scope, and the owner approves from the window instead.
pub const ASSISTANT_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/projects"),
    (Method::GET, "/teams"),
    (Method::GET, "/jobs"),
    (Method::GET, "/jobs/{id}"),
    (Method::GET, "/runs/{id}"),
    (Method::GET, "/projects/{id}/ls"),
    (Method::GET, "/projects/{id}/cat"),
    (Method::GET, "/projects/{id}/grep"),
    (Method::GET, "/projects/{id}/diff"),
    (Method::GET, "/scoreboard"),
    (Method::GET, "/shadow-decisions"),
    (Method::GET, "/proposals"),
    (Method::GET, "/models/latest"),
    (Method::GET, "/autopilot/budget"),
    (Method::GET, "/autopilot/kill"),
    (Method::GET, "/email/queue"),
    (Method::GET, "/email/{id}"),
    (Method::GET, "/council/{id}"),
    (Method::GET, "/files"),
    (Method::POST, "/runs"),
    (Method::POST, "/jobs"),
    (Method::POST, "/runs/{id}/cancel"),
    (Method::POST, "/jobs/{id}/cancel"),
    (Method::POST, "/email/triage"),
    (Method::POST, "/web/search"),
    (Method::POST, "/web/read"),
    (Method::POST, "/team-files/read"),
    (Method::POST, "/loadout/tool-requests"),
    (Method::POST, "/context/read"),
    (Method::POST, "/knowledge"),
    (Method::POST, "/knowledge/recall"),
    (Method::POST, "/knowledge/findings"),
    (Method::POST, "/council"),
    (Method::POST, "/team-actions"),
    (Method::POST, "/team-reports"),
    (Method::POST, "/team-notes"),
    (Method::POST, "/team-recruits"),
    (Method::POST, "/assistant/chats/{chat_id}/relay"),
    (Method::POST, "/assistant/browser-agents"),
    // The conductor tools: start, read and abandon a tree, read the backlog, cancel a finish, and
    // the cost advice. None of them approves or lands anything.
    (Method::POST, "/trees"),
    (Method::GET, "/trees/{id}"),
    (Method::POST, "/trees/{id}/abandon"),
    (Method::GET, "/backlog"),
    (Method::POST, "/backlog/finish/cancel"),
    (Method::GET, "/cost/wait-advice"),
];
