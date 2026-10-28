//! The MCP panel of a conversation (slash spec 2026-10-22 §8.3): the `McpCardData` the panel, the
//! `/mcp` card and the toggle routes all answer, and the registry routes beside them.
//!
//! - `GET /assistant/chats/{id}/mcp` - the card data.
//! - `PUT|DELETE /assistant/chats/{id}/mcp/{name}` - switch one registry server on or off here.
//! - `GET /assistant/mcp/servers`, `DELETE /assistant/mcp/servers/{name}` - the registry itself.
//! - `POST /assistant/mcp/import` - copy servers from the owner's Claude/Codex configs into it.
//!
//! The `/mcp` built-in (`slash`) answers the same data as cards, through `builtins::run`.
//!
//! **Env values never leave this file.** Every server is serialized through
//! `McpServer::view`, which carries env NAMES only; no body, log line or error detail here
//! quotes a value.

use super::builtins::{card, internal, summary};
use super::command::{Refusal, refuse, slash_refusal};
use crate::assistant::slash::capabilities::{
    Capability, ChatEngine, Support, engine_of, require, support,
};
use crate::assistant::slash::parse::Parsed;
use crate::assistant::slash::{CardKind, Outcome, SlashError};
use crate::chats::mcp_import::{self, ImportPaths, ImportReport};
use crate::chats::mcp_servers::{
    self, CODEX_CAN_DISABLE_USER_SERVERS, FIXED_SERVER, McpToggleError,
};
use crate::execution::runner::ToolPolicy;
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use serde_json::{Value, json};

/// The argument syntax of `/mcp`, shown on an invalid argument.
pub(super) const USAGE: &str = "/mcp [on <name> | off <name> | import]";

const ENGINE_NOTE: &str = "not supported on this engine in v1";
const LEGACY_HINT: &str = "this chat used Claude's servers; use Import";
const CODEX_NOTE: &str =
    "still loaded by Codex from ~/.codex/config.toml; Codex cannot be told to skip it";

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/assistant/chats/{chat_id}/mcp", get(get_chat_mcp))
        .route(
            "/assistant/chats/{chat_id}/mcp/{name}",
            put(put_chat_mcp).delete(delete_chat_mcp),
        )
        .route("/assistant/mcp/servers", get(get_servers))
        .route("/assistant/mcp/servers/{name}", delete(delete_server))
        .route("/assistant/mcp/import", post(post_import))
}

/// Why a toggle or an import said no. Both the panel routes and `/mcp` answer `off nucleos` as
/// 400 `cannot_disable_nucleos`; an unknown name is `invalid_argument` + `known` on the routes,
/// while the command route (through `into_slash`) words it as `invalid_argument` with the usage.
#[derive(Debug)]
pub(super) enum McpRefusal {
    Slash(SlashError),
    CannotDisableNucleos,
    Unknown { name: String, known: Vec<String> },
    ChatNotFound,
}

impl From<SlashError> for McpRefusal {
    fn from(error: SlashError) -> Self {
        Self::Slash(error)
    }
}

fn unknown_detail(name: &str, known: &[String]) -> String {
    if known.is_empty() {
        format!("no MCP server named {name}; the registry is empty")
    } else {
        format!("no MCP server named {name}; known: {}", known.join(", "))
    }
}

impl McpRefusal {
    /// The panel routes' answer.
    pub(super) fn into_refusal(self) -> Refusal {
        match self {
            Self::Slash(error) => slash_refusal(error),
            Self::CannotDisableNucleos => slash_refusal(SlashError::CannotDisableNucleos),
            Self::Unknown { name, known } => refuse(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                unknown_detail(&name, &known),
                json!({ "known": known }),
            ),
            Self::ChatNotFound => chat_not_found(),
        }
    }

    /// The `/mcp` built-in's answer; the command route turns it into a status itself.
    pub(super) fn into_slash(self) -> SlashError {
        let invalid = |detail: String| SlashError::InvalidArgument {
            usage: USAGE.to_string(),
            detail,
        };
        match self {
            Self::Slash(error) => error,
            Self::CannotDisableNucleos => SlashError::CannotDisableNucleos,
            Self::Unknown { name, known } => invalid(unknown_detail(&name, &known)),
            Self::ChatNotFound => internal("the conversation is gone"),
        }
    }
}

