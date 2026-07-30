//! The voice pillar's domain: what a transcript means, and what happens to it.
//!
//! Owns the `voice_captures` table's SQL and the `/voice/*` handlers, and nothing else. Audio capture,
//! hotkeys, the clipboard and synthetic paste live in the shell because they need the desktop session;
//! spawning a transcriber lives in `transcribe.rs`; talking to a local model lives in `runner.rs`. What
//! is left here is the part that is neither mechanism nor transport: apply the hints, ask for a
//! cleanup, decide whether to trust the answer, and remember it for as long as it is allowed to exist.
//!
//! Deliberately NOT a run. A dictation is a reflex, not a job: it is free, it happens dozens of times
//! an hour, and it has to keep working when the kill switch is thrown and the budget is spent. Those
//! brakes exist to restrain the agent, and a typing aid is not the agent — so nothing here touches
//! `runs`, `budget.rs`, `wip.rs` or `attention.rs`.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;

/// Longest capture accepted, and therefore the size of the biggest request body.
///
/// It lives in the core rather than in the shell on purpose: the shell measures the recording, but if
/// the cap were defined there then no single `cargo test` could see both this number and the HTTP body
/// limit derived from it — `shell/src-tauri` is outside the workspace with its own lockfile. Defining
/// it here is what makes `body_limit_agrees_with_max_duration` possible at all. The shell reads it
/// from `GET /voice/config`.
pub const MAX_CAPTURE_SECONDS: u64 = 20 * 60;

/// 16 kHz mono 16-bit PCM: what Whisper wants, so resampling later could only lose information.
const SAMPLE_RATE: u64 = 16_000;
const BYTES_PER_SAMPLE: u64 = 2;
/// Canonical WAV header, plus room for the extra chunks some encoders insert.
const WAV_OVERHEAD_BYTES: u64 = 1024;

/// PURE: the request-body ceiling implied by `MAX_CAPTURE_SECONDS`.
///
/// Needed because every route not given its own limit inherits axum's 2 MB default, and a
/// twenty-minute memo is roughly 38 MB — so the default would reject exactly the long recordings that
/// are most expensive to make and least repeatable. That is the same family of bug as a flat
/// transcription deadline: invisible on short input, deterministic on long.
pub fn max_body_bytes() -> usize {
    (MAX_CAPTURE_SECONDS * SAMPLE_RATE * BYTES_PER_SAMPLE + WAV_OVERHEAD_BYTES) as usize
}

/// How much shorter than the raw transcript a cleaned document may be before it is disbelieved.
///
/// A local model asked to tidy long text fails by dropping a chunk, not by garbling one, and a dropped
/// chunk is invisible in the result — it reads as a clean, shorter document.
const MIN_CLEANUP_RATIO: f64 = 0.6;

/// Characters of transcript handed to the cleanup model at once.
///
/// A twenty-minute transcript does not fit a small model's context, so cleanup is chunked. Sized well
/// inside the probed context so the chunk plus the prompt plus the answer all fit.
const CLEANUP_CHUNK_CHARS: usize = 2_000;

/// Context requested for every cleanup call, and proven at startup before cleanup is armed.
///
/// Stated EXPLICITLY on each request and never inherited from Ollama's factory default, which is the
/// single most expensive lesson the local-triage pillar recorded: Ollama truncates an over-long prompt
/// in silence, the call still succeeds, and the caller cannot tell. Holds a chunk in, an answer of
/// comparable size out, and the instructions, with margin.
pub const CLEANUP_NUM_CTX: usize = 4_096;

/// Which kind of capture this is. The discriminator on `voice_captures`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Short, pasted into whatever had focus, and expires (§7.1).
    Dictation,
    /// Long, kept as a document until deleted.
    Memo,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Dictation => "dictation",
            Kind::Memo => "memo",
        }
    }
}

/// Whether the text a caller receives was cleaned, and if not, why not.
///
/// One flag, read by the API, the stored row and the shell's badge alike. Three independent booleans
/// would drift, and the interface would eventually call a document clean that the guard had rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupState {
    Cleaned,
    /// Delivered as transcribed. Either no cleanup model is armed, or the model could not be reached.
    Raw,
    /// A cleanup came back implausibly short and was discarded in favour of the raw transcript.
    Shrunk,
}

impl CleanupState {
    fn as_str(self) -> &'static str {
        match self {
            CleanupState::Cleaned => "cleaned",
            CleanupState::Raw => "raw",
            CleanupState::Shrunk => "shrunk",
        }
    }
}

/// The voice pillar's process-wide settings, resolved once at startup like `EmailRuntime`.
///
/// Grouped into one struct, and reached through a single `AppState` field, for the reason
/// `EmailRuntime` already is: these are read together, they change together, and a `Default` that
/// means "off" is what lets every context which does not care about voice — nearly every test in the
/// crate — stay untouched by the pillar existing.
///
/// No `Debug`: it carries a transcriber trait object, and nothing needs to print it.
#[derive(Clone)]
pub struct VoiceRuntime {
    /// True only when a transcriber is actually configured — see `VoiceConfig::armed`.
    pub armed: bool,
    /// Retained beyond building the transcriber so `health.rs` can report whether the program it names
    /// actually exists. A hotkey that records and then fails is worse than one that was never armed.
    pub stt_command: String,
    pub hints: Vec<String>,
    pub cleanup_prompt: String,
    pub retain_dictations_days: u8,
    /// `None` leaves cleanup unarmed, which yields raw transcripts rather than no transcripts.
    pub cleanup_model: Option<String>,
    pub ollama_base_url: String,
    /// Absent means there is nothing to transcribe with, which is the same thing as voice being off.
    pub transcriber: Option<Arc<dyn crate::transcribe::Transcriber>>,
    /// Retained so cleanup reuses connections across dictations instead of building a client per
    /// keystroke-sized request.
    pub client: reqwest::Client,
}

