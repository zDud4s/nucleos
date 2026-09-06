//! §spec pilar-de-voz
//!
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

/// What voice cleanup requires from a model, read by `capabilities::missing_capabilities` instead of
/// a fourth copy of the `/api/show` probe `main.rs` runs today. Only the context window: cleanup
/// checks nothing else today, and `capabilities`' own guard test
/// (`nenhum_dos_tres_papeis_exige_hoje_mais_do_que_a_janela`) fails if this ever claims more without
/// that being a deliberate change.
pub const CAPABILITY_REQUIREMENT: crate::capabilities::Requirement =
    crate::capabilities::Requirement {
        context_tokens: CLEANUP_NUM_CTX,
        tools: false,
        vision: false,
        structured_output: false,
    };

/// Which kind of capture this is. The discriminator on `voice_captures`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Short, pasted into whatever had focus, and expires (§7.1).
    Dictation,
    /// Long, kept as a document until deleted.
    Memo,
    /// A turn of conversation: sent to the agent instead of pasted, and answered out loud.
    ///
    /// The third `Kind` and NOT a flag on the first, because it changes what the recording IS. A
    /// dictation is text the person is writing and the núcleo is only holding the pen; a
    /// conversation turn is a question addressed to the agent. Every difference below follows from
    /// that one — no cleanup, no row of its own, and an answer.
    Conversation,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Dictation => "dictation",
            Kind::Memo => "memo",
            Kind::Conversation => "conversation",
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
    /// A cleanup was REFUSED by a guard and the raw transcript was kept instead.
    ///
    /// The name is narrower than the meaning, and deliberately not renamed: this value is written into
    /// `voice_captures.cleanup_state`, whose CHECK constraint names it, so changing the spelling is a
    /// migration and a shell change for no gain. It covers a cleanup that came back implausibly short,
    /// and one that changed a number — see `accept_cleanup` for why the second is a guard rather than a
    /// line in the prompt.
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
    /// The two chords, carried through so `GET /voice/config` can report them.
    ///
    /// The núcleo never listens for a hotkey — it has no desktop. It holds these because the shell
    /// must obey the same `.ai/voice.yaml` the daemon read, and the shell cannot read that file: it is
    /// a self-governing file, and a second reader would be a second answer. Without this the two keys
    /// sat in the config file with nothing anywhere reading them.
    pub hotkey: String,
    pub memo_hotkey: String,
    /// `None` leaves cleanup unarmed, which yields raw transcripts rather than no transcripts.
    pub cleanup_model: Option<String>,
    pub ollama_base_url: String,
    /// Absent means there is nothing to transcribe with, which is the same thing as voice being off.
    pub transcriber: Option<Arc<dyn crate::transcribe::Transcriber>>,
    /// Retained beside the speaker for the reason `stt_command` is retained beside the transcriber:
    /// `health.rs` has to be able to say that the program named here does not exist. A conversation
    /// that hears the question and then answers in silence is the failure this makes reportable.
    pub tts_command: String,
    /// The chord that toggles hands-free conversation, carried for the shell like the other two.
    pub conversation_hotkey: String,
    /// Absent means the núcleo has no voice. NOT the same as voice being off — the conversation
    /// still happens, it is just read rather than heard (`VoiceConfig::speaks`).
    pub speaker: Option<Arc<dyn crate::speak::Speaker>>,
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
            hotkey: String::new(),
            memo_hotkey: String::new(),
            cleanup_model: None,
            ollama_base_url: crate::runner::OLLAMA_BASE_URL.to_string(),
            transcriber: None,
            tts_command: String::new(),
            conversation_hotkey: String::new(),
            speaker: None,
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
            hotkey: config.hotkey.clone(),
            memo_hotkey: config.memo_hotkey.clone(),
            cleanup_model,
            transcriber: transcriber_for(config),
            tts_command: config.tts_command.clone(),
            conversation_hotkey: config.conversation_hotkey.clone(),
            speaker: speaker_for(config),
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
/// `pub(crate)` so `speak.rs` cuts an over-long answer with the SAME logic that cuts an over-long
/// transcript. Two implementations of one cut is how the two come to disagree about where a word ends.
pub(crate) fn split_oversized(sentence: &str, budget: usize) -> Vec<&str> {
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
/// Two guards, both of which answer a measured failure rather than a hypothetical one.
///
/// Length is a crude proxy, and that is the point: the failure it catches is a chunk silently going
/// missing, which is precisely a length event. A cleanup that legitimately removes filler loses a
/// little; one that lost a paragraph loses a lot.
///
/// Digits are not a proxy at all. §14.6 measured a 4B model turning "prune dictations after 7 days"
/// into "pruning dictations after seven days", and the prompt gained a fourth prohibition forbidding
/// exactly that. Running the real pipeline afterwards showed the prohibition DID NOT WORK -- the model
/// produced the same rewrite with the rule in front of it. A small model does not reliably obey a
/// negative instruction of that shape, so this stops being a request and becomes a guard, which is the
/// same move the length check already represents: what must not happen is enforced, not asked for.
pub fn accept_cleanup(raw: &str, cleaned: &str) -> bool {
    if cleaned.trim().is_empty() {
        return false;
    }
    if digit_runs(raw) != digit_runs(cleaned) {
        return false;
    }
    let raw_len = raw.trim().chars().count() as f64;
    if raw_len == 0.0 {
        return true;
    }
    (cleaned.trim().chars().count() as f64 / raw_len) >= MIN_CLEANUP_RATIO
}

/// PURE: every run of digits in a transcript, in the order it appears.
///
/// Compared as an ordered list rather than as a set, which makes one comparison catch three different
/// ways for a cleanup to corrupt what was said: a number spelled out (`7` → `seven`) loses a run, a
/// number invented (`version 1` → `version 1 of 2024`) gains one, and a number moved changes the order.
/// Someone dictating a version, a time, a port or a path needs the characters they said; every one of
/// those is a digit run, and none of them survives being paraphrased.
fn digit_runs(text: &str) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(tail.len());
        runs.push(&tail[..end]);
        rest = &tail[end..];
    }
    runs
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
    /// A conversation turn arrived without naming a chat to hold it.
    ///
    /// Refused rather than defaulted to some "current" chat, because there is no such thing here: the
    /// núcleo has no window and no idea which conversation is open. A default would send a spoken
    /// question into whichever chat happened to sort first.
    NoChat,
    /// The chat refused the turn, and this is what it said.
    Refused(String),
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

