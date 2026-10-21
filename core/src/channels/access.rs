//! Access policy, pairing, primary channel and management routes (flow C). The management routes
//! of Anexo E (list, pairing, identities, home, primary) are all real.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::SqlitePool;

mod identities;
mod pairing;

pub use identities::{
    ConversationRow, ForgetError, Forgotten, HomeSet, IdentitiesReadout, ProjectConversation,
    SenderIdentities, SetHomeError, forget, identities, set_home, set_primary,
};
pub(crate) use identities::{home_identity, identity_counts};
pub use pairing::{
    MintedCode, PairOutcome, PairedIdentity, PairingStatus, cancel_pairing, mint_pairing_code,
    pair, pairing_status,
};

use super::{contract, registry};
use crate::state::AppState;

/// Single source of the number of tries a pairing code allows (E.4 `attempts_left` =
/// `MAX_PAIR_ATTEMPTS - attempts`).
pub const MAX_PAIR_ATTEMPTS: u32 = 5;
/// A pairing code is valid for 10 minutes, checked at use; the row is kept 1 day (states and the
/// failure tally) and then pruned.
pub const PAIR_TTL: chrono::Duration = chrono::Duration::minutes(10);
pub const PAIR_ROW_RETENTION: chrono::Duration = chrono::Duration::days(1);

/// What `admit` decides about one inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Owner in an allowed conversation (its ref or its parent is paired, or the ref is a
    /// project conversation of this channel's yaml).
    Admit,
    Drop(DropReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    UnknownSender,
    ConversationNotAllowed,
}

/// A stored row of `channel_identities` (D.17). `identities` groups these by sender.
#[derive(Debug, Clone)]
pub struct Identity {
    pub sender_ref: String,
    pub sender_name: Option<String>,
    pub conversation_ref: String,
    pub conversation_kind: contract::ConversationKind,
    pub parent: Option<String>,
    pub role: String,
    pub home: bool,
    pub created_at: DateTime<Utc>,
}

/// `set_primary`'s answer (E.3). The function lives in `identities.rs`.
#[derive(Debug, serde::Serialize)]
pub struct PrimarySet {
    pub primary: Option<&'static str>,
    pub reseeded: bool,
}

#[derive(Debug)]
pub enum PrimaryError {
    UnknownChannel,
    ChannelNotConfigured,
    NoHomeConversation,
    Db(sqlx::Error),
}

/// Fail-closed (D1): the sender must be an owner (a row of `channel_identities` carries its
/// `sender_ref`) AND the conversation allowed: its ref is paired, or its `parent` is, or its ref is
/// a project conversation of the channel yaml. An unknown sender also bumps the in-memory
/// `Drops::unknown_sender` of the channel.
pub async fn admit(
    state: &AppState,
    channel: &'static str,
    conversation: &contract::Conversation,
    sender: &contract::Sender,
) -> sqlx::Result<Verdict> {
    let pool = &state.pool;
    let owner: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM channel_identities WHERE channel = ? AND sender_ref = ? LIMIT 1",
    )
    .bind(channel)
    .bind(&sender.reference)
    .fetch_optional(pool)
    .await?;
    if owner.is_none() {
        state.channels.presence.drop_unknown_sender(channel);
        return Ok(Verdict::Drop(DropReason::UnknownSender));
    }
    if conversation_is_paired(pool, channel, &conversation.reference).await? {
        return Ok(Verdict::Admit);
    }
    if let Some(parent) = &conversation.parent
        && conversation_is_paired(pool, channel, parent).await?
    {
        return Ok(Verdict::Admit);
    }
    let mapped = state.channels.config(channel).is_some_and(|config| {
        registry::project_for_conversation(&config, &conversation.reference).is_some()
    });
    if mapped {
        return Ok(Verdict::Admit);
    }
    Ok(Verdict::Drop(DropReason::ConversationNotAllowed))
}

async fn conversation_is_paired(
    pool: &SqlitePool,
    channel: &str,
    conversation_ref: &str,
) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM channel_identities WHERE channel = ? AND conversation_ref = ? LIMIT 1",
    )
    .bind(channel)
    .bind(conversation_ref)
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// The conversation notifications and fallbacks go to; `None` when the channel has no home.
pub async fn home_conversation(pool: &SqlitePool, channel: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT conversation_ref FROM channel_identities WHERE channel = ? AND home = 1",
    )
    .bind(channel)
    .fetch_optional(pool)
    .await
}