impl Default for VoiceRuntime {
    /// The pillar off — what every context that has not configured it should see, tests included.
    fn default() -> Self {
        Self {
            armed: false,
            stt_command: String::new(),
            hints: Vec::new(),
            cleanup_prompt: crate::config::DEFAULT_CLEANUP_PROMPT.to_string(),
            retain_dictations_days: 7,
            cleanup_model: None,
            ollama_base_url: crate::runner::OLLAMA_BASE_URL.to_string(),
            transcriber: None,
            client: reqwest::Client::new(),
        }
    }
}

impl VoiceRuntime {
    pub fn from_config(config: &crate::config::VoiceConfig, cleanup_model: Option<String>) -> Self {
        Self {
            armed: config.armed(),
            stt_command: config.stt_command.clone(),
            hints: config.hints.clone(),
            cleanup_prompt: config.cleanup_prompt.clone(),
            retain_dictations_days: config.retain_dictations_days,
            cleanup_model,
            transcriber: transcriber_for(config),
            ..Self::default()
        }
    }
}

/// PURE: rewrites known terms the transcriber is likely to have mangled.
///
/// Runs on the RAW transcript, before cleanup, so the model reads correct terminology instead of being
/// asked to guess from something broken. Matching is on whole words and case-insensitive: naive
/// substring replacement rewrites the inside of unrelated words, which is how a hint list starts
/// corrupting text it was added to fix.
pub fn apply_hints(raw: &str, hints: &[String]) -> String {
    if hints.is_empty() {
        return raw.to_string();
    }
    // Trimmed, because a hint carrying a stray space from the YAML list folds to a form no single
    // token can ever equal: it would sit in the file looking active and silently match nothing.
    // Blank entries drop out for the same reason.
    let folded_hints = hints
        .iter()
        .filter_map(|hint| {
            let hint = hint.trim();
            (!hint.is_empty()).then(|| (fold_for_hint(hint), hint))
        })
        .collect::<Vec<_>>();
    if folded_hints.is_empty() {
        return raw.to_string();
    }

    raw.split_inclusive(|c: char| !c.is_alphanumeric())
        .map(|piece| {
            // Split the token from whatever punctuation or space closed it, so the tail survives.
            let boundary = piece
                .char_indices()
                .find(|(_, c)| !c.is_alphanumeric())
                .map_or(piece.len(), |(index, _)| index);
            let (word, tail) = piece.split_at(boundary);
            let folded_word = fold_for_hint(word);
            match folded_hints
                .iter()
                .find(|(folded, hint)| *folded == folded_word && **hint != *word)
            {
                Some((_, hint)) => format!("{hint}{tail}"),
                None => piece.to_string(),
            }
        })
        .collect()
}