fn chat_not_found() -> Refusal {
    refuse(
        StatusCode::NOT_FOUND,
        "chat_not_found",
        "no conversation with that id",
        Value::Null,
    )
}

/// The chat's directory (`Ok(None)` when it has none), or `ChatNotFound`.
async fn opened_in(state: &AppState, chat_id: &str) -> Result<Option<String>, McpRefusal> {
    crate::chats::opened_in(&state.pool, chat_id)
        .await
        .map_err(|error| McpRefusal::Slash(internal(error)))?
        .ok_or(McpRefusal::ChatNotFound)
}

/// The policy the person's own next message here would get: the same question
/// `read_chat_project` asks, of the same function the turn path asks.
async fn policy_for(cwd: Option<&str>) -> Result<ToolPolicy, SlashError> {
    let wired = match cwd {
        None => false,
        Some(cwd) => {
            let dir = std::path::PathBuf::from(cwd);
            tokio::task::spawn_blocking(move || {
                crate::governance::autopilot::classifier_hook_is_wired(&dir)
            })
            .await
            .map_err(internal)?
        }
    };
    Ok(crate::assistant::tool_policy_for(
        cwd,
        crate::assistant::Origin::Shell,
        wired,
        crate::relay::Handover::Typed,
    ))
}

fn engine_for(chat: &crate::chats::ChatSummary) -> ChatEngine {
    engine_of(
        &crate::config::models_config_now(),
        crate::chats::Brain::from_wire(&chat.brain),
        chat.model.as_deref(),
    )
}

/// `McpCardData`: what the panel and the `mcp` card show of this conversation.
pub(super) async fn card_data(state: &AppState, chat_id: &str) -> Result<Value, SlashError> {
    let chat = summary(state, chat_id).await?;
    let engine = engine_for(&chat);
    let supported = support(engine, Capability::RegistryMcp) != Support::No;
    let cwd = crate::chats::opened_in(&state.pool, chat_id)
        .await
        .map_err(internal)?
        .flatten();
    let policy = policy_for(cwd.as_deref()).await?;
    let policy_name = format!("{policy:?}").to_lowercase();

    let registry = mcp_servers::list(&state.pool).await.map_err(internal)?;
    let enabled = mcp_servers::enabled_for_chat(&state.pool, chat_id)
        .await
        .map_err(internal)?;
    let active = mcp_servers::active_names(policy, &enabled);
    let legacy = mcp_servers::legacy_ambient_mcp(&state.pool, chat_id).await;

    let servers: Vec<Value> = registry
        .iter()
        .map(|server| {
            let is_enabled = enabled.iter().any(|e| e.name == server.name);
            let is_active = supported && active.contains(&server.name);
            let inactive_reason = (is_enabled && !is_active && supported)
                .then(|| format!("mode {policy_name} only allows NucleOS tools"));
            let note = (!CODEX_CAN_DISABLE_USER_SERVERS
                && engine == ChatEngine::Codex
                && server.imported_from.as_deref() == Some(mcp_import::SOURCE_CODEX)
                && !is_enabled)
                .then_some(CODEX_NOTE);
            let mut row = serde_json::to_value(server.view()).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut row {
                map.insert("enabled".into(), json!(is_enabled));
                map.insert("active".into(), json!(is_active));
                map.insert("inactive_reason".into(), json!(inactive_reason));
                map.insert("note".into(), json!(note));
            }
            row
        })
        .collect();

    Ok(json!({
        "engine": engine.as_str(),
        "supported": supported,
        "engine_note": (!supported).then_some(ENGINE_NOTE),
        "policy": policy_name,
        "fixed": { "name": FIXED_SERVER, "active": supported },
        "servers": servers,
        "hint": (registry.is_empty() && legacy).then_some(LEGACY_HINT),
    }))
}

