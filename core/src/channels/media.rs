//! Media bytes, transcription and turn input (flow B). P0 skeleton: the routes answer 501.

use std::path::PathBuf;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use sqlx::SqlitePool;

use crate::channels::contract::InPart;
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
}
