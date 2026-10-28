//! Secure remote access, domain half: pairing codes, device sessions and the config switch.
//!
//! A browser on another machine reaches the daemon only through the gateway sidecar, and the only
//! thing it ever holds is an opaque session secret in a cookie. This module owns what that secret
//! means: it is minted when a one-time pairing code is exchanged, only its SHA-256 is stored, and
//! every request re-reads the row, so a revocation applies on the next request.
//!
//! Everything takes `now` as an argument; there is no clock in here. All refusals of a code or a
//! session look identical to the caller (`Invalid`), so a probe learns nothing about which of
//! unknown / used / expired / burned / revoked it hit. What a device may REACH is `auth.rs`'s
//! business, and the HTTP surface is `http/remote.rs`'s.

use std::path::Path;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::auth::ApiTokenLevel;

/// How long a pairing code can be exchanged.
pub const PAIRING_TTL: Duration = Duration::seconds(300);
/// Wrong guesses that burn every open code.
pub const MAX_PAIRING_ATTEMPTS: i64 = 5;
/// A session ends this long after it was created, however busy it is.
pub const SESSION_ABSOLUTE: Duration = Duration::days(30);
/// A session ends after this long without a request.
pub const SESSION_IDLE: Duration = Duration::days(14);
/// `last_seen_at` is written at most this often per device.
pub const LAST_SEEN_GRANULARITY: Duration = Duration::seconds(60);

/// The machine-config file, under `~/.nucleos`, that arms remote access.
pub const REMOTE_FILE: &str = "remote.yaml";

/// The `service_tokens` row whose existence means "remote access is armed".
const SERVICE_NAME: &str = "remote";
/// No 0/O/1/I/L: a code is read off one screen and typed on another.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 10;

/// SHA-256 of `secret`, lowercase hex (formatted by hand: there is no hex dependency).
pub fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Upper-cases and drops dashes and whitespace, so `abcde-fghjk` and `ABCDE FGHJK` are one code.
pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_uppercase)
        .collect()
}

/// `1..=64` printable characters (no control characters), after trimming.
pub fn valid_device_name(name: &str) -> bool {
    let name = name.trim();
    let chars = name.chars().count();
    (1..=64).contains(&chars) && name.chars().all(|c| !c.is_control())
}

fn stamp(at: DateTime<Utc>) -> String {
    // Fixed width, so SQL string comparison orders the same way the instants do.
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_stamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// A freshly created pairing code. `code` is shown once and never stored.
#[derive(Debug, Clone, Serialize)]
pub struct CreatedCode {
    /// `XXXXX-XXXXX`.
    pub code: String,
    pub access_level: ApiTokenLevel,
    pub device_name: String,
    pub expires_at: String,
}

/// Creates a pairing code valid for [`PAIRING_TTL`], and sweeps codes expired more than a day ago.
pub async fn create_pairing_code(
    pool: &SqlitePool,
    level: ApiTokenLevel,
    device_name: &str,
    now: DateTime<Utc>,
) -> Result<CreatedCode, sqlx::Error> {
    let raw: String = {
        let mut rng = rand::rng();
        (0..CODE_LEN)
            .map(|_| CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char)
            .collect()
    };
    let expires_at = stamp(now + PAIRING_TTL);
    let device_name = device_name.trim().to_string();

    sqlx::query("DELETE FROM pairing_codes WHERE expires_at < ?")
        .bind(stamp(now - Duration::days(1)))
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO pairing_codes (code_hash, access_level, device_name, expires_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(hash_secret(&raw))
    .bind(level.as_str())
    .bind(&device_name)
    .bind(&expires_at)
    .execute(pool)
    .await?;

    Ok(CreatedCode {
        code: format!("{}-{}", &raw[..CODE_LEN / 2], &raw[CODE_LEN / 2..]),
        access_level: level,
        device_name,
        expires_at,
    })
}

/// What a successful exchange hands back. `session` is the only copy of the secret.
#[derive(Debug, Clone)]
pub struct Paired {
    pub session: String,
    pub device_id: i64,
    pub name: String,
    pub access_level: ApiTokenLevel,
    pub expires_at: String,
}

#[derive(Debug)]
pub enum ExchangeError {
    /// Unknown, used, expired and burned codes are indistinguishable.
    Invalid,
    Storage(sqlx::Error),
}

impl From<sqlx::Error> for ExchangeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error)
    }
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid_code"),
            Self::Storage(error) => write!(f, "storage: {error}"),
        }
    }
}