/// Switches `name` on or off for `chat_id` and answers the updated card data.
///
/// Check order of the wire format: engine, nucleos, busy, store. A toggle that changed something
/// evicts the live session, so the next turn respawns with the new server set.
pub(super) async fn toggle(
    state: &AppState,
    chat_id: &str,
    name: &str,
    on: bool,
) -> Result<Value, McpRefusal> {
    let chat = summary(state, chat_id).await?;
    require(
        engine_for(&chat),
        Capability::RegistryMcp,
        "switch MCP servers",
    )?;
    if !on && name.eq_ignore_ascii_case(FIXED_SERVER) {
        return Err(McpRefusal::CannotDisableNucleos);
    }
    if crate::assistant::is_busy(chat_id) {
        return Err(SlashError::Busy.into());
    }
    let was_on = mcp_servers::enabled_for_chat(&state.pool, chat_id)
        .await
        .map_err(internal)?
        .iter()
        .any(|s| s.name == name);
    match mcp_servers::set_enabled(&state.pool, chat_id, name, on).await {
        Ok(()) => {}
        Err(McpToggleError::CannotDisableNucleos) => return Err(McpRefusal::CannotDisableNucleos),
        Err(McpToggleError::UnknownServer { known }) => {
            return Err(McpRefusal::Unknown {
                name: name.to_string(),
                known,
            });
        }
        Err(McpToggleError::ChatNotFound) => return Err(McpRefusal::ChatNotFound),
        Err(McpToggleError::Db(error)) => return Err(internal(error).into()),
    }
    // `on nucleos` is a no-op, and so is switching a server to the state it already has.
    if !name.eq_ignore_ascii_case(FIXED_SERVER) && was_on != on {
        crate::assistant::evict_live(chat_id);
    }
    Ok(card_data(state, chat_id).await?)
}

/// Imports the owner's servers into the registry. With a chat, its directory adds
/// `<cwd>/.mcp.json` and `projects[<cwd>]` of `~/.claude.json`. Never enables anything.
pub(super) async fn import(
    state: &AppState,
    chat_id: Option<&str>,
) -> Result<ImportReport, McpRefusal> {
    let cwd = match chat_id {
        Some(chat_id) => opened_in(state, chat_id).await?,
        None => None,
    };
    let cwd = cwd.as_deref().map(std::path::Path::new);
    let home = super::command::home();
    let paths = ImportPaths::discover(home.as_deref(), cwd);
    mcp_import::import(&state.pool, &paths, cwd)
        .await
        .map_err(|error| internal(error).into())
}

/// `/mcp [on <name> | off <name> | import]`: the card, a toggle, or an import.
pub(super) async fn slash(
    state: &AppState,
    chat_id: &str,
    parsed: &Parsed,
) -> Result<Outcome, SlashError> {
    let invalid = |detail: String| SlashError::InvalidArgument {
        usage: USAGE.to_string(),
        detail,
    };
    let toggles = |verb: &str| verb.eq_ignore_ascii_case("on") || verb.eq_ignore_ascii_case("off");
    match parsed.argv.as_slice() {
        [] => Ok(card(CardKind::Mcp, card_data(state, chat_id).await?)),
        [verb, name] if toggles(verb) => {
            let on = verb.eq_ignore_ascii_case("on");
            let data = toggle(state, chat_id, name, on)
                .await
                .map_err(McpRefusal::into_slash)?;
            Ok(card(CardKind::Mcp, data))
        }
        [verb] if verb.eq_ignore_ascii_case("import") => {
            let report = import(state, Some(chat_id))
                .await
                .map_err(McpRefusal::into_slash)?;
            let data = serde_json::to_value(report).map_err(internal)?;
            Ok(card(CardKind::McpImport, data))
        }
        [verb] if toggles(verb) => Err(invalid(format!(
            "usage: {USAGE} (/mcp {verb} takes one server name)"
        ))),
        _ => Err(invalid(format!("usage: {USAGE}"))),
    }
}

async fn get_chat_mcp(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    opened_in(&state, &chat_id)
        .await
        .map_err(McpRefusal::into_refusal)?;
    card_data(&state, &chat_id)
        .await
        .map(Json)
        .map_err(slash_refusal)
}

