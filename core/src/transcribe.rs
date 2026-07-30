//! The núcleo↔STT boundary: where audio becomes text, and where a third-party program is given a
//! chance to misbehave.
//!
//! Deliberately the same shape as `runner.rs` — a trait, a real implementation that spawns something
//! the operator configured, and a fake for tests. Keeping it in its own file is what lets the voice
//! domain in `voice.rs` stay ignorant of processes, deadlines and temp files, and what lets a resident
//! `whisper-server` arrive later as a second `impl` rather than as a rewrite.
//!
//! The contract is the one `sidecars/telegram/transcribe/transcribe.go` already established: the
//! command is whitespace-split into program + args, the audio path is appended last, and the
//! transcript is read from stdout. Simple splitting is sufficient because the string is local and
//! operator-supplied; it is not a shell.

use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Floor for any transcription, however short the audio.
///
/// This is the value the Telegram sidecar uses as its *entire* deadline, and its comment explains why
/// it is enough there: "a voice note is seconds of audio". It survives here only as the floor, because
/// process startup and model loading are paid whether the clip is two seconds or none.
const MIN_DEADLINE: Duration = Duration::from_secs(60);

/// How much slower than realtime a transcriber is still allowed to be.
///
/// Whisper on CPU runs slower than realtime; three times gives a laptop room to be busy without
/// declaring a working transcriber stuck. This multiple, not a constant, is the whole point of
/// `deadline_for`.
const REALTIME_MULTIPLE: u32 = 3;

/// Transcript bytes accepted before the transcriber is treated as broken.
///
/// Same ceiling as the Go sidecar, opposite reaction: see `output_ceiling_errors_not_clips`.
const MAX_TRANSCRIPT_BYTES: usize = 1 << 20;

/// PURE: how long a transcription of `audio` may take before it is considered wedged.
///
/// Scaling with duration rather than using a constant is a correctness requirement, not a nicety. A
/// flat one-minute deadline is right for a voice note and wrong for a twenty-minute memo, and it fails
/// the same way every time: the same recordings die at the same point, so a retry does not help. That
/// is the shape of bug that filed mail as unreadable in the email pillar.
pub fn deadline_for(audio: Duration) -> Duration {
    MIN_DEADLINE.max(audio * REALTIME_MULTIPLE)
}

/// Audio becoming text. Implementors own the mechanics; callers own the meaning.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// `audio` is the recording's own length, which the caller measured while recording — it sizes the
    /// deadline and is not recoverable from the bytes without trusting a header.
    async fn transcribe(&self, wav: &[u8], audio: Duration) -> std::io::Result<String>;
}

/// Deletes the file it names when dropped, whatever the reason.
///
/// This type IS the "transcribe, then delete" rule. Written as a statement after the transcription
/// await instead, the deletion would be happy-path-only code: a dropped future stops at its last
/// `.await` and never runs another line, so a client that disconnects mid-dictation would leave a
/// recording of somebody's voice on disk indefinitely (`core/AGENTS.md`, cancellation safety).
struct TempAudio {
    path: PathBuf,
}

impl Drop for TempAudio {
    fn drop(&mut self) {
        // Best effort by necessity: Drop cannot report, and a failure here must not mask the
        // transcription's own result. A leftover file is visible in the temp directory; a panic in
        // Drop during unwinding is not recoverable.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Distinct per call within a process, and per process, without a clock or a random source.
fn temp_wav_path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nucleos-voice-{}-{ordinal}.wav",
        std::process::id()
    ))
}

/// Transcribes by spawning the command named in `.ai/voice.yaml`.
///
/// Chosen over embedding whisper.cpp in-process for the reason the local-triage design already paid
/// for: compiling a heavy native dependency with CUDA under this repository's pinned GNU/MinGW host is
/// a toolchain saga, and it would fuse that dependency into the núcleo against the module map.
pub struct CommandTranscriber {
    command: String,
}

impl CommandTranscriber {
    pub fn new(command: String) -> Self {
        Self { command }
    }
}

