//! The núcleo↔TTS boundary: where text becomes audio, and where a third-party program is given a
//! chance to misbehave.
//!
//! The mirror of `transcribe.rs`, deliberately down to the shape of its guards — a trait, a real
//! implementation that spawns something the operator configured, and a fake for tests. Keeping it in
//! its own file is what lets the voice domain in `voice.rs` stay ignorant of processes and deadlines,
//! and what lets a cloned voice arrive later as a second `impl` rather than as a rewrite. That second
//! `impl` has a name already: Voicebox's backend speaks over HTTP on loopback, which is a different
//! mechanism and the same trait.
//!
//! **The contract is not `transcribe.rs`'s, and the difference is deliberate.** There the audio path
//! is appended as the last ARGUMENT, because a path is short and bounded. Here the text goes on
//! STDIN, for two reasons that both bite:
//!
//! - **Length.** The text is a model's answer. Nothing in this process bounds it, and Windows caps a
//!   command line at about 32k characters — so an argument would work for every test and fail on the
//!   one long answer somebody actually wanted read aloud.
//! - **It is what the engine wants.** Piper reads text from stdin and writes a WAV to stdout with
//!   `--output_file -`. A contract invented against that would be a contract with an adapter script
//!   in it.
//!
//! What is copied exactly is every guard: whitespace splitting with quotes honoured (the SAME
//! `split_command`, not a second one), a deadline that scales with the work, an output ceiling that
//! ERRORS rather than clips, and `kill_on_drop` so an abandoned request leaves no process holding the
//! machine.

use async_trait::async_trait;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Floor for any synthesis, however short the text.
///
/// Half the transcriber's minute, and the asymmetry is measured rather than guessed: §14 of the voice
/// design found that model LOADING dominates a short job, and a Piper voice is tens of megabytes
/// against whisper's gigabytes. What this has to survive is a cold process start on a busy laptop,
/// not a model being paged in.
const MIN_DEADLINE: Duration = Duration::from_secs(30);

/// Characters per second the engine is assumed to manage, wall clock, at its very worst.
///
/// Slack by two orders of magnitude on purpose: Piper on CPU runs around 150x realtime, which is
/// thousands of characters a second. This number is not a performance estimate — it is the point at
/// which an engine is declared wedged rather than slow, and putting it near the real figure would
/// turn a busy machine into a failed turn.
const CHARS_PER_SECOND_FLOOR: u64 = 20;

/// Audio bytes accepted from one utterance before the engine is treated as broken.
///
/// Sixteen megabytes is about six minutes of 22 kHz mono PCM, against a `MAX_SPEAK_CHARS` unit that
/// should be twenty seconds of speech. Anything approaching this has stopped being a sentence.
const MAX_AUDIO_BYTES: usize = 16 << 20;

/// Longest run of text handed to the engine as one unit.
///
/// Two jobs, and the second is the one that matters. It bounds the deadline, which is what the
/// paragraph above is about — but it also bounds TIME TO FIRST SOUND. A conversation where the answer
/// begins after the whole answer is synthesised is a conversation with a pause in it, and the pause
/// grows with the answer. Roughly twenty seconds of speech.
pub const MAX_SPEAK_CHARS: usize = 320;

/// PURE: how long synthesising `chars` characters may take before the engine is considered wedged.
///
/// Scales for the same reason `transcribe::deadline_for` does, and against the same class of bug: a
/// flat deadline is right for the short case and wrong for the long one, and it fails identically
/// every time — so the same input dies at the same point and a retry does not help.
pub fn deadline_for_text(chars: usize) -> Duration {
    MIN_DEADLINE.max(Duration::from_secs(chars as u64 / CHARS_PER_SECOND_FLOOR))
}

/// Text becoming audio. Implementors own the mechanics; callers own the meaning.
#[async_trait]
pub trait Speaker: Send + Sync {
    /// Returns a self-contained audio file — WAV from `CommandSpeaker`, because that is what Piper
    /// writes and what a browser can play without being told a sample rate.
    ///
    /// Takes `&str` and not a sentence type: what counts as one unit of speech is `speakable`'s
    /// decision, made against a whole answer, and an implementation has no business revisiting it.
    async fn speak(&self, text: &str) -> std::io::Result<Vec<u8>>;
}

