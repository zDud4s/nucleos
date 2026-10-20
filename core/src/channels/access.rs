//! Access policy, pairing, primary channel and management routes (flow C). P0 skeleton: the
//! routes answer 501.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{delete, get, put};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::SqlitePool;

mod pairing;

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

/// `set_primary`'s answer (E.3). The function itself belongs to the identities chunk.
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

/// Routes of D.7 and E, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels", get(not_implemented))
        .route("/channels/primary", put(not_implemented))
        .route(
            "/channels/{channel}/pairing",
            get(not_implemented)
                .post(not_implemented)
                .delete(not_implemented),
        )
        .route("/channels/{channel}/identities", get(not_implemented))
        .route(
            "/channels/{channel}/identities/{sender_ref}",
            delete(not_implemented),
        )
        .route(
            "/channels/{channel}/identities/{sender_ref}/home",
            put(not_implemented),
        )
}

/// Placeholder handler: the real one replaces it in the chunk that owns the route.
async fn not_implemented() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
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
}
