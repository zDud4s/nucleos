//! Identities: list, forget, home conversation and the primary channel (Anexo D D.7, Anexo E
//! E.3 and E.5).
//!
//! The `channel_identities` rows are the only source of "who may talk"; the channel yaml's
//! `projects` mapping is the second, read-only one. Everything that deletes rows revokes what is no
//! longer allowed in the same transaction.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::{Identity, PrimaryError, PrimarySet};
use crate::channels::contract::ConversationKind;
use crate::channels::{outbox, registry, routing, tokens};
use crate::state::AppState;

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn kind_from_text(text: &str) -> Option<ConversationKind> {
    match text {
        "private" => Some(ConversationKind::Private),
        "group" => Some(ConversationKind::Group),
        "thread" => Some(ConversationKind::Thread),
        _ => None,
    }
}

type IdentityRow = (
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    i64,
    String,
);

/// Every row of the channel, ordered by sender then `created_at`. A row whose `created_at` cannot
/// be read is kept (at the epoch) rather than hidden: this is an access list.
async fn load(pool: &SqlitePool, channel: &str) -> sqlx::Result<Vec<Identity>> {
    let rows: Vec<IdentityRow> = sqlx::query_as(
        "SELECT sender_ref, sender_name, conversation_ref, conversation_kind, parent, role, \
         home, created_at FROM channel_identities WHERE channel = ? \
         ORDER BY sender_ref, created_at, conversation_ref",
    )
    .bind(channel)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(
            |(sender_ref, sender_name, conversation_ref, kind, parent, role, home, created)| {
                Some(Identity {
                    sender_ref,
                    sender_name,
                    conversation_ref,
                    conversation_kind: kind_from_text(&kind)?,
                    parent,
                    role,
                    home: home == 1,
                    created_at: parse_time(&created).unwrap_or(DateTime::UNIX_EPOCH),
                })
            },
        )
        .collect())
}

/// The row that holds the channel's home, if any (`registry::list` shows it).
pub(crate) async fn home_identity(
    pool: &SqlitePool,
    channel: &str,
) -> sqlx::Result<Option<Identity>> {
    Ok(load(pool, channel).await?.into_iter().find(|row| row.home))
}

/// Distinct senders and distinct conversations of the channel (`registry::list`).
pub(crate) async fn identity_counts(pool: &SqlitePool, channel: &str) -> sqlx::Result<(u32, u32)> {
    let (senders, conversations): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(DISTINCT sender_ref), COUNT(DISTINCT conversation_ref) \
         FROM channel_identities WHERE channel = ?",
    )
    .bind(channel)
    .fetch_one(pool)
    .await?;
    Ok((senders as u32, conversations as u32))
}

/// Anexo E, E.5: `identities` ordered by `sender_ref`, each with its `conversations` ordered by
/// `created_at`; `covers_threads` is true for a `Group`; `projects` is the yaml's mapping, read-only.
#[derive(Debug, serde::Serialize)]
pub struct IdentitiesReadout {
    pub channel: &'static str,
    pub home: Option<String>,
    pub identities: Vec<SenderIdentities>,
    pub projects: Vec<ProjectConversation>,
}

#[derive(Debug, serde::Serialize)]
pub struct SenderIdentities {
    pub sender_ref: String,
    pub sender_name: Option<String>,
    pub role: String,
    pub conversations: Vec<ConversationRow>,
}

#[derive(Debug, serde::Serialize)]
pub struct ConversationRow {
    pub conversation_ref: String,
    pub kind: ConversationKind,
    pub parent: Option<String>,
    pub home: bool,
    pub covers_threads: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, serde::Serialize)]
pub struct ProjectConversation {
    pub project_id: String,
    pub conversation_ref: String,
}