impl std::error::Error for ExchangeError {}

/// Exchanges a pairing code for a device session, exactly once.
///
/// A failed exchange counts against EVERY open code (`attempts` + 1), so a guesser cannot spread
/// its tries over codes; the fifth wrong guess burns them all. A good exchange claims the code with
/// a conditional UPDATE and needs one row affected, so two concurrent exchanges cannot both win.
pub async fn exchange(
    pool: &SqlitePool,
    code: &str,
    now: DateTime<Utc>,
) -> Result<Paired, ExchangeError> {
    let hash = hash_secret(&normalize_code(code));
    let now_text = stamp(now);
    let mut tx = pool.begin().await?;

    let claimed = sqlx::query(
        "UPDATE pairing_codes SET used_at = ? \
         WHERE code_hash = ? AND used_at IS NULL AND expires_at > ? AND attempts < ?",
    )
    .bind(&now_text)
    .bind(&hash)
    .bind(&now_text)
    .bind(MAX_PAIRING_ATTEMPTS)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    if claimed != 1 {
        sqlx::query(
            "UPDATE pairing_codes SET attempts = attempts + 1 \
             WHERE used_at IS NULL AND expires_at > ? AND attempts < ?",
        )
        .bind(&now_text)
        .bind(MAX_PAIRING_ATTEMPTS)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Err(ExchangeError::Invalid);
    }

    let (level_text, name): (String, String) =
        sqlx::query_as("SELECT access_level, device_name FROM pairing_codes WHERE code_hash = ?")
            .bind(&hash)
            .fetch_one(&mut *tx)
            .await?;
    let level = ApiTokenLevel::from_str(&level_text).ok_or(ExchangeError::Invalid)?;
    let session = crate::auth::mint_secret("device");
    let (device_id,): (i64,) = sqlx::query_as(
        "INSERT INTO remote_devices (name, access_level, session_hash, created_at) \
         VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(&name)
    .bind(level.as_str())
    .bind(hash_secret(&session))
    .bind(&now_text)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Paired {
        session,
        device_id,
        name,
        access_level: level,
        expires_at: stamp(now + SESSION_ABSOLUTE),
    })
}

/// A paired browser whose session is currently valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: i64,
    pub name: String,
    pub level: ApiTokenLevel,
    /// When the session ends no matter what (created + [`SESSION_ABSOLUTE`]).
    pub expires_at: String,
}

#[derive(Debug)]
pub enum SessionError {
    /// Unknown, revoked, expired and idle sessions are indistinguishable.
    Invalid,
    Storage(sqlx::Error),
}

impl From<sqlx::Error> for SessionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error)
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("session_invalid"),
            Self::Storage(error) => write!(f, "storage: {error}"),
        }
    }
}

impl std::error::Error for SessionError {}

type DeviceRow = (i64, String, String, String, Option<String>, Option<String>);

