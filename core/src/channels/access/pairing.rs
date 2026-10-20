//! Pairing codes: mint, status, cancel and redeem (Anexo D D.7, Anexo E E.4).
//!
//! A code is six digits from the system RNG, valid for `PAIR_TTL`, and stored only as
//! `sha256(channel + code)`. A channel has at most one live code (the schema enforces it), so
//! `pair` compares against that one row only.

use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngExt;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::{MAX_PAIR_ATTEMPTS, PAIR_TTL};
use crate::channels::contract::{Conversation, ConversationKey, ConversationKind, OutPart, Sender};
use crate::channels::outbox::{self, Class, NewAction};
use crate::channels::strings;
use crate::state::AppState;

/// How long after `expires_at` the status still reports a terminal state; then it reads `none`.
const STATUS_GRACE: chrono::Duration = chrono::Duration::minutes(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairOutcome {
    /// Registered. `home` is true when the channel had NO home conversation before this pairing
    /// (not "the first conversation": after `forget` cleared the home, the next pairing takes it).
    Paired { home: bool },
    /// Wrong, expired, used, cancelled, superseded or over `MAX_PAIR_ATTEMPTS`.
    Refused,
}

/// What `POST /channels/{channel}/pairing` answers (201, E.4). The code leaves the núcleo only here.
#[derive(Debug, serde::Serialize)]
pub struct MintedCode {
    pub code: String,
    pub command: String,
    pub expires_at: DateTime<Utc>,
    pub attempts_left: u32,
}

/// The identity a code registered, as `GET /channels/{channel}/pairing` shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PairedIdentity {
    pub sender_ref: String,
    pub sender_name: Option<String>,
    pub conversation_ref: String,
    pub conversation_kind: ConversationKind,
    pub parent: Option<String>,
    pub home: bool,
}

/// `GET /channels/{channel}/pairing` (E.4): the state of the channel's most recent code.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PairingStatus {
    Waiting {
        expires_at: DateTime<Utc>,
        attempts_left: u32,
    },
    Paired {
        paired_at: DateTime<Utc>,
        identity: PairedIdentity,
    },
    Expired {
        expires_at: DateTime<Utc>,
    },
    Exhausted {
        attempts_left: u32,
    },
    Superseded,
    None,
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `sha256(channel + code)` as lowercase hex.
fn hash_code(channel: &str, code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(channel.as_bytes());
    hasher.update(code.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn kind_from_text(text: &str) -> Option<ConversationKind> {
    match text {
        "private" => Some(ConversationKind::Private),
        "group" => Some(ConversationKind::Group),
        "thread" => Some(ConversationKind::Thread),
        _ => None,
    }
}

fn kind_text(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Private => "private",
        ConversationKind::Group => "group",
        ConversationKind::Thread => "thread",
    }
}

/// Mints a code and supersedes the channel's previous live one, in one transaction. The code is
/// returned here and nowhere else: only its hash is stored.
pub async fn mint_pairing_code(
    pool: &SqlitePool,
    channel: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<MintedCode> {
    let number: u32 = rand::rng().random_range(0..1_000_000u32);
    let code = format!("{number:06}");
    let created = stamp(now);
    let expires = stamp(now + PAIR_TTL);
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE channel_pairing_codes SET superseded_at = ? WHERE channel = ? \
         AND superseded_at IS NULL AND cancelled_at IS NULL AND paired_at IS NULL",
    )
    .bind(&created)
    .bind(channel)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO channel_pairing_codes (channel, code_hash, created_at, expires_at, attempts) \
         VALUES (?, ?, ?, ?, 0)",
    )
    .bind(channel)
    .bind(hash_code(channel, &code))
    .bind(&created)
    .bind(&expires)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(MintedCode {
        command: strings::pair_command(&code),
        code,
        expires_at: parse_time(&expires).unwrap_or(now + PAIR_TTL),
        attempts_left: MAX_PAIR_ATTEMPTS,
    })
}

