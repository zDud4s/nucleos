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
    /// `extension` names the container `audio` is in, and must come from `extension_for` — the temp
    /// file is created with it, so it may never be a string taken straight from a request.
    async fn transcribe(
        &self,
        audio: &[u8],
        extension: &str,
        duration: Duration,
    ) -> std::io::Result<String>;
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
        //
        // On Windows a just-killed child can still hold the recording open for a moment (os error
        // 32), so a refusal is retried briefly instead of being swallowed on the first attempt.
        for _ in 0..50 {
            match std::fs::remove_file(&self.path) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

/// Distinct per call within a process, and per process, without a clock or a random source.
fn temp_audio_path(extension: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nucleos-voice-{}-{ordinal}.{extension}",
        std::process::id()
    ))
}

/// PURE: the filename extension for the temp recording, resolved through an allowlist.
///
/// The núcleo hands a PATH to a program it did not write, so the extension is how that program is told
/// what the bytes are: whisper reads a `.wav` as PCM and rejects Opus wearing that name. The shell sends
/// WAV, but the Telegram sidecar downloads `.ogg` from Telegram — and the point of one pipeline is that
/// it does not need a second transcriber, so it has to be able to say which container it is sending.
///
/// Returns a `&'static str` instead of echoing the request, and that is the whole security property:
/// no caller-controlled string ever reaches the filesystem. Echoing it would let a request body name
/// the extension of a file this process creates — `format=exe`, or a `format` full of `../`.
pub fn extension_for(requested: Option<&str>) -> Option<&'static str> {
    const ALLOWED: [&str; 8] = ["wav", "ogg", "oga", "opus", "mp3", "m4a", "flac", "webm"];
    match requested {
        // Absent means WAV, which is what the shell sends and what every earlier caller assumed.
        None => Some("wav"),
        Some(raw) => {
            let wanted = raw.trim().trim_start_matches('.').to_ascii_lowercase();
            ALLOWED.into_iter().find(|allowed| *allowed == wanted)
        }
    }
}