/// Looks a session secret up and checks it is revoked, expired or idle for none of the reasons.
/// Touches `last_seen_at` at most once per [`LAST_SEEN_GRANULARITY`].
pub async fn authenticate(
    pool: &SqlitePool,
    session_secret: &str,
    now: DateTime<Utc>,
) -> Result<Device, SessionError> {
    let row: Option<DeviceRow> = sqlx::query_as(
        "SELECT id, name, access_level, created_at, last_seen_at, revoked_at \
         FROM remote_devices WHERE session_hash = ?",
    )
    .bind(hash_secret(session_secret))
    .fetch_optional(pool)
    .await?;
    let Some((id, name, level_text, created_text, seen_text, revoked)) = row else {
        return Err(SessionError::Invalid);
    };
    if revoked.is_some() {
        return Err(SessionError::Invalid);
    }
    let level = ApiTokenLevel::from_str(&level_text).ok_or(SessionError::Invalid)?;
    let created = parse_stamp(&created_text).ok_or(SessionError::Invalid)?;
    let seen = seen_text.as_deref().and_then(parse_stamp);

    if now >= created + SESSION_ABSOLUTE {
        return Err(SessionError::Invalid);
    }
    if now - seen.unwrap_or(created) > SESSION_IDLE {
        return Err(SessionError::Invalid);
    }
    if seen.is_none_or(|seen| now - seen >= LAST_SEEN_GRANULARITY) {
        sqlx::query("UPDATE remote_devices SET last_seen_at = ? WHERE id = ?")
            .bind(stamp(now))
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(Device {
        id,
        name,
        level,
        expires_at: stamp(created + SESSION_ABSOLUTE),
    })
}

/// A device as the owner sees it. Carries no hash and no secret.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: i64,
    pub name: String,
    pub access_level: String,
    pub created_at: String,
    pub last_seen_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// Every device ever paired, newest first.
pub async fn list_devices(pool: &SqlitePool) -> Result<Vec<DeviceInfo>, sqlx::Error> {
    let rows: Vec<DeviceRow> = sqlx::query_as(
        "SELECT id, name, access_level, created_at, last_seen_at, revoked_at \
         FROM remote_devices ORDER BY id DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, name, access_level, created_at, last_seen_at, revoked_at)| DeviceInfo {
                id,
                name,
                access_level,
                created_at,
                last_seen_at,
                revoked_at,
            },
        )
        .collect())
}

/// Revokes a device. `Ok(false)` only when no such device exists; an already revoked one is
/// `Ok(true)` and keeps its original `revoked_at`.
pub async fn revoke_device(
    pool: &SqlitePool,
    id: i64,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    sqlx::query("UPDATE remote_devices SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
        .bind(stamp(now))
        .bind(id)
        .execute(pool)
        .await?;
    let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM remote_devices WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(exists.is_some())
}

/// A device logging itself out.
pub async fn revoke_self(
    pool: &SqlitePool,
    device: &Device,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    revoke_device(pool, device.id, now).await.map(|_| ())
}

/// Remote access is armed exactly when the gateway's key is stored.
pub async fn is_armed(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT name FROM service_tokens WHERE name = ?")
        .bind(SERVICE_NAME)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

/// Disarms: deletes the gateway's key, so it authenticates nothing from the next request on.
pub async fn disarm(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM service_tokens WHERE name = ?")
        .bind(SERVICE_NAME)
        .execute(pool)
        .await?;
    Ok(())
}

/// `~/.nucleos/remote.yaml`. Ships off.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    pub enabled: bool,
    /// `https://host[:port]` the browser sees; required when enabled.
    pub public_origin: String,
    /// Built shell for the gateway; `None` means `<dir of daemon exe>/dist-remote`.
    pub dist_dir: Option<String>,
    /// Gateway cap on streams of unknown length, 10..=900.
    pub stream_max_seconds: u32,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            public_origin: String::new(),
            dist_dir: None,
            stream_max_seconds: 120,
        }
    }
}

impl RemoteConfig {
    pub fn parse(contents: &str) -> Result<Self, String> {
        serde_yaml::from_str::<Self>(contents).map_err(|error| error.to_string())
    }

