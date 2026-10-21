//! Button action tokens (flow F, Anexo D.13). Issued beside the message that shows a button,
//! redeemed once, in the conversation they were issued to, while the proposal is still pending.
//! This module never approves or rejects a proposal: `ingest` does that after `Fresh`.

use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use sqlx::{SqliteConnection, SqlitePool};

use crate::channels::contract::ConversationKey;

const TOKEN_PREFIX: &str = "tk_";
const TOKEN_ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const TOKEN_RANDOM_LEN: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAction {
    Approve,
    Reject,
}

impl TokenAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "approve" => Some(Self::Approve),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redeemed {
    /// First use, in the conversation it was issued to.
    Fresh {
        action: TokenAction,
        proposal_id: i64,
    },
    /// Used, unknown, another conversation, or the proposal is no longer pending.
    Stale,
}

fn generate() -> String {
    let mut rng = rand::rng();
    let mut out = String::with_capacity(TOKEN_PREFIX.len() + TOKEN_RANDOM_LEN);
    out.push_str(TOKEN_PREFIX);
    for _ in 0..TOKEN_RANDOM_LEN {
        let byte: u8 = rng.random();
        out.push(TOKEN_ALPHABET[usize::from(byte) % TOKEN_ALPHABET.len()] as char);
    }
    out
}