/// What the microphone actually said, with hints applied and nothing else done to it.
///
/// Split out of `capture` because the two consumers diverge here and only here. A dictation goes on
/// to cleanup; a conversation turn goes to the agent. Everything BEFORE this point — the transcriber,
/// the length ceiling, the empty-transcript refusal, the hints — is identical for both, and one
/// pipeline with several consumers is the promise decision 1 of the voice design made.
///
/// Hints run here, on the raw transcript, for both. It is tempting to think a conversation does not
/// need them because no human reads the text — but the agent does, and a question about the `núcleo`
/// transcribed as "nucleo" is a question about something else.
async fn heard(
    voice: &VoiceRuntime,
    audio: &[u8],
    extension: &str,
    duration: Duration,
) -> Result<String, CaptureError> {
    let Some(transcriber) = voice.transcriber.as_ref() else {
        return Err(CaptureError::NotConfigured);
    };
    if duration.as_secs() > MAX_CAPTURE_SECONDS {
        return Err(CaptureError::TooLong);
    }

    let raw = transcriber
        .transcribe(audio, extension, duration)
        .await
        .map_err(|error| CaptureError::Transcription(error.to_string()))?;

    // Reported, never pasted: an empty string is indistinguishable from the feature being broken.
    if raw.trim().is_empty() {
        return Err(CaptureError::NothingHeard);
    }

    // Hints run on the RAW transcript, before cleanup, so the model reads correct terminology rather
    // than being asked to guess from something mangled.
    Ok(apply_hints(&raw, &voice.hints))
}

pub async fn capture(
    voice: &VoiceRuntime,
    audio: &[u8],
    extension: &str,
    duration: Duration,
) -> Result<Captured, CaptureError> {
    let hinted = heard(voice, audio, extension, duration).await?;
    let cleaned = clean_up(voice, &hinted).await;
    Ok(Captured {
        raw: hinted,
        cleaned,
    })
}

/// What a conversation turn produced: what was heard, and where the answer will appear.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Conversed {
    /// The transcript, so the window can show what it thinks was said WITHOUT waiting for the answer.
    /// A hands-free mode that shows nothing until the agent replies is a mode in which a
    /// misheard question is invisible until it has already been answered.
    pub text: String,
    /// `None` when the message was queued behind a turn already in flight.
    pub turn_id: Option<i64>,
    pub queued: bool,
}

/// One turn of conversation: transcribe, hand to the chat, and say where the answer will be.
///
/// Three things this deliberately does NOT do, each of which was a live option:
///
/// - **No cleanup.** The cleanup model exists to make text presentable where it will be READ — pasted
///   into a document, kept as a memo. Nobody reads a conversation turn; the agent does. §14.4 measured
///   that with a resident server cleanup becomes "the big half" of the latency, so skipping it is the
///   largest saving available and it costs nothing anybody would notice.
/// - **No row in `voice_captures`.** The turn is already stored as a chat message and a `runs` row.
///   Writing it a third time would create a second place to look for the same sentence — and the
///   table's own CHECK constraint (`kind IN ('dictation','memo')`) enforces this, so a future wiring
///   mistake fails loudly instead of quietly duplicating.
/// - **No opinion about who answers.** `send_or_queue` routes by the chat's `answered_by`, exactly as
///   a typed message does. Voice is an input channel, not a second brain.
pub async fn converse(
    state: &AppState,
    chat_id: &str,
    audio: &[u8],
    extension: &str,
    duration: Duration,
) -> Result<Conversed, CaptureError> {
    if chat_id.trim().is_empty() {
        return Err(CaptureError::NoChat);
    }
    let text = heard(&state.voice, audio, extension, duration).await?;

    // `send_or_queue` rather than `send_message`, and the choice is the opposite of the Telegram
    // sidecar's. Hands-free means the next thing said arrives while the last answer is still being
    // spoken, and refusing it would make the mode drop every second utterance. The person is here,
    // watching; waiting is what they expect.
    match crate::assistant::send_or_queue(
        state,
        chat_id,
        &text,
        &[],
        crate::assistant::Origin::Voice,
    )
    .await
    {
        Ok(crate::assistant::Sent::Turn(turn_id)) => Ok(Conversed {
            text,
            turn_id: Some(turn_id),
            queued: false,
        }),
        Ok(crate::assistant::Sent::Queued) => Ok(Conversed {
            text,
            turn_id: None,
            queued: true,
        }),
        Err(refusal) => Err(CaptureError::Refused(refusal)),
    }
}

