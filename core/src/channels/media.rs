//! Media bytes, transcription and turn input (flow B). P0 skeleton: the routes answer 501.

use std::path::PathBuf;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use sqlx::SqlitePool;

use crate::channels::registry;
use crate::state::AppState;

/// Routes of D.9, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels/{channel}/media", post(not_implemented))
        .route("/channels/{channel}/media/{media_id}", get(not_implemented))
}

/// Placeholder handler: the real one replaces it in the chunk that owns the route.
async fn not_implemented() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// The folder under `files_root` where channel media lands, like `CHAT_IMAGES` does for pasted
/// pictures: `channels/<channel id>/`.
pub const MEDIA_FOLDER: &str = "channels";

const ID_PREFIX: &str = "m_";
const ID_ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const ID_RANDOM_LEN: usize = 24;

#[derive(Debug)]
pub enum MediaError {
    NoFilesRoot,
    TooLarge,
    UnknownMedia,
    WrongChannel,
    Io(std::io::Error),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for MediaError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMedia {
    pub path: PathBuf,
    /// Relative to `files_root`.
    pub relative: String,
    pub mime: String,
    pub channel: String,
}

/// `m_<random>`: never derived from the content or the platform's own file id.
fn generate() -> String {
    let mut rng = rand::rng();
    let mut out = String::with_capacity(ID_PREFIX.len() + ID_RANDOM_LEN);
    out.push_str(ID_PREFIX);
    for _ in 0..ID_RANDOM_LEN {
        let byte: u8 = rng.random();
        out.push(ID_ALPHABET[usize::from(byte) % ID_ALPHABET.len()] as char);
    }
    out
}

/// A file extension for a claimed media type; the sender's claim, used only to name a file.
fn extension_of(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "video/mp4" => "mp4",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    }
}

fn path_error(error: crate::files::PathError) -> MediaError {
    MediaError::Io(std::io::Error::other(format!("{error:?}")))
}

/// Writes the bytes under `channels/<channel>/` and records the id. The size limit is the
/// channel's manifest limit and is checked before anything touches the disk.
pub async fn store(
    state: &AppState,
    channel: &'static str,
    mime: &str,
    bytes: &[u8],
    now: DateTime<Utc>,
) -> Result<String, MediaError> {
    let manifest = registry::by_id(channel).ok_or(MediaError::WrongChannel)?;
    if bytes.len() > manifest.max_media_bytes {
        return Err(MediaError::TooLarge);
    }
    let root = state.files_root.clone().ok_or(MediaError::NoFilesRoot)?;

    let id = generate();
    let folder = format!("{MEDIA_FOLDER}/{channel}");
    let filename = format!("{id}.{}", extension_of(mime));
    let data = bytes.to_vec();
    let written = tokio::task::spawn_blocking(move || {
        crate::files::create_folder(&root, &folder).map_err(path_error)?;
        crate::files::write_file(&root, &folder, &filename, &data).map_err(path_error)
    })
    .await
    .map_err(|e| MediaError::Io(std::io::Error::other(e.to_string())))??;

    sqlx::query(
        "INSERT INTO channel_media (id, channel, path, mime, name, bytes, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(channel)
    .bind(format!("{MEDIA_FOLDER}/{channel}/{written}"))
    .bind(mime)
    .bind(&written)
    .bind(bytes.len() as i64)
    .bind(now.to_rfc3339())
    .execute(&state.pool)
    .await?;
    Ok(id)
}

/// Resolves a stored id for `channel`. Another channel's media is `WrongChannel`; the HTTP layer
/// answers 404 for it and for `UnknownMedia` alike.
pub async fn open(
    state: &AppState,
    channel: &str,
    media_id: &str,
) -> Result<StoredMedia, MediaError> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT channel, path, mime FROM channel_media WHERE id = ?")
            .bind(media_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((owner, relative, mime)) = row else {
        return Err(MediaError::UnknownMedia);
    };
    if owner != channel {
        return Err(MediaError::WrongChannel);
    }
    let root = state.files_root.as_deref().ok_or(MediaError::NoFilesRoot)?;
    let path = crate::files::resolve_file(root, &relative).map_err(path_error)?;
    Ok(StoredMedia {
        path,
        relative,
        mime,
        channel: owner,
    })
}

/// For `outbox` degradation: the stored name, or the mime when the row has none.
pub async fn display_name(pool: &SqlitePool, media_id: &str) -> sqlx::Result<Option<String>> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT name, mime FROM channel_media WHERE id = ?")
            .bind(media_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(name, mime)| name.filter(|n| !n.is_empty()).unwrap_or(mime)))
}