#[async_trait]
impl Transcriber for CommandTranscriber {
    async fn transcribe(&self, wav: &[u8], audio: Duration) -> std::io::Result<String> {
        let mut fields = self.command.split_whitespace();
        let program = fields.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no transcribe command configured",
            )
        })?;
        let args = fields.collect::<Vec<_>>();

        // Built before anything can await, and owning the file from this line onwards. Constructing it
        // after the spawn would reintroduce exactly the leak it exists to prevent.
        let audio_file = TempAudio {
            path: temp_wav_path(),
        };
        std::fs::write(&audio_file.path, wav)?;

        let mut command = Command::new(program);
        command
            .args(&args)
            .arg(&audio_file.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Turns an abandoned request into a dead transcriber instead of an orphan holding the GPU.
            .kill_on_drop(true);

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .expect("stdout was piped when the command was configured");

        let deadline = deadline_for(audio);
        let collected = tokio::time::timeout(deadline, async move {
            let mut text = Vec::new();
            // One byte past the ceiling is all it takes to know the ceiling was breached, and reading
            // no further is what stops a program that prints forever from growing this process.
            stdout
                .take(MAX_TRANSCRIPT_BYTES as u64 + 1)
                .read_to_end(&mut text)
                .await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((text, status))
        })
        .await;

        let (text, status) = match collected {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) => return Err(error),
            Err(_elapsed) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "transcriber gave up after {}s for {}s of audio",
                        deadline.as_secs(),
                        audio.as_secs()
                    ),
                ));
            }
        };

        if text.len() > MAX_TRANSCRIPT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("transcriber produced more than {MAX_TRANSCRIPT_BYTES} bytes"),
            ));
        }
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "transcriber exited with {status}"
            )));
        }

        Ok(String::from_utf8_lossy(&text).trim().to_string())
    }
}

/// What a `FakeTranscriber` will do when asked.
#[cfg(test)]
pub enum FakeOutcome {
    Text(String),
    Failure(std::io::ErrorKind, String),
}