#[derive(Deserialize)]
pub struct CaptureQuery {
    pub kind: Kind,
    pub duration_ms: u64,
    /// Which container the body is in, as a bare extension. Absent means WAV.
    ///
    /// Resolved through `transcribe::extension_for`, which answers from an allowlist rather than
    /// echoing this string — the value names a file this process creates.
    pub format: Option<String>,
    /// Which conversation a `kind=conversation` recording belongs to. Ignored by the other two kinds,
    /// and required by that one — see `CaptureError::NoChat` for why it has no default.
    pub chat_id: Option<String>,
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
    // Resolved before any work starts, and refused loudly rather than silently treated as WAV: a
    // transcriber handed Opus in a file named `.wav` fails in a way that reads as a broken recording.
    let Some(extension) = crate::transcribe::extension_for(query.format.as_deref()) else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported audio format; send wav, ogg, oga, opus, mp3, m4a, flac or webm",
        )
            .into_response();
    };
    // Branched before the recording path, because a conversation turn shares the transcriber and
    // nothing after it: no cleanup, no row, and an answer instead of a paste.
    //
    // Cancellable, like a dictation and unlike a memo (decision 14). The window this leaves open is
    // only the transcription: once `send_or_queue` has returned, the turn is a registered task that
    // finishes whoever is listening. And nobody wants an answer to a question they walked away from.
    if query.kind == Kind::Conversation {
        return match converse(
            &state,
            query.chat_id.as_deref().unwrap_or_default(),
            &wav,
            extension,
            duration,
        )
        .await
        {
            Ok(conversed) => axum::Json(conversed).into_response(),
            Err(error) => capture_error(error).into_response(),
        };
    }

    let work = capture_and_record(
        state.pool.clone(),
        state.voice.clone(),
        query.kind,
        wav,
        extension,
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
        // A dictation dies with its request. A conversation turn would too — but none reaches here:
        // `Kind::Conversation` returned above, before `work` was built. The arm is written out rather
        // than folded into a `_` so that a fourth kind is a compile error here instead of silently
        // inheriting a cancellation policy nobody chose for it.
        Kind::Dictation | Kind::Conversation => work.await,
    };

    match outcome {
        Ok(response) => axum::Json(response).into_response(),
        Err(error) => capture_error(error).into_response(),
    }
}

/// One capture failure as a status code, so both paths through `post_capture` answer identically.
///
/// Factored out when conversation arrived rather than copied: two `match`es over the same enum are
/// two places for a new variant to be forgotten, and the one that forgets it answers 500.
fn capture_error(error: CaptureError) -> axum::response::Response {
    match error {
        // Not an error condition — the capability genuinely does not exist on this machine, and saying
        // so beats a hotkey that silently does nothing.
        CaptureError::NotConfigured => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no transcriber is configured; voice is off",
        )
            .into_response(),
        CaptureError::TooLong => (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a capture may not exceed {MAX_CAPTURE_SECONDS}s"),
        )
            .into_response(),
        CaptureError::Transcription(error) => {
            tracing::warn!(%error, "voice: transcription failed");
            (StatusCode::BAD_GATEWAY, "the transcriber failed").into_response()
        }
        CaptureError::NothingHeard => (StatusCode::NO_CONTENT, "nothing was heard").into_response(),
        CaptureError::NoChat => (
            StatusCode::BAD_REQUEST,
            "a conversation turn must name a chat_id",
        )
            .into_response(),
        // The chat said no for a reason it knows and this does not — a kill switch, a chat set to a
        // local model this machine does not have. Passed through verbatim: `assistant.rs` writes
        // these sentences to be read by a person, and paraphrasing them here would lose the only
        // explanation there is.
        CaptureError::Refused(refusal) => (StatusCode::CONFLICT, refusal).into_response(),
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
    audio: Bytes,
    extension: &'static str,
    duration: Duration,
) -> Result<CaptureResponse, CaptureError> {
    let captured = capture(&voice, &audio, extension, duration).await?;
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
    /// The two chords the shell must register.
    ///
    /// Reported rather than left to the shell to decide, because `.ai/voice.yaml` is where they are
    /// configured and the shell may not read that file — it is self-governing, and a second reader
    /// would be a second answer. Before this endpoint carried them, both keys existed in the config
    /// and nothing anywhere read either one.
    pub hotkey: String,
    pub memo_hotkey: String,
    pub conversation_hotkey: String,
    /// Whether an answer can be SPOKEN, as opposed to merely arrived at.
    ///
    /// Separate from `armed` because the two failures are different sizes and the window must be able
    /// to tell them apart: unarmed means no conversation at all, while `speaks: false` means the
    /// conversation happens and is read instead of heard. A single flag would hide a working feature
    /// behind a missing one.
    pub speaks: bool,
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
        hotkey: state.voice.hotkey.clone(),
        memo_hotkey: state.voice.memo_hotkey.clone(),
        conversation_hotkey: state.voice.conversation_hotkey.clone(),
        speaks: state.voice.speaker.is_some(),
        max_capture_seconds: MAX_CAPTURE_SECONDS,
        max_body_bytes: max_body_bytes(),
    })
}