/// Housekeeping hook called hourly by `run_housekeeping`. TTL of `channel_media` is 7 days; the
/// file follows the files-folder rules, so it stays.
pub async fn prune(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let cutoff = (now - Duration::days(7)).to_rfc3339();
    let result = sqlx::query("DELETE FROM channel_media WHERE created_at < ?")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn state_with_root() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(dir.path()).unwrap();
        let mut state = crate::channels::testkit::state().await;
        state.files_root = Some(root);
        (state, dir)
    }

    #[tokio::test]
    async fn store_writes_the_file_and_a_row() {
        let (state, _dir) = state_with_root().await;
        let id = store(&state, "fake", "image/png", b"png-bytes", Utc::now())
            .await
            .unwrap();
        let opened = open(&state, "fake", &id).await.unwrap();
        assert_eq!(std::fs::read(&opened.path).unwrap(), b"png-bytes");
        assert_eq!(opened.mime, "image/png");
        assert_eq!(opened.channel, "fake");
        assert!(
            opened.relative.starts_with("channels/fake/"),
            "{}",
            opened.relative
        );
        let (bytes,): (i64,) = sqlx::query_as("SELECT bytes FROM channel_media WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(bytes, 9);
    }

    #[tokio::test]
    async fn ids_match_the_contract_grammar() {
        let (state, _dir) = state_with_root().await;
        for _ in 0..10 {
            let id = store(&state, "fake", "text/plain", b"x", Utc::now())
                .await
                .unwrap();
            let rest = id.strip_prefix("m_").expect("prefix");
            assert!(
                (1..=30).contains(&rest.len())
                    && rest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
                "{id}"
            );
        }
    }

    #[tokio::test]
    async fn a_second_store_gets_a_different_id() {
        let (state, _dir) = state_with_root().await;
        let a = store(&state, "fake", "text/plain", b"same", Utc::now())
            .await
            .unwrap();
        let b = store(&state, "fake", "text/plain", b"same", Utc::now())
            .await
            .unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn too_large_is_refused_before_writing() {
        let (state, _dir) = state_with_root().await;
        let limit = registry::by_id("fake").unwrap().max_media_bytes;
        let big = vec![0u8; limit + 1];
        let result = store(&state, "fake", "application/pdf", &big, Utc::now()).await;
        assert!(matches!(result, Err(MediaError::TooLarge)), "{result:?}");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_media")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        let root = state.files_root.as_deref().unwrap();
        assert!(!root.join(MEDIA_FOLDER).exists());
    }

    #[tokio::test]
    async fn no_files_root_is_a_distinct_error() {
        let state = crate::channels::testkit::state().await;
        let result = store(&state, "fake", "text/plain", b"x", Utc::now()).await;
        assert!(matches!(result, Err(MediaError::NoFilesRoot)), "{result:?}");
    }

    #[tokio::test]
    async fn open_refuses_another_channels_media() {
        let (state, _dir) = state_with_root().await;
        let id = store(&state, "fake", "text/plain", b"x", Utc::now())
            .await
            .unwrap();
        let wrong = open(&state, "telegram", &id).await;
        assert!(matches!(wrong, Err(MediaError::WrongChannel)), "{wrong:?}");
        let unknown = open(&state, "fake", "m_nope").await;
        assert!(
            matches!(unknown, Err(MediaError::UnknownMedia)),
            "{unknown:?}"
        );
    }

    #[tokio::test]
    async fn display_name_returns_the_stored_name_or_mime() {
        let (state, _dir) = state_with_root().await;
        let id = store(&state, "fake", "image/png", b"x", Utc::now())
            .await
            .unwrap();
        let name = display_name(&state.pool, &id).await.unwrap().unwrap();
        assert!(name.starts_with("m_") && name.ends_with(".png"), "{name}");
        sqlx::query(
            "INSERT INTO channel_media (id, channel, path, mime, name, bytes, created_at) \
             VALUES ('m_noname', 'fake', 'channels/fake/x', 'audio/ogg', NULL, 1, ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            display_name(&state.pool, "m_noname")
                .await
                .unwrap()
                .as_deref(),
            Some("audio/ogg")
        );
        assert_eq!(display_name(&state.pool, "m_missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn prune_removes_rows_older_than_seven_days() {
        let (state, _dir) = state_with_root().await;
        let now = Utc::now();
        let old = store(&state, "fake", "text/plain", b"o", now - Duration::days(8))
            .await
            .unwrap();
        let fresh = store(&state, "fake", "text/plain", b"f", now - Duration::days(1))
            .await
            .unwrap();
        let old_file = open(&state, "fake", &old).await.unwrap().path;
        assert_eq!(prune(&state.pool, now).await.unwrap(), 1);
        assert!(matches!(
            open(&state, "fake", &old).await,
            Err(MediaError::UnknownMedia)
        ));
        assert!(open(&state, "fake", &fresh).await.is_ok());
        // The file follows the files-folder rules: pruning the row does not delete it.
        assert!(old_file.exists());
    }
}
