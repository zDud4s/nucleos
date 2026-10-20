//! Media bytes, transcription and turn input (flow B). The two routes at the top only decode,
//! authorise and answer; `store` and `open` are the logic.

use std::path::PathBuf;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use sqlx::SqlitePool;

use super::ingest::own_channel;
use crate::auth::Scope;
use crate::channels::contract::{ErrorResponse, InPart, MediaCreated};
use crate::channels::registry;
use crate::state::AppState;

/// Routes of D.9, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    // The layer lifts axum's 2 MB default to the largest manifest limit. The handler reads the body
    // by hand and enforces the channel's OWN limit, so the 413 carries the JSON body of T9 (R14)
    // instead of the layer's bare one.
    let ceiling = registry::all()
        .iter()
        .map(|m| m.max_media_bytes)
        .max()
        .unwrap_or(0);
    Router::new()
        .route(
            "/channels/{channel}/media",
            post(post_media).layer(DefaultBodyLimit::max(ceiling)),
        )
        .route("/channels/{channel}/media/{media_id}", get(get_media))
}

/// One half of a mime type, `^[a-z0-9][a-z0-9.+-]*$` (T7).
fn mime_half(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..].iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(*c, b'.' | b'+' | b'-')
        })
}

/// `type/subtype`, no parameters (`; codecs=opus` is refused), the same grammar as an event part.
fn valid_mime(s: &str) -> bool {
    s.split_once('/')
        .is_some_and(|(kind, subtype)| mime_half(kind) && mime_half(subtype))
}

fn refusal(status: StatusCode, error: &str, detail: Option<String>) -> Response {
    let body = ErrorResponse {
        error: error.to_string(),
        at: None,
        detail,
        contract_version: None,
    };
    (status, Json(body)).into_response()
}

fn too_large(limit: usize) -> Response {
    refusal(
        StatusCode::PAYLOAD_TOO_LARGE,
        "media_too_large",
        Some(format!("limit is {limit} bytes")),
    )
}