async fn switch(
    state: &AppState,
    chat_id: &str,
    name: &str,
    on: bool,
) -> Result<Json<Value>, Refusal> {
    opened_in(state, chat_id)
        .await
        .map_err(McpRefusal::into_refusal)?;
    toggle(state, chat_id, name, on)
        .await
        .map(Json)
        .map_err(McpRefusal::into_refusal)
}

async fn put_chat_mcp(
    State(state): State<AppState>,
    Path((chat_id, name)): Path<(String, String)>,
) -> Result<Json<Value>, Refusal> {
    switch(&state, &chat_id, &name, true).await
}

async fn delete_chat_mcp(
    State(state): State<AppState>,
    Path((chat_id, name)): Path<(String, String)>,
) -> Result<Json<Value>, Refusal> {
    switch(&state, &chat_id, &name, false).await
}

async fn get_servers(State(state): State<AppState>) -> Result<Json<Value>, Refusal> {
    let servers = mcp_servers::list(&state.pool)
        .await
        .map_err(|error| slash_refusal(internal(error)))?;
    let views: Vec<_> = servers.iter().map(|s| s.view()).collect();
    Ok(Json(json!({ "servers": views })))
}

async fn delete_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, Refusal> {
    let removed = mcp_servers::delete(&state.pool, &name)
        .await
        .map_err(|error| slash_refusal(internal(error)))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(refuse(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no MCP server named {name}"),
            Value::Null,
        ))
    }
}

/// The optional body of `POST /assistant/mcp/import`.
#[derive(serde::Deserialize, Default)]
struct ImportIn {
    #[serde(default)]
    chat_id: Option<String>,
}

async fn post_import(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<ImportReport>, Refusal> {
    // An empty body is the plain import; anything else must be the `{"chat_id"}` shape.
    let input: ImportIn = if body.iter().all(u8::is_ascii_whitespace) {
        ImportIn::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| {
            refuse(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                "the body is empty or {\"chat_id\": \"<id>\"}",
                Value::Null,
            )
        })?
    };
    import(&state, input.chat_id.as_deref())
        .await
        .map(Json)
        .map_err(McpRefusal::into_refusal)
}

#[cfg(test)]
mod tests {
    use crate::chats::mcp_servers::{self, NewMcpServer};
    use crate::http::build_router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    const SECRET: &str = "s3cret-value";

    async fn state() -> AppState {
        crate::http::tests::test_state().await
    }

    async fn request(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn seed_chat(state: &AppState, id: &str, brain: &str, cwd: Option<&std::path::Path>) {
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at, cwd) VALUES (?, ?, '2026-08-20T10:00:00Z', ?)")
            .bind(id)
            .bind(brain)
            .bind(cwd.map(|p| p.to_string_lossy().to_string()))
            .execute(&state.pool)
            .await
            .unwrap();
    }

    async fn seed_server(state: &AppState, name: &str) {
        let server = NewMcpServer {
            name: name.to_string(),
            command: format!("{name}-mcp"),
            args: vec![],
            env: BTreeMap::from([("GH_TOKEN".to_string(), SECRET.to_string())]),
            imported_from: Some("claude-user".to_string()),
        };
        assert!(
            mcp_servers::insert_if_absent(&state.pool, &server)
                .await
                .unwrap()
        );
    }

    /// Asserts a body carries no env value, without printing the body.
    fn assert_no_secret(body: &Value) {
        assert!(
            !body.to_string().contains(SECRET),
            "an env value leaked into a response body"
        );
    }

    #[tokio::test]
    async fn mcp_get_lists_registry_with_env_names_only() {
        let state = state().await;
        seed_chat(&state, "m-get", "cloud", None).await;
        seed_server(&state, "gh").await;

        let (status, body) = request(&state, "GET", "/assistant/chats/m-get/mcp", None).await;

        assert_eq!(status, StatusCode::OK);
        assert_no_secret(&body);
        // A cloud chat answers on Claude or Codex, and both support the registry.
        assert!(body["engine"] == "claude" || body["engine"] == "codex");
        assert_eq!(body["supported"], true);
        assert_eq!(body["fixed"]["name"], "nucleos");
        assert_eq!(body["servers"][0]["name"], "gh");
        assert_eq!(body["servers"][0]["env_names"], json!(["GH_TOKEN"]));
        assert_eq!(body["servers"][0]["enabled"], false);
        assert!(body["hint"].is_null());
    }