    /// Absent, unreadable or malformed -> defaults, i.e. disabled (the safe direction).
    pub fn load(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(path).map(|text| Self::parse(&text)) {
            Ok(Ok(config)) => config,
            Ok(Err(error)) => {
                tracing::warn!(%error, path = %path.display(), "remote config: could not be parsed; remote access stays off");
                Self::default()
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "remote config: could not be read; remote access stays off");
                Self::default()
            }
        }
    }

    /// Only meaningful for an enabled file; a disabled one is always fine. An enabled file that
    /// fails here must start nothing (fail closed).
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let origin = url::Url::parse(&self.public_origin)
            .map_err(|error| format!("public_origin is not a URL: {error}"))?;
        if origin.scheme() != "https" {
            return Err("public_origin must be https://".to_string());
        }
        if origin.host_str().is_none() {
            return Err("public_origin needs a host".to_string());
        }
        if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() {
            return Err("public_origin must be an origin: no path, query or fragment".to_string());
        }
        // The gateway compares the browser's Origin with this string byte for byte, so it must be
        // written as the browser serializes it: no userinfo, no default `:443`, lowercase host.
        let written = self
            .public_origin
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase();
        if written != origin.origin().ascii_serialization() {
            return Err(format!(
                "public_origin must be written as {}",
                origin.origin().ascii_serialization()
            ));
        }
        if !(10..=900).contains(&self.stream_max_seconds) {
            return Err("stream_max_seconds must be within 10..=900".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-27T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn paired(pool: &SqlitePool, level: ApiTokenLevel) -> Paired {
        let code = create_pairing_code(pool, level, "laptop", t0())
            .await
            .unwrap();
        exchange(pool, &code.code, t0()).await.unwrap()
    }

    async fn device_count(pool: &SqlitePool) -> i64 {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM remote_devices")
            .fetch_one(pool)
            .await
            .unwrap();
        count
    }

    #[tokio::test]
    async fn pairing_code_is_stored_only_as_a_hash() {
        let pool = pool().await;
        let code = create_pairing_code(&pool, ApiTokenLevel::Admin, "laptop", t0())
            .await
            .unwrap();
        assert_eq!(code.code.len(), 11);
        let raw = normalize_code(&code.code);
        let (stored,): (String,) = sqlx::query_as("SELECT code_hash FROM pairing_codes")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, hash_secret(&raw));
        assert_eq!(stored.len(), 64);
        let (hits,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pairing_codes WHERE code_hash = ? OR device_name = ? \
             OR access_level = ?",
        )
        .bind(&raw)
        .bind(&raw)
        .bind(&raw)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(hits, 0);
    }

    #[tokio::test]
    async fn pairing_code_exchanges_exactly_once() {
        let pool = pool().await;
        let code = create_pairing_code(&pool, ApiTokenLevel::RunCreating, "phone", t0())
            .await
            .unwrap();
        // Lower case and no dash is the same code.
        let typed = code.code.replace('-', "").to_lowercase();
        let first = exchange(&pool, &typed, t0() + Duration::seconds(5))
            .await
            .unwrap();
        assert_eq!(first.name, "phone");
        assert_eq!(first.access_level, ApiTokenLevel::RunCreating);
        assert!(first.session.starts_with("nos_device_"));
        let again = exchange(&pool, &code.code, t0() + Duration::seconds(6)).await;
        assert!(matches!(again, Err(ExchangeError::Invalid)));
        assert_eq!(device_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn pairing_code_expires_after_five_minutes() {
        let pool = pool().await;
        let code = create_pairing_code(&pool, ApiTokenLevel::Admin, "late", t0())
            .await
            .unwrap();
        let late = exchange(&pool, &code.code, t0() + Duration::seconds(300)).await;
        assert!(matches!(late, Err(ExchangeError::Invalid)));
        let fresh = create_pairing_code(&pool, ApiTokenLevel::Admin, "ok", t0())
            .await
            .unwrap();
        assert!(
            exchange(&pool, &fresh.code, t0() + Duration::seconds(299))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn pairing_code_is_burned_by_five_wrong_guesses() {
        let pool = pool().await;
        let code = create_pairing_code(&pool, ApiTokenLevel::Admin, "target", t0())
            .await
            .unwrap();
        for _ in 0..MAX_PAIRING_ATTEMPTS {
            let wrong = exchange(&pool, "AAAAA-AAAAA", t0()).await;
            assert!(matches!(wrong, Err(ExchangeError::Invalid)));
        }
        let right = exchange(&pool, &code.code, t0()).await;
        assert!(matches!(right, Err(ExchangeError::Invalid)));
        assert_eq!(device_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn session_secret_is_stored_only_as_a_hash() {
        let pool = pool().await;
        let paired = paired(&pool, ApiTokenLevel::ReadOnly).await;
        let (stored,): (String,) = sqlx::query_as("SELECT session_hash FROM remote_devices")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, hash_secret(&paired.session));
        assert_ne!(stored, paired.session);
        let listed: Vec<String> = list_devices(&pool)
            .await
            .unwrap()
            .iter()
            .map(|d| format!("{d:?}"))
            .collect();
        let listed = listed.join(" ");
        assert!(!listed.contains(&paired.session));
        assert!(!listed.contains(&stored));
        let device = authenticate(&pool, &paired.session, t0()).await.unwrap();
        assert_eq!(device.id, paired.device_id);
        assert_eq!(device.level, ApiTokenLevel::ReadOnly);
    }

    #[tokio::test]
    async fn session_is_refused_when_revoked_expired_or_idle() {
        let pool = pool().await;
        let revoked = paired(&pool, ApiTokenLevel::Admin).await;
        assert!(revoke_device(&pool, revoked.device_id, t0()).await.unwrap());
        assert!(matches!(
            authenticate(&pool, &revoked.session, t0()).await,
            Err(SessionError::Invalid)
        ));
        // Revoking again is idempotent; an unknown id is not.
        assert!(revoke_device(&pool, revoked.device_id, t0()).await.unwrap());
        assert!(!revoke_device(&pool, 9999, t0()).await.unwrap());

        let unknown = authenticate(&pool, "nos_device_nothing", t0()).await;
        assert!(matches!(unknown, Err(SessionError::Invalid)));

        // Idle: 14 days without a request, though well inside the 30-day life.
        let idle = paired(&pool, ApiTokenLevel::Admin).await;
        let idle_at = t0() + SESSION_IDLE + Duration::seconds(1);
        assert!(matches!(
            authenticate(&pool, &idle.session, idle_at).await,
            Err(SessionError::Invalid)
        ));

        // Absolute: used every week, still ends at 30 days.
        let busy = paired(&pool, ApiTokenLevel::Admin).await;
        for week in 1..=3 {
            let at = t0() + Duration::days(7 * week);
            assert!(authenticate(&pool, &busy.session, at).await.is_ok());
        }
        let end = t0() + SESSION_ABSOLUTE;
        assert!(matches!(
            authenticate(&pool, &busy.session, end).await,
            Err(SessionError::Invalid)
        ));
    }

    #[tokio::test]
    async fn last_seen_is_written_at_most_once_a_minute() {
        let pool = pool().await;
        let paired = paired(&pool, ApiTokenLevel::Admin).await;
        authenticate(&pool, &paired.session, t0()).await.unwrap();
        authenticate(&pool, &paired.session, t0() + Duration::seconds(30))
            .await
            .unwrap();
        let (seen,): (Option<String>,) = sqlx::query_as("SELECT last_seen_at FROM remote_devices")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(seen, Some(stamp(t0())));
        authenticate(&pool, &paired.session, t0() + Duration::seconds(61))
            .await
            .unwrap();
        let (seen,): (Option<String>,) = sqlx::query_as("SELECT last_seen_at FROM remote_devices")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(seen, Some(stamp(t0() + Duration::seconds(61))));
    }

    #[test]
    fn config_defaults_to_off_and_refuses_a_non_https_origin() {
        let missing = RemoteConfig::load(Path::new("definitely/not/here/remote.yaml"));
        assert_eq!(missing, RemoteConfig::default());
        assert!(!missing.enabled);
        assert!(missing.validate().is_ok());
        assert_eq!(RemoteConfig::parse("").unwrap(), RemoteConfig::default());

        let enabled = |origin: &str| {
            RemoteConfig::parse(&format!("enabled: true\npublic_origin: \"{origin}\"\n")).unwrap()
        };
        assert!(enabled("https://box.tail1234.ts.net").validate().is_ok());
        assert!(enabled("http://box.tail1234.ts.net").validate().is_err());
        assert!(enabled("https://box.ts.net/app").validate().is_err());
        assert!(enabled("https://box.ts.net/?x=1").validate().is_err());
        assert!(enabled("").validate().is_err());
        assert!(enabled("not a url").validate().is_err());
        assert!(enabled("https://user@box.ts.net").validate().is_err());
        assert!(enabled("https://box.ts.net:443").validate().is_err());
        assert!(enabled("https://box.ts.net:8443").validate().is_ok());
        assert!(enabled("https://box.ts.net/").validate().is_ok());

        let mut slow = enabled("https://box.ts.net");
        slow.stream_max_seconds = 5;
        assert!(slow.validate().is_err());
        slow.stream_max_seconds = 901;
        assert!(slow.validate().is_err());
        slow.stream_max_seconds = 900;
        assert!(slow.validate().is_ok());

        assert!(RemoteConfig::parse("enabled: [").is_err());
    }
}