/// PURE: the form two spellings of the same word share.
///
/// Folds case AND diacritics, and the second half is the whole reason hints are useful here. A
/// transcriber's most common mistake in Portuguese is dropping the accent — it hears "nucleo" for
/// "núcleo" — so an ASCII case-insensitive comparison would never match the very word the hint was
/// added to fix. Only the folded forms are compared; the hint's own spelling is what gets written back.
fn fold_for_hint(word: &str) -> String {
    word.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// PURE: the exact text handed to the cleanup model.
pub fn build_cleanup_prompt(instructions: &str, transcript: &str) -> String {
    format!("{instructions}\n\n=== BEGIN TRANSCRIPT ===\n{transcript}\n=== END TRANSCRIPT ===")
}

/// PURE: splits a transcript on sentence boundaries into pieces a small model can hold.
///
/// Sentence-first, because a chunk boundary inside a sentence is what makes a map-reduce cleanup
/// produce two half-sentences that each look finished.
///
/// But sentence-first is not sentence-only, and that distinction is the whole point of this
/// function. Greedy decoding -- which §14.2 made mandatory for latency -- routinely returns a wall
/// of words with no full stop anywhere in it, and a transcript with no terminator is ONE sentence:
/// every character of a twenty-minute memo, handed whole to a `CLEANUP_NUM_CTX` window. Ollama then
/// truncates the input, the reply covers only the prefix, and `accept_cleanup`'s shrink guard
/// rejects the result -- so the ENTIRE memo silently falls back to raw, not just the oversized
/// piece. The budget is therefore a ceiling, not a preference.
///
/// Counted in characters throughout, matching `CLEANUP_CHUNK_CHARS`'s name. Bytes would make every
/// chunk of accented Portuguese smaller than the budget says without anything explaining why.
pub fn chunk_transcript(text: &str, budget: usize) -> Vec<String> {
    // A zero budget would make every piece over-long and leave `split_oversized` with no cut it is
    // allowed to make.
    let budget = budget.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;
    for sentence in text.split_inclusive(['.', '!', '?', '\n']) {
        for piece in split_oversized(sentence, budget) {
            let piece_chars = piece.chars().count();
            if current_chars > 0 && current_chars + piece_chars > budget {
                chunks.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            current.push_str(piece);
            current_chars += piece_chars;
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

/// PURE: cuts one over-long sentence into pieces of at most `budget` characters.
///
/// Prefers the last space in each piece, so a word survives whole. A run containing no space at all
/// -- a dictated URL, a path, a string of digits -- is cut at the budget anyway: letting it through
/// is the failure this exists to prevent, and a cut word costs a seam the cleanup model can repair.
///
/// Every index comes from `char_indices`, so a cut can never land inside a multibyte character. A
/// byte-arithmetic version of this would panic on the first accented word.
fn split_oversized(sentence: &str, budget: usize) -> Vec<&str> {
    if sentence.chars().count() <= budget {
        return vec![sentence];
    }
    let mut pieces = Vec::new();
    let mut rest = sentence;
    while rest.chars().count() > budget {
        let hard = rest
            .char_indices()
            .nth(budget)
            .map_or(rest.len(), |(index, _)| index);
        let head = &rest[..hard];
        let cut = head
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(hard, |(index, c)| index + c.len_utf8());
        // A piece whose only whitespace is its first character would otherwise cut nothing and loop
        // forever.
        let cut = if cut == 0 { hard } else { cut };
        pieces.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// PURE: whether a cleaned transcript is plausible enough to hand back.
///
/// Length is a crude proxy, and that is the point: the failure it catches is a chunk silently going
/// missing, which is precisely a length event. A cleanup that legitimately removes filler loses a
/// little; one that lost a paragraph loses a lot.
pub fn accept_cleanup(raw: &str, cleaned: &str) -> bool {
    if cleaned.trim().is_empty() {
        return false;
    }
    let raw_len = raw.trim().chars().count() as f64;
    if raw_len == 0.0 {
        return true;
    }
    (cleaned.trim().chars().count() as f64 / raw_len) >= MIN_CLEANUP_RATIO
}

/// A transcript plus what was done to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cleaned {
    pub text: String,
    pub state: CleanupState,
}

/// Asks the local model to tidy `raw`, and falls back to `raw` itself on any failure.
///
/// The fallback is the OPPOSITE call from local email triage, which deliberately has none, and the
/// difference is which requirement is at stake. There, falling back meant sending message bodies to a
/// remote model — a privacy breach at the moment nobody is watching. Here, falling back means slightly
/// messier text that never leaves the machine. For a typing aid, degraded output beats no output.
///
/// What the fallback may not do is hide: the returned state says `Raw`, so a bad day cannot read as bad
/// cleanup quality — and cleanup quality is exactly what gets tuned by hand.
pub async fn clean_up(voice: &VoiceRuntime, raw: &str) -> Cleaned {
    let Some(model) = voice.cleanup_model.as_deref() else {
        return Cleaned {
            text: raw.to_string(),
            state: CleanupState::Raw,
        };
    };

    let mut cleaned = String::new();
    for chunk in chunk_transcript(raw, CLEANUP_CHUNK_CHARS) {
        let answer = crate::runner::ollama_chat(
            &voice.client,
            &voice.ollama_base_url,
            model,
            &build_cleanup_prompt(&voice.cleanup_prompt, &chunk),
            serde_json::json!({"num_ctx": CLEANUP_NUM_CTX, "temperature": 0}),
            // No grammar: cleanup is plain text in, plain text out. A sampling grammar here would
            // constrain prose to a shape it has no reason to take.
            None,
            false,
        )
        .await;

        match answer {
            Ok(text) => cleaned.push_str(text.trim()),
            Err(error) => {
                tracing::warn!(%error, "voice: cleanup model unreachable; delivering the raw transcript");
                return Cleaned {
                    text: raw.to_string(),
                    state: CleanupState::Raw,
                };
            }
        }
        cleaned.push(' ');
    }

    if accept_cleanup(raw, &cleaned) {
        Cleaned {
            text: cleaned.trim().to_string(),
            state: CleanupState::Cleaned,
        }
    } else {
        tracing::warn!("voice: cleanup came back implausibly short; keeping the raw transcript");
        Cleaned {
            text: raw.to_string(),
            state: CleanupState::Shrunk,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Capture {
    pub id: i64,
    pub kind: String,
    pub created_at: String,
    pub duration_ms: i64,
    pub raw_text: String,
    pub clean_text: Option<String>,
    pub cleanup_state: String,
    pub model: Option<String>,
}

async fn record(
    pool: &sqlx::SqlitePool,
    kind: Kind,
    duration: Duration,
    raw: &str,
    cleaned: &Cleaned,
    model: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let stored_clean = match cleaned.state {
        CleanupState::Cleaned => Some(cleaned.text.as_str()),
        // Raw and Shrunk both mean "there is no cleaned text worth keeping"; `cleanup_state` carries
        // the distinction, so storing the raw text twice would only invite the two to disagree.
        CleanupState::Raw | CleanupState::Shrunk => None,
    };
    let result = sqlx::query(
        "INSERT INTO voice_captures \
         (kind, created_at, duration_ms, raw_text, clean_text, cleanup_state, model) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(kind.as_str())
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(duration.as_millis() as i64)
    .bind(raw)
    .bind(stored_clean)
    .bind(cleaned.state.as_str())
    .bind(model)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

pub async fn list(pool: &sqlx::SqlitePool, kind: Kind) -> Result<Vec<Capture>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, kind, created_at, duration_ms, raw_text, clean_text, cleanup_state, model \
         FROM voice_captures WHERE kind = ? ORDER BY created_at DESC, id DESC",
    )
    .bind(kind.as_str())
    .fetch_all(pool)
    .await
}

/// Scoped by kind as well as id, and the kind is not redundant.
///
/// `voice_captures.id` is ONE sequence shared by both kinds, so without this clause
/// `GET /voice/memos/{id}` happily serves a dictation and `DELETE /voice/memos/{id}` deletes one --
/// a route named for the documents you keep, silently reaching into the retention-bound corpus of
/// everything you have ever dictated. `list` was already kind-scoped; these two were not.
pub async fn get(
    pool: &sqlx::SqlitePool,
    kind: Kind,
    id: i64,
) -> Result<Option<Capture>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, kind, created_at, duration_ms, raw_text, clean_text, cleanup_state, model \
         FROM voice_captures WHERE id = ? AND kind = ?",
    )
    .bind(id)
    .bind(kind.as_str())
    .fetch_optional(pool)
    .await
}

pub async fn delete(pool: &sqlx::SqlitePool, kind: Kind, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM voice_captures WHERE id = ? AND kind = ?")
        .bind(id)
        .bind(kind.as_str())
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Deletes dictations past their retention cutoff. Memos are never touched.
///
/// Dictations expire because keeping them forever builds a searchable record of everything said inside
/// a pillar whose first stated requirement is privacy. Memos do not, because those are documents
/// somebody asked to keep.
pub async fn prune(pool: &sqlx::SqlitePool, retain_days: u8) -> Result<u64, sqlx::Error> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(i64::from(retain_days))).to_rfc3339();
    let result =
        sqlx::query("DELETE FROM voice_captures WHERE kind = 'dictation' AND created_at < ?")
            .bind(cutoff)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// How often retention runs while the daemon is up.
const PRUNE_INTERVAL_HOURS: u64 = 6;

/// Retention's own loop, spawned whether or not the pillar is armed.
///
/// Copied deliberately from the triage loop, whose comment states the reason: dictations already stored
/// do not stop needing to expire because the feature was switched off. Gating this on `armed` would
/// freeze the history at the moment somebody disabled voice — the exact opposite of what disabling it
/// is meant to achieve.
pub async fn run_retention_loop(state: AppState) {
    let mut ticker = tokio::time::interval(Duration::from_secs(PRUNE_INTERVAL_HOURS * 60 * 60));
    loop {
        ticker.tick().await;
        match prune(&state.pool, state.voice.retain_dictations_days).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "voice: pruned expired dictations"),
            Err(error) => tracing::warn!(%error, "voice: could not prune dictations"),
        }
    }
}

/// Why a capture produced no text.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// No transcriber is configured, so the capability does not exist on this machine.
    NotConfigured,
    /// The recording is longer than `MAX_CAPTURE_SECONDS`.
    TooLong,
    /// The transcriber ran and failed.
    Transcription(String),
    /// The transcriber succeeded and heard nothing — a muted microphone or the wrong input device.
    NothingHeard,
}

/// The whole pipeline for one recording: transcribe, apply hints, clean up.
///
/// Split out of `post_capture` so the domain can be tested without a router, a token or a pool, and so
/// the handler is left doing only what a handler should: turn this result into a status code. The
/// module map's rule that no module mixes HTTP transport with domain logic applies inside a module too.
/// One recording's outcome: what was heard, and what was made of it.
///
/// Both halves are kept because they answer different questions. `cleaned` is what the caller pastes;
/// `raw` is what the cleanup prompt gets tuned against, and the only evidence when cleanup made things
/// worse. Collapsing them would mean storing the cleaned text as though it were the transcript.
#[derive(Debug, PartialEq, Eq)]
pub struct Captured {
    /// The transcript with hints applied, before any model saw it.
    pub raw: String,
    pub cleaned: Cleaned,
}

pub async fn capture(
    voice: &VoiceRuntime,
    wav: &[u8],
    duration: Duration,
) -> Result<Captured, CaptureError> {
    let Some(transcriber) = voice.transcriber.as_ref() else {
        return Err(CaptureError::NotConfigured);
    };
    if duration.as_secs() > MAX_CAPTURE_SECONDS {
        return Err(CaptureError::TooLong);
    }

    let raw = transcriber
        .transcribe(wav, duration)
        .await
        .map_err(|error| CaptureError::Transcription(error.to_string()))?;

    // Reported, never pasted: an empty string is indistinguishable from the feature being broken.
    if raw.trim().is_empty() {
        return Err(CaptureError::NothingHeard);
    }

    // Hints run on the RAW transcript, before cleanup, so the model reads correct terminology rather
    // than being asked to guess from something mangled.
    let hinted = apply_hints(&raw, &voice.hints);
    let cleaned = clean_up(voice, &hinted).await;
    Ok(Captured {
        raw: hinted,
        cleaned,
    })
}

#[derive(Deserialize)]
pub struct CaptureQuery {
    pub kind: Kind,
    pub duration_ms: u64,
}

#[derive(Serialize)]
pub struct CaptureResponse {
    pub id: i64,
    pub text: String,
    pub state: CleanupState,
}

/// `POST /voice/capture` — WAV bytes in, cleaned text out.
///
/// The body is raw `application/octet-stream` rather than base64 in JSON: base64 would inflate every
/// recording by a third and force the body ceiling up by the same amount, for nothing.
pub async fn post_capture(
    State(state): State<AppState>,
    Query(query): Query<CaptureQuery>,
    wav: Bytes,
) -> impl IntoResponse {
    let duration = Duration::from_millis(query.duration_ms);
    let work = capture_and_record(
        state.pool.clone(),
        state.voice.clone(),
        query.kind,
        wav,
        duration,
    );

    // Decision 14, and the two kinds genuinely want opposite things here.
    //
    // A memo records something that happened once. A client that goes away mid-capture -- the shell
    // restarting, a laptop sleeping, the Telegram sidecar timing out its turn -- would otherwise drop
    // this future at its last `.await`, and because decision 6 already deleted the audio there is
    // nothing left to retry from: the recording is transcribed and then thrown away, with no row and
    // no reply. So a memo runs in its own task and finishes even if nobody is listening.
    //
    // A dictation is the opposite. Nobody wants the transcript of an utterance they walked away from,
    // and letting it die with its request stops paying a model for an answer with nowhere to go.
    let outcome = match query.kind {
        Kind::Memo => match crate::http::uncancellable(work).await {
            Ok(outcome) => outcome,
            Err(status) => return status.into_response(),
        },
        Kind::Dictation => work.await,
    };

    match outcome {
        Ok(response) => axum::Json(response).into_response(),
        // Not an error condition — the capability genuinely does not exist on this machine, and saying
        // so beats a hotkey that silently does nothing.
        Err(CaptureError::NotConfigured) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no transcriber is configured; voice is off",
        )
            .into_response(),
        Err(CaptureError::TooLong) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a capture may not exceed {MAX_CAPTURE_SECONDS}s"),
        )
            .into_response(),
        Err(CaptureError::Transcription(error)) => {
            tracing::warn!(%error, "voice: transcription failed");
            (StatusCode::BAD_GATEWAY, "the transcriber failed").into_response()
        }
        Err(CaptureError::NothingHeard) => {
            (StatusCode::NO_CONTENT, "nothing was heard").into_response()
        }
    }
}

/// One capture from end to end -- transcribe, clean, persist -- as a single OWNED future.
///
/// Owned, rather than borrowing `AppState`, because `http::uncancellable` needs `'static`: the future
/// has to be able to outlive the request that created it. That is also what puts the audio guard in
/// the right place. `TempAudio` is built inside `transcribe`, so it travels with whatever task holds
/// this future, which §5.5 asks for in as many words -- the guard lives with the work, not with the
/// connection.
async fn capture_and_record(
    pool: sqlx::SqlitePool,
    voice: Arc<VoiceRuntime>,
    kind: Kind,
    wav: Bytes,
    duration: Duration,
) -> Result<CaptureResponse, CaptureError> {
    let captured = capture(&voice, &wav, duration).await?;
    let id = match record(
        &pool,
        kind,
        duration,
        &captured.raw,
        &captured.cleaned,
        voice.cleanup_model.as_deref(),
    )
    .await
    {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, "voice: could not record the capture");
            // The text is what the caller actually needs; losing the row is not worth losing the
            // dictation the person just spoke. `id: 0` says no row exists to fetch later.
            0
        }
    };
    Ok(CaptureResponse {
        id,
        text: captured.cleaned.text,
        state: captured.cleaned.state,
    })
}