/// `channels.primary` (section 7.2), stored in `channels_global`. A stored id that is no longer a
/// compiled manifest reads as `None`.
pub async fn primary(pool: &SqlitePool) -> sqlx::Result<Option<&'static str>> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT primary_channel FROM channels_global WHERE id = 1")
            .fetch_optional(pool)
            .await?
            .flatten();
    Ok(raw
        .as_deref()
        .and_then(registry::by_id)
        .map(|manifest| manifest.id))
}

/// Routes of D.7 and E, literal paths (no `nest`) so `map::seam` can read them. All of them are
/// Control-only: `auth.rs` answers a channel key with a bare 403 before any handler here runs.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels", get(get_channels))
        .route("/channels/primary", put(put_primary))
        .route(
            "/channels/{channel}/pairing",
            get(get_pairing).post(post_pairing).delete(delete_pairing),
        )
        .route("/channels/{channel}/identities", get(get_identities))
        .route(
            "/channels/{channel}/identities/{sender_ref}",
            delete(delete_identity),
        )
        .route(
            "/channels/{channel}/identities/{sender_ref}/home",
            put(put_home),
        )
}

/// `GET /channels` (E.2): one status per compiled manifest plus the primary and routing state.
async fn get_channels(State(state): State<AppState>) -> Response {
    match registry::list(&state).await {
        Ok(readout) => Json(readout).into_response(),
        Err(error) => database_failure(&error, "channel list"),
    }
}

/// The body of every named refusal of Anexo E (E.1): `{"refusal": wire, "detail": english}`. It is
/// not `door::refusal`, which carries no `detail`.
fn refuse(refusal: contract::ManagementRefusal, detail: &str) -> Response {
    (
        refusal.status(),
        Json(serde_json::json!({ "refusal": refusal.wire(), "detail": detail })),
    )
        .into_response()
}

fn database_failure(error: &sqlx::Error, what: &str) -> Response {
    tracing::warn!(%error, "channel management {what} failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// E.1 common checks, in order: `400 malformed_channel`, then `404 unknown_channel`.
// The refusal is a ready-made HTTP response the caller returns as is; boxing it would only add noise.
#[allow(clippy::result_large_err)]
fn resolve_channel(channel: &str) -> Result<&'static registry::Manifest, Response> {
    if !contract::valid_channel_id(channel) {
        return Err(refuse(
            contract::ManagementRefusal::MalformedChannel,
            "the channel id must match [a-z][a-z0-9-]{0,31}",
        ));
    }
    registry::by_id(channel).ok_or_else(|| {
        refuse(
            contract::ManagementRefusal::UnknownChannel,
            "no channel of that kind is compiled into this build",
        )
    })
}

/// E.1: a ref is 1 to 128 bytes, else `400 malformed_ref`.
// The refusal is a ready-made HTTP response the caller returns as is; boxing it would only add noise.
#[allow(clippy::result_large_err)]
fn check_ref(reference: &str, what: &str) -> Result<(), Response> {
    if contract::valid_ref(reference) {
        return Ok(());
    }
    Err(refuse(
        contract::ManagementRefusal::MalformedRef,
        &format!("{what} must be between 1 and 128 bytes"),
    ))
}

/// A body that is not the JSON the route takes: the plain 400 of the extractors it replaces, with
/// no refusal name (E.1: unnamed refusals carry the status only).
fn bad_body() -> Response {
    StatusCode::BAD_REQUEST.into_response()
}

/// `POST /channels/{channel}/pairing` (E.4): mints a code, 201; `409 channel_not_configured` while
/// a required secret is missing (the cache `launch_all` fills).
async fn post_pairing(State(state): State<AppState>, Path(channel): Path<String>) -> Response {
    let manifest = match resolve_channel(&channel) {
        Ok(manifest) => manifest,
        Err(refusal) => return refusal,
    };
    if !registry::is_configured(&state, manifest.id) {
        return refuse(
            contract::ManagementRefusal::ChannelNotConfigured,
            "the channel is not configured, so it cannot pair yet",
        );
    }
    match mint_pairing_code(&state.pool, manifest.id, Utc::now()).await {
        Ok(minted) => (StatusCode::CREATED, Json(minted)).into_response(),
        Err(error) => database_failure(&error, "pairing mint"),
    }
}

/// `GET /channels/{channel}/pairing` (E.4): the state of the most recent code.
async fn get_pairing(State(state): State<AppState>, Path(channel): Path<String>) -> Response {
    let manifest = match resolve_channel(&channel) {
        Ok(manifest) => manifest,
        Err(refusal) => return refusal,
    };
    match pairing_status(&state.pool, manifest.id, Utc::now()).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => database_failure(&error, "pairing status"),
    }
}