/// `tk_<random>`. Inside the caller's transaction, beside the `enqueue` that shows the button.
pub async fn issue(
    conn: &mut SqliteConnection,
    key: &ConversationKey,
    action: TokenAction,
    proposal_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<String> {
    let token = generate();
    sqlx::query(
        "INSERT INTO channel_action_tokens \
         (token, channel, conversation, action, proposal_id, created_at, used_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(&token)
    .bind(key.channel)
    .bind(&key.reference)
    .bind(action.as_str())
    .bind(proposal_id)
    .bind(now.to_rfc3339())
    .execute(&mut *conn)
    .await?;
    Ok(token)
}

/// Single use: the `UPDATE … WHERE used_at IS NULL` decides. A token that is not redeemable
/// (unknown, other channel or conversation, used, proposal no longer pending) stays unspent
/// unless it was already used.
pub async fn redeem(
    pool: &SqlitePool,
    key: &ConversationKey,
    token: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Redeemed> {
    let row: Option<(String, String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT channel, conversation, action, proposal_id, used_at \
         FROM channel_action_tokens WHERE token = ?",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    let Some((channel, conversation, action, proposal_id, used_at)) = row else {
        return Ok(Redeemed::Stale);
    };
    if used_at.is_some() || channel != key.channel || conversation != key.reference {
        return Ok(Redeemed::Stale);
    }
    let Some(action) = TokenAction::parse(&action) else {
        return Ok(Redeemed::Stale);
    };
    // Confirm the proposal is still pending before spending the token.
    let pending: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM proposals WHERE id = ? AND status = 'pending'")
            .bind(proposal_id)
            .fetch_optional(pool)
            .await?;
    if pending.is_none() {
        return Ok(Redeemed::Stale);
    }
    let spent = sqlx::query(
        "UPDATE channel_action_tokens SET used_at = ? WHERE token = ? AND used_at IS NULL",
    )
    .bind(now.to_rfc3339())
    .bind(token)
    .execute(pool)
    .await?;
    if spent.rows_affected() == 1 {
        Ok(Redeemed::Fresh {
            action,
            proposal_id,
        })
    } else {
        Ok(Redeemed::Stale)
    }
}

/// Inside `access::forget`'s transaction: deletes the UNUSED tokens of `channel` whose
/// conversation is no longer allowed. Allowed (conservative rule, Q13 = A): equal to a remaining
/// identity's `conversation_ref`, or listed in `project_conversations`. Returns the rows deleted.
pub async fn purge_unallowed(
    conn: &mut SqliteConnection,
    channel: &str,
    project_conversations: &[&str],
) -> sqlx::Result<u64> {
    let candidates: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT conversation FROM channel_action_tokens \
         WHERE channel = ?1 AND used_at IS NULL \
         AND conversation NOT IN \
             (SELECT conversation_ref FROM channel_identities WHERE channel = ?1)",
    )
    .bind(channel)
    .fetch_all(&mut *conn)
    .await?;
    let mut deleted = 0;
    for (conversation,) in candidates {
        if project_conversations.contains(&conversation.as_str()) {
            continue;
        }
        let result = sqlx::query(
            "DELETE FROM channel_action_tokens \
             WHERE channel = ? AND conversation = ? AND used_at IS NULL",
        )
        .bind(channel)
        .bind(&conversation)
        .execute(&mut *conn)
        .await?;
        deleted += result.rows_affected();
    }
    Ok(deleted)
}

/// Rows whose proposal was decided more than 7 days ago, plus unused rows whose proposal no
/// longer exists; called by `run_housekeeping`.
pub async fn prune(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let cutoff = (now - Duration::days(7)).to_rfc3339();
    let result = sqlx::query(
        "DELETE FROM channel_action_tokens WHERE \
         proposal_id IN (SELECT id FROM proposals \
                         WHERE decided_at IS NOT NULL AND decided_at < ?) \
         OR (used_at IS NULL AND proposal_id NOT IN (SELECT id FROM proposals))",
    )
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn proposal(pool: &SqlitePool, status: &str, decided_at: Option<String>) -> i64 {
        sqlx::query(
            "INSERT INTO proposals (kind, status, reasoning, created_at, decided_at) \
             VALUES ('action-approval', ?, 'because', ?, ?)",
        )
        .bind(status)
        .bind(Utc::now().to_rfc3339())
        .bind(decided_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn issue_one(pool: &SqlitePool, key: &ConversationKey, proposal_id: i64) -> String {
        let mut conn = pool.acquire().await.unwrap();
        issue(
            &mut conn,
            key,
            TokenAction::Approve,
            proposal_id,
            Utc::now(),
        )
        .await
        .unwrap()
    }

    async fn count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM channel_action_tokens")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn issued_tokens_match_the_grammar() {
        let pool = crate::testdb::fresh_pool().await;
        let key = ConversationKey::new("telegram", "42");
        let id = proposal(&pool, "pending", None).await;
        for _ in 0..20 {
            let token = issue_one(&pool, &key, id).await;
            assert!(token.len() <= 64, "{token}");
            let rest = token.strip_prefix("tk_").expect("prefix");
            assert!(
                (1..=61).contains(&rest.len())
                    && rest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
                "{token}"
            );
        }
    }

    #[tokio::test]
    async fn a_token_redeems_once() {
        let pool = crate::testdb::fresh_pool().await;
        let key = ConversationKey::new("telegram", "42");
        let id = proposal(&pool, "pending", None).await;
        let token = issue_one(&pool, &key, id).await;
        assert_eq!(
            redeem(&pool, &key, &token, Utc::now()).await.unwrap(),
            Redeemed::Fresh {
                action: TokenAction::Approve,
                proposal_id: id
            }
        );
        assert_eq!(
            redeem(&pool, &key, &token, Utc::now()).await.unwrap(),
            Redeemed::Stale
        );
    }

    #[tokio::test]
    async fn a_token_of_another_conversation_is_stale() {
        let pool = crate::testdb::fresh_pool().await;
        let key = ConversationKey::new("telegram", "42");
        let id = proposal(&pool, "pending", None).await;
        let token = issue_one(&pool, &key, id).await;
        for other in [
            ConversationKey::new("telegram", "43"),
            ConversationKey::new("fake", "42"),
        ] {
            assert_eq!(
                redeem(&pool, &other, &token, Utc::now()).await.unwrap(),
                Redeemed::Stale
            );
        }
        assert_eq!(
            redeem(&pool, &key, "tk_unknown", Utc::now()).await.unwrap(),
            Redeemed::Stale
        );
        // The wrong-conversation attempts did not spend it.
        assert!(matches!(
            redeem(&pool, &key, &token, Utc::now()).await.unwrap(),
            Redeemed::Fresh { .. }
        ));
    }

    #[tokio::test]
    async fn a_token_for_a_decided_proposal_is_stale_and_unspent() {
        let pool = crate::testdb::fresh_pool().await;
        let key = ConversationKey::new("telegram", "42");
        let id = proposal(&pool, "approved", Some(Utc::now().to_rfc3339())).await;
        let token = issue_one(&pool, &key, id).await;
        assert_eq!(
            redeem(&pool, &key, &token, Utc::now()).await.unwrap(),
            Redeemed::Stale
        );
        let used: Option<String> =
            sqlx::query_scalar("SELECT used_at FROM channel_action_tokens WHERE token = ?")
                .bind(&token)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(used.is_none());
    }

    #[tokio::test]
    async fn purge_unallowed_keeps_allowed_and_used_tokens() {
        let pool = crate::testdb::fresh_pool().await;
        let id = proposal(&pool, "pending", None).await;
        sqlx::query(
            "INSERT INTO channel_identities \
             (channel, sender_ref, conversation_ref, conversation_kind, created_at) \
             VALUES ('telegram', 's1', 'kept', 'private', ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
        let kept = issue_one(&pool, &ConversationKey::new("telegram", "kept"), id).await;
        let listed = issue_one(&pool, &ConversationKey::new("telegram", "proj"), id).await;
        let used = issue_one(&pool, &ConversationKey::new("telegram", "gone"), id).await;
        let other_channel = issue_one(&pool, &ConversationKey::new("fake", "gone"), id).await;
        let dropped = issue_one(&pool, &ConversationKey::new("telegram", "gone"), id).await;
        sqlx::query("UPDATE channel_action_tokens SET used_at = ? WHERE token = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(&used)
            .execute(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let deleted = purge_unallowed(&mut conn, "telegram", &["proj"])
            .await
            .unwrap();
        drop(conn);
        assert_eq!(deleted, 1);
        let left: Vec<String> = sqlx::query_scalar("SELECT token FROM channel_action_tokens")
            .fetch_all(&pool)
            .await
            .unwrap();
        for token in [&kept, &listed, &used, &other_channel] {
            assert!(left.contains(token), "{token} should remain");
        }
        assert!(!left.contains(&dropped));
    }

    #[tokio::test]
    async fn prune_drops_old_decided_rows_only() {
        let pool = crate::testdb::fresh_pool().await;
        let key = ConversationKey::new("telegram", "42");
        let now = Utc::now();
        let old = proposal(
            &pool,
            "approved",
            Some((now - Duration::days(8)).to_rfc3339()),
        )
        .await;
        let recent = proposal(
            &pool,
            "approved",
            Some((now - Duration::days(1)).to_rfc3339()),
        )
        .await;
        let pending = proposal(&pool, "pending", None).await;
        issue_one(&pool, &key, old).await;
        issue_one(&pool, &key, recent).await;
        let keeper = issue_one(&pool, &key, pending).await;
        // Never used, and its proposal does not exist any more.
        issue_one(&pool, &key, 999_999).await;
        assert_eq!(count(&pool).await, 4);

        assert_eq!(prune(&pool, now).await.unwrap(), 2);
        assert_eq!(count(&pool).await, 2);
        assert!(matches!(
            redeem(&pool, &key, &keeper, now).await.unwrap(),
            Redeemed::Fresh { .. }
        ));
    }
}