pub async fn list_memos(State(state): State<AppState>) -> impl IntoResponse {
    match list(&state.pool, Kind::Memo).await {
        Ok(memos) => axum::Json(memos).into_response(),
        Err(error) => db_error(error),
    }
}

/// `GET /voice/dictations` — the corpus the cleanup prompt gets tuned against (§4.2).
///
/// Read by hand, not by the shell's tab. It exists because storing dictations with no way to read them
/// would be retention of everything said with nothing gained in return.
pub async fn list_dictations(State(state): State<AppState>) -> impl IntoResponse {
    match list(&state.pool, Kind::Dictation).await {
        Ok(dictations) => axum::Json(dictations).into_response(),
        Err(error) => db_error(error),
    }
}

pub async fn get_memo(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    match get(&state.pool, Kind::Memo, id).await {
        Ok(Some(memo)) => axum::Json(memo).into_response(),
        // A dictation's id reaching this route is "no such memo", not a memo. Both kinds draw from
        // one id sequence, so this is the answer that keeps the two surfaces separate.
        Ok(None) => (StatusCode::NOT_FOUND, "no such memo").into_response(),
        Err(error) => db_error(error),
    }
}

pub async fn delete_memo(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    match delete(&state.pool, Kind::Memo, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such memo").into_response(),
        Err(error) => db_error(error),
    }
}

