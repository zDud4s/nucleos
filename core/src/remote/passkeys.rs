//! The passkeys of an owner's remote device, as the daemon keeps them.
//!
//! The WebAuthn ceremony is the gateway's; this module only stores the credential it produced and
//! decides the two rules that must hold even if the gateway is fooled:
//!
//! 1. A device may register a passkey only if it is an owner AND (it has none yet AND was paired
//!    within `FIRST_PASSKEY_WINDOW`, OR it is stepped up right now). Otherwise a stolen owner session could add its own passkey and defeat the
//!    step-up.
//! 2. A step-up is opened only for a live owner device that has a passkey.
//!
//! The credential JSON is opaque here (a public key and a counter) and is never logged.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use super::{STEP_UP_WINDOW, grant_step_up, parse_stamp, stamp};
use crate::auth::ApiTokenLevel;

/// How long after a device is paired its first passkey may be registered. A device that misses it
/// cannot enrol: its owner makes a new owner pairing code on the main machine instead.
pub const FIRST_PASSKEY_WINDOW: Duration = Duration::minutes(15);
/// More than any WebAuthn credential record is.
const CREDENTIAL_LIMIT: usize = 16 * 1024;
/// Passkeys one device may hold: a laptop's own and a phone's, with room to spare.
const PER_DEVICE_LIMIT: i64 = 8;

/// One stored credential of a device.
#[derive(Debug, Clone, Serialize)]
pub struct Passkey {
    pub id: i64,
    /// The gateway's credential record, as the gateway wrote it.
    pub credential: String,
}

/// Why a passkey call was refused. The words are the contract with the gateway.
#[derive(Debug)]
pub enum PasskeyError {
    /// No live (unrevoked) device has that id.
    NoDevice,
    NotOwner,
    /// The first passkey is only registered within [`FIRST_PASSKEY_WINDOW`] of the pairing.
    PairingWindowClosed,
    /// Registering a further passkey needs a live step-up.
    StepUpRequired,
    /// A step-up needs a passkey to stand on.
    NoPasskey,
    /// The credential to replace is not this device's, or the record is not acceptable.
    BadRecord,
    Storage(sqlx::Error),
}

impl From<sqlx::Error> for PasskeyError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error)
    }
}

/// What the daemon knows about a live device, for the gateway.
#[derive(Debug, Clone)]
pub struct Standing {
    pub owner: bool,
    pub stepped_up: bool,
    /// Paired no more than [`FIRST_PASSKEY_WINDOW`] ago (false for an unreadable `created_at`).
    pub first_window_open: bool,
    pub passkeys: Vec<Passkey>,
}

impl Standing {
    /// The registration rule: an owner with no passkey yet and still inside the first-passkey
    /// window, or an owner stepped up right now.
    pub fn can_register(&self) -> bool {
        self.owner && ((self.passkeys.is_empty() && self.first_window_open) || self.stepped_up)
    }
}

/// Whether a stored `created_at` is within the first-passkey window at `now`. Fails closed: a
/// malformed value, or one in the future, is outside it.
fn first_window_is_open(created_at: &str, now: DateTime<Utc>) -> bool {
    parse_stamp(created_at)
        .is_some_and(|created| created <= now && created > now - FIRST_PASSKEY_WINDOW)
}

/// The device's standing and credentials, or `None` for an unknown or revoked device.
pub async fn standing(
    pool: &SqlitePool,
    device_id: i64,
    now: DateTime<Utc>,
) -> Result<Option<Standing>, sqlx::Error> {
    let row: Option<(String, i64, Option<String>, String)> = sqlx::query_as(
        "SELECT access_level, owner, stepup_until, created_at FROM remote_devices \
         WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(device_id)
    .fetch_optional(pool)
    .await?;
    let Some((level, owner, stepup, created)) = row else {
        return Ok(None);
    };
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, credential_json FROM remote_passkeys WHERE device_id = ? ORDER BY id",
    )
    .bind(device_id)
    .fetch_all(pool)
    .await?;
    Ok(Some(Standing {
        owner: owner != 0 && level == ApiTokenLevel::Admin.as_str(),
        stepped_up: super::step_up_is_live(stepup.as_deref(), now),
        first_window_open: first_window_is_open(&created, now),
        passkeys: rows
            .into_iter()
            .map(|(id, credential)| Passkey { id, credential })
            .collect(),
    }))
}