/// PURE: the units of `text` that are ready to be spoken, in order.
///
/// This is the whole of the streaming design, and it is a pure function on purpose — the alternative
/// was a background task racing the turn, holding synthesised audio in a map nobody evicts.
///
/// `finished` is the load-bearing argument. While a CLI turn streams, its last sentence is HALF
/// WRITTEN, and speaking it would read a fragment aloud and then read the same fragment again with
/// its ending attached. So a trailing piece with no terminator is withheld — unless the turn is over,
/// in which case there is nothing more coming and the fragment IS the ending.
///
/// The terminator set is `chunk_transcript`'s, and shares its weakness knowingly: greedy decoding
/// returns walls of words with no full stop in them, so an over-long run is cut by
/// `voice::split_oversized` rather than held whole. Reusing that function rather than writing a second
/// one is the same rule the transcriber applies to `split_command` — one cut, one implementation, no
/// way for two of them to drift.
pub fn speakable(text: &str, finished: bool) -> Vec<String> {
    let mut pieces: Vec<&str> = text.split_inclusive(['.', '!', '?', '\n']).collect();

    // A turn still writing has one piece that is not a sentence yet. `split_inclusive` keeps the
    // terminator, so "ends with one" is exactly "is complete" — and text ending ON a terminator
    // produces no trailing fragment at all, which is why this is a check and not an unconditional pop.
    if !finished
        && let Some(last) = pieces.last()
        && !last.trim_end().ends_with(['.', '!', '?'])
    {
        pieces.pop();
    }

    let mut units = Vec::new();
    for piece in pieces {
        if piece.trim().is_empty() {
            continue;
        }
        for part in crate::voice::split_oversized(piece, MAX_SPEAK_CHARS) {
            let part = part.trim();
            // A cut can land such that one side is only whitespace; an engine handed "" either errors
            // or emits a click, and both are worse than the unit not existing.
            if !part.is_empty() {
                units.push(part.to_string());
            }
        }
    }
    units
}

/// Speaks by spawning the command named in `.ai/voice.yaml`.
///
/// Chosen over embedding a synthesiser in-process for the reason decision 5 of the voice design
/// already paid for once: a heavy native dependency compiled with CUDA under this repository's pinned
/// GNU/MinGW host is a toolchain saga, and it would fuse that dependency into the núcleo against the
/// module map. The engine is an implementation; this is the architecture.
pub struct CommandSpeaker {
    command: String,
}

impl CommandSpeaker {
    pub fn new(command: String) -> Self {
        Self { command }
    }
}