/// `DELETE /channels/{channel}/pairing` (E.4): 204, also when there is no live code.
async fn delete_pairing(State(state): State<AppState>, Path(channel): Path<String>) -> Response {
    let manifest = match resolve_channel(&channel) {
        Ok(manifest) => manifest,
        Err(refusal) => return refusal,
    };
    match cancel_pairing(&state.pool, manifest.id, Utc::now()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => database_failure(&error, "pairing cancel"),
    }
}

/// `GET /channels/{channel}/identities` (E.5).
async fn get_identities(State(state): State<AppState>, Path(channel): Path<String>) -> Response {
    let manifest = match resolve_channel(&channel) {
        Ok(manifest) => manifest,
        Err(refusal) => return refusal,
    };
    match identities(&state, manifest.id).await {
        Ok(readout) => Json(readout).into_response(),
        Err(error) => database_failure(&error, "identities"),
    }
}

#[derive(serde::Deserialize)]
struct ForgetQuery {
    /// Percent-decoded by the extractor; present means "only this conversation".
    conversation: Option<String>,
}

/// `DELETE /channels/{channel}/identities/{sender_ref}[?conversation=<ref>]` (E.5).
async fn delete_identity(
    State(state): State<AppState>,
    Path((channel, sender_ref)): Path<(String, String)>,
    Query(query): Query<ForgetQuery>,
) -> Response {
    let manifest = match resolve_channel(&channel) {
        Ok(manifest) => manifest,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = check_ref(&sender_ref, "sender_ref") {
        return refusal;
    }
    if let Some(conversation) = &query.conversation
        && let Err(refusal) = check_ref(conversation, "conversation")
    {
        return refusal;
    }
    match forget(
        &state,
        manifest.id,
        &sender_ref,
        query.conversation.as_deref(),
    )
    .await
    {
        Ok(forgotten) => Json(forgotten).into_response(),
        Err(ForgetError::UnknownIdentity) => refuse(
            contract::ManagementRefusal::UnknownIdentity,
            "that sender has no identity on this channel",
        ),
        Err(ForgetError::UnknownConversation) => refuse(
            contract::ManagementRefusal::UnknownConversation,
            "that conversation is not one of this sender's on this channel",
        ),
        Err(ForgetError::Db(error)) => database_failure(&error, "identity forget"),
    }
}

/// `PUT /channels/{channel}/identities/{sender_ref}/home {"conversation_ref": ...}` (E.5).
async fn put_home(
    State(state): State<AppState>,
    Path((channel, sender_ref)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let manifest = match resolve_channel(&channel) {
        Ok(manifest) => manifest,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = check_ref(&sender_ref, "sender_ref") {
        return refusal;
    }
    let conversation_ref = match serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .as_ref()
        .and_then(|value| value.as_object())
        .and_then(|object| object.get("conversation_ref"))
        .and_then(|value| value.as_str())
    {
        Some(reference) => reference.to_string(),
        None => return bad_body(),
    };
    if let Err(refusal) = check_ref(&conversation_ref, "conversation_ref") {
        return refusal;
    }
    match set_home(&state.pool, manifest.id, &sender_ref, &conversation_ref).await {
        Ok(set) => Json(set).into_response(),
        Err(SetHomeError::UnknownIdentity) => refuse(
            contract::ManagementRefusal::UnknownIdentity,
            "that sender has no identity on this channel",
        ),
        Err(SetHomeError::UnknownConversation) => refuse(
            contract::ManagementRefusal::UnknownConversation,
            "that conversation is not one of this sender's on this channel",
        ),
        Err(SetHomeError::Db(error)) => database_failure(&error, "home change"),
    }
}

/// `PUT /channels/primary {"channel": "<id>" | null}` (E.3). The `channel` key is required, so an
/// empty object cannot clear the primary by accident.
async fn put_primary(State(state): State<AppState>, body: Bytes) -> Response {
    let value = serde_json::from_slice::<serde_json::Value>(&body).ok();
    let requested: Option<String> = match value
        .as_ref()
        .and_then(|value| value.as_object())
        .and_then(|object| object.get("channel"))
    {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(id)) => Some(id.clone()),
        _ => return bad_body(),
    };
    if let Some(id) = &requested {
        // The unknown-channel refusal comes from `set_primary` (same 404), after the grammar.
        if !contract::valid_channel_id(id) {
            return refuse(
                contract::ManagementRefusal::MalformedChannel,
                "the channel id must match [a-z][a-z0-9-]{0,31}",
            );
        }
    }
    match set_primary(&state, requested.as_deref()).await {
        Ok(set) => Json(set).into_response(),
        Err(PrimaryError::UnknownChannel) => refuse(
            contract::ManagementRefusal::UnknownChannel,
            "no channel of that kind is compiled into this build",
        ),
        Err(PrimaryError::ChannelNotConfigured) => refuse(
            contract::ManagementRefusal::ChannelNotConfigured,
            "the channel is not configured, so it cannot become the primary yet",
        ),
        Err(PrimaryError::NoHomeConversation) => refuse(
            contract::ManagementRefusal::NoHomeConversation,
            "the channel has no home conversation, so notifications would have no destination",
        ),
        Err(PrimaryError::Db(error)) => database_failure(&error, "primary change"),
    }
}

/// Housekeeping hook called hourly by `run_housekeeping`.
pub async fn prune(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let cutoff = (now - PAIR_ROW_RETENTION).to_rfc3339_opts(SecondsFormat::Secs, true);
    let done = sqlx::query("DELETE FROM channel_pairing_codes WHERE expires_at < ?")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::contract::{Conversation, ConversationKind, Sender};
    use crate::channels::testkit::state;

    const NOW: &str = "2026-10-20T10:00:00Z";

    fn conv(reference: &str, kind: ConversationKind, parent: Option<&str>) -> Conversation {
        Conversation {
            reference: reference.to_string(),
            kind,
            parent: parent.map(str::to_string),
        }
    }

    fn who(reference: &str) -> Sender {
        Sender {
            reference: reference.to_string(),
            name: None,
        }
    }

    async fn seed(pool: &SqlitePool, sender: &str, conversation: &str, kind: &str, home: bool) {
        sqlx::query(
            "INSERT INTO channel_identities \
             (channel, sender_ref, conversation_ref, conversation_kind, home, created_at) \
             VALUES ('telegram', ?, ?, ?, ?, ?)",
        )
        .bind(sender)
        .bind(conversation)
        .bind(kind)
        .bind(home as i64)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn owner_in_a_paired_private_chat_is_admitted() {
        let st = state().await;
        seed(&st.pool, "u1", "c1", "private", true).await;
        let verdict = admit(
            &st,
            "telegram",
            &conv("c1", ConversationKind::Private, None),
            &who("u1"),
        )
        .await
        .unwrap();
        assert_eq!(verdict, Verdict::Admit);
    }

    #[tokio::test]
    async fn a_thread_is_admitted_through_its_paired_parent() {
        let st = state().await;
        seed(&st.pool, "u1", "g1", "group", false).await;
        let verdict = admit(
            &st,
            "telegram",
            &conv("g1:7", ConversationKind::Thread, Some("g1")),
            &who("u1"),
        )
        .await
        .unwrap();
        assert_eq!(verdict, Verdict::Admit);
    }

    #[tokio::test]
    async fn a_group_without_an_owner_is_dropped() {
        let st = state().await;
        seed(&st.pool, "u1", "c1", "private", true).await;
        let verdict = admit(
            &st,
            "telegram",
            &conv("g9", ConversationKind::Group, None),
            &who("stranger"),
        )
        .await
        .unwrap();
        assert_eq!(verdict, Verdict::Drop(DropReason::UnknownSender));
    }

    #[tokio::test]
    async fn an_unknown_sender_is_dropped_and_counted() {
        let st = state().await;
        seed(&st.pool, "u1", "c1", "private", true).await;
        let verdict = admit(
            &st,
            "telegram",
            &conv("c1", ConversationKind::Private, None),
            &who("stranger"),
        )
        .await
        .unwrap();
        assert_eq!(verdict, Verdict::Drop(DropReason::UnknownSender));
        assert_eq!(st.channels.presence.drops("telegram").unknown_sender, 1);
    }

    #[tokio::test]
    async fn a_project_mapped_thread_is_admitted() {
        let st = state().await;
        seed(&st.pool, "u1", "c1", "private", true).await;
        let mut config = registry::ChannelConfig::default();
        config
            .projects
            .insert("proj".to_string(), "g5:42".to_string());
        st.channels
            .configs
            .write()
            .unwrap()
            .insert("telegram", config);
        let verdict = admit(
            &st,
            "telegram",
            &conv("g5:42", ConversationKind::Thread, Some("g5")),
            &who("u1"),
        )
        .await
        .unwrap();
        assert_eq!(verdict, Verdict::Admit);
    }

    #[tokio::test]
    async fn an_owner_in_an_unlisted_conversation_is_dropped() {
        let st = state().await;
        seed(&st.pool, "u1", "c1", "private", true).await;
        let verdict = admit(
            &st,
            "telegram",
            &conv("g2", ConversationKind::Group, None),
            &who("u1"),
        )
        .await
        .unwrap();
        assert_eq!(verdict, Verdict::Drop(DropReason::ConversationNotAllowed));
        assert_eq!(st.channels.presence.drops("telegram").unknown_sender, 0);
    }

    #[tokio::test]
    async fn home_conversation_is_none_without_a_home_and_the_ref_with_one() {
        let st = state().await;
        assert_eq!(home_conversation(&st.pool, "telegram").await.unwrap(), None);
        seed(&st.pool, "u1", "g1", "group", false).await;
        assert_eq!(home_conversation(&st.pool, "telegram").await.unwrap(), None);
        seed(&st.pool, "u1", "c1", "private", true).await;
        assert_eq!(
            home_conversation(&st.pool, "telegram").await.unwrap(),
            Some("c1".to_string())
        );
        assert_eq!(home_conversation(&st.pool, "fake").await.unwrap(), None);
    }

    #[tokio::test]
    async fn primary_is_none_then_the_stored_channel() {
        let st = state().await;
        assert_eq!(primary(&st.pool).await.unwrap(), None);
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram' WHERE id = 1")
            .execute(&st.pool)
            .await
            .unwrap();
        assert_eq!(primary(&st.pool).await.unwrap(), Some("telegram"));
    }

    #[tokio::test]
    async fn prune_removes_pairing_rows_older_than_a_day() {
        let st = state().await;
        let now: DateTime<Utc> = "2026-10-20T10:00:00Z".parse().unwrap();
        for (hash, expires) in [
            ("old", "2026-10-19T09:59:59Z"),
            ("recent", "2026-10-19T10:00:01Z"),
            ("future", "2026-10-20T10:10:00Z"),
        ] {
            sqlx::query(
                "INSERT INTO channel_pairing_codes (channel, code_hash, created_at, expires_at, cancelled_at) \
                 VALUES ('telegram', ?, ?, ?, ?)",
            )
            .bind(hash)
            .bind(NOW)
            .bind(expires)
            .bind(NOW)
            .execute(&st.pool)
            .await
            .unwrap();
        }
        assert_eq!(prune(&st.pool, now).await.unwrap(), 1);
        let left: Vec<String> =
            sqlx::query_scalar("SELECT code_hash FROM channel_pairing_codes ORDER BY id")
                .fetch_all(&st.pool)
                .await
                .unwrap();
        assert_eq!(left, vec!["recent".to_string(), "future".to_string()]);
    }

    // ---- the management routes (RC) --------------------------------------------------------

    const CONTROL_KEY: &str = "test-token";

    /// The channel routes behind the real token middleware.
    fn app(st: &AppState) -> Router {
        crate::channels::routes()
            .layer(axum::middleware::from_fn_with_state(
                st.clone(),
                crate::auth::require_token,
            ))
            .with_state(st.clone())
    }

    async fn call(
        st: &AppState,
        key: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let response = app(st).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Calls as the control key and asserts a named refusal of Anexo E (status, wire name and a
    /// non-empty English `detail`).
    async fn refused(
        st: &AppState,
        method: &str,
        path: &str,
        body: &str,
        refusal: contract::ManagementRefusal,
    ) {
        let (status, json) = call(st, CONTROL_KEY, method, path, body).await;
        assert_eq!(status, refusal.status(), "{method} {path}");
        assert_eq!(json["refusal"], refusal.wire(), "{method} {path}");
        assert!(
            json["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "{method} {path}: {json}"
        );
    }

    async fn seed_fake(
        pool: &SqlitePool,
        sender: &str,
        conversation: &str,
        kind: &str,
        parent: Option<&str>,
        home: bool,
    ) {
        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, parent, home, created_at) VALUES ('fake', ?, ?, ?, ?, ?, ?)",
        )
        .bind(sender)
        .bind(conversation)
        .bind(kind)
        .bind(parent)
        .bind(home as i64)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn route_malformed_channel_is_400_before_anything_else() {
        use contract::ManagementRefusal::MalformedChannel;
        let st = state().await;
        refused(
            &st,
            "POST",
            "/channels/Bad_Id/pairing",
            "",
            MalformedChannel,
        )
        .await;
        refused(
            &st,
            "GET",
            "/channels/Bad_Id/identities",
            "",
            MalformedChannel,
        )
        .await;
        refused(
            &st,
            "PUT",
            "/channels/primary",
            r#"{"channel":"BAD"}"#,
            MalformedChannel,
        )
        .await;
    }

    #[tokio::test]
    async fn route_unknown_channel_is_404() {
        use contract::ManagementRefusal::UnknownChannel;
        let st = state().await;
        refused(&st, "GET", "/channels/nope/pairing", "", UnknownChannel).await;
        refused(&st, "DELETE", "/channels/nope/pairing", "", UnknownChannel).await;
        refused(&st, "GET", "/channels/nope/identities", "", UnknownChannel).await;
        refused(
            &st,
            "PUT",
            "/channels/primary",
            r#"{"channel":"nope"}"#,
            UnknownChannel,
        )
        .await;
    }

    #[tokio::test]
    async fn route_refs_of_129_bytes_or_empty_are_malformed_ref() {
        use contract::ManagementRefusal::MalformedRef;
        let st = state().await;
        seed_fake(&st.pool, "u1", "c1", "private", None, true).await;
        let long = "a".repeat(129);
        refused(
            &st,
            "DELETE",
            &format!("/channels/fake/identities/{long}"),
            "",
            MalformedRef,
        )
        .await;
        refused(
            &st,
            "DELETE",
            "/channels/fake/identities/u1?conversation=",
            "",
            MalformedRef,
        )
        .await;
        refused(
            &st,
            "DELETE",
            &format!("/channels/fake/identities/u1?conversation={long}"),
            "",
            MalformedRef,
        )
        .await;
        refused(
            &st,
            "PUT",
            &format!("/channels/fake/identities/{long}/home"),
            r#"{"conversation_ref":"c1"}"#,
            MalformedRef,
        )
        .await;
        refused(
            &st,
            "PUT",
            "/channels/fake/identities/u1/home",
            &format!(r#"{{"conversation_ref":"{long}"}}"#),
            MalformedRef,
        )
        .await;
        refused(
            &st,
            "PUT",
            "/channels/fake/identities/u1/home",
            r#"{"conversation_ref":""}"#,
            MalformedRef,
        )
        .await;
        // The 128-byte edge is accepted by the grammar (and then simply unknown).
        let edge = "a".repeat(128);
        refused(
            &st,
            "DELETE",
            &format!("/channels/fake/identities/{edge}"),
            "",
            contract::ManagementRefusal::UnknownIdentity,
        )
        .await;
    }

    #[tokio::test]
    async fn route_channel_not_configured_is_409_on_primary() {
        let st = state().await;
        seed_fake(&st.pool, "u1", "c1", "private", None, true).await;
        refused(
            &st,
            "PUT",
            "/channels/primary",
            r#"{"channel":"fake"}"#,
            contract::ManagementRefusal::ChannelNotConfigured,
        )
        .await;
        // A1: the stored primary did not change.
        assert_eq!(primary(&st.pool).await.unwrap(), None);
    }

    #[tokio::test]
    async fn route_no_home_conversation_is_409_on_primary() {
        let st = state().await;
        refused(
            &st,
            "PUT",
            "/channels/primary",
            r#"{"channel":"fake"}"#,
            contract::ManagementRefusal::NoHomeConversation,
        )
        .await;
    }

    #[tokio::test]
    async fn route_unknown_identity_is_404_on_forget_and_home() {
        use contract::ManagementRefusal::UnknownIdentity;
        let st = state().await;
        refused(
            &st,
            "DELETE",
            "/channels/fake/identities/ghost",
            "",
            UnknownIdentity,
        )
        .await;
        refused(
            &st,
            "PUT",
            "/channels/fake/identities/ghost/home",
            r#"{"conversation_ref":"c1"}"#,
            UnknownIdentity,
        )
        .await;
    }

    #[tokio::test]
    async fn route_unknown_conversation_is_404_on_forget_and_home() {
        use contract::ManagementRefusal::UnknownConversation;
        let st = state().await;
        seed_fake(&st.pool, "u1", "c1", "private", None, true).await;
        refused(
            &st,
            "DELETE",
            "/channels/fake/identities/u1?conversation=zz",
            "",
            UnknownConversation,
        )
        .await;
        refused(
            &st,
            "PUT",
            "/channels/fake/identities/u1/home",
            r#"{"conversation_ref":"zz"}"#,
            UnknownConversation,
        )
        .await;
    }

    #[tokio::test]
    async fn route_primary_null_clears_and_the_current_one_is_a_noop() {
        let st = state().await;
        sqlx::query("UPDATE channels_global SET primary_channel = 'fake' WHERE id = 1")
            .execute(&st.pool)
            .await
            .unwrap();
        let (status, body) = call(
            &st,
            CONTROL_KEY,
            "PUT",
            "/channels/primary",
            r#"{"channel":"fake"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"primary":"fake","reseeded":false}));
        let (status, body) = call(
            &st,
            CONTROL_KEY,
            "PUT",
            "/channels/primary",
            r#"{"channel":null}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"primary":null,"reseeded":false}));
        assert_eq!(primary(&st.pool).await.unwrap(), None);
    }

    #[tokio::test]
    async fn route_primary_needs_the_channel_key() {
        let st = state().await;
        for body in ["", "{}", r#"{"channel":7}"#, "not json"] {
            let (status, _) = call(&st, CONTROL_KEY, "PUT", "/channels/primary", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body {body:?}");
        }
    }

    #[tokio::test]
    async fn route_pairing_mints_reads_and_cancels() {
        let st = state().await;
        st.channels.presence.set_secrets_present("fake", Vec::new());
        let (status, body) = call(&st, CONTROL_KEY, "GET", "/channels/fake/pairing", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "none");

        let (status, body) = call(&st, CONTROL_KEY, "POST", "/channels/fake/pairing", "").await;
        assert_eq!(status, StatusCode::CREATED);
        let code = body["code"].as_str().unwrap().to_string();
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|c| c.is_ascii_digit()));
        assert_eq!(body["command"], format!("/pair {code}"));
        assert_eq!(body["attempts_left"], MAX_PAIR_ATTEMPTS);
        assert!(body["expires_at"].is_string());

        let (status, body) = call(&st, CONTROL_KEY, "GET", "/channels/fake/pairing", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], "waiting");
        assert!(body.get("code").is_none(), "the code never comes back");

        for _ in 0..2 {
            let (status, _) = call(&st, CONTROL_KEY, "DELETE", "/channels/fake/pairing", "").await;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }
        let (_, body) = call(&st, CONTROL_KEY, "GET", "/channels/fake/pairing", "").await;
        assert_eq!(body["state"], "none");
    }

    #[tokio::test]
    async fn route_pairing_post_is_409_while_the_channel_is_not_configured() {
        let st = state().await;
        let (status, body) = call(&st, CONTROL_KEY, "POST", "/channels/fake/pairing", "").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["refusal"], "channel_not_configured");
        let minted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_pairing_codes")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(minted, 0);
        // A missing required secret keeps it refused; an empty missing list lets it mint.
        st.channels
            .presence
            .set_secrets_present("fake", vec!["fake.token"]);
        let (status, _) = call(&st, CONTROL_KEY, "POST", "/channels/fake/pairing", "").await;
        assert_eq!(status, StatusCode::CONFLICT);
        st.channels.presence.set_secrets_present("fake", Vec::new());
        let (status, _) = call(&st, CONTROL_KEY, "POST", "/channels/fake/pairing", "").await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn route_channels_list_is_200_with_every_manifest() {
        let st = state().await;
        let (status, body) = call(&st, CONTROL_KEY, "GET", "/channels", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["contract_version"], contract::CONTRACT_VERSION);
        assert_eq!(body["routing"]["state"], "no_primary");
        assert!(body["primary"].is_null());
        assert!(
            body["channels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "fake")
        );
    }

    #[tokio::test]
    async fn route_identities_list_groups_by_sender() {
        let st = state().await;
        seed_fake(&st.pool, "u1", "g1", "group", None, false).await;
        seed_fake(&st.pool, "u1", "g1:5", "thread", Some("g1"), true).await;
        let (status, body) = call(&st, CONTROL_KEY, "GET", "/channels/fake/identities", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["channel"], "fake");
        assert_eq!(body["home"], "g1:5");
        let senders = body["identities"].as_array().unwrap();
        assert_eq!(senders.len(), 1);
        assert_eq!(senders[0]["sender_ref"], "u1");
        assert_eq!(senders[0]["conversations"].as_array().unwrap().len(), 2);
        assert_eq!(body["projects"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn route_forget_decodes_percent_encoded_refs() {
        let st = state().await;
        seed_fake(&st.pool, "a:b", "-100", "group", None, false).await;
        seed_fake(&st.pool, "a:b", "-100:5", "thread", Some("-100"), true).await;
        let (status, body) = call(
            &st,
            CONTROL_KEY,
            "DELETE",
            "/channels/fake/identities/a%3Ab?conversation=-100%3A5",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"removed":1,"home_cleared":true}));
        let (status, body) = call(
            &st,
            CONTROL_KEY,
            "DELETE",
            "/channels/fake/identities/a%3Ab",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"removed":1,"home_cleared":false}));
    }

    #[tokio::test]
    async fn route_home_moves_and_is_idempotent() {
        let st = state().await;
        seed_fake(&st.pool, "u1", "c1", "private", None, true).await;
        seed_fake(&st.pool, "u1", "c2", "private", None, false).await;
        let (status, body) = call(
            &st,
            CONTROL_KEY,
            "PUT",
            "/channels/fake/identities/u1/home",
            r#"{"conversation_ref":"c2"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"home":"c2","previous":"c1"}));
        let (_, body) = call(
            &st,
            CONTROL_KEY,
            "PUT",
            "/channels/fake/identities/u1/home",
            r#"{"conversation_ref":"c2"}"#,
        )
        .await;
        assert_eq!(body, serde_json::json!({"home":"c2","previous":"c2"}));
        let (status, _) = call(
            &st,
            CONTROL_KEY,
            "PUT",
            "/channels/fake/identities/u1/home",
            "not json",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Regression for P0-9: a channel key never reaches a management handler, not even its own
    /// channel's.
    #[tokio::test]
    async fn route_a_channel_key_is_403_on_management() {
        let st = state().await;
        let key = crate::auth::mint_service_token(&st.pool, crate::auth::Service::Channel("fake"))
            .await
            .unwrap();
        for (method, path, body) in [
            ("POST", "/channels/fake/pairing", ""),
            ("GET", "/channels/fake/identities", ""),
            ("PUT", "/channels/primary", r#"{"channel":null}"#),
        ] {
            let (status, _) = call(&st, &key, method, path, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
        let minted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_pairing_codes")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(minted, 0);
    }
}
