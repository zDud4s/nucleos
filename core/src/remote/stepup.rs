//! One-time browser step-up tickets. A ticket is an approval request, never a session.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use serde::Serialize;
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::{STEP_UP_WINDOW, hash_secret, stamp};

pub const TICKET_TTL: Duration = Duration::minutes(5);

/// The fixed set of grave actions a ticket may describe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepUpAction {
    MoveChat,
}

impl StepUpAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::MoveChat => "Move a chat",
        }
    }
}

impl std::str::FromStr for StepUpAction {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "move_chat" => Ok(Self::MoveChat),
            _ => Err(()),
        }
    }
}

#[derive(Debug)]
pub enum TicketError {
    NotOwner,
    InvalidAction,
    Invalid,
    Storage(sqlx::Error),
}

impl From<sqlx::Error> for TicketError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error)
    }
}

#[derive(Serialize)]
pub struct CreatedTicket {
    pub ticket: String,
    pub device_name: String,
    pub label: String,
    pub code: String,
    pub expires_at: String,
}

#[derive(Serialize)]
pub struct TicketInfo {
    pub device_id: i64,
    pub device_name: String,
    pub label: String,
    pub code: String,
    pub credentials: Vec<super::passkeys::Passkey>,
    pub can_register: bool,
}

fn fresh_ticket() -> String {
    URL_SAFE_NO_PAD.encode(rand::rng().random::<[u8; 32]>())
}

fn comparison_code() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000))
}

pub async fn create(
    pool: &SqlitePool,
    device_id: i64,
    caller_is_owner: bool,
    action: StepUpAction,
    now: DateTime<Utc>,
) -> Result<CreatedTicket, TicketError> {
    if !caller_is_owner {
        return Err(TicketError::NotOwner);
    }
    let name: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM remote_devices WHERE id = ? AND revoked_at IS NULL AND owner = 1 AND access_level = 'admin'",
    )
    .bind(device_id)
    .fetch_optional(pool)
    .await?;
    let Some((device_name,)) = name else {
        return Err(TicketError::NotOwner);
    };
    let ticket = fresh_ticket();
    let code = comparison_code();
    let expires_at = stamp(now + TICKET_TTL);
    let label = action.label();
    let now_text = stamp(now);
    let mut tx = pool.begin().await?;
    // Tickets are short lived. A fresh request replaces this device's unfinished ticket, so a
    // compromised session cannot accumulate valid prompts and an older browser page goes inert.
    sqlx::query("DELETE FROM remote_stepup_tickets WHERE expires_at <= ?")
        .bind(&now_text)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM remote_stepup_tickets WHERE device_id = ? AND consumed_at IS NULL AND expires_at > ?",
    )
    .bind(device_id)
    .bind(&now_text)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO remote_stepup_tickets (ticket_hash, device_id, label, code, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(hash_secret(&ticket))
    .bind(device_id)
    .bind(label)
    .bind(&code)
    .bind(&now_text)
    .bind(&expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreatedTicket {
        ticket,
        device_name,
        label: label.to_owned(),
        code,
        expires_at,
    })
}

pub async fn info(
    pool: &SqlitePool,
    ticket: &str,
    now: DateTime<Utc>,
) -> Result<TicketInfo, TicketError> {
    let row: Option<(i64, String, String, String)> = sqlx::query_as(
        "SELECT d.id, d.name, t.label, t.code FROM remote_stepup_tickets t \
         JOIN remote_devices d ON d.id = t.device_id \
         WHERE t.ticket_hash = ? AND t.consumed_at IS NULL AND t.expires_at > ? \
         AND d.revoked_at IS NULL AND d.owner = 1 AND d.access_level = 'admin'",
    )
    .bind(hash_secret(ticket))
    .bind(stamp(now))
    .fetch_optional(pool)
    .await?;
    let Some((device_id, device_name, label, code)) = row else {
        return Err(TicketError::Invalid);
    };
    let Some(standing) = super::passkeys::standing(pool, device_id, now).await? else {
        return Err(TicketError::Invalid);
    };
    let can_register = standing.can_register();
    Ok(TicketInfo {
        device_id,
        device_name,
        label,
        code,
        credentials: standing.passkeys,
        can_register,
    })
}

/// Consumes exactly one live ticket and opens only the ticket device's normal five-minute window.
pub async fn consume(
    pool: &SqlitePool,
    ticket: &str,
    now: DateTime<Utc>,
) -> Result<(), TicketError> {
    let now_text = stamp(now);
    let mut tx = pool.begin().await?;
    let id: Option<(i64,)> = sqlx::query_as(
        "UPDATE remote_stepup_tickets SET consumed_at = ? WHERE ticket_hash = ? AND consumed_at IS NULL \
         AND expires_at > ? AND EXISTS (SELECT 1 FROM remote_devices d WHERE d.id = device_id \
         AND d.revoked_at IS NULL AND d.owner = 1 AND d.access_level = 'admin' AND EXISTS \
         (SELECT 1 FROM remote_passkeys p WHERE p.device_id = d.id)) RETURNING device_id",
    )
    .bind(&now_text)
    .bind(hash_secret(ticket))
    .bind(&now_text)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((device_id,)) = id else {
        return Err(TicketError::Invalid);
    };
    if !grant_step_up_in_transaction(&mut tx, device_id, now).await? {
        return Err(TicketError::Invalid);
    }
    tx.commit().await?;
    Ok(())
}

/// This is [`super::grant_step_up`] against an existing transaction, so consuming a ticket and
/// opening the device window are one atomic operation.
async fn grant_step_up_in_transaction(
    tx: &mut Transaction<'_, Sqlite>,
    device_id: i64,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE remote_devices SET stepup_until = ? WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(stamp(now + STEP_UP_WINDOW))
    .bind(device_id)
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected() == 1)
}