#[async_trait]
impl Speaker for CommandSpeaker {
    async fn speak(&self, text: &str) -> std::io::Result<Vec<u8>> {
        let tokens = crate::transcribe::split_command(&self.command);
        let (program, args) = tokens.split_first().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no speak command configured",
            )
        })?;

        // Refused here rather than spawned and discovered empty: an engine handed nothing tends to
        // succeed with a zero-length file, which is indistinguishable downstream from a voice that
        // has stopped working.
        if text.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "nothing to speak",
            ));
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Turns an abandoned request into a dead engine instead of an orphan holding the machine.
            // Load-bearing here in a way it is not for the transcriber: barge-in ABANDONS synthesis
            // by design, so this is the ordinary path and not the exceptional one.
            .kill_on_drop(true);

        let mut child = command.spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .expect("stdin was piped when the command was configured");
        let stdout = child
            .stdout
            .take()
            .expect("stdout was piped when the command was configured");

        let owned = text.to_string();
        // Written from its own task, concurrently with the read below, and that concurrency is a
        // correctness requirement rather than a speed one. An engine that starts emitting audio
        // before it has read all its input fills the stdout pipe; if this process were still writing
        // stdin at that moment, both ends would block forever and the only thing to notice would be
        // the deadline. Sequential write-then-read deadlocks on exactly the inputs that matter — the
        // long ones.
        let writing = tokio::spawn(async move {
            let result = stdin.write_all(owned.as_bytes()).await;
            // EOF is what tells the engine the text is over. Dropped explicitly because a `stdin`
            // still alive at the end of this task would keep the pipe open until the task's locals
            // are collected, which is not a moment anything here controls.
            drop(stdin);
            result
        });

        let deadline = deadline_for_text(text.chars().count());
        let collected = tokio::time::timeout(deadline, async move {
            let mut audio = Vec::new();
            // One byte past the ceiling is all it takes to know the ceiling was breached, and reading
            // no further is what stops a program that writes forever from growing this process.
            stdout
                .take(MAX_AUDIO_BYTES as u64 + 1)
                .read_to_end(&mut audio)
                .await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((audio, status))
        })
        .await;

        let (audio, status) = match collected {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) => return Err(error),
            Err(_elapsed) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "speaker gave up after {}s for {} characters",
                        deadline.as_secs(),
                        text.chars().count()
                    ),
                ));
            }
        };

        // Awaited AFTER the read, never before — see the comment on the spawn. A write error is
        // reported only if the read did not already explain the failure better: an engine that exits
        // early makes the write fail with a broken pipe, and "broken pipe" is the symptom while the
        // exit status is the cause.
        let write_failed = match writing.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(join) => Some(std::io::Error::other(format!(
                "the task writing to the speaker panicked: {join}"
            ))),
        };

        if audio.len() > MAX_AUDIO_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("speaker produced more than {MAX_AUDIO_BYTES} bytes"),
            ));
        }
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "speaker exited with {status}"
            )));
        }
        if let Some(error) = write_failed {
            return Err(error);
        }
        // A success with no audio is the failure this catches, and it is the likely one: an engine
        // given a model path that does not exist prints a diagnostic to stderr — which is `null` here
        // — and exits 0 having written nothing. Silence would then present as the voice being off.
        if audio.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the speaker produced no audio",
            ));
        }

        Ok(audio)
    }
}

/// What a `FakeSpeaker` will do when asked.
#[cfg(test)]
pub enum FakeOutcome {
    Audio(Vec<u8>),
    Failure(std::io::ErrorKind, String),
}