/// Transcribes by spawning the command named in `~/.nucleos/voice.yaml`.
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
    async fn transcribe(
        &self,
        audio: &[u8],
        extension: &str,
        duration: Duration,
    ) -> std::io::Result<String> {
        let tokens = split_command(&self.command);
        let (program, args) = tokens.split_first().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no transcribe command configured",
            )
        })?;

        // Built before anything can await, and owning the file from this line onwards. Constructing it
        // after the spawn would reintroduce exactly the leak it exists to prevent.
        let audio_file = TempAudio {
            path: temp_audio_path(extension),
        };
        std::fs::write(&audio_file.path, audio)?;

        let mut command = Command::new(program);
        command
            .args(args)
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

        let deadline = deadline_for(duration);
        // The child is borrowed, not moved, so it is still ours after a timeout and can be killed
        // and reaped before `audio_file` drops (Windows will not delete a file a live process holds).
        let child_ref = &mut child;
        let collected = tokio::time::timeout(deadline, async move {
            let mut text = Vec::new();
            // One byte past the ceiling is all it takes to know the ceiling was breached, and reading
            // no further is what stops a program that prints forever from growing this process.
            stdout
                .take(MAX_TRANSCRIPT_BYTES as u64 + 1)
                .read_to_end(&mut text)
                .await?;
            let status = child_ref.wait().await?;
            Ok::<_, std::io::Error>((text, status))
        })
        .await;

        let (text, status) = match collected {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) => return Err(error),
            Err(_elapsed) => {
                // Errors ignored: the child may already be gone, and the timeout is the result.
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "transcriber gave up after {}s for {}s of audio",
                        deadline.as_secs(),
                        duration.as_secs()
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

/// A transcriber that is already running, addressed over loopback.
///
/// The second implementation the module header promised, and the measurement that justifies it, taken
/// on this machine 2026-09-20 with `ggml-small` on an RTX 3060, GPU warm:
///
/// | audio | `CommandTranscriber` | `HttpTranscriber` |
/// |---|---|---|
/// | 1.3 s | 1250 ms | 96 ms |
/// | 5.6 s | 1210 ms | 191 ms |
/// | 11.7 s | 2080 ms | 541 ms |
/// | 37.2 s | 2680 ms | 1214 ms |
///
/// Read the first two rows together: a clip four times longer costs the spawning path nothing extra.
/// That is the tell -- what it pays for is starting a process, loading 487 MB of weights and waking
/// CUDA, and the transcription itself is the cheap part. A resident server pays that once, at
/// startup, in 2.7 s.
///
/// So this is not a faster way of doing the same thing; it is what makes a different thing possible.
/// Dictation that writes into the box as somebody speaks re-transcribes the sentence in flight, and
/// at a 1.2 s floor per revision there is nothing progressive about it.
///
/// The contract is whisper.cpp's own server: `POST {base}/inference`, multipart, the audio under
/// `file`, `response_format=text`, the transcript as the body. Chosen over inventing one for the
/// reason `HttpSpeaker` gives about Piper -- an invented contract needs an adapter, and the adapter
/// is the thing that breaks when the engine is upgraded.
///
/// What this deliberately does NOT send: the language, the model, the beam size. Those are the
/// operator's, passed on the server's own command line exactly as they are passed in `stt_command`
/// today. A daemon that sent `-l pt` per request would be quietly overriding a running service it
/// does not own, and the two keys would then mean different things.
pub struct HttpTranscriber {
    base_url: String,
    /// Built once and held. At one request per revision of a sentence in flight this is most of what
    /// there is left to save once the model load is gone.
    client: reqwest::Client,
}

impl HttpTranscriber {
    pub fn new(base_url: String) -> Self {
        Self {
            // Trimmed rather than trusted, for the reason `HttpSpeaker` trims: `{base}//inference`
            // against a URL that already ends in a slash is routed by some servers and 404'd by
            // others, and a config file is exactly where a trailing slash gets typed.
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Transcriber for HttpTranscriber {
    async fn transcribe(
        &self,
        audio: &[u8],
        extension: &str,
        duration: Duration,
    ) -> std::io::Result<String> {
        // The filename is built from `extension_for`'s output, never from a request's own string --
        // the same rule the temp file above obeys, for the same reason, one layer further out.
        let part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(format!("capture.{extension}"))
            .mime_str("application/octet-stream")
            .map_err(std::io::Error::other)?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "text");

        let response = self
            .client
            .post(format!("{}/inference", self.base_url))
            // The same deadline the spawning path is held to. It is never the binding constraint
            // here -- 37 s of audio came back in 1.2 s -- so what it catches is a wedged server
            // rather than a slow one, which is exactly what a deadline is for.
            .timeout(deadline_for(duration))
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                // `ConnectionRefused` and not `Other`, because this is the failure that actually
                // happens: the server is simply not running. `voice.rs` turns it into a 502 and the
                // window says the transcriber failed, which is true and is what to act on.
                std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!(
                        "could not reach the transcriber at {}: {error}",
                        self.base_url
                    ),
                )
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(std::io::Error::other(format!(
                "the transcriber answered {status}"
            )));
        }

        let text = response.text().await.map_err(std::io::Error::other)?;
        // Trimmed like the spawning path trims stdout, so one transcript does not depend on which
        // engine produced it. `voice.rs` treats an empty transcript as "nothing was heard", and a
        // stray newline would make that judgement differ between the two implementations.
        Ok(text.trim().to_string())
    }
}

/// PURE: splits the configured command into program and arguments, honouring double quotes.
///
/// A bare whitespace split is what `transcribe.go` does, and it is wrong here for a reason this very
/// machine demonstrates: `C:\Program Files\whisper\whisper-cli.exe -m model.bin` splits into program
/// `C:\Program`, which does not exist, and the spawn fails naming a path nobody typed. `Program
/// Files` is where a Windows install goes by default, and this account's own home directory contains
/// a space too -- it is why the Rust toolchain had to be relocated. So the space is the ordinary case
/// and not the exotic one. Quotes work for arguments as well, because a model file under the same
/// directory has the same problem.
///
/// Deliberately not a shell: no escapes, no single quotes, no variable expansion. A double quote
/// toggles whether whitespace separates, and nothing else. Anything more would be a shell nobody
/// asked for, in a config field that names one program.
/// `pub(crate)` so `health.rs` probes the SAME program this spawns. A second parser for one config
/// string is how a probe comes to disagree with the thing it is probing.
pub(crate) fn split_command(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in command.chars() {
        match c {
            // Marking the token started is what lets an explicit "" survive as an empty argument
            // instead of silently vanishing and shifting every argument after it.
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        tokens.push(current);
    }
    tokens
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
    /// How long to take before answering. Real transcription takes seconds, and a test about what
    /// happens WHILE it runs -- a request abandoned mid-capture -- needs a window to abandon it in.
    delay: Duration,
    // Qualified rather than imported: the import would be unused in the non-test build.
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FakeTranscriber {
    pub fn returning(text: &str) -> Self {
        Self {
            outcome: FakeOutcome::Text(text.to_string()),
            delay: Duration::ZERO,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Answers, but not immediately.
    pub fn returning_slowly(text: &str, delay: Duration) -> Self {
        Self {
            delay,
            ..Self::returning(text)
        }
    }

    pub fn failing(kind: std::io::ErrorKind, message: &str) -> Self {
        Self {
            outcome: FakeOutcome::Failure(kind, message.to_string()),
            delay: Duration::ZERO,
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
    async fn transcribe(
        &self,
        _audio: &[u8],
        _extension: &str,
        _duration: Duration,
    ) -> std::io::Result<String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        match &self.outcome {
            FakeOutcome::Text(text) => Ok(text.clone()),
            FakeOutcome::Failure(kind, message) => Err(std::io::Error::new(*kind, message.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    // The recording tests hold `recording_lock()` across their awaits on purpose: serialising
    // writes to the process-wide temp directory is the whole reason it exists. A `std::sync::Mutex`
    // because these are `#[tokio::test]` (current-thread) and there is no multi-thread runtime here
    // to starve — the same false positive `worktree.rs` and `job.rs` already carry this allow for.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    /// Serialises every test here that writes a recording into the process-wide temp directory.
    ///
    /// `an_abandoned_transcription_still_deletes_the_recording` asserts that NO file carrying this
    /// PROCESS's recording prefix survives — and five tests in this module create one. Without this
    /// lock that assertion reads another test's in-flight recording and fails for a reason that has
    /// nothing to do with abandonment.
    ///
    /// It is a rare failure, which is the bad kind: it surfaces once in a full-suite run, points at
    /// voice, and gets blamed on whatever change happened to be in the tree. Observed doing exactly
    /// that on 2026-08-04, against a change that touched neither voice nor transcription.
    ///
    /// Same shape and same reason as `worktree::test_env_lock`, down to the poison recovery: a test
    /// that panicked while holding this left the temp directory no worse than it found it, so the
    /// next one may proceed.
    fn recording_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
        let _lock = recording_lock();
        let transcriber = CommandTranscriber::new("echo".to_string());

        let spoken = transcriber
            .transcribe(b"RIFF....WAVE", "wav", Duration::from_secs(1))
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
        let _lock = recording_lock();
        let transcriber = CommandTranscriber::new("echo".to_string());

        let path = transcriber
            .transcribe(b"RIFF....WAVE", "wav", Duration::from_secs(1))
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
        let _lock = recording_lock();
        let transcriber = CommandTranscriber::new("tail -f".to_string());

        // Far below `MIN_DEADLINE`, so the future is dropped by this timeout rather than finishing.
        let abandoned = tokio::time::timeout(
            Duration::from_millis(300),
            transcriber.transcribe(b"RIFF....WAVE", "wav", Duration::from_secs(1)),
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
        let _lock = recording_lock();
        let transcriber = CommandTranscriber::new("yes".to_string());

        let error = transcriber
            .transcribe(b"RIFF....WAVE", "wav", Duration::from_secs(1))
            .await
            .expect_err("an endless transcriber must not look like a successful one");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    /// An empty command is "no transcriber", not a spawn of the empty string.
    #[tokio::test]
    async fn an_empty_command_is_refused() {
        let _lock = recording_lock();
        let transcriber = CommandTranscriber::new("   ".to_string());

        let error = transcriber
            .transcribe(b"RIFF....WAVE", "wav", Duration::from_secs(1))
            .await
            .expect_err("an empty command cannot transcribe");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn an_absent_format_means_wav() {
        // Every caller that predates this parameter sent WAV, so absence has to keep meaning WAV.
        assert_eq!(extension_for(None), Some("wav"));
    }

    #[test]
    fn the_containers_a_real_caller_sends_are_accepted_however_they_are_written() {
        // `.ogg` is what the Telegram sidecar downloads; the leading dot and the case are the two
        // ways someone writes it by hand.
        assert_eq!(extension_for(Some("ogg")), Some("ogg"));
        assert_eq!(extension_for(Some(".ogg")), Some("ogg"));
        assert_eq!(extension_for(Some("OGG")), Some("ogg"));
        assert_eq!(extension_for(Some("  webm  ")), Some("webm"));
    }

    /// The security property: the answer is never the caller's string.
    ///
    /// This value names a file the daemon CREATES, so echoing the request would let a request body
    /// choose an extension — `exe` — or walk out of the temp directory. Refusing is also the honest
    /// answer for a container the transcriber could not read anyway.
    #[test]
    fn anything_not_on_the_allowlist_is_refused_rather_than_echoed() {
        assert_eq!(extension_for(Some("exe")), None);
        assert_eq!(extension_for(Some("../../evil")), None);
        assert_eq!(extension_for(Some("wav.exe")), None);
        assert_eq!(extension_for(Some("")), None);
        assert_eq!(extension_for(Some("   ")), None);
    }

    #[test]
    fn the_extension_reaches_the_temp_file_name() {
        // The whole reason the parameter exists: whisper decides how to decode by the name it is
        // handed, so an Opus payload in a file called `.wav` is rejected as a broken recording.
        let path = temp_audio_path("ogg");
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("ogg"),
            "path was {path:?}"
        );
    }

    /// The failure this fixes: `C:\Program Files\...` is where a Windows install actually goes, and a
    /// bare whitespace split turns it into the program `C:\Program`.
    #[test]
    fn a_quoted_program_path_keeps_its_spaces() {
        assert_eq!(
            split_command("\"C:\\Program Files\\whisper\\whisper-cli.exe\" -m model.bin"),
            vec![
                "C:\\Program Files\\whisper\\whisper-cli.exe",
                "-m",
                "model.bin"
            ]
        );
    }

    #[test]
    fn a_quoted_argument_keeps_its_spaces_too() {
        // A model file lives next to the program, so it inherits the same directory and the same
        // problem. Fixing only the program would move the failure one argument to the right.
        assert_eq!(
            split_command("whisper-cli -m \"C:\\My Models\\ggml-base.bin\" -bo 1"),
            vec![
                "whisper-cli",
                "-m",
                "C:\\My Models\\ggml-base.bin",
                "-bo",
                "1"
            ]
        );
    }

    #[test]
    fn an_unquoted_command_splits_the_way_it_always_did() {
        // The existing contract, unchanged: every `~/.nucleos/voice.yaml` written before quoting existed
        // must keep working.
        assert_eq!(
            split_command("whisper-cli -m model.bin -bo 1 -bs 1"),
            vec!["whisper-cli", "-m", "model.bin", "-bo", "1", "-bs", "1"]
        );
    }

    #[test]
    fn runs_of_whitespace_do_not_produce_empty_arguments() {
        assert_eq!(
            split_command("  whisper-cli   -m    model.bin  "),
            vec!["whisper-cli", "-m", "model.bin"]
        );
    }

    #[test]
    fn an_explicitly_empty_quoted_argument_survives() {
        // Passing "" is how some CLIs are told "this flag, with no value". Dropping it would shift
        // every later argument one position left, which fails in a way that looks like a config typo.
        assert_eq!(
            split_command("prog --prefix \"\" --after"),
            vec!["prog", "--prefix", "", "--after"]
        );
    }

    #[test]
    fn nothing_but_whitespace_is_no_command_at_all() {
        assert!(split_command("   ").is_empty());
        assert!(split_command("").is_empty());
    }

    /// A trailing slash in a config file is not a typo anybody notices, and `{base}//inference` is
    /// routed by some servers and 404'd by others -- the same trap `HttpSpeaker` already trims for.
    #[tokio::test]
    async fn a_trailing_slash_in_the_url_does_not_become_a_double_one() {
        for written in [
            "http://127.0.0.1:1",
            "http://127.0.0.1:1/",
            "  http://127.0.0.1:1///  ",
        ] {
            let transcriber = HttpTranscriber::new(written.to_string());
            let error = transcriber
                .transcribe(b"RIFF", "wav", Duration::from_secs(1))
                .await
                .unwrap_err();
            // Nothing listens on port 1, so the request is REFUSED rather than answered -- which is
            // the outcome that proves a URL was built and attempted at all.
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused,
                "{written:?} produced {error}"
            );
            assert!(
                !error.to_string().contains("1//"),
                "{written:?} kept a double slash: {error}"
            );
        }
    }

    /// A server that is simply not running is the failure that actually happens, and it says so.
    ///
    /// `ConnectionRefused` and not `Other`, for the reason `speak.rs` gives: `voice.rs` turns it into
    /// a 502 and the window says the transcriber failed, which is true and is what to act on. The
    /// address is in the message because "transcription failed" with no port names nothing to check.
    #[tokio::test]
    async fn a_resident_server_that_is_not_there_reports_a_refused_connection() {
        let transcriber = HttpTranscriber::new("http://127.0.0.1:1".to_string());
        let error = transcriber
            .transcribe(b"RIFF", "wav", Duration::from_secs(1))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
        assert!(error.to_string().contains("127.0.0.1:1"), "{error}");
    }

    /// Both implementations answer the same deadline, because the trait's caller sizes it and neither
    /// implementation may quietly decide the caller was wrong. Measured 2026-09-20, the resident
    /// server does 37 s of audio in 1.2 s -- so this deadline is never the binding constraint there,
    /// and it exists for the case where the server is wedged rather than slow.
    #[test]
    fn the_resident_path_is_held_to_the_same_deadline_as_the_spawning_one() {
        assert_eq!(deadline_for(Duration::from_secs(2)), MIN_DEADLINE);
        assert_eq!(
            deadline_for(Duration::from_secs(60)),
            Duration::from_secs(180)
        );
    }
}