/// Cancels the channel's live code. Idempotent: no live code is not an error.
pub async fn cancel_pairing(
    pool: &SqlitePool,
    channel: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE channel_pairing_codes SET cancelled_at = ? WHERE channel = ? \
         AND superseded_at IS NULL AND cancelled_at IS NULL AND paired_at IS NULL",
    )
    .bind(stamp(now))
    .bind(channel)
    .execute(pool)
    .await?;
    Ok(())
}

type CodeRow = (
    String,         // expires_at
    Option<String>, // superseded_at
    Option<String>, // cancelled_at
    Option<String>, // paired_at
    i64,            // attempts
    Option<String>, // paired_sender_ref
    Option<String>, // paired_conversation_ref
);

/// The state of the channel's most recent code. `None` when there is no code, when it was
/// cancelled, or when more than ten minutes passed since its `expires_at`.
pub async fn pairing_status(
    pool: &SqlitePool,
    channel: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<PairingStatus> {
    let row: Option<CodeRow> = sqlx::query_as(
        "SELECT expires_at, superseded_at, cancelled_at, paired_at, attempts, \
         paired_sender_ref, paired_conversation_ref \
         FROM channel_pairing_codes WHERE channel = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(channel)
    .fetch_optional(pool)
    .await?;
    let Some((expires, superseded, cancelled, paired, attempts, sender_ref, conversation_ref)) =
        row
    else {
        return Ok(PairingStatus::None);
    };
    let Some(expires_at) = parse_time(&expires) else {
        return Ok(PairingStatus::None);
    };
    if cancelled.is_some() || now >= expires_at + STATUS_GRACE {
        return Ok(PairingStatus::None);
    }
    let attempts_left = MAX_PAIR_ATTEMPTS.saturating_sub(attempts.max(0) as u32);
    if let Some(paired_text) = paired {
        let (Some(sender_ref), Some(conversation_ref)) = (sender_ref, conversation_ref) else {
            return Ok(PairingStatus::None);
        };
        let identity: Option<(Option<String>, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT sender_name, conversation_kind, parent, home FROM channel_identities \
             WHERE channel = ? AND sender_ref = ? AND conversation_ref = ?",
        )
        .bind(channel)
        .bind(&sender_ref)
        .bind(&conversation_ref)
        .fetch_optional(pool)
        .await?;
        // The identity may have been forgotten since; there is nothing left to show then.
        let Some((sender_name, kind, parent, home)) = identity else {
            return Ok(PairingStatus::None);
        };
        let (Some(conversation_kind), Some(paired_at)) =
            (kind_from_text(&kind), parse_time(&paired_text))
        else {
            return Ok(PairingStatus::None);
        };
        return Ok(PairingStatus::Paired {
            paired_at,
            identity: PairedIdentity {
                sender_ref,
                sender_name,
                conversation_ref,
                conversation_kind,
                parent,
                home: home == 1,
            },
        });
    }
    if superseded.is_some() {
        return Ok(PairingStatus::Superseded);
    }
    if attempts_left == 0 {
        return Ok(PairingStatus::Exhausted { attempts_left: 0 });
    }
    if now >= expires_at {
        return Ok(PairingStatus::Expired { expires_at });
    }
    Ok(PairingStatus::Waiting {
        expires_at,
        attempts_left,
    })
}

/// `/pair <code>`, called by `ingest` BEFORE `admit`. Compares `sha256(channel + code)` with the
/// channel's ONE live code. On success, in one transaction: the code is marked used, the identity
/// rows are written (a thread writes its parent too, `home = 0`) and the confirmation is queued.
/// A wrong code counts an attempt; an expired, used, cancelled, superseded or exhausted code is
/// refused without counting.
pub async fn pair(
    state: &AppState,
    channel: &'static str,
    code: &str,
    conversation: &Conversation,
    sender: &Sender,
    now: DateTime<Utc>,
) -> sqlx::Result<PairOutcome> {
    let mut tx = state.pool.begin().await?;
    let live: Option<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, code_hash, expires_at, attempts FROM channel_pairing_codes \
         WHERE channel = ? AND superseded_at IS NULL AND cancelled_at IS NULL \
         AND paired_at IS NULL",
    )
    .bind(channel)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, code_hash, expires, attempts)) = live else {
        return Ok(PairOutcome::Refused);
    };
    let live_deadline = parse_time(&expires).is_some_and(|expires_at| now < expires_at);
    if !live_deadline || attempts >= i64::from(MAX_PAIR_ATTEMPTS) {
        return Ok(PairOutcome::Refused);
    }
    if hash_code(channel, code.trim()) != code_hash {
        sqlx::query("UPDATE channel_pairing_codes SET attempts = attempts + 1 WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        state.channels.presence.drop_pairing_failure(channel);
        return Ok(PairOutcome::Refused);
    }

    let marked = sqlx::query(
        "UPDATE channel_pairing_codes SET paired_at = ?, paired_sender_ref = ?, \
         paired_conversation_ref = ? WHERE id = ? AND paired_at IS NULL",
    )
    .bind(stamp(now))
    .bind(&sender.reference)
    .bind(&conversation.reference)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if marked.rows_affected() != 1 {
        return Ok(PairOutcome::Refused);
    }

    let had_home: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM channel_identities WHERE channel = ? AND home = 1")
            .bind(channel)
            .fetch_optional(&mut *tx)
            .await?;
    let takes_home = had_home.is_none();

    let parent = match (conversation.kind, &conversation.parent) {
        (ConversationKind::Thread, Some(parent)) => Some(parent.as_str()),
        _ => None,
    };
    if let Some(parent) = parent {
        // The parent of a thread is a group chat (a private chat has no threads here).
        write_identity(
            &mut tx,
            channel,
            sender,
            parent,
            ConversationKind::Group,
            None,
            false,
            now,
        )
        .await?;
    }
    write_identity(
        &mut tx,
        channel,
        sender,
        &conversation.reference,
        conversation.kind,
        parent,
        takes_home,
        now,
    )
    .await?;

    outbox::enqueue(
        &mut tx,
        &ConversationKey::new(channel, conversation.reference.clone()),
        NewAction::Send {
            reply_to: None,
            parts: vec![OutPart::Text {
                text: strings::render(&strings::Text::Paired),
            }],
            buttons: None,
            silent: false,
        },
        Class::Command,
        None,
        now,
    )
    .await?;
    tx.commit().await?;
    outbox::wake(state, channel);
    Ok(PairOutcome::Paired { home: takes_home })
}