/// A speaker that answers from a script, so the voice domain can be tested without a TTS engine.
///
/// `#[cfg(test)]` for the reason `FakeTranscriber` is: building it into the daemon would ship
/// something that can fabricate audio.
#[cfg(test)]
pub struct FakeSpeaker {
    outcome: FakeOutcome,
    /// Real synthesis takes time. Zero here for every test so far; kept because a test about what
    /// happens WHILE synthesis runs needs a window to act in, and adding it back later would mean
    /// touching every construction site.
    delay: Duration,
    calls: std::sync::atomic::AtomicUsize,
    said: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl FakeSpeaker {
    pub fn returning(audio: &[u8]) -> Self {
        Self {
            outcome: FakeOutcome::Audio(audio.to_vec()),
            delay: Duration::ZERO,
            calls: std::sync::atomic::AtomicUsize::new(0),
            said: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn failing(kind: std::io::ErrorKind, message: &str) -> Self {
        Self {
            outcome: FakeOutcome::Failure(kind, message.to_string()),
            delay: Duration::ZERO,
            calls: std::sync::atomic::AtomicUsize::new(0),
            said: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Every text handed to this speaker, in order — so a test can assert WHAT was spoken and not
    /// merely that something was.
    pub fn said(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait]
impl Speaker for FakeSpeaker {
    async fn speak(&self, text: &str) -> std::io::Result<Vec<u8>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.said.lock().unwrap().push(text.to_string());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        match &self.outcome {
            FakeOutcome::Audio(audio) => Ok(audio.clone()),
            FakeOutcome::Failure(kind, message) => Err(std::io::Error::new(*kind, message.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The streaming rule, stated as the test that would fail without it.
    ///
    /// A turn still writing has said "Olá." and started on the next sentence. Speaking the fragment
    /// would read half a clause aloud and then read it again complete a second later — the specific
    /// stutter this function exists to prevent.
    #[test]
    fn a_half_written_sentence_waits_and_a_finished_one_does_not() {
        let streaming = speakable("Olá. Como es", false);
        assert_eq!(streaming, vec!["Olá."]);

        let done = speakable("Olá. Como es", true);
        assert_eq!(done, vec!["Olá.", "Como es"]);
    }

    /// Text ending exactly on a terminator has no trailing fragment, so nothing may be withheld.
    ///
    /// The bug this catches is an unconditional "drop the last piece while streaming", which would
    /// hold the final sentence of every answer hostage until the turn ended — turning streaming into
    /// a slower version of not streaming.
    #[test]
    fn a_complete_last_sentence_is_spoken_while_the_turn_is_still_running() {
        assert_eq!(
            speakable("Está tudo a correr. O run acabou.", false),
            vec!["Está tudo a correr.", "O run acabou."]
        );
    }

    /// Greedy decoding returns walls of words with no full stop, and `chunk_transcript` records the
    /// same weakness. Held whole, one of those is a single unit minutes long: the deadline would be
    /// enormous and nothing would be heard until all of it was synthesised.
    #[test]
    fn an_over_long_run_with_no_terminator_is_cut_rather_than_held_whole() {
        let wall = "palavra ".repeat(200);
        let units = speakable(&wall, true);
        assert!(units.len() > 1, "expected the wall to be cut, got one unit");
        for unit in &units {
            assert!(
                unit.chars().count() <= MAX_SPEAK_CHARS,
                "a unit exceeded the ceiling: {} chars",
                unit.chars().count()
            );
        }
        // The cut may not lose text. Whitespace is normalised away by the trim, so words are compared.
        let spoken: Vec<&str> = units.iter().flat_map(|u| u.split_whitespace()).collect();
        assert_eq!(spoken.len(), 200);
    }

    /// Blank lines between paragraphs are pieces too, and an engine handed one emits a click or an
    /// error. Neither is a thing anybody asked to hear.
    #[test]
    fn whitespace_between_sentences_produces_no_unit() {
        assert_eq!(
            speakable("Pronto.\n\n\nAcabou.", true),
            vec!["Pronto.", "Acabou."]
        );
    }

    /// Portuguese is the language this pillar exists for, so a cut that lands inside a multibyte
    /// character is not an exotic case — it is Tuesday. `split_oversized` indexes by `char_indices`
    /// precisely for this, and the test is here so a future rewrite cannot quietly lose it.
    #[test]
    fn cutting_accented_text_never_lands_inside_a_character() {
        let accented = "ação ".repeat(300);
        let units = speakable(&accented, true);
        assert!(units.len() > 1);
        for unit in units {
            assert!(unit.contains("ação"));
        }
    }

    /// An empty answer is not a unit of speech. A turn that failed writes no text, and the speech
    /// endpoint must be able to say "there is nothing" rather than synthesise a silence.
    #[test]
    fn nothing_to_say_produces_nothing() {
        assert!(speakable("", true).is_empty());
        assert!(speakable("   \n  ", true).is_empty());
        assert!(speakable("Ainda a escrever", false).is_empty());
    }

    /// The deadline scales, and the floor holds for anything short.
    #[test]
    fn the_deadline_scales_with_the_text_and_never_falls_below_the_floor() {
        assert_eq!(deadline_for_text(0), MIN_DEADLINE);
        assert_eq!(deadline_for_text(MAX_SPEAK_CHARS), MIN_DEADLINE);
        // Long enough to exceed the floor: 20 chars/s means 30s of floor is 600 characters.
        assert!(deadline_for_text(60_000) > MIN_DEADLINE);
        assert_eq!(deadline_for_text(60_000), Duration::from_secs(3_000));
    }

    /// The engine is handed nothing and must not be spawned for it.
    #[tokio::test]
    async fn an_empty_utterance_is_refused_before_anything_is_spawned() {
        // A command that would fail loudly if it ever ran, so a spawn is detectable as a different
        // error kind than the refusal being asserted.
        let speaker = CommandSpeaker::new("no-such-program-anywhere".to_string());
        let error = speaker.speak("   ").await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// A command string with no program in it is a configuration error, not a crash.
    #[tokio::test]
    async fn an_empty_command_reports_rather_than_panics() {
        let speaker = CommandSpeaker::new("   ".to_string());
        let error = speaker.speak("olá").await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