    #[tokio::test]
    async fn mcp_put_enables_and_returns_card() {
        let state = state().await;
        seed_chat(&state, "m-put", "cloud", None).await;
        seed_server(&state, "gh").await;

        let (status, body) = request(&state, "PUT", "/assistant/chats/m-put/mcp/gh", None).await;

        assert_eq!(status, StatusCode::OK);
        assert_no_secret(&body);
        let gh = &body["servers"][0];
        assert_eq!(gh["enabled"], true);
        // No directory: the policy is not Unrestricted, so the server is on but not reaching turns.
        assert_ne!(body["policy"], "unrestricted");
        assert_eq!(gh["active"], false);
        assert!(gh["inactive_reason"].is_string());
        let enabled = mcp_servers::enabled_for_chat(&state.pool, "m-put")
            .await
            .unwrap();
        assert_eq!(enabled.len(), 1);

        let (status, body) = request(&state, "DELETE", "/assistant/chats/m-put/mcp/gh", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["servers"][0]["enabled"], false);
        assert!(body["servers"][0]["inactive_reason"].is_null());
    }

    #[tokio::test]
    async fn mcp_delete_nucleos_400_cannot_disable_nucleos() {
        let state = state().await;
        seed_chat(&state, "m-nucleos", "cloud", None).await;

        let (status, body) = request(
            &state,
            "DELETE",
            "/assistant/chats/m-nucleos/mcp/nucleos",
            None,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "cannot_disable_nucleos", "{body}");
    }

    #[tokio::test]
    async fn mcp_put_unknown_400_with_known_list() {
        let state = state().await;
        seed_chat(&state, "m-unknown", "cloud", None).await;
        seed_server(&state, "zeta").await;
        seed_server(&state, "gh").await;

        let (status, body) =
            request(&state, "PUT", "/assistant/chats/m-unknown/mcp/nope", None).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_no_secret(&body);
        assert_eq!(body["error"], "invalid_argument");
        assert_eq!(body["known"], json!(["gh", "zeta"]));
    }

    #[tokio::test]
    async fn mcp_put_local_422_unsupported_engine() {
        let state = state().await;
        seed_chat(&state, "m-local", "local", None).await;
        seed_server(&state, "gh").await;

        let (status, body) = request(&state, "PUT", "/assistant/chats/m-local/mcp/gh", None).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "unsupported_engine", "{body}");