#[allow(clippy::too_many_arguments)]
async fn write_identity(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    channel: &str,
    sender: &Sender,
    conversation_ref: &str,
    kind: ConversationKind,
    parent: Option<&str>,
    home: bool,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO channel_identities \
         (channel, sender_ref, sender_name, conversation_ref, conversation_kind, parent, role, \
          home, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, 'owner', ?, ?) \
         ON CONFLICT (channel, sender_ref, conversation_ref) DO UPDATE SET \
          home = MAX(home, excluded.home), \
          sender_name = COALESCE(excluded.sender_name, sender_name)",
    )
    .bind(channel)
    .bind(&sender.reference)
    .bind(&sender.name)
    .bind(conversation_ref)
    .bind(kind_text(kind))
    .bind(parent)
    .bind(home as i64)
    .bind(stamp(now))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::testkit::state;

    const NOW: &str = "2026-10-20T10:00:00Z";

    fn now() -> DateTime<Utc> {
        NOW.parse().unwrap()
    }

    fn private(reference: &str) -> Conversation {
        Conversation {
            reference: reference.to_string(),
            kind: ConversationKind::Private,
            parent: None,
        }
    }

    fn thread(reference: &str, parent: &str) -> Conversation {
        Conversation {
            reference: reference.to_string(),
            kind: ConversationKind::Thread,
            parent: Some(parent.to_string()),
        }
    }

    fn who(reference: &str) -> Sender {
        Sender {
            reference: reference.to_string(),
            name: Some("Duarte".to_string()),
        }
    }

    fn wrong(code: &str) -> String {
        if code == "000000" {
            "000001".to_string()
        } else {
            "000000".to_string()
        }
    }

    async fn rows(pool: &SqlitePool) -> Vec<(String, String, Option<String>, i64)> {
        sqlx::query_as(
            "SELECT conversation_ref, conversation_kind, parent, home FROM channel_identities \
             ORDER BY conversation_ref",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn mint_returns_six_digits_command_and_deadline() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        assert_eq!(minted.code.len(), 6);
        assert!(minted.code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(minted.command, format!("/pair {}", minted.code));
        assert_eq!(minted.expires_at, now() + PAIR_TTL);
        assert_eq!(minted.attempts_left, MAX_PAIR_ATTEMPTS);
    }

    #[tokio::test]
    async fn only_the_hash_is_stored() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let stored: String = sqlx::query_scalar("SELECT code_hash FROM channel_pairing_codes")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_ne!(stored, minted.code);
        assert_eq!(stored.len(), 64);
        assert_eq!(stored, hash_code("telegram", &minted.code));
        assert_ne!(stored, hash_code("fake", &minted.code));
    }

    #[tokio::test]
    async fn a_second_mint_supersedes_the_first() {
        let st = state().await;
        let first = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let second = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let superseded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM channel_pairing_codes WHERE superseded_at IS NOT NULL",
        )
        .fetch_one(&st.pool)
        .await
        .unwrap();
        assert_eq!(superseded, 1);
        if first.code != second.code {
            let outcome = pair(
                &st,
                "telegram",
                &first.code,
                &private("c1"),
                &who("u1"),
                now(),
            )
            .await
            .unwrap();
            assert_eq!(outcome, PairOutcome::Refused);
        }
        let outcome = pair(
            &st,
            "telegram",
            &second.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Paired { home: true });
    }

    #[tokio::test]
    async fn a_valid_pair_registers_a_private_owner_with_home() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let outcome = pair(
            &st,
            "telegram",
            &minted.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Paired { home: true });
        assert_eq!(
            rows(&st.pool).await,
            vec![("c1".to_string(), "private".to_string(), None, 1)]
        );
        let (role, name): (String, Option<String>) =
            sqlx::query_as("SELECT role, sender_name FROM channel_identities")
                .fetch_one(&st.pool)
                .await
                .unwrap();
        assert_eq!((role.as_str(), name.as_deref()), ("owner", Some("Duarte")));
        assert!(matches!(
            pairing_status(&st.pool, "telegram", now()).await.unwrap(),
            PairingStatus::Paired { .. }
        ));
    }

    #[tokio::test]
    async fn a_valid_pair_in_a_thread_writes_parent_and_thread_rows() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let outcome = pair(
            &st,
            "telegram",
            &minted.code,
            &thread("g1:7", "g1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Paired { home: true });
        assert_eq!(
            rows(&st.pool).await,
            vec![
                ("g1".to_string(), "group".to_string(), None, 0),
                (
                    "g1:7".to_string(),
                    "thread".to_string(),
                    Some("g1".to_string()),
                    1
                ),
            ]
        );
        match pairing_status(&st.pool, "telegram", now()).await.unwrap() {
            PairingStatus::Paired { identity, .. } => {
                assert_eq!(identity.conversation_ref, "g1:7");
                assert_eq!(identity.conversation_kind, ConversationKind::Thread);
                assert_eq!(identity.parent.as_deref(), Some("g1"));
                assert!(identity.home);
            }
            other => panic!("expected paired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn home_is_not_taken_when_the_channel_already_has_one() {
        let st = state().await;
        let first = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        pair(
            &st,
            "telegram",
            &first.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        let second = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let outcome = pair(
            &st,
            "telegram",
            &second.code,
            &thread("g1:7", "g1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Paired { home: false });
        let homes: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM channel_identities WHERE home = 1")
                .fetch_one(&st.pool)
                .await
                .unwrap();
        assert_eq!(homes, 1);
        assert_eq!(
            crate::channels::access::home_conversation(&st.pool, "telegram")
                .await
                .unwrap(),
            Some("c1".to_string())
        );
    }

    #[tokio::test]
    async fn after_forget_cleared_the_home_the_next_pairing_takes_it() {
        let st = state().await;
        let first = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        pair(
            &st,
            "telegram",
            &first.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE channel_identities SET home = 0 WHERE channel = 'telegram'")
            .execute(&st.pool)
            .await
            .unwrap();
        let second = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let outcome = pair(
            &st,
            "telegram",
            &second.code,
            &private("c2"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Paired { home: true });
        assert_eq!(
            crate::channels::access::home_conversation(&st.pool, "telegram")
                .await
                .unwrap(),
            Some("c2".to_string())
        );
    }

    #[tokio::test]
    async fn a_wrong_code_counts_an_attempt_and_the_fifth_exhausts() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let bad = wrong(&minted.code);
        for tried in 1..=MAX_PAIR_ATTEMPTS {
            let outcome = pair(&st, "telegram", &bad, &private("c1"), &who("u1"), now())
                .await
                .unwrap();
            assert_eq!(outcome, PairOutcome::Refused);
            let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM channel_pairing_codes")
                .fetch_one(&st.pool)
                .await
                .unwrap();
            assert_eq!(attempts, i64::from(tried));
        }
        assert_eq!(
            st.channels.presence.drops("telegram").pairing_failures,
            u64::from(MAX_PAIR_ATTEMPTS)
        );
        assert_eq!(
            pairing_status(&st.pool, "telegram", now()).await.unwrap(),
            PairingStatus::Exhausted { attempts_left: 0 }
        );
        // The right code no longer works, and the refusal does not count.
        let outcome = pair(
            &st,
            "telegram",
            &minted.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Refused);
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM channel_pairing_codes")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(attempts, i64::from(MAX_PAIR_ATTEMPTS));
        assert!(rows(&st.pool).await.is_empty());
    }

    #[tokio::test]
    async fn an_expired_code_is_refused_without_counting() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let late = now() + chrono::Duration::minutes(11);
        let outcome = pair(
            &st,
            "telegram",
            &minted.code,
            &private("c1"),
            &who("u1"),
            late,
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Refused);
        let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM channel_pairing_codes")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(attempts, 0);
        assert_eq!(st.channels.presence.drops("telegram").pairing_failures, 0);
    }

    #[tokio::test]
    async fn a_used_code_cannot_pair_twice() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let first = pair(
            &st,
            "telegram",
            &minted.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(first, PairOutcome::Paired { home: true });
        let again = pair(
            &st,
            "telegram",
            &minted.code,
            &private("c9"),
            &who("u9"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(again, PairOutcome::Refused);
        assert_eq!(rows(&st.pool).await.len(), 1);
    }

    #[tokio::test]
    async fn status_reflects_waiting_paired_expired_exhausted_superseded_none() {
        let st = state().await;
        let pool = &st.pool;
        assert_eq!(
            pairing_status(pool, "telegram", now()).await.unwrap(),
            PairingStatus::None
        );

        // waiting
        let minted = mint_pairing_code(pool, "telegram", now()).await.unwrap();
        assert_eq!(
            pairing_status(pool, "telegram", now()).await.unwrap(),
            PairingStatus::Waiting {
                expires_at: now() + PAIR_TTL,
                attempts_left: MAX_PAIR_ATTEMPTS
            }
        );
        pair(
            &st,
            "telegram",
            &wrong(&minted.code),
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(
            pairing_status(pool, "telegram", now()).await.unwrap(),
            PairingStatus::Waiting {
                expires_at: now() + PAIR_TTL,
                attempts_left: MAX_PAIR_ATTEMPTS - 1
            }
        );

        // expired, kept up to ten minutes, then none
        let after = now() + PAIR_TTL + chrono::Duration::minutes(1);
        assert_eq!(
            pairing_status(pool, "telegram", after).await.unwrap(),
            PairingStatus::Expired {
                expires_at: now() + PAIR_TTL
            }
        );
        let much_later = now() + PAIR_TTL + chrono::Duration::minutes(10);
        assert_eq!(
            pairing_status(pool, "telegram", much_later).await.unwrap(),
            PairingStatus::None
        );

        // superseded (the latest row carries the mark)
        sqlx::query("UPDATE channel_pairing_codes SET superseded_at = ?")
            .bind(NOW)
            .execute(pool)
            .await
            .unwrap();
        assert_eq!(
            pairing_status(pool, "telegram", now()).await.unwrap(),
            PairingStatus::Superseded
        );

        // exhausted
        sqlx::query("UPDATE channel_pairing_codes SET superseded_at = NULL, attempts = 5")
            .execute(pool)
            .await
            .unwrap();
        assert_eq!(
            pairing_status(pool, "telegram", now()).await.unwrap(),
            PairingStatus::Exhausted { attempts_left: 0 }
        );

        // paired
        let fresh = mint_pairing_code(pool, "telegram", now()).await.unwrap();
        pair(
            &st,
            "telegram",
            &fresh.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert!(matches!(
            pairing_status(pool, "telegram", now()).await.unwrap(),
            PairingStatus::Paired { .. }
        ));

        // another channel is untouched
        assert_eq!(
            pairing_status(pool, "fake", now()).await.unwrap(),
            PairingStatus::None
        );
    }

    #[tokio::test]
    async fn cancel_is_idempotent() {
        let st = state().await;
        cancel_pairing(&st.pool, "telegram", now()).await.unwrap();
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        cancel_pairing(&st.pool, "telegram", now()).await.unwrap();
        cancel_pairing(&st.pool, "telegram", now()).await.unwrap();
        assert_eq!(
            pairing_status(&st.pool, "telegram", now()).await.unwrap(),
            PairingStatus::None
        );
        let outcome = pair(
            &st,
            "telegram",
            &minted.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Refused);
        // A cancelled code frees the slot for a new one.
        let next = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        let outcome = pair(
            &st,
            "telegram",
            &next.code,
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, PairOutcome::Paired { home: true });
    }

    #[tokio::test]
    async fn the_confirmation_is_enqueued_in_the_same_transaction() {
        let st = state().await;
        let minted = mint_pairing_code(&st.pool, "telegram", now())
            .await
            .unwrap();
        pair(
            &st,
            "telegram",
            &wrong(&minted.code),
            &private("c1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_outbox")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(queued, 0, "a refused attempt queues nothing");

        pair(
            &st,
            "telegram",
            &minted.code,
            &thread("g1:7", "g1"),
            &who("u1"),
            now(),
        )
        .await
        .unwrap();
        let (conversation, action, class, payload): (String, String, String, String) =
            sqlx::query_as("SELECT conversation, action, class, payload FROM channel_outbox")
                .fetch_one(&st.pool)
                .await
                .unwrap();
        assert_eq!(conversation, "g1:7");
        assert_eq!(action, "send");
        assert_eq!(class, "command");
        assert!(
            payload.contains(&strings::render(&strings::Text::Paired)),
            "{payload}"
        );
        let paired: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM channel_pairing_codes WHERE paired_at IS NOT NULL",
        )
        .fetch_one(&st.pool)
        .await
        .unwrap();
        assert_eq!(paired, 1);
    }
}