#[derive(Serialize)]
pub struct VoiceConfigView {
    pub armed: bool,
    pub hints: Vec<String>,
    pub cleanup_prompt: String,
    pub cleanup_model: Option<String>,
    pub retain_dictations_days: u8,
    /// The shell reads this rather than defining its own, so the two cannot disagree.
    pub max_capture_seconds: u64,
    pub max_body_bytes: usize,
}

/// `GET /voice/config` — what is in force, for a shell that displays it and does not own it.
pub async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(VoiceConfigView {
        armed: state.voice.armed,
        hints: state.voice.hints.clone(),
        cleanup_prompt: state.voice.cleanup_prompt.clone(),
        cleanup_model: state.voice.cleanup_model.clone(),
        retain_dictations_days: state.voice.retain_dictations_days,
        max_capture_seconds: MAX_CAPTURE_SECONDS,
        max_body_bytes: max_body_bytes(),
    })
}

fn db_error(error: sqlx::Error) -> axum::response::Response {
    tracing::warn!(%error, "voice: database read failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
}

/// Builds the transcriber a configured command implies, or `None` when voice is off.
pub fn transcriber_for(
    config: &crate::config::VoiceConfig,
) -> Option<Arc<dyn crate::transcribe::Transcriber>> {
    config.armed().then(|| {
        Arc::new(crate::transcribe::CommandTranscriber::new(
            config.stt_command.clone(),
        )) as Arc<dyn crate::transcribe::Transcriber>
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn hints() -> Vec<String> {
        vec!["núcleo".to_string(), "NucleOS".to_string()]
    }

    /// Hints must not rewrite the inside of unrelated words.
    ///
    /// The negative half is the point: "nucleoside" contains "nucleo", and a substring replacement
    /// turns it into "núcleoside" — a hint list that was added to fix terminology quietly corrupting
    /// text instead. Whole-word matching is the difference.
    #[test]
    fn hints_match_on_word_boundaries_only() {
        // The motivating case, and the one an ASCII case-insensitive comparison silently misses: the
        // transcriber drops the accent, which is precisely the mistake the hint exists to correct.
        let fixed = apply_hints("the nucleo owns the db", &hints());
        assert_eq!(fixed, "the núcleo owns the db");

        let untouched = apply_hints("a nucleoside is unrelated", &hints());
        assert_eq!(untouched, "a nucleoside is unrelated");

        // Case-insensitive match, canonical spelling out; punctuation survives.
        assert_eq!(apply_hints("nucleos, then.", &hints()), "NucleOS, then.");

        // Already correct text is left exactly as it is, accents and all.
        assert_eq!(
            apply_hints("the núcleo owns the db", &hints()),
            "the núcleo owns the db"
        );
    }

    /// The body ceiling and the duration cap have to be one decision.
    ///
    /// They are the same number in two units, so raising the recording limit without raising the body
    /// limit would produce a 413 on exactly the long memos that are least repeatable. Tying them here
    /// means that mistake breaks `cargo test` instead of a recording.
    #[test]
    fn body_limit_agrees_with_max_duration() {
        let pcm = MAX_CAPTURE_SECONDS * SAMPLE_RATE * BYTES_PER_SAMPLE;
        assert!(
            max_body_bytes() as u64 >= pcm,
            "the ceiling must hold the PCM"
        );
        assert!(
            (max_body_bytes() as u64) < pcm * 2,
            "a ceiling this loose is not a ceiling"
        );
        // The reason the constant exists at all: axum's untuned default would reject a long memo.
        assert!(max_body_bytes() > 2 * 1024 * 1024);
    }

    /// A cleanup that lost a chunk is rejected, and the raw transcript wins.
    #[test]
    fn shrunken_cleanup_is_rejected() {
        let raw = "one. two. three. four. five. six. seven. eight. nine. ten.";

        assert!(!accept_cleanup(raw, "one. two."));
        assert!(!accept_cleanup(raw, "   "));
        // Removing filler legitimately shortens things a little, and must still be accepted.
        assert!(accept_cleanup(
            raw,
            "One. Two. Three. Four. Five. Six. Seven. Eight. Nine."
        ));
    }

    /// Sentences are never cut in half, even when that means an over-long chunk.
    #[test]
    fn chunks_break_on_sentences_not_mid_thought() {
        let chunks = chunk_transcript("aaaa. bbbb. cccc.", 10);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(
                chunk.trim_end().ends_with('.'),
                "cut mid-sentence: {chunk:?}"
            );
        }
    }

    /// The budget is a ceiling even when the transcript offers nowhere good to cut.
    ///
    /// This is what greedy decoding actually returns -- §14.2 made `-bo 1 -bs 1` mandatory for
    /// latency, and it under-produces punctuation. Before this, a wall of words with no full stop
    /// was one "sentence" and went to the model whole, so a twenty-minute memo overflowed
    /// `CLEANUP_NUM_CTX` and lost its cleanup entirely. An earlier version of the test above
    /// asserted exactly that behaviour and so pinned the bug in place rather than the fix.
    #[test]
    fn an_unpunctuated_wall_of_words_is_still_cut_to_the_budget() {
        let wall = "palavra ".repeat(50);
        let chunks = chunk_transcript(&wall, 20);
        assert!(chunks.len() > 1, "an unpunctuated wall must still be split");
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 20, "over budget: {chunk:?}");
        }
        assert_eq!(chunks.concat(), wall, "cutting lost or duplicated text");
    }

    #[test]
    fn a_run_with_no_space_is_cut_without_splitting_a_character() {
        // A dictated URL or path, in a language where a character is not a byte. Byte arithmetic
        // here would panic rather than mis-cut, which is why the cut comes from char_indices.
        let run = "ação".repeat(10);
        let chunks = chunk_transcript(&run, 7);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 7, "over budget: {chunk:?}");
        }
        assert_eq!(chunks.concat(), run);
    }

    #[test]
    fn a_zero_budget_terminates_instead_of_hanging() {
        // Nothing configures this, but a budget of zero has no legal cut and the loop has to be
        // proven to end anyway.
        assert_eq!(chunk_transcript("abc", 0).concat(), "abc");
    }

    /// An unreachable cleanup model yields the raw transcript, FLAGGED raw.
    ///
    /// Both halves matter. Returning nothing would make a local outage look like a broken feature;
    /// returning the raw text as though it were cleaned would make a bad day look like bad cleanup
    /// quality, which is the thing the prompt gets tuned against.
    #[tokio::test]
    async fn unreachable_cleanup_returns_raw_flagged_raw() {
        let voice = VoiceRuntime {
            cleanup_model: Some("qwen3.5:4b".to_string()),
            // Port 1 on loopback: nothing listens, so this is a connection refusal, not a timeout.
            ollama_base_url: "http://127.0.0.1:1".to_string(),
            ..VoiceRuntime::default()
        };

        let cleaned = clean_up(&voice, "the nucleo owns the db").await;

        assert_eq!(cleaned.state, CleanupState::Raw);
        assert_eq!(cleaned.text, "the nucleo owns the db");
    }

    /// With no model configured, cleanup is skipped rather than attempted — and still says `Raw`.
    #[tokio::test]
    async fn unarmed_cleanup_is_raw_not_an_error() {
        let cleaned = clean_up(&VoiceRuntime::default(), "plain words").await;

        assert_eq!(cleaned.state, CleanupState::Raw);
        assert_eq!(cleaned.text, "plain words");
    }

    /// The migration exists and both kinds round-trip through it.
    #[tokio::test]
    async fn migration_creates_voice_captures() {
        let pool = pool().await;

        let cleaned = Cleaned {
            text: "The núcleo owns the DB.".to_string(),
            state: CleanupState::Cleaned,
        };
        let id = record(
            &pool,
            Kind::Memo,
            Duration::from_secs(90),
            "the nucleo owns the db",
            &cleaned,
            Some("qwen3.5:4b"),
        )
        .await
        .unwrap();

        let stored = get(&pool, Kind::Memo, id)
            .await
            .unwrap()
            .expect("the memo was recorded");
        assert_eq!(stored.kind, "memo");
        assert_eq!(stored.cleanup_state, "cleaned");
        assert_eq!(
            stored.clean_text.as_deref(),
            Some("The núcleo owns the DB.")
        );
        assert_eq!(stored.duration_ms, 90_000);

        assert_eq!(list(&pool, Kind::Memo).await.unwrap().len(), 1);
        assert!(list(&pool, Kind::Dictation).await.unwrap().is_empty());
        assert!(delete(&pool, Kind::Memo, id).await.unwrap());
        assert!(!delete(&pool, Kind::Memo, id).await.unwrap());
    }

    /// A raw or shrunk capture stores no cleaned text, so the two can never disagree.
    #[tokio::test]
    async fn a_raw_capture_stores_no_clean_text() {
        let pool = pool().await;

        let id = record(
            &pool,
            Kind::Dictation,
            Duration::from_secs(3),
            "as spoken",
            &Cleaned {
                text: "as spoken".to_string(),
                state: CleanupState::Raw,
            },
            None,
        )
        .await
        .unwrap();

        let stored = get(&pool, Kind::Dictation, id).await.unwrap().unwrap();
        assert_eq!(stored.cleanup_state, "raw");
        assert_eq!(stored.clean_text, None);
        assert_eq!(stored.raw_text, "as spoken");
    }

    /// Retention removes expired dictations and leaves memos alone, however old they are.
    #[tokio::test]
    async fn prune_removes_dictations_past_retention() {
        let pool = pool().await;
        let long_ago = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();

        for kind in ["dictation", "memo"] {
            sqlx::query(
                "INSERT INTO voice_captures \
                 (kind, created_at, duration_ms, raw_text, clean_text, cleanup_state, model) \
                 VALUES (?, ?, 1000, 'old words', NULL, 'raw', NULL)",
            )
            .bind(kind)
            .bind(&long_ago)
            .execute(&pool)
            .await
            .unwrap();
        }
        // A dictation from just now must survive the same sweep.
        record(
            &pool,
            Kind::Dictation,
            Duration::from_secs(1),
            "fresh words",
            &Cleaned {
                text: "fresh words".to_string(),
                state: CleanupState::Raw,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(prune(&pool, 7).await.unwrap(), 1);

        let dictations = list(&pool, Kind::Dictation).await.unwrap();
        assert_eq!(dictations.len(), 1);
        assert_eq!(dictations[0].raw_text, "fresh words");
        assert_eq!(
            list(&pool, Kind::Memo).await.unwrap().len(),
            1,
            "a memo is a document somebody asked to keep; retention must not touch it"
        );
    }

    fn voice_with(transcriber: crate::transcribe::FakeTranscriber) -> VoiceRuntime {
        VoiceRuntime {
            armed: true,
            hints: hints(),
            transcriber: Some(Arc::new(transcriber)),
            ..VoiceRuntime::default()
        }
    }

    /// The end-to-end domain path, with no engine and no router.
    ///
    /// Also pins the ordering that matters: hints are applied to the RAW transcript, and `raw` keeps
    /// that hinted text rather than the cleaned text — storing the cleaned text as the transcript would
    /// destroy the only evidence available when cleanup makes things worse.
    #[tokio::test]
    async fn a_capture_applies_hints_and_keeps_the_raw_transcript() {
        let voice = voice_with(crate::transcribe::FakeTranscriber::returning(
            "the nucleo owns the db",
        ));

        let captured = capture(&voice, b"RIFF", Duration::from_secs(4))
            .await
            .expect("a configured transcriber produces a capture");

        assert_eq!(captured.raw, "the núcleo owns the db");
        // No cleanup model is configured, so the text is delivered raw and SAYS so.
        assert_eq!(captured.cleaned.state, CleanupState::Raw);
        assert_eq!(captured.cleaned.text, "the núcleo owns the db");
    }

    /// Silence is reported, never returned as an empty transcript.
    ///
    /// An empty string pasted into an editor is indistinguishable from the feature being broken, and
    /// the usual causes — a muted microphone, the wrong input device — are worth naming.
    #[tokio::test]
    async fn silence_is_reported_not_pasted() {
        let voice = voice_with(crate::transcribe::FakeTranscriber::returning("   \n  "));

        assert_eq!(
            capture(&voice, b"RIFF", Duration::from_secs(4)).await,
            Err(CaptureError::NothingHeard)
        );
    }

    /// A failing engine is a distinct outcome from silence and from being unconfigured.
    #[tokio::test]
    async fn a_failing_transcriber_is_its_own_outcome() {
        let voice = voice_with(crate::transcribe::FakeTranscriber::failing(
            std::io::ErrorKind::TimedOut,
            "gave up",
        ));

        assert!(matches!(
            capture(&voice, b"RIFF", Duration::from_secs(4)).await,
            Err(CaptureError::Transcription(_))
        ));

        // Unconfigured is NOT a failure: the capability simply does not exist here.
        assert_eq!(
            capture(&VoiceRuntime::default(), b"RIFF", Duration::from_secs(4)).await,
            Err(CaptureError::NotConfigured)
        );
    }

    /// An over-long recording is refused before the engine is asked to do anything.
    ///
    /// Checked against the transcriber's call count, because the point is that nothing was spawned —
    /// the body limit and this check are the same decision at two layers.
    #[tokio::test]
    async fn an_over_long_capture_never_reaches_the_engine() {
        let transcriber = Arc::new(crate::transcribe::FakeTranscriber::returning("unreachable"));
        let voice = VoiceRuntime {
            armed: true,
            transcriber: Some(transcriber.clone()),
            ..VoiceRuntime::default()
        };

        let too_long = Duration::from_secs(MAX_CAPTURE_SECONDS + 1);
        assert_eq!(
            capture(&voice, b"RIFF", too_long).await,
            Err(CaptureError::TooLong)
        );
        assert_eq!(transcriber.calls(), 0);
    }

    /// A configured command produces a transcriber; an unconfigured one produces nothing at all.
    #[test]
    fn a_transcriber_exists_only_when_configured() {
        assert!(transcriber_for(&crate::config::VoiceConfig::default()).is_none());

        let armed = crate::config::VoiceConfig {
            enabled: true,
            stt_command: "whisper-cli -m model.bin".to_string(),
            ..crate::config::VoiceConfig::default()
        };
        assert!(transcriber_for(&armed).is_some());
    }

    /// Decision 14, both halves at once — the pair §10 test 5b names.
    ///
    /// Without this, decision 14 was only an intention: the handler awaited both kinds identically, so
    /// a memo whose client went away mid-transcription was transcribed and then thrown away. No row,
    /// no reply, and decision 6 had already deleted the audio it came from, so there was nothing left
    /// to retry with. A dictation must keep dying, though — that half is not an oversight to fix.
    ///
    /// Abandonment here is `timeout` dropping the future, which is the same drop a disconnecting
    /// client causes. For the memo it drops the future that AWAITS the JoinHandle, and dropping a
    /// JoinHandle only detaches its task; that detachment is exactly the property `uncancellable`
    /// exists to buy, so the test would fail if the handler stopped using it.
    #[tokio::test]
    async fn a_memo_survives_the_client_disconnecting_and_a_dictation_does_not() {
        let pool = pool().await;
        let slow = || {
            Arc::new(crate::transcribe::FakeTranscriber::returning_slowly(
                "gravado em voz alta",
                Duration::from_millis(150),
            )) as Arc<dyn crate::transcribe::Transcriber>
        };
        let runtime = |transcriber| {
            Arc::new(VoiceRuntime {
                armed: true,
                transcriber: Some(transcriber),
                ..VoiceRuntime::default()
            })
        };
        // 20ms against a 150ms transcriber: the drop lands inside transcription, not after it.
        let abandon = Duration::from_millis(20);

        let memo = capture_and_record(
            pool.clone(),
            runtime(slow()),
            Kind::Memo,
            Bytes::from_static(b"wav"),
            Duration::from_secs(3),
        );
        assert!(
            tokio::time::timeout(abandon, crate::http::uncancellable(memo))
                .await
                .is_err(),
            "the request must be gone before transcription could finish"
        );

        // The task is detached, so there is no handle left to await — poll for the row instead of
        // sleeping a guessed interval.
        let mut waited = Duration::ZERO;
        let step = Duration::from_millis(25);
        while list(&pool, Kind::Memo).await.unwrap().is_empty() && waited < Duration::from_secs(5) {
            tokio::time::sleep(step).await;
            waited += step;
        }
        let memos = list(&pool, Kind::Memo).await.unwrap();
        assert_eq!(
            memos.len(),
            1,
            "the memo must persist with nobody listening for the answer"
        );
        assert_eq!(memos[0].raw_text, "gravado em voz alta");

        let dictation = capture_and_record(
            pool.clone(),
            runtime(slow()),
            Kind::Dictation,
            Bytes::from_static(b"wav"),
            Duration::from_secs(3),
        );
        assert!(tokio::time::timeout(abandon, dictation).await.is_err());
        // Twice the transcriber's delay: long enough that a surviving task would have landed by now.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            list(&pool, Kind::Dictation).await.unwrap().is_empty(),
            "an abandoned dictation must leave nothing behind"
        );
    }

    /// The memos-only routes must not reach a dictation, even by its exact id.
    ///
    /// `voice_captures.id` is one sequence shared by both kinds, so an unscoped `WHERE id = ?` let
    /// `GET /voice/memos/{id}` serve a dictation and `DELETE /voice/memos/{id}` delete one — the
    /// memos surface silently reaching into the retention-bound corpus of everything ever dictated.
    #[tokio::test]
    async fn the_memo_routes_cannot_reach_a_dictation() {
        let pool = pool().await;
        let cleaned = Cleaned {
            text: "texto limpo".to_string(),
            state: CleanupState::Cleaned,
        };
        let id = record(
            &pool,
            Kind::Dictation,
            Duration::from_secs(2),
            "texto cru",
            &cleaned,
            None,
        )
        .await
        .unwrap();

        assert!(
            get(&pool, Kind::Memo, id).await.unwrap().is_none(),
            "a dictation must not be readable as a memo"
        );
        assert!(
            !delete(&pool, Kind::Memo, id).await.unwrap(),
            "the memo route must not delete a dictation"
        );
        assert!(
            get(&pool, Kind::Dictation, id).await.unwrap().is_some(),
            "and the dictation must still be there afterwards"
        );
    }

    /// A hint carrying a stray space from a hand-edited list must still match.
    #[test]
    fn a_hint_with_surrounding_whitespace_still_matches() {
        // Untrimmed, this folds to " nucleo " — a form no single token can ever equal. It sat in the
        // file looking active and quietly did nothing, which is the worst way for a hint to fail.
        let hints = vec!["  núcleo  ".to_string()];

        assert_eq!(apply_hints("o nucleo escreve", &hints), "o núcleo escreve");
    }

    #[test]
    fn blank_hints_are_dropped_rather_than_matching_everything() {
        let hints = vec!["   ".to_string(), String::new()];

        assert_eq!(apply_hints("texto intacto", &hints), "texto intacto");
    }
}