        let (_, card) = request(&state, "GET", "/assistant/chats/m-local/mcp", None).await;
        assert_eq!(card["supported"], false);
        assert_eq!(card["engine_note"], "not supported on this engine in v1");
        assert_eq!(card["fixed"]["active"], false);
    }

    #[tokio::test]
    async fn mcp_put_busy_409() {
        let state = state().await;
        seed_chat(&state, "m-busy-409", "cloud", None).await;
        seed_server(&state, "gh").await;
        let _busy = crate::assistant::take_the_slot_for_testing("m-busy-409");

        let (status, body) =
            request(&state, "PUT", "/assistant/chats/m-busy-409/mcp/gh", None).await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "busy", "{body}");
        let enabled = mcp_servers::enabled_for_chat(&state.pool, "m-busy-409")
            .await
            .unwrap();
        assert!(enabled.is_empty(), "nothing is written while busy");
    }

    #[tokio::test]
    async fn mcp_put_missing_chat_404() {
        let state = state().await;
        seed_server(&state, "gh").await;

        let (status, body) = request(&state, "PUT", "/assistant/chats/ghost/mcp/gh", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "chat_not_found", "{body}");

        let (status, body) = request(&state, "GET", "/assistant/chats/ghost/mcp", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "chat_not_found", "{body}");
    }

    #[tokio::test]
    async fn mcp_hint_for_legacy_ambient_chat() {
        let state = state().await;
        seed_chat(&state, "m-legacy", "cloud", None).await;
        sqlx::query("UPDATE chats SET ambient_mcp = 1 WHERE chat_id = ?")
            .bind("m-legacy")
            .execute(&state.pool)
            .await
            .unwrap();

        let (status, body) = request(&state, "GET", "/assistant/chats/m-legacy/mcp", None).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["servers"], json!([]));
        assert_eq!(body["hint"], "this chat used Claude's servers; use Import");
    }

    #[tokio::test]
    async fn mcp_servers_get_and_delete() {
        let state = state().await;
        seed_server(&state, "gh").await;

        let (status, body) = request(&state, "GET", "/assistant/mcp/servers", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_no_secret(&body);
        assert_eq!(body["servers"][0]["name"], "gh");
        assert_eq!(body["servers"][0]["env_names"], json!(["GH_TOKEN"]));
        assert_eq!(body["servers"][0]["imported_from"], "claude-user");

        let (status, _) = request(&state, "DELETE", "/assistant/mcp/servers/gh", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, body) = request(&state, "DELETE", "/assistant/mcp/servers/gh", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");
    }

    #[tokio::test]
    async fn mcp_import_reads_fixture_home_and_enables_nothing() {
        let state = state().await;
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            json!({ "mcpServers": {
                "local1": { "command": "npx", "args": ["x"], "env": { "K": SECRET } },
                "remote": { "type": "http", "url": "https://example.com/mcp" }
            }})
            .to_string(),
        )
        .unwrap();
        seed_chat(&state, "m-import", "cloud", Some(project.path())).await;

        let (status, body) = super::super::command::TEST_HOME
            .scope(
                Some(home.path().to_path_buf()),
                request(
                    &state,
                    "POST",
                    "/assistant/mcp/import",
                    Some(json!({ "chat_id": "m-import" })),
                ),
            )
            .await;

        assert_eq!(status, StatusCode::OK);
        assert_no_secret(&body);
        assert_eq!(body["imported"], json!(["local1"]));
        assert_eq!(body["skipped"][0]["name"], "remote");
        assert_eq!(body["skipped"][0]["reason"], "http_transport");
        assert_eq!(body["unread"], json!([]));
        let enabled = mcp_servers::enabled_for_chat(&state.pool, "m-import")
            .await
            .unwrap();
        assert!(enabled.is_empty(), "an import never enables a server");

        let (status, body) = super::super::command::TEST_HOME
            .scope(
                Some(home.path().to_path_buf()),
                request(
                    &state,
                    "POST",
                    "/assistant/mcp/import",
                    Some(json!({ "chat_id": "ghost" })),
                ),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "chat_not_found");
    }

    #[tokio::test]
    async fn mcp_import_malformed_is_200_unread() {
        let state = state().await;
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".claude.json"), "{ not json").unwrap();

        let (status, body) = super::super::command::TEST_HOME
            .scope(
                Some(home.path().to_path_buf()),
                request(&state, "POST", "/assistant/mcp/import", None),
            )
            .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["imported"], json!([]));
        assert_eq!(body["unread"].as_array().map(Vec::len), Some(1), "{body}");
    }

    async fn command(state: &AppState, chat: &str, text: &str) -> (StatusCode, Value) {
        request(
            state,
            "POST",
            &format!("/assistant/chats/{chat}/command"),
            Some(json!({ "text": text })),
        )
        .await
    }

    #[tokio::test]
    async fn command_d_mcp_card() {
        let state = state().await;
        seed_chat(&state, "d-card", "cloud", None).await;
        seed_server(&state, "gh").await;

        let (status, body) = command(&state, "d-card", "/mcp").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_no_secret(&body);
        assert_eq!(body["kind"], "card");
        assert_eq!(body["card"]["kind"], "mcp");
        assert_eq!(body["card"]["data"]["fixed"]["name"], "nucleos");
        assert_eq!(
            body["card"]["data"]["servers"][0]["env_names"],
            json!(["GH_TOKEN"])
        );
    }

    #[tokio::test]
    async fn command_d_mcp_on_off() {
        let state = state().await;
        seed_chat(&state, "d-onoff", "cloud", None).await;
        seed_server(&state, "gh").await;

        let (status, body) = command(&state, "d-onoff", "/mcp ON gh").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["card"]["kind"], "mcp");
        assert_eq!(body["card"]["data"]["servers"][0]["enabled"], true);

        let (status, body) = command(&state, "d-onoff", "/mcp off gh").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["card"]["data"]["servers"][0]["enabled"], false);
        let enabled = mcp_servers::enabled_for_chat(&state.pool, "d-onoff")
            .await
            .unwrap();
        assert!(enabled.is_empty());
    }

    #[tokio::test]
    async fn command_d_mcp_on_local_422() {
        let state = state().await;
        seed_chat(&state, "d-local", "local", None).await;
        seed_server(&state, "gh").await;

        let (status, body) = command(&state, "d-local", "/mcp on gh").await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "unsupported_engine", "{body}");
    }

    #[tokio::test]
    async fn command_d_mcp_on_busy_409() {
        let state = state().await;
        seed_chat(&state, "d-busy-409", "cloud", None).await;
        seed_server(&state, "gh").await;
        let _busy = crate::assistant::take_the_slot_for_testing("d-busy-409");

        let (status, body) = command(&state, "d-busy-409", "/mcp on gh").await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "busy", "{body}");
        let enabled = mcp_servers::enabled_for_chat(&state.pool, "d-busy-409")
            .await
            .unwrap();
        assert!(enabled.is_empty(), "nothing is written while busy");
    }

    #[tokio::test]
    async fn command_d_mcp_off_nucleos_400() {
        let state = state().await;
        seed_chat(&state, "d-nucleos", "cloud", None).await;

        let (status, body) = command(&state, "d-nucleos", "/mcp off nucleos").await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "cannot_disable_nucleos", "{body}");
    }

    #[tokio::test]
    async fn command_d_mcp_bad_args_400() {
        let state = state().await;
        seed_chat(&state, "d-bad", "cloud", None).await;
        seed_server(&state, "gh").await;

        for text in [
            "/mcp frob",
            "/mcp on",
            "/mcp on a b",
            "/mcp import now",
            "/mcp on ghost",
        ] {
            let (status, body) = command(&state, "d-bad", text).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{text}: {body}");
            assert_eq!(body["error"], "invalid_argument", "{text}: {body}");
            assert_eq!(body["usage"], super::USAGE, "{text}: {body}");
        }
    }

    #[tokio::test]
    async fn command_d_mcp_import_card() {
        let state = state().await;
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            json!({ "mcpServers": {
                "local1": { "command": "npx", "args": ["x"], "env": { "K": SECRET } }
            }})
            .to_string(),
        )
        .unwrap();
        seed_chat(&state, "d-import", "cloud", None).await;

        let (status, body) = super::super::command::TEST_HOME
            .scope(
                Some(home.path().to_path_buf()),
                command(&state, "d-import", "/mcp import"),
            )
            .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_no_secret(&body);
        assert_eq!(body["kind"], "card");
        assert_eq!(body["card"]["kind"], "mcp_import");
        assert_eq!(body["card"]["data"]["imported"], json!(["local1"]));
        let enabled = mcp_servers::enabled_for_chat(&state.pool, "d-import")
            .await
            .unwrap();
        assert!(enabled.is_empty(), "an import never enables a server");
    }

    #[test]
    fn mcp_refusal_into_slash_keeps_cannot_disable_nucleos() {
        use super::{McpRefusal, SlashError};
        let unknown = McpRefusal::Unknown {
            name: "x".into(),
            known: vec!["gh".into()],
        };
        match unknown.into_slash() {
            SlashError::InvalidArgument { usage, .. } => assert_eq!(usage, super::USAGE),
            other => panic!("expected invalid_argument, got {other:?}"),
        }
        assert_eq!(
            McpRefusal::CannotDisableNucleos.into_slash(),
            SlashError::CannotDisableNucleos
        );
    }
}