/// `POST /channels/{channel}/media` (section 5.2): raw bytes, `Content-Type` is the mime.
async fn post_media(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Path(named): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let channel = match own_channel(&scope, &named) {
        Ok(channel) => channel,
        Err(refusal) => return refusal,
    };
    let Some(manifest) = registry::by_id(channel) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = match headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        Some(mime) if valid_mime(mime) => mime.to_string(),
        _ => {
            return refusal(
                StatusCode::BAD_REQUEST,
                "grammar",
                Some("Content-Type must be a bare mime type".to_string()),
            );
        }
    };
    // One byte over the limit is enough to know; a read error (the peer went away) answers the
    // same, to nobody.
    let limit = manifest.max_media_bytes;
    let bytes = match axum::body::to_bytes(body, limit.saturating_add(1)).await {
        Ok(bytes) => bytes,
        Err(_) => return too_large(limit),
    };
    match store(&state, channel, &mime, &bytes, Utc::now()).await {
        Ok(media_id) => Json(MediaCreated { media_id }).into_response(),
        Err(MediaError::TooLarge) => too_large(limit),
        Err(MediaError::NoFilesRoot) => refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_files_root",
            Some("the files folder is not available".to_string()),
        ),
        Err(error) => {
            tracing::warn!(?error, channel, "media could not be stored");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /channels/{channel}/media/{media_id}`: the bytes with the stored `Content-Type`. Unknown
/// ids and another channel's ids are the same 404.
async fn get_media(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Path((named, media_id)): Path<(String, String)>,
) -> Response {
    let channel = match own_channel(&scope, &named) {
        Ok(channel) => channel,
        Err(refusal) => return refusal,
    };
    let stored = match open(&state, channel, &media_id).await {
        Ok(stored) => stored,
        Err(MediaError::UnknownMedia | MediaError::WrongChannel) => {
            return refusal(StatusCode::NOT_FOUND, "unknown_media", None);
        }
        Err(MediaError::NoFilesRoot) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(error) => {
            tracing::warn!(?error, channel, "media could not be opened");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let bytes = match tokio::fs::read(&stored.path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return refusal(StatusCode::NOT_FOUND, "unknown_media", None);
        }
        Err(error) => {
            tracing::warn!(%error, channel, "media file could not be read");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let content_type = HeaderValue::from_str(&stored.mime)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
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

/// For `outbox` degradation: the stored name, or the mime when the row has none. Generic over the
/// executor so it runs on a pool or on a connection inside a transaction.
pub async fn display_name<'e, E>(executor: E, media_id: &str) -> sqlx::Result<Option<String>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT name, mime FROM channel_media WHERE id = ?")
            .bind(media_id)
            .fetch_optional(executor)
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

/// What a message becomes for the turn (§7.1 `media.rs`).
#[derive(Debug, Default)]
pub struct TurnInput {
    /// Text parts, captions, transcripts and "file at <path>" lines, in order.
    pub text: String,
    /// Within `MAX_IMAGES` / `MAX_IMAGE_CHARS`; the excess goes as files.
    pub images: Vec<crate::execution::runner::Attachment>,
    /// True when any part came from a transcript: commands never fire from it.
    pub transcribed: bool,
    /// True when the message held anything but typed text (captures refuse it).
    pub non_text: bool,
    /// Parts that could not be used, answered with `strings::Text::Unsupported`.
    pub refused: Vec<String>,
}

/// The media types an image block may claim; the same four as `send.rs`' `IMAGE_MEDIA_TYPES`,
/// which is not visible from here. Anything else goes to the turn as a file.
const IMAGE_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

fn push_line(text: &mut String, line: &str) {
    if line.is_empty() {
        return;
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(line);
}

/// A stored id and its bytes, or `None` when either is gone (a refused part, never fatal).
async fn load(state: &AppState, channel: &str, media_id: &str) -> Option<(StoredMedia, Vec<u8>)> {
    let stored = open(state, channel, media_id).await.ok()?;
    let bytes = tokio::fs::read(&stored.path).await.ok()?;
    Some((stored, bytes))
}

/// Transcribes audio through `state.voice.transcriber`. `None` = no transcriber, an unusable
/// container, a failure or silence; the caller refuses the part.
async fn transcribe(
    state: &AppState,
    stored: &StoredMedia,
    bytes: &[u8],
    duration_ms: Option<u64>,
) -> Option<String> {
    let transcriber = state.voice.transcriber.as_ref()?;
    let extension = crate::transcribe::extension_for(Some(extension_of(&stored.mime)))?;
    let duration = std::time::Duration::from_millis(duration_ms.unwrap_or(0));
    let heard = transcriber
        .transcribe(bytes, extension, duration)
        .await
        .ok()?;
    let heard = heard.trim();
    (!heard.is_empty()).then(|| heard.to_string())
}

/// Turns the parts of an inbound message into what the turn takes. No transcriber, a missing
/// media id or an unreadable file is a refused part, not an error.
pub async fn to_turn_input(state: &AppState, channel: &'static str, parts: &[InPart]) -> TurnInput {
    use base64::Engine;

    let mut input = TurnInput::default();
    for part in parts {
        match part {
            InPart::Text { text } => push_line(&mut input.text, text),
            InPart::Audio {
                media, duration_ms, ..
            } => {
                input.non_text = true;
                let heard = match load(state, channel, media).await {
                    Some((stored, bytes)) => transcribe(state, &stored, &bytes, *duration_ms).await,
                    None => None,
                };
                match heard {
                    Some(text) => {
                        input.transcribed = true;
                        push_line(&mut input.text, &text);
                    }
                    None => input.refused.push("audio".to_string()),
                }
            }
            InPart::Image { media, caption } => {
                input.non_text = true;
                let Some((stored, bytes)) = load(state, channel, media).await else {
                    input.refused.push("image".to_string());
                    continue;
                };
                if let Some(caption) = caption {
                    push_line(&mut input.text, caption);
                }
                let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let media_type = stored.mime.split(';').next().unwrap_or("").trim();
                if input.images.len() < crate::assistant::MAX_IMAGES
                    && data.len() <= crate::assistant::MAX_IMAGE_CHARS
                    && IMAGE_MEDIA_TYPES.contains(&media_type)
                {
                    input.images.push(crate::execution::runner::Attachment {
                        media_type: media_type.to_string(),
                        data,
                    });
                } else {
                    push_line(&mut input.text, &format!("file at {}", stored.relative));
                }
            }
            InPart::File { media, caption, .. } => {
                input.non_text = true;
                let Some((stored, _)) = load(state, channel, media).await else {
                    input.refused.push("file".to_string());
                    continue;
                };
                if let Some(caption) = caption {
                    push_line(&mut input.text, caption);
                }
                push_line(&mut input.text, &format!("file at {}", stored.relative));
            }
            InPart::Unsupported { label } => {
                input.non_text = true;
                input.refused.push(label.clone());
            }
        }
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    mod turn_input {
        use std::sync::Arc;

        use super::*;
        use crate::transcribe::FakeTranscriber;

        async fn stored(state: &AppState, mime: &str, bytes: &[u8]) -> String {
            store(state, "fake", mime, bytes, Utc::now()).await.unwrap()
        }

        fn with_transcriber(state: &mut AppState, text: &str) {
            state.voice = Arc::new(crate::integrations::voice::VoiceRuntime {
                transcriber: Some(Arc::new(FakeTranscriber::returning(text))),
                ..Default::default()
            });
        }

        #[tokio::test]
        async fn typed_text_passes_through() {
            let (state, _dir) = state_with_root().await;
            let parts = [
                InPart::Text { text: "one".into() },
                InPart::Text { text: "two".into() },
            ];
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.text, "one\ntwo");
            assert!(!input.non_text && !input.transcribed);
            assert!(input.images.is_empty() && input.refused.is_empty());
        }

        #[tokio::test]
        async fn audio_is_transcribed_and_flags_transcribed() {
            let (mut state, _dir) = state_with_root().await;
            with_transcriber(&mut state, "  what time is it  ");
            let media = stored(&state, "audio/ogg", b"ogg").await;
            let parts = [InPart::Audio {
                media,
                mime: "audio/ogg".into(),
                duration_ms: Some(1500),
            }];
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.text, "what time is it");
            assert!(input.transcribed && input.non_text);
            assert!(input.refused.is_empty());
        }

        #[tokio::test]
        async fn no_transcriber_refuses_the_audio_part() {
            let (state, _dir) = state_with_root().await;
            let media = stored(&state, "audio/ogg", b"ogg").await;
            let parts = [InPart::Audio {
                media,
                mime: "audio/ogg".into(),
                duration_ms: None,
            }];
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.refused, vec!["audio".to_string()]);
            assert!(input.non_text && !input.transcribed);
            assert!(input.text.is_empty());
        }

        #[tokio::test]
        async fn images_within_limits_become_attachments() {
            let (state, _dir) = state_with_root().await;
            let media = stored(&state, "image/png", b"png-bytes").await;
            let parts = [InPart::Image {
                media,
                caption: Some("look".into()),
            }];
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.text, "look");
            assert_eq!(input.images.len(), 1);
            assert_eq!(input.images[0].media_type, "image/png");
            assert_eq!(input.images[0].data, "cG5nLWJ5dGVz");
            assert!(input.non_text);
        }

        #[tokio::test]
        async fn the_sixth_image_goes_as_a_file() {
            let (state, _dir) = state_with_root().await;
            let mut parts = Vec::new();
            let mut last = String::new();
            for _ in 0..=crate::assistant::MAX_IMAGES {
                last = stored(&state, "image/png", b"png-bytes").await;
                parts.push(InPart::Image {
                    media: last.clone(),
                    caption: None,
                });
            }
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.images.len(), crate::assistant::MAX_IMAGES);
            let relative = open(&state, "fake", &last).await.unwrap().relative;
            assert_eq!(input.text, format!("file at {relative}"));
        }

        #[tokio::test]
        async fn a_file_becomes_a_path_line() {
            let (state, _dir) = state_with_root().await;
            let media = stored(&state, "application/pdf", b"pdf").await;
            let relative = open(&state, "fake", &media).await.unwrap().relative;
            let parts = [InPart::File {
                media,
                name: Some("a.pdf".into()),
                caption: Some("the report".into()),
            }];
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.text, format!("the report\nfile at {relative}"));
            assert!(input.non_text && input.images.is_empty());
        }

        #[tokio::test]
        async fn unsupported_parts_are_listed_in_refused() {
            let (state, _dir) = state_with_root().await;
            let parts = [
                InPart::Unsupported {
                    label: "sticker".into(),
                },
                InPart::Text { text: "hi".into() },
            ];
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.refused, vec!["sticker".to_string()]);
            assert_eq!(input.text, "hi");
            assert!(input.non_text);
        }

        #[tokio::test]
        async fn a_missing_media_id_is_refused_not_fatal() {
            let (state, _dir) = state_with_root().await;
            let parts = [
                InPart::Image {
                    media: "m_nope".into(),
                    caption: None,
                },
                InPart::Text {
                    text: "still here".into(),
                },
            ];
            let input = to_turn_input(&state, "fake", &parts).await;
            assert_eq!(input.refused, vec!["image".to_string()]);
            assert_eq!(input.text, "still here");
            assert!(input.images.is_empty() && input.non_text);
        }
    }

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

    // ---- the routes (RA) ------------------------------------------------------------------

    async fn route_key(state: &AppState, channel: &'static str) -> String {
        crate::auth::mint_service_token(&state.pool, crate::auth::Service::Channel(channel))
            .await
            .unwrap()
    }

    /// One request through the real token middleware and the channel routes.
    async fn call(
        state: &AppState,
        key: &str,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        use tower::ServiceExt;
        let app = crate::channels::routes()
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::auth::require_token,
            ))
            .with_state(state.clone());
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {key}"));
        if let Some(content_type) = content_type {
            request = request.header("Content-Type", content_type);
        }
        let response = app
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, bytes.to_vec())
    }

    #[tokio::test]
    async fn route_media_of_one_byte_answers_an_id_and_serves_it_back() {
        let (state, _dir) = state_with_root().await;
        let key = route_key(&state, "fake").await;
        let (status, _, body) = call(
            &state,
            &key,
            "POST",
            "/channels/fake/media",
            Some("image/png"),
            vec![7],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = answer["media_id"].as_str().unwrap().to_string();
        assert!(id.starts_with("m_"), "{id}");

        let (status, headers, bytes) = call(
            &state,
            &key,
            "GET",
            &format!("/channels/fake/media/{id}"),
            None,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers.get("content-type").unwrap(), "image/png");
        assert_eq!(bytes, vec![7]);
    }

    #[tokio::test]
    async fn route_media_above_the_manifest_limit_is_413_with_the_contract_body() {
        let (state, _dir) = state_with_root().await;
        let key = route_key(&state, "fake").await;
        let limit = registry::by_id("fake").unwrap().max_media_bytes;
        let (status, _, body) = call(
            &state,
            &key,
            "POST",
            "/channels/fake/media",
            Some("application/pdf"),
            vec![0; limit + 1],
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            answer,
            serde_json::json!({
                "error": "media_too_large",
                "detail": format!("limit is {limit} bytes")
            })
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_media")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn route_media_with_a_mime_parameter_is_a_400() {
        let (state, _dir) = state_with_root().await;
        let key = route_key(&state, "fake").await;
        let (status, _, body) = call(
            &state,
            &key,
            "POST",
            "/channels/fake/media",
            Some("audio/ogg; codecs=opus"),
            vec![1],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(answer["error"], "grammar");
    }

    #[tokio::test]
    async fn route_media_of_another_channel_is_404() {
        let (state, _dir) = state_with_root().await;
        let theirs = store(&state, "telegram", "text/plain", b"x", Utc::now())
            .await
            .unwrap();
        let key = route_key(&state, "fake").await;
        let (status, _, _) = call(
            &state,
            &key,
            "GET",
            &format!("/channels/fake/media/{theirs}"),
            None,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = call(
            &state,
            &key,
            "GET",
            "/channels/fake/media/m_unknown",
            None,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