fn acceptable(credential_json: &str) -> bool {
    credential_json.len() <= CREDENTIAL_LIMIT
        && serde_json::from_str::<serde_json::Value>(credential_json)
            .is_ok_and(|value| value.is_object())
}

/// Stores a new credential for the device when the registration rule allows it. The rule is part of
/// the INSERT, so a step-up that lapses between a check and the write cannot be used.
pub async fn add_passkey(
    pool: &SqlitePool,
    device_id: i64,
    credential_json: &str,
    now: DateTime<Utc>,
) -> Result<i64, PasskeyError> {
    if !acceptable(credential_json) {
        return Err(PasskeyError::BadRecord);
    }
    let done = sqlx::query(
        "INSERT INTO remote_passkeys (device_id, credential_json, created_at) \
         SELECT d.id, ?, ? FROM remote_devices d \
         WHERE d.id = ? AND d.revoked_at IS NULL AND d.owner = 1 AND d.access_level = 'admin' \
           AND (SELECT COUNT(*) FROM remote_passkeys p WHERE p.device_id = d.id) < ? \
           AND ((NOT EXISTS (SELECT 1 FROM remote_passkeys p WHERE p.device_id = d.id) \
                 AND length(d.created_at) = ? AND d.created_at > ? AND d.created_at <= ?) \
                OR (d.stepup_until > ? AND d.stepup_until <= ?))",
    )
    .bind(credential_json)
    .bind(stamp(now))
    .bind(device_id)
    .bind(PER_DEVICE_LIMIT)
    // Stamps are fixed width, so string order is time order; anything of another width is refused.
    .bind(i64::try_from(stamp(now).len()).unwrap_or(0))
    .bind(stamp(now - FIRST_PASSKEY_WINDOW))
    .bind(stamp(now))
    .bind(stamp(now))
    .bind(stamp(now + STEP_UP_WINDOW))
    .execute(pool)
    .await?;
    if done.rows_affected() == 1 {
        return Ok(done.last_insert_rowid());
    }
    // Refused: say why, from the same facts the INSERT looked at.
    match standing(pool, device_id, now).await? {
        None => Err(PasskeyError::NoDevice),
        Some(s) if !s.owner => Err(PasskeyError::NotOwner),
        Some(s) if s.can_register() => Err(PasskeyError::BadRecord),
        Some(s) if s.passkeys.is_empty() => Err(PasskeyError::PairingWindowClosed),
        Some(_) => Err(PasskeyError::StepUpRequired),
    }
}

/// Replaces the record of one of the device's own credentials (the new sign count after a verify).
/// No registration rule applies: nothing is added.
pub async fn replace_passkey(
    pool: &SqlitePool,
    device_id: i64,
    passkey_id: i64,
    credential_json: &str,
) -> Result<(), PasskeyError> {
    if !acceptable(credential_json) {
        return Err(PasskeyError::BadRecord);
    }
    let done = sqlx::query(
        "UPDATE remote_passkeys SET credential_json = ? WHERE id = ? AND device_id = ? \
         AND EXISTS (SELECT 1 FROM remote_devices d WHERE d.id = ? AND d.revoked_at IS NULL)",
    )
    .bind(credential_json)
    .bind(passkey_id)
    .bind(device_id)
    .bind(device_id)
    .execute(pool)
    .await?;
    if done.rows_affected() == 1 {
        Ok(())
    } else {
        Err(PasskeyError::BadRecord)
    }
}

/// Opens the step-up window of a live owner device that has a passkey. The caller (the gateway) has
/// already verified the ceremony; this is the daemon's own refusal of what must never be granted.
pub async fn open_step_up(
    pool: &SqlitePool,
    device_id: i64,
    now: DateTime<Utc>,
) -> Result<(), PasskeyError> {
    let Some(found) = standing(pool, device_id, now).await? else {
        return Err(PasskeyError::NoDevice);
    };
    if !found.owner {
        return Err(PasskeyError::NotOwner);
    }
    if found.passkeys.is_empty() {
        return Err(PasskeyError::NoPasskey);
    }
    if grant_step_up(pool, device_id, now).await? {
        Ok(())
    } else {
        Err(PasskeyError::NoDevice)
    }
}