fn db_error(error: sqlx::Error) -> axum::response::Response {
    tracing::warn!(%error, "voice: database read failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
}

/// The answer `turn_id` has produced so far, and whether more can still arrive.
///
/// Two sources, and which one applies is decided by the tail's own lifetime rather than by asking the
/// database first. `runs::read_tail` returns `Some` exactly while the run is registered in THIS
/// daemon; when the run ends, its `Registration` drops and the tail disappears, so the absence is the
/// signal that the durable copy is now the truth.
///
/// The two answer paths store their result differently, and getting this wrong would make voice work
/// for one brain and not the other — and, since `spawn_local_turn` now drives more than one brain
/// itself, for one of ITS routes and not the other:
///
/// - **Every turn `spawn_local_turn` drives writes `stdout` as the plain answer** — the local model
///   on this machine and the hosted one over OpenRouter alike — and streams nothing at all, so its
///   tail stays empty for the whole turn and everything arrives at once at the end. That is not a
///   defect to work around: it is why `speakable` had to be a pure function of whatever text exists,
///   rather than a subscriber to a stream that only the other path has. `assistant::
///   answered_by_a_local_agent_loop` is the one place that knows which `answered_by` values these
///   are, so this reads through it rather than spelling `"local"` out and quietly leaving a hosted
///   turn's answer for the branch below to mangle as JSONL — which is exactly what happened here
///   before that helper existed.
/// - **A CLI turn streams JSONL** into the tail and stores the same stream in `stdout`, so the
///   finished text comes back through `extract_reply`.
async fn answer_so_far(state: &AppState, turn_id: i64) -> Option<(String, bool)> {
    if let Some(stream) = crate::runs::read_tail(&state.run_tails, turn_id, 0) {
        return Some((crate::runner::live_from_stream(&stream).text, false));
    }

    let row: Option<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT status, stdout, answered_by FROM runs WHERE id = ?")
            .bind(turn_id)
            .fetch_optional(&state.pool)
            .await
            .ok()?;
    let (status, stdout, answered_by) = row?;
    let stdout = stdout.unwrap_or_default();
    let text = if answered_by
        .as_deref()
        .is_some_and(crate::assistant::answered_by_a_local_agent_loop)
    {
        stdout
    } else {
        crate::runner::extract_reply(&stdout).unwrap_or_default()
    };
    // Anything not `running` is over, however it ended. A turn that failed or was cancelled has no
    // more text coming, and saying so is what stops the window polling a dead turn forever.
    Some((text, status != "running"))
}

/// `GET /voice/turns/{turn_id}/speech/{index}` — one unit of the answer, as WAV.
///
/// Pull-driven and stateless, which was a choice against the obvious alternative: a background task
/// that synthesises the whole answer as it arrives and holds the audio in a map. That map would need
/// eviction, would keep speech for turns nobody is listening to, and — the deciding argument — would
/// synthesise everything that gets INTERRUPTED. Hands-free conversation exists so a person can cut in;
/// barge-in is the ordinary path, not the exception. Here, cutting in simply stops the next request
/// being made, and nothing was spent.
///
/// The three answers are distinct on purpose, because the window does three different things with
/// them: **200** plays it and asks for the next, **204** waits and asks again, **404** stops asking.
/// Collapsing "not yet" and "no more" into one status is how a client comes to poll forever.
///
/// Not restricted to conversation turns. Reading any answer aloud is a legitimate thing to want, the
/// endpoint is local and authenticated, and each request synthesises at most `MAX_SPEAK_CHARS` — so
/// there is no bound to enforce that `speakable` is not already enforcing.
pub async fn get_turn_speech(
    State(state): State<AppState>,
    Path((turn_id, index)): Path<(i64, usize)>,
) -> impl IntoResponse {
    let Some(speaker) = state.voice.speaker.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no speaker is configured; the answer will have to be read",
        )
            .into_response();
    };
    let Some((text, finished)) = answer_so_far(&state, turn_id).await else {
        return (StatusCode::NOT_FOUND, "no such turn").into_response();
    };

    let units = crate::speak::speakable(&text, finished);
    let Some(unit) = units.get(index) else {
        return if finished {
            (StatusCode::NOT_FOUND, "no further speech").into_response()
        } else {
            // Deliberately not an error. This is what every poll of a turn that is still thinking
            // looks like, and it is the common path.
            StatusCode::NO_CONTENT.into_response()
        };
    };

    match speaker.speak(unit).await {
        Ok(audio) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "audio/wav")],
            audio,
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, turn_id, index, "voice: synthesis failed");
            (StatusCode::BAD_GATEWAY, "the speaker failed").into_response()
        }
    }
}