/// `GET /channels/{channel}/identities`.
pub async fn identities(
    state: &AppState,
    channel: &'static str,
) -> sqlx::Result<IdentitiesReadout> {
    let rows = load(&state.pool, channel).await?;
    let home = rows
        .iter()
        .find(|row| row.home)
        .map(|row| row.conversation_ref.clone());
    let mut grouped: Vec<SenderIdentities> = Vec::new();
    for row in rows {
        let conversation = ConversationRow {
            covers_threads: row.conversation_kind == ConversationKind::Group,
            conversation_ref: row.conversation_ref,
            kind: row.conversation_kind,
            parent: row.parent,
            home: row.home,
            created_at: row.created_at,
        };
        if let Some(last) = grouped
            .last_mut()
            .filter(|last| last.sender_ref == row.sender_ref)
        {
            last.conversations.push(conversation);
            continue;
        }
        grouped.push(SenderIdentities {
            sender_ref: row.sender_ref,
            sender_name: row.sender_name,
            role: row.role,
            conversations: vec![conversation],
        });
    }
    let projects = state
        .channels
        .config(channel)
        .map(|config| {
            config
                .projects
                .into_iter()
                .map(|(project_id, conversation_ref)| ProjectConversation {
                    project_id,
                    conversation_ref,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(IdentitiesReadout {
        channel,
        home,
        identities: grouped,
        projects,
    })
}

/// `DELETE /channels/{channel}/identities/{sender_ref}[?conversation=<ref>]`.
#[derive(Debug, serde::Serialize)]
pub struct Forgotten {
    pub removed: u32,
    pub home_cleared: bool,
}

#[derive(Debug)]
pub enum ForgetError {
    UnknownIdentity,
    UnknownConversation,
    Db(sqlx::Error),
}

type DoomedRow = (String, i64, Option<String>);

/// One transaction: delete the rows (all of the sender's, or the one `conversation`); the home goes
/// with its row (`home_cleared`); then revoke what is no longer allowed: pending or leased actions
/// (`outbox::revoke_unallowed`) and unused button tokens (`tokens::purge_unallowed`) of
/// conversations no longer covered by a remaining row, by the yaml `projects`, or (conservatively,
/// there is no `parent` column on the outbox) by a deleted thread whose parent is still paired.
/// Removing the last identity is allowed.
pub async fn forget(
    state: &AppState,
    channel: &'static str,
    sender_ref: &str,
    conversation: Option<&str>,
) -> Result<Forgotten, ForgetError> {
    let now = Utc::now();
    let mut tx = state.pool.begin().await.map_err(ForgetError::Db)?;
    let rows: Vec<DoomedRow> = sqlx::query_as(
        "SELECT conversation_ref, home, parent FROM channel_identities \
         WHERE channel = ? AND sender_ref = ?",
    )
    .bind(channel)
    .bind(sender_ref)
    .fetch_all(&mut *tx)
    .await
    .map_err(ForgetError::Db)?;
    if rows.is_empty() {
        return Err(ForgetError::UnknownIdentity);
    }
    let doomed: Vec<DoomedRow> = match conversation {
        None => rows,
        Some(wanted) => rows
            .into_iter()
            .filter(|(reference, _, _)| reference.as_str() == wanted)
            .collect(),
    };
    if doomed.is_empty() {
        return Err(ForgetError::UnknownConversation);
    }
    let mut removed: u64 = 0;
    for (reference, _, _) in &doomed {
        let done = sqlx::query(
            "DELETE FROM channel_identities \
             WHERE channel = ? AND sender_ref = ? AND conversation_ref = ?",
        )
        .bind(channel)
        .bind(sender_ref)
        .bind(reference)
        .execute(&mut *tx)
        .await
        .map_err(ForgetError::Db)?;
        removed += done.rows_affected();
    }
    let home_cleared = doomed.iter().any(|(_, home, _)| *home == 1);

    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT conversation_ref FROM channel_identities WHERE channel = ?")
            .bind(channel)
            .fetch_all(&mut *tx)
            .await
            .map_err(ForgetError::Db)?;
    let projects: Vec<String> = state
        .channels
        .config(channel)
        .map(|config| config.projects.into_values().collect())
        .unwrap_or_default();
    // Deleted refs whose parent is still paired stay allowed.
    let still_covered = doomed.iter().filter_map(|(reference, _, parent)| {
        parent
            .as_ref()
            .filter(|parent| remaining.contains(parent))
            .map(|_| reference.as_str())
    });
    let allowed: Vec<&str> = projects
        .iter()
        .map(String::as_str)
        .chain(still_covered)
        .collect();
    outbox::revoke_unallowed(&mut tx, channel, &allowed, now)
        .await
        .map_err(ForgetError::Db)?;
    tokens::purge_unallowed(&mut tx, channel, &allowed)
        .await
        .map_err(ForgetError::Db)?;
    tx.commit().await.map_err(ForgetError::Db)?;
    Ok(Forgotten {
        removed: removed as u32,
        home_cleared,
    })
}

/// `PUT …/identities/{sender_ref}/home`. The conversation must be one of the sender's rows.
#[derive(Debug, serde::Serialize)]
pub struct HomeSet {
    pub home: String,
    pub previous: Option<String>,
}

#[derive(Debug)]
pub enum SetHomeError {
    UnknownIdentity,
    UnknownConversation,
    Db(sqlx::Error),
}

/// Clears the previous home and sets the new one in one transaction (the partial unique index makes
/// two homes impossible). Idempotent: `previous == home` when nothing changes.
pub async fn set_home(
    pool: &SqlitePool,
    channel: &str,
    sender_ref: &str,
    conversation_ref: &str,
) -> Result<HomeSet, SetHomeError> {
    let mut tx = pool.begin().await.map_err(SetHomeError::Db)?;
    let owned: Vec<String> = sqlx::query_scalar(
        "SELECT conversation_ref FROM channel_identities WHERE channel = ? AND sender_ref = ?",
    )
    .bind(channel)
    .bind(sender_ref)
    .fetch_all(&mut *tx)
    .await
    .map_err(SetHomeError::Db)?;
    if owned.is_empty() {
        return Err(SetHomeError::UnknownIdentity);
    }
    if !owned.iter().any(|reference| reference == conversation_ref) {
        return Err(SetHomeError::UnknownConversation);
    }
    let previous: Option<String> = sqlx::query_scalar(
        "SELECT conversation_ref FROM channel_identities WHERE channel = ? AND home = 1",
    )
    .bind(channel)
    .fetch_optional(&mut *tx)
    .await
    .map_err(SetHomeError::Db)?;
    if previous.as_deref() == Some(conversation_ref) {
        return Ok(HomeSet {
            home: conversation_ref.to_string(),
            previous,
        });
    }
    sqlx::query("UPDATE channel_identities SET home = 0 WHERE channel = ? AND home = 1")
        .bind(channel)
        .execute(&mut *tx)
        .await
        .map_err(SetHomeError::Db)?;
    sqlx::query(
        "UPDATE channel_identities SET home = 1 \
         WHERE channel = ? AND sender_ref = ? AND conversation_ref = ?",
    )
    .bind(channel)
    .bind(sender_ref)
    .bind(conversation_ref)
    .execute(&mut *tx)
    .await
    .map_err(SetHomeError::Db)?;
    tx.commit().await.map_err(SetHomeError::Db)?;
    Ok(HomeSet {
        home: conversation_ref.to_string(),
        previous,
    })
}

/// `PUT /channels/primary {channel}` (E.3). `None` clears it. Asking for the current primary is a
/// no-op (`reseeded: false`); an id that is not a compiled kind is `UnknownChannel`; a channel
/// without a home is `NoHomeConversation`.
pub async fn set_primary(
    state: &AppState,
    channel: Option<&str>,
) -> Result<PrimarySet, PrimaryError> {
    let pool = &state.pool;
    let Some(requested) = channel else {
        sqlx::query("UPDATE channels_global SET primary_channel = NULL WHERE id = 1")
            .execute(pool)
            .await
            .map_err(PrimaryError::Db)?;
        return Ok(PrimarySet {
            primary: None,
            reseeded: false,
        });
    };
    let manifest = registry::by_id(requested).ok_or(PrimaryError::UnknownChannel)?;
    let current = super::primary(pool).await.map_err(PrimaryError::Db)?;
    if current == Some(manifest.id) {
        return Ok(PrimarySet {
            primary: current,
            reseeded: false,
        });
    }
    let home = super::home_conversation(pool, manifest.id)
        .await
        .map_err(PrimaryError::Db)?;
    if home.is_none() {
        return Err(PrimaryError::NoHomeConversation);
    }
    // A channel without its required secrets is never launched, so it cannot be primary. Read
    // before the transaction opens: `list` uses the pool, which has one connection in tests.
    let configured = registry::list(state)
        .await
        .map_err(PrimaryError::Db)?
        .channels
        .iter()
        .any(|entry| entry.id == manifest.id && entry.configured);
    if !configured {
        return Err(PrimaryError::ChannelNotConfigured);
    }
    // A different channel becomes primary: store it and reset its cursors to "now" in one
    // transaction, so the backlog it missed while it was not primary is never announced.
    let mut tx = pool.begin().await.map_err(PrimaryError::Db)?;
    sqlx::query("UPDATE channels_global SET primary_channel = ? WHERE id = 1")
        .bind(manifest.id)
        .execute(&mut *tx)
        .await
        .map_err(PrimaryError::Db)?;
    routing::reseed(&mut tx, manifest.id)
        .await
        .map_err(PrimaryError::Db)?;
    tx.commit().await.map_err(PrimaryError::Db)?;
    Ok(PrimarySet {
        primary: Some(manifest.id),
        reseeded: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::testkit::state;

    #[allow(clippy::too_many_arguments)]
    async fn seed(
        pool: &SqlitePool,
        sender: &str,
        name: Option<&str>,
        conversation: &str,
        kind: &str,
        parent: Option<&str>,
        home: bool,
        created_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, sender_name, conversation_ref, \
             conversation_kind, parent, home, created_at) \
             VALUES ('telegram', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(sender)
        .bind(name)
        .bind(conversation)
        .bind(kind)
        .bind(parent)
        .bind(home as i64)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn outbox_row(pool: &SqlitePool, conversation: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             state, due_at, attempts, created_at) \
             VALUES ('telegram', ?, 'ordered', 'send', '{}', 'notification', 'pending', \
             '2026-10-20T10:00:00Z', 0, '2026-10-20T10:00:00Z') RETURNING id",
        )
        .bind(conversation)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn outbox_state(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT state, fail_reason FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn token(pool: &SqlitePool, token: &str, conversation: &str) {
        sqlx::query(
            "INSERT INTO channel_action_tokens (token, channel, conversation, action, \
             proposal_id, created_at) \
             VALUES (?, 'telegram', ?, 'approve', 1, '2026-10-20T10:00:00Z')",
        )
        .bind(token)
        .bind(conversation)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn token_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM channel_action_tokens")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn stored_primary(pool: &SqlitePool) -> Option<String> {
        sqlx::query_scalar("SELECT primary_channel FROM channels_global WHERE id = 1")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn identities_groups_orders_and_marks_group_coverage() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "9",
            None,
            "9",
            "private",
            None,
            false,
            "2026-10-12T09:00:00Z",
        )
        .await;
        seed(
            pool,
            "4242",
            Some("Duarte"),
            "-100123:5",
            "thread",
            Some("-100123"),
            true,
            "2026-10-12T18:02:11Z",
        )
        .await;
        seed(
            pool,
            "4242",
            Some("Duarte"),
            "4242",
            "private",
            None,
            false,
            "2026-10-14T09:15:40Z",
        )
        .await;
        seed(
            pool,
            "4242",
            Some("Duarte"),
            "-100123",
            "group",
            None,
            false,
            "2026-10-12T18:02:10Z",
        )
        .await;
        let mut config = registry::ChannelConfig::default();
        config
            .projects
            .insert("alpha".to_string(), "-100123:7".to_string());
        st.channels
            .configs
            .write()
            .unwrap()
            .insert("telegram", config);

        let readout = identities(&st, "telegram").await.unwrap();
        assert_eq!(readout.channel, "telegram");
        assert_eq!(readout.home.as_deref(), Some("-100123:5"));
        let senders: Vec<_> = readout
            .identities
            .iter()
            .map(|i| i.sender_ref.as_str())
            .collect();
        assert_eq!(senders, ["4242", "9"]);
        let first = &readout.identities[0];
        assert_eq!(first.sender_name.as_deref(), Some("Duarte"));
        assert_eq!(first.role, "owner");
        let order: Vec<_> = first
            .conversations
            .iter()
            .map(|c| c.conversation_ref.as_str())
            .collect();
        assert_eq!(order, ["-100123", "-100123:5", "4242"]);
        let covers: Vec<_> = first
            .conversations
            .iter()
            .map(|c| c.covers_threads)
            .collect();
        assert_eq!(covers, [true, false, false]);
        let thread = &first.conversations[1];
        assert!(thread.home);
        assert_eq!(thread.parent.as_deref(), Some("-100123"));
        assert_eq!(thread.kind, ConversationKind::Thread);
        assert_eq!(readout.projects.len(), 1);
        assert_eq!(readout.projects[0].project_id, "alpha");
        assert_eq!(readout.projects[0].conversation_ref, "-100123:7");
        assert!(identities(&st, "fake").await.unwrap().identities.is_empty());
    }

    #[tokio::test]
    async fn forget_one_conversation_keeps_the_other_rows() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        seed(
            pool,
            "1",
            None,
            "g1",
            "group",
            None,
            false,
            "2026-10-12T09:01:00Z",
        )
        .await;
        seed(
            pool,
            "2",
            None,
            "c2",
            "private",
            None,
            false,
            "2026-10-12T09:02:00Z",
        )
        .await;

        let done = forget(&st, "telegram", "1", Some("g1")).await.unwrap();
        assert_eq!((done.removed, done.home_cleared), (1, false));
        let left: Vec<String> =
            sqlx::query_scalar("SELECT conversation_ref FROM channel_identities ORDER BY 1")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(left, ["c1", "c2"]);
    }

    #[tokio::test]
    async fn forget_all_clears_the_home_and_revokes_pending_actions_and_unused_tokens() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        seed(
            pool,
            "2",
            None,
            "c2",
            "private",
            None,
            false,
            "2026-10-12T09:02:00Z",
        )
        .await;
        let gone = outbox_row(pool, "c1").await;
        let kept = outbox_row(pool, "c2").await;
        token(pool, "t-gone", "c1").await;
        token(pool, "t-kept", "c2").await;

        let done = forget(&st, "telegram", "1", None).await.unwrap();
        assert_eq!((done.removed, done.home_cleared), (1, true));
        assert_eq!(
            outbox_state(pool, gone).await,
            ("failed".to_string(), Some("revoked".to_string()))
        );
        assert_eq!(
            outbox_state(pool, kept).await,
            ("pending".to_string(), None)
        );
        let tokens_left: Vec<String> =
            sqlx::query_scalar("SELECT token FROM channel_action_tokens")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(tokens_left, ["t-kept"]);
        assert_eq!(
            home_identity(pool, "telegram")
                .await
                .unwrap()
                .map(|i| i.conversation_ref),
            None
        );
    }

    #[tokio::test]
    async fn forget_keeps_actions_for_threads_still_covered_by_a_remaining_parent() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "1",
            None,
            "g1",
            "group",
            None,
            false,
            "2026-10-12T09:00:00Z",
        )
        .await;
        seed(
            pool,
            "1",
            None,
            "g1:5",
            "thread",
            Some("g1"),
            true,
            "2026-10-12T09:00:01Z",
        )
        .await;
        let thread = outbox_row(pool, "g1:5").await;
        let stranger = outbox_row(pool, "elsewhere").await;
        token(pool, "t-thread", "g1:5").await;

        let done = forget(&st, "telegram", "1", Some("g1:5")).await.unwrap();
        assert_eq!((done.removed, done.home_cleared), (1, true));
        assert_eq!(outbox_state(pool, thread).await.0, "pending");
        assert_eq!(outbox_state(pool, stranger).await.0, "failed");
        assert_eq!(token_count(pool).await, 1);

        // With the parent gone as well nothing covers the thread any more.
        forget(&st, "telegram", "1", None).await.unwrap();
        assert_eq!(outbox_state(pool, thread).await.0, "failed");
        assert_eq!(token_count(pool).await, 0);
    }

    #[tokio::test]
    async fn forgetting_the_last_identity_is_allowed() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        let done = forget(&st, "telegram", "1", None).await.unwrap();
        assert_eq!((done.removed, done.home_cleared), (1, true));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_identities")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn forget_unknown_sender_and_conversation_are_named_refusals() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        assert!(matches!(
            forget(&st, "telegram", "nobody", None).await,
            Err(ForgetError::UnknownIdentity)
        ));
        assert!(matches!(
            forget(&st, "telegram", "1", Some("nope")).await,
            Err(ForgetError::UnknownConversation)
        ));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_identities")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn set_home_moves_it_and_is_idempotent() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        seed(
            pool,
            "1",
            None,
            "g1",
            "group",
            None,
            false,
            "2026-10-12T09:01:00Z",
        )
        .await;

        let moved = set_home(pool, "telegram", "1", "g1").await.unwrap();
        assert_eq!(
            (moved.home.as_str(), moved.previous.as_deref()),
            ("g1", Some("c1"))
        );
        assert_eq!(
            home_identity(pool, "telegram")
                .await
                .unwrap()
                .unwrap()
                .conversation_ref,
            "g1"
        );
        let again = set_home(pool, "telegram", "1", "g1").await.unwrap();
        assert_eq!(
            (again.home.as_str(), again.previous.as_deref()),
            ("g1", Some("g1"))
        );
        let homes: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM channel_identities WHERE home = 1")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(homes, 1);
    }

    #[tokio::test]
    async fn set_home_to_an_unlisted_conversation_is_refused() {
        let st = state().await;
        let pool = &st.pool;
        seed(
            pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        seed(
            pool,
            "2",
            None,
            "c2",
            "private",
            None,
            false,
            "2026-10-12T09:01:00Z",
        )
        .await;
        assert!(matches!(
            set_home(pool, "telegram", "1", "c2").await,
            Err(SetHomeError::UnknownConversation)
        ));
        assert!(matches!(
            set_home(pool, "telegram", "1", "g9:3").await,
            Err(SetHomeError::UnknownConversation)
        ));
        assert!(matches!(
            set_home(pool, "telegram", "nobody", "c1").await,
            Err(SetHomeError::UnknownIdentity)
        ));
        assert_eq!(
            home_identity(pool, "telegram")
                .await
                .unwrap()
                .unwrap()
                .conversation_ref,
            "c1"
        );
    }

    #[tokio::test]
    async fn set_primary_none_clears() {
        let st = state().await;
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram' WHERE id = 1")
            .execute(&st.pool)
            .await
            .unwrap();
        let set = set_primary(&st, None).await.unwrap();
        assert_eq!((set.primary, set.reseeded), (None, false));
        assert_eq!(stored_primary(&st.pool).await, None);
    }

    #[tokio::test]
    async fn set_primary_unknown_channel_is_refused() {
        let st = state().await;
        assert!(matches!(
            set_primary(&st, Some("nope")).await,
            Err(PrimaryError::UnknownChannel)
        ));
    }

    #[tokio::test]
    async fn set_primary_to_a_channel_without_a_home_is_refused() {
        let st = state().await;
        assert!(matches!(
            set_primary(&st, Some("telegram")).await,
            Err(PrimaryError::NoHomeConversation)
        ));
        assert_eq!(stored_primary(&st.pool).await, None);
    }

    #[tokio::test]
    async fn switching_primary_to_a_channel_without_its_secrets_is_refused() {
        let st = state().await;
        seed(
            &st.pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        assert!(matches!(
            set_primary(&st, Some("telegram")).await,
            Err(PrimaryError::ChannelNotConfigured)
        ));
        assert_eq!(stored_primary(&st.pool).await, None);
        let seeded: Option<i64> = sqlx::query_scalar(
            "SELECT feed_cursor FROM channel_routing_state WHERE channel = 'telegram'",
        )
        .fetch_optional(&st.pool)
        .await
        .unwrap();
        assert_eq!(seeded, None, "a refused switch reseeds nothing");
    }

    /// A state where `telegram` has its secrets and a home, and the feed has moved on.
    async fn switchable() -> (AppState, i64) {
        let st = state().await;
        st.channels
            .presence
            .set_secrets_present("telegram", Vec::new());
        seed(
            &st.pool,
            "1",
            None,
            "c1",
            "private",
            None,
            true,
            "2026-10-12T09:00:00Z",
        )
        .await;
        let old = nucleos_base::feed::append(&st.pool, None, "summary", "old news", None, None)
            .await
            .unwrap();
        (st, old)
    }

    #[tokio::test]
    async fn switching_primary_reseeds_and_reports_it() {
        let (st, old) = switchable().await;
        let set = set_primary(&st, Some("telegram")).await.unwrap();
        assert_eq!((set.primary, set.reseeded), (Some("telegram"), true));
        assert_eq!(stored_primary(&st.pool).await.as_deref(), Some("telegram"));
        let cursor: i64 = sqlx::query_scalar(
            "SELECT feed_cursor FROM channel_routing_state WHERE channel = 'telegram'",
        )
        .fetch_one(&st.pool)
        .await
        .unwrap();
        assert_eq!(cursor, old, "the cursor sits at the newest line");

        // Asking again is the no-op of E.3: no second reseed.
        let again = set_primary(&st, Some("telegram")).await.unwrap();
        assert_eq!((again.primary, again.reseeded), (Some("telegram"), false));
    }

    #[tokio::test]
    async fn primary_switch_does_not_announce_the_backlog() {
        let (st, _) = switchable().await;
        set_primary(&st, Some("telegram")).await.unwrap();
        let wanted = routing::Wanted {
            feed: true,
            ..routing::Wanted::default()
        };
        let report = routing::route_once(&st, Utc::now(), wanted).await.unwrap();
        assert_eq!(report.feed, 0, "the line from before the switch is history");

        nucleos_base::feed::append(&st.pool, None, "summary", "fresh news", None, None)
            .await
            .unwrap();
        let report = routing::route_once(&st, Utc::now(), wanted).await.unwrap();
        assert_eq!(report.feed, 1);
        let sent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_outbox")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(sent, 1);
    }

    #[tokio::test]
    async fn setting_the_current_primary_is_a_noop() {
        let st = state().await;
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram' WHERE id = 1")
            .execute(&st.pool)
            .await
            .unwrap();
        let set = set_primary(&st, Some("telegram")).await.unwrap();
        assert_eq!((set.primary, set.reseeded), (Some("telegram"), false));
        assert_eq!(stored_primary(&st.pool).await.as_deref(), Some("telegram"));
    }
}