/// A transcriber that answers from a script, so the voice domain can be tested without an STT engine.
///
/// `#[cfg(test)]` because every user of it is a test, and for the same reason `FakeCommandRunner` is:
/// building it into the daemon would ship something that can fabricate a transcript.
#[cfg(test)]
pub struct FakeTranscriber {
    outcome: FakeOutcome,
    // Qualified rather than imported: the import would be unused in the non-test build.
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FakeTranscriber {
    pub fn returning(text: &str) -> Self {
        Self {
            outcome: FakeOutcome::Text(text.to_string()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn failing(kind: std::io::ErrorKind, message: &str) -> Self {
        Self {
            outcome: FakeOutcome::Failure(kind, message.to_string()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
#[async_trait]
impl Transcriber for FakeTranscriber {
    async fn transcribe(&self, _wav: &[u8], _audio: Duration) -> std::io::Result<String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        match &self.outcome {
            FakeOutcome::Text(text) => Ok(text.clone()),
            FakeOutcome::Failure(kind, message) => Err(std::io::Error::new(*kind, message.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this guards is deterministic destruction of long recordings.
    ///
    /// Stated in the negative: a twenty-minute memo must NOT inherit the one-minute deadline that suits
    /// a voice note. With a flat deadline every memo past ~20 minutes of processing dies at the same
    /// point on every attempt, so retrying cannot help — which is precisely how the email pillar once
    /// filed valid mail as unreadable.
    #[test]
    fn deadline_scales_with_duration() {
        // Short audio keeps the floor: startup and model load are paid regardless.
        assert_eq!(deadline_for(Duration::from_secs(2)), MIN_DEADLINE);
        assert_eq!(deadline_for(Duration::ZERO), MIN_DEADLINE);

        let memo = Duration::from_secs(20 * 60);
        assert!(
            deadline_for(memo) > MIN_DEADLINE,
            "a 20-minute memo must not get a voice note's deadline"
        );
        assert_eq!(deadline_for(memo), memo * REALTIME_MULTIPLE);
    }

    /// The audio path goes LAST, after the operator's own arguments.
    ///
    /// `echo` is the whole assertion here: whatever it prints is the argument list it received, so a
    /// transcript equal to the temp path proves both the append order and that stdout is what is read.
    #[tokio::test]
    async fn the_audio_path_is_appended_last() {
        let transcriber = CommandTranscriber::new("echo".to_string());

        let spoken = transcriber
            .transcribe(b"RIFF....WAVE", Duration::from_secs(1))
            .await
            .expect("echo is a working transcriber for this purpose");

        assert!(
            spoken.contains("nucleos-voice-"),
            "expected the appended audio path, got {spoken:?}"
        );
        assert!(spoken.ends_with(".wav"), "got {spoken:?}");
    }

    /// The temp recording is gone by the time a caller has the transcript.
    ///
    /// Uses the same `echo` trick to learn the path the guard was responsible for, then asserts the
    /// file is not there. Without this the deletion is only an intention written in a comment.
    #[tokio::test]
    async fn the_recording_is_deleted_once_transcribed() {
        let transcriber = CommandTranscriber::new("echo".to_string());

        let path = transcriber
            .transcribe(b"RIFF....WAVE", Duration::from_secs(1))
            .await
            .expect("echo is a working transcriber for this purpose");

        assert!(
            !std::path::Path::new(path.trim()).exists(),
            "audio survived transcription at {path}"
        );
    }

    /// A dropped request must still take the recording with it.
    ///
    /// This is the case a statement placed after the transcription await cannot cover, and the reason
    /// `TempAudio` exists.
    ///
    /// `tail -f` is chosen for two properties, both needed: it never exits, and it prints almost
    /// nothing. `yes` has only the first — it trips the output ceiling within milliseconds, so the
    /// future COMPLETES with an error instead of being abandoned, and the test would prove the wrong
    /// path. Here the future is still mid-transcription when it is dropped, exactly as a disconnecting
    /// client would leave it.
    #[tokio::test]
    async fn an_abandoned_transcription_still_deletes_the_recording() {
        let transcriber = CommandTranscriber::new("tail -f".to_string());

        // Far below `MIN_DEADLINE`, so the future is dropped by this timeout rather than finishing.
        let abandoned = tokio::time::timeout(
            Duration::from_millis(300),
            transcriber.transcribe(b"RIFF....WAVE", Duration::from_secs(1)),
        )
        .await;
        assert!(
            abandoned.is_err(),
            "`tail -f` should still have been running"
        );

        let mine = format!("nucleos-voice-{}-", std::process::id());
        let leftovers = std::fs::read_dir(std::env::temp_dir())
            .expect("the temp directory should be readable")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&mine))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();

        assert!(
            leftovers.is_empty(),
            "abandoned transcription left recordings behind: {leftovers:?}"
        );
    }

    /// The ceiling ERRORS; it does not quietly hand back a shortened transcript.
    ///
    /// The Go sidecar returns the clipped text and calls that success, which is defensible for a voice
    /// note. Here it is the silent-truncation path the voice pillar already builds a shrink guard
    /// against, so the same event has to be loud on this side of the boundary.
    #[tokio::test]
    async fn output_ceiling_errors_not_clips() {
        let transcriber = CommandTranscriber::new("yes".to_string());

        let error = transcriber
            .transcribe(b"RIFF....WAVE", Duration::from_secs(1))
            .await
            .expect_err("an endless transcriber must not look like a successful one");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    /// An empty command is "no transcriber", not a spawn of the empty string.
    #[tokio::test]
    async fn an_empty_command_is_refused() {
        let transcriber = CommandTranscriber::new("   ".to_string());

        let error = transcriber
            .transcribe(b"RIFF....WAVE", Duration::from_secs(1))
            .await
            .expect_err("an empty command cannot transcribe");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