/// Builds the speaker a configured command implies, or `None` when nothing can be said out loud.
///
/// The twin of `transcriber_for`, gated on `speaks()` rather than `armed()` — a machine with an STT
/// engine and no TTS one has a working conversation that answers in writing, and treating that as
/// "off" would take the feature away from every machine on the day it ships.
pub fn speaker_for(config: &crate::config::VoiceConfig) -> Option<Arc<dyn crate::speak::Speaker>> {
    if !config.speaks() {
        return None;
    }
    let url = config.tts_url.trim();
    if url.is_empty() {
        return Some(Arc::new(crate::speak::CommandSpeaker::new(
            config.tts_command.clone(),
        )));
    }
    // Said out loud rather than resolved in silence. Both keys set is not an error — the URL is
    // simply the better one — but a config line that is ignored without comment is how somebody
    // spends an evening editing a `tts_command` that nothing reads.
    if !config.tts_command.trim().is_empty() {
        tracing::warn!(
            "voice: both tts_url and tts_command are set; using tts_url, which is roughly ten times faster per sentence"
        );
    }
    Some(Arc::new(crate::speak::HttpSpeaker::new(url.to_string())))
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

        let captured = capture(&voice, b"RIFF", "wav", Duration::from_secs(4))
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
            capture(&voice, b"RIFF", "wav", Duration::from_secs(4)).await,
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
            capture(&voice, b"RIFF", "wav", Duration::from_secs(4)).await,
            Err(CaptureError::Transcription(_))
        ));

        // Unconfigured is NOT a failure: the capability simply does not exist here.
        assert_eq!(
            capture(
                &VoiceRuntime::default(),
                b"RIFF",
                "wav",
                Duration::from_secs(4)
            )
            .await,
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
            capture(&voice, b"RIFF", "wav", too_long).await,
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
            "wav",
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
            "wav",
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

    /// Proves the whole pipeline against the REAL transcriber and the REAL cleanup model.
    ///
    /// `#[ignore]` because it needs three things a gate cannot assume: a CUDA whisper build, a running
    /// Ollama, and a recording on disk. Everything else in this file uses a fake transcriber, which
    /// proves the wiring and proves nothing about whether the configured command actually works —
    /// and a `stt_command` that does not spawn is the single most likely way for this pillar to be
    /// broken on a given machine.
    ///
    /// Run it deliberately, from the repository root:
    ///   CARGO_TARGET_DIR=target/gate cargo test -p nucleos-core -- --ignored --nocapture real_pipeline
    ///
    /// Paths resolve through `CARGO_MANIFEST_DIR` because cargo runs tests with the working directory
    /// set to the PACKAGE root (`core/`), while the daemon reads `.ai/voice.yaml` relative to wherever
    /// it was launched. A bare relative path here would look for `core/.ai/voice.yaml`.
    #[tokio::test]
    #[ignore = "needs a CUDA whisper build, a running Ollama, and a probe recording"]
    async fn real_pipeline_transcribes_and_cleans_an_actual_recording() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("core/ has a parent");
        let config = crate::config::load_voice_config(&repo.join(".ai/voice.yaml"));
        assert!(
            config.armed(),
            "`.ai/voice.yaml` must set enabled: true and a stt_command for this test to mean anything"
        );
        let models = crate::config::load_models_config(&repo.join(".ai/nucleos-models.yaml"))
            .expect("the daemon's own models config must parse");
        let cleanup_model = models
            .voice_cleanup_model
            .clone()
            .expect("`voice_cleanup_model` must be set for this test to exercise cleanup");
        let voice = VoiceRuntime::from_config(&config, Some(cleanup_model));

        // 5.6s of TTS speech. Valid for latency and for "does the command work"; NOT valid for
        // accuracy, because a synthesised voice is easier than a real one (§14.1).
        let wav = std::fs::read("C:/Projects/whisper/probe_short.wav")
            .expect("the step-A probe recording must be on disk");

        let captured = capture(&voice, &wav, "wav", Duration::from_millis(5600))
            .await
            .expect("the configured transcriber must produce a transcript");

        println!("raw:     {:?}", captured.raw);
        println!("cleaned: {:?}", captured.cleaned.text);
        println!("state:   {:?}", captured.cleaned.state);

        assert!(
            !captured.raw.trim().is_empty(),
            "an empty transcript means the command ran and said nothing, which is a config failure"
        );
        // The probe says "voice captures table" and "prune dictations after 7 days", so this word is
        // in the audio and its absence would mean the wrong file was read.
        assert!(
            captured.raw.to_lowercase().contains("dictation"),
            "the transcript does not match the probe recording: {:?}",
            captured.raw
        );
        // `Raw` would mean Ollama was unreachable, which makes the rest of this test vacuous.
        assert_ne!(
            captured.cleaned.state,
            CleanupState::Raw,
            "the cleanup model was not reached at all, so nothing here was exercised"
        );
        // The invariant, asserted instead of the outcome: the numeral survives. It does not matter
        // whether cleanup was accepted or refused — what must never happen is the digit being
        // paraphrased into the text a person then pastes into a config file.
        //
        // This is the assertion that caught the real defect. The fourth prohibition alone did NOT hold:
        // the model returned "after seven days" with the rule in front of it, which is why
        // `accept_cleanup` now enforces digits rather than asking for them.
        assert!(
            captured.cleaned.text.contains('7'),
            "the numeral did not survive cleanup: {:?}",
            captured.cleaned.text
        );
    }

    /// The guard that the prompt could not be trusted to enforce.
    ///
    /// Measured, not imagined: with the fourth prohibition in the prompt, `qwen3.5:4b` still turned
    /// "prune dictations after 7 days" into "pruning dictations after seven days". A cleanup that
    /// paraphrases a number is worse than no cleanup, because the number is usually the reason the
    /// sentence was dictated — a version, a port, a time, a path.
    #[test]
    fn a_cleanup_that_spells_out_a_number_is_refused() {
        assert!(!accept_cleanup(
            "prune dictations after 7 days",
            "pruning dictations after seven days"
        ));
    }

    #[test]
    fn a_cleanup_that_keeps_the_digits_is_accepted() {
        // Punctuation, capitalisation and filler removal are what cleanup is FOR. Only the numbers
        // are protected.
        assert!(accept_cleanup(
            "um prune dictations after 7 days",
            "Prune dictations after 7 days."
        ));
    }

    #[test]
    fn a_cleanup_that_invents_a_number_is_refused() {
        // The same guard, catching the first prohibition instead of the fourth: a model that adds a
        // year nobody said has added information, and the ordered comparison sees the extra run.
        assert!(!accept_cleanup(
            "release version 1",
            "release version 1 of 2024"
        ));
    }

    #[test]
    fn a_cleanup_that_reorders_numbers_is_refused() {
        assert!(!accept_cleanup(
            "ports 8791 and 11434",
            "ports 11434 and 8791"
        ));
    }

    #[test]
    fn a_transcript_with_no_numbers_is_judged_on_length_alone() {
        assert!(accept_cleanup(
            "olá isto é uma frase",
            "Olá, isto é uma frase."
        ));
        assert!(!accept_cleanup(
            "uma frase inteira que foi dita em voz alta",
            "frase"
        ));
    }

    /// The hotkeys have to survive the trip from the config file to the runtime.
    ///
    /// They are the only two config values the núcleo itself never acts on — it has no desktop — so
    /// nothing else in the daemon would notice them being dropped. Before they were carried, both keys
    /// were dead config: readable in `.ai/voice.yaml`, and read by nothing.
    #[test]
    fn the_hotkeys_reach_the_runtime_so_the_shell_can_obey_the_config() {
        let config = crate::config::VoiceConfig {
            enabled: true,
            stt_command: "whisper-cli -m model.bin".to_string(),
            hotkey: "Ctrl+Shift+D".to_string(),
            memo_hotkey: "Ctrl+Shift+M".to_string(),
            ..crate::config::VoiceConfig::default()
        };

        let runtime = VoiceRuntime::from_config(&config, None);

        assert_eq!(runtime.hotkey, "Ctrl+Shift+D");
        assert_eq!(runtime.memo_hotkey, "Ctrl+Shift+M");
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

    // ---------------------------------------------------------------------------------------------
    // Conversation: the second consumer of this same pipeline.
    // ---------------------------------------------------------------------------------------------

    /// An `AppState` whose voice runtime is whatever the test needs and whose runner never runs.
    async fn conversing_state(voice: VoiceRuntime) -> AppState {
        AppState {
            token: crate::auth::Token("t".into()),
            pool: pool().await,
            runner: Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            // A doctrine is a Telegram channel's standing instruction. A conversation held at this
            // machine has none by definition, so `None` here is the value under test, not a stub.
            telegram_doctrine: None,
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(voice),
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::github::GithubRuntime::default()),
            web: Arc::new(crate::web::WebRuntime::disabled()),
            calendar: Arc::new(crate::calendar::CalendarRuntime::default()),
            council: Arc::new(crate::council::CouncilRuntime::default()),
            workflow_library: None,
            machine_config_root: None,
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// A voice runtime that hears `heard`, with hints armed and a cleanup model named.
    ///
    /// The cleanup model is named ON PURPOSE in the conversation tests: it is what makes
    /// "cleanup was skipped" an assertable fact rather than a property of an unconfigured runtime.
    fn hearing(heard: &str) -> VoiceRuntime {
        VoiceRuntime {
            armed: true,
            hints: hints(),
            cleanup_model: Some("qwen3.5:4b".to_string()),
            // Nothing listens here. A cleanup that ran would have to reach it, and a cleanup that is
            // skipped never notices.
            ollama_base_url: "http://127.0.0.1:1".to_string(),
            transcriber: Some(Arc::new(crate::transcribe::FakeTranscriber::returning(
                heard,
            ))),
            ..VoiceRuntime::default()
        }
    }

    /// The turn goes to the chat, and NOTHING is written to `voice_captures`.
    ///
    /// The absence is the assertion. A conversation turn is already stored twice — as a chat message
    /// and as a `runs` row — and a third copy would be a third place to look for one sentence. The
    /// table's CHECK constraint would refuse the insert anyway; this proves nobody tries.
    #[tokio::test]
    async fn a_conversation_is_sent_to_the_chat_and_recorded_nowhere_else() {
        let state = conversing_state(hearing("o que esta a correr?")).await;

        let conversed = converse(&state, "a-conversa", b"RIFF", "wav", Duration::from_secs(3))
            .await
            .expect("the turn was accepted");

        assert_eq!(conversed.text, "o que esta a correr?");
        assert!(conversed.turn_id.is_some());
        assert!(!conversed.queued);

        assert!(list(&state.pool, Kind::Dictation).await.unwrap().is_empty());
        assert!(list(&state.pool, Kind::Memo).await.unwrap().is_empty());
    }

    /// What the agent is asked is what was heard, with hints applied and nothing else done to it.
    ///
    /// Both halves matter. Hints must run — a question about the `núcleo` heard as "nucleo" is a
    /// question about something else. Cleanup must not: the `ollama_base_url` above points at a
    /// closed port, so a cleanup that ran would have to fail and fall back, which is indistinguishable
    /// from success here — but it would also cost a round trip per chunk on the one path whose whole
    /// requirement is latency.
    #[tokio::test]
    async fn the_agent_is_asked_exactly_what_was_heard() {
        let state = conversing_state(hearing("o nucleo esta a correr?")).await;

        let conversed = converse(&state, "hints", b"RIFF", "wav", Duration::from_secs(2))
            .await
            .unwrap();

        assert_eq!(conversed.text, "o núcleo esta a correr?");
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(conversed.turn_id.unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(prompt, "o núcleo esta a correr?");
    }

    /// The turn is recorded as having arrived by voice, and survives the round trip.
    ///
    /// `Origin` is written to the row so a queued message is sent as the thing it was. A voice turn
    /// that came back as `shell` would answer in writing.
    #[test]
    fn a_spoken_turn_is_recorded_as_spoken() {
        assert_eq!(crate::assistant::Origin::Voice.as_wire(), "voice");
        assert_eq!(
            crate::assistant::Origin::from_wire(Some("voice")),
            crate::assistant::Origin::Voice
        );
    }

    /// There is no "current chat" in a daemon with no window, so a turn that names none is refused.
    #[tokio::test]
    async fn a_conversation_that_names_no_chat_is_refused_rather_than_guessed() {
        let state = conversing_state(hearing("olá")).await;

        assert_eq!(
            converse(&state, "   ", b"RIFF", "wav", Duration::from_secs(1)).await,
            Err(CaptureError::NoChat)
        );
        // Refused BEFORE the transcriber ran: there was nowhere to send the answer, so recording the
        // question would have been work done for nothing — and a recording made for nothing is still
        // a recording of somebody's voice.
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0);
    }

    /// A machine with an STT engine and no TTS one still converses — it just answers in writing.
    ///
    /// The regression this guards is collapsing `speaks` into `armed`, which would take conversation
    /// away from every machine that has not installed a voice yet. Which is all of them, today.
    #[test]
    fn hearing_without_speaking_is_a_working_pillar() {
        let config = crate::config::VoiceConfig {
            enabled: true,
            stt_command: "whisper-cli -m model.bin".to_string(),
            tts_command: String::new(),
            ..Default::default()
        };

        assert!(config.armed());
        assert!(!config.speaks());
        assert!(transcriber_for(&config).is_some());
        assert!(speaker_for(&config).is_none());
    }

    /// A resident engine is a voice too — `speaks()` may not be a synonym for `tts_command`.
    ///
    /// The regression this guards is the obvious one when a second implementation arrives: the
    /// capability check keeps naming the first, so a machine configured entirely correctly for the
    /// FASTER path reports having no voice at all.
    #[test]
    fn a_resident_engine_is_a_voice_even_with_no_command() {
        let config = crate::config::VoiceConfig {
            enabled: true,
            stt_command: "whisper-cli".to_string(),
            tts_url: "http://127.0.0.1:5017".to_string(),
            ..Default::default()
        };

        assert!(config.speaks());
        assert!(speaker_for(&config).is_some());
    }

    /// Both set is not an error, and the URL wins — because the difference is ~2.8 s a sentence.
    ///
    /// Asserted through `FakeSpeaker`-free means: what is observable here is that a speaker exists
    /// and that the *command* is never consulted, which is what the `speak.rs` measurement makes
    /// worth enforcing. A spawning speaker built from this command would try to run `no-such-program`.
    #[tokio::test]
    async fn a_resident_engine_wins_over_a_spawning_one() {
        let config = crate::config::VoiceConfig {
            enabled: true,
            stt_command: "whisper-cli".to_string(),
            tts_command: "no-such-program-anywhere".to_string(),
            // Nothing listens here, which is the point: the failure must be a REFUSED CONNECTION and
            // not a missing program. Those are the two implementations' distinct signatures.
            tts_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };

        let speaker = speaker_for(&config).expect("both configured means a speaker exists");
        let error = speaker.speak("olá").await.unwrap_err();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused,
            "the spawning speaker was built instead of the resident one: {error}"
        );
    }

    /// And a voice configured on a pillar that is off is not a capability either.
    #[test]
    fn a_speaker_without_a_transcriber_is_not_a_capability() {
        let config = crate::config::VoiceConfig {
            enabled: true,
            stt_command: String::new(),
            tts_command: "piper -m voz.onnx -f -".to_string(),
            ..Default::default()
        };

        assert!(!config.armed());
        assert!(!config.speaks());
        assert!(speaker_for(&config).is_none());
    }

    // ---------------------------------------------------------------------------------------------
    // Speech: one unit of an answer at a time.
    // ---------------------------------------------------------------------------------------------

    /// A finished local turn whose answer is `answer`, as `spawn_local_turn` would have left it.
    async fn finished_local_turn(pool: &sqlx::SqlitePool, answer: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, stdout, created_at) \
             VALUES ('perguntei', 'completed', 'assistant', 's', 'c', 'local', ?, ?)",
        )
        .bind(answer)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A finished HOSTED turn whose answer is `answer`, as `spawn_local_turn` would have left it
    /// when the model behind it was reached over OpenRouter rather than Ollama — same plain
    /// `stdout`, same nothing streamed, and the only difference from `finished_local_turn` above is
    /// the one wire word this whole fix is about.
    async fn finished_hosted_turn(pool: &sqlx::SqlitePool, answer: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, stdout, created_at) \
             VALUES ('perguntei', 'completed', 'assistant', 's', 'c', 'openrouter', ?, ?)",
        )
        .bind(answer)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A running turn, as it looks for every poll before the answer exists.
    async fn running_turn(pool: &sqlx::SqlitePool, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, created_at) \
             VALUES ('perguntei', ?, 'assistant', 's', 'c', 'local', ?)",
        )
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn speaking(audio: &[u8]) -> VoiceRuntime {
        VoiceRuntime {
            armed: true,
            speaker: Some(Arc::new(crate::speak::FakeSpeaker::returning(audio))),
            ..VoiceRuntime::default()
        }
    }

    async fn body_of(response: axum::response::Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    /// The three answers are distinct, and the window does three different things with them.
    ///
    /// Sentence by sentence until they run out, then `404` — which is what tells a hands-free client
    /// to stop asking. A `204` in that position would poll a finished turn forever.
    #[tokio::test]
    async fn a_finished_answer_is_spoken_one_sentence_at_a_time_and_then_ends() {
        let state = conversing_state(speaking(b"RIFFfake")).await;
        let turn = finished_local_turn(&state.pool, "Esta a correr. Acabou agora.").await;

        let first = get_turn_speech(State(state.clone()), Path((turn, 0)))
            .await
            .into_response();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(
            first
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "audio/wav"
        );
        assert_eq!(body_of(first).await, b"RIFFfake");

        let second = get_turn_speech(State(state.clone()), Path((turn, 1)))
            .await
            .into_response();
        assert_eq!(second.status(), StatusCode::OK);

        let past_the_end = get_turn_speech(State(state.clone()), Path((turn, 2)))
            .await
            .into_response();
        assert_eq!(past_the_end.status(), StatusCode::NOT_FOUND);
    }

    /// What was spoken is the sentence, not the whole answer.
    ///
    /// The bug this catches is an off-by-one that synthesises unit 0 for every index — audible as the
    /// same sentence read twice, which is easy to mistake for a stutter in the player.
    #[tokio::test]
    async fn each_index_speaks_its_own_sentence() {
        let fake = Arc::new(crate::speak::FakeSpeaker::returning(b"RIFF"));
        let state = conversing_state(VoiceRuntime {
            armed: true,
            speaker: Some(fake.clone()),
            ..VoiceRuntime::default()
        })
        .await;
        let turn = finished_local_turn(&state.pool, "Primeira. Segunda.").await;

        for index in 0..2 {
            let _ = get_turn_speech(State(state.clone()), Path((turn, index)))
                .await
                .into_response();
        }
        assert_eq!(fake.said(), vec!["Primeira.", "Segunda."]);
    }

    /// A hosted turn's answer is spoken, not swallowed as unparsed JSONL.
    ///
    /// The hazard `assistant::answered_by_a_local_agent_loop` exists to close: `spawn_local_turn`
    /// writes `stdout` as the plain answer for a turn answered over OpenRouter exactly as it does
    /// for a local one, but `answer_so_far` used to recognise only `answered_by == "local"` by name
    /// — so a hosted turn's plain sentence fell to the branch built for a CLI's streamed JSONL,
    /// `extract_reply` found no `result` event in it, and the answer came back empty. A
    /// Voice-originated hosted chat would have spoken nothing at all, silently.
    #[tokio::test]
    async fn a_hosted_turns_answer_is_spoken_not_swallowed_as_unparsed_jsonl() {
        let fake = Arc::new(crate::speak::FakeSpeaker::returning(b"RIFF"));
        let state = conversing_state(VoiceRuntime {
            armed: true,
            speaker: Some(fake.clone()),
            ..VoiceRuntime::default()
        })
        .await;
        let turn = finished_hosted_turn(&state.pool, "A resposta veio do modelo alojado.").await;

        let _ = get_turn_speech(State(state), Path((turn, 0)))
            .await
            .into_response();

        assert_eq!(fake.said(), vec!["A resposta veio do modelo alojado."]);
    }

    /// A turn still thinking answers `204`, which means "ask again" and not "there is nothing".
    ///
    /// This is the whole of the local path's behaviour, and it is why `speakable` is a pure function
    /// of whatever text exists: `spawn_local_turn` streams NOTHING, so a local turn's tail stays
    /// empty for its entire life and every poll before the end looks exactly like this one.
    #[tokio::test]
    async fn a_turn_that_is_still_thinking_says_ask_again() {
        let state = conversing_state(speaking(b"RIFF")).await;
        let turn = running_turn(&state.pool, "running").await;

        let response = get_turn_speech(State(state), Path((turn, 0)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    /// A turn that failed has no more text coming, so it ends rather than being polled forever.
    #[tokio::test]
    async fn a_turn_that_failed_ends_instead_of_waiting() {
        let state = conversing_state(speaking(b"RIFF")).await;
        let turn = running_turn(&state.pool, "failed").await;

        let response = get_turn_speech(State(state), Path((turn, 0)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// No speaker is not an error condition, and it is not a 500 either.
    ///
    /// `503` says the capability is absent, which is the one thing a window can act on: it stops
    /// asking for audio and reads the answer instead.
    #[tokio::test]
    async fn a_machine_with_no_voice_says_so() {
        let state = conversing_state(VoiceRuntime {
            armed: true,
            ..VoiceRuntime::default()
        })
        .await;
        let turn = finished_local_turn(&state.pool, "Qualquer coisa.").await;

        let response = get_turn_speech(State(state), Path((turn, 0)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// A turn nobody ever created is a 404 and not a silence.
    #[tokio::test]
    async fn speech_for_a_turn_that_does_not_exist_is_not_found() {
        let state = conversing_state(speaking(b"RIFF")).await;

        let response = get_turn_speech(State(state), Path((4321, 0)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// An engine that breaks is reported as a gateway failure, not as an empty answer.
    #[tokio::test]
    async fn a_broken_speaker_is_reported_rather_than_silent() {
        let state = conversing_state(VoiceRuntime {
            armed: true,
            speaker: Some(Arc::new(crate::speak::FakeSpeaker::failing(
                std::io::ErrorKind::NotFound,
                "piper is not installed",
            ))),
            ..VoiceRuntime::default()
        })
        .await;
        let turn = finished_local_turn(&state.pool, "Isto devia soar.").await;

        let response = get_turn_speech(State(state), Path((turn, 0)))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    /// The window is told the third chord and whether anything can say a word, from the one file that
    /// owns both. The shell may not read `.ai/voice.yaml`, so anything it is not told, it cannot know.
    #[test]
    fn the_runtime_carries_the_third_chord_and_whether_there_is_a_voice() {
        let config = crate::config::VoiceConfig {
            enabled: true,
            stt_command: "whisper-cli".to_string(),
            tts_command: "piper -m voz.onnx -f -".to_string(),
            conversation_hotkey: "Ctrl+Alt+C".to_string(),
            ..Default::default()
        };
        let runtime = VoiceRuntime::from_config(&config, None);

        assert_eq!(runtime.conversation_hotkey, "Ctrl+Alt+C");
        assert_eq!(runtime.tts_command, "piper -m voz.onnx -f -");
        assert!(runtime.speaker.is_some());
    }

    /// Nothing is synthesised until it is asked for, and THAT is what makes barge-in free.
    ///
    /// The design this pins down is the one that was chosen against a background task synthesising
    /// the whole answer as it arrives. Hands-free conversation exists so a person can cut in, so
    /// interruption is the ordinary path — and under the rejected design every interruption would
    /// leave a queue of audio nobody will ever hear, already paid for. Here, cutting in is simply
    /// the next request not being made.
    #[tokio::test]
    async fn a_unit_nobody_asks_for_is_never_synthesised() {
        let fake = Arc::new(crate::speak::FakeSpeaker::returning(b"RIFF"));
        let state = conversing_state(VoiceRuntime {
            armed: true,
            speaker: Some(fake.clone()),
            ..VoiceRuntime::default()
        })
        .await;
        let turn = finished_local_turn(&state.pool, "Uma. Duas. Tres. Quatro.").await;

        // One sentence asked for, as a client interrupted after the first would have done.
        let _ = get_turn_speech(State(state), Path((turn, 0)))
            .await
            .into_response();

        assert_eq!(
            fake.calls(),
            1,
            "three unheard sentences were synthesised anyway"
        );
    }
}
