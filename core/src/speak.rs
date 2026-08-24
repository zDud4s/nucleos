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
//! - **It is what the engine wants.** Piper reads text from stdin and writes a WAV to stdout when
//!   given `-f -`. A contract invented against that would be a contract with an adapter script in it.
//!
//! What is copied exactly is every guard: whitespace splitting with quotes honoured (the SAME
//! `split_command`, not a second one), a deadline that scales with the work, an output ceiling that
//! ERRORS rather than clips, and `kill_on_drop` so an abandoned request leaves no process holding the
//! machine.
//!
//! **Two implementations, and the second one is the one to use.** Measured on this machine against
//! `pt_PT-tugao-medium`, spawning per utterance:
//!
//! | | short sentence (1.6s of audio) | long text (27s of audio) |
//! |---|---|---|
//! | `CommandSpeaker` | 2.90 s | 4.44 s |
//! | `HttpSpeaker` | 0.21 s | — |
//!
//! Solving those two points gives **~2.8 s of fixed cost per spawn** and synthesis at ~16x realtime:
//! loading a 63 MB voice dominates everything else, exactly as §14 of the voice design found for
//! whisper. And because `speakable` hands out one SENTENCE at a time, a spawning speaker pays that
//! 2.8 s again for every sentence — which takes the whole point out of streaming, since the first
//! sentence is no longer cheaper than the last.
//!
//! So `CommandSpeaker` is kept and `HttpSpeaker` is what a conversation should be pointed at. Keeping
//! both is not indecision: the command shape is the one an engine that is a plain binary will have,
//! and having written the trait for exactly this substitution, throwing away the first implementation
//! the moment the second arrives would be discarding the evidence that the trait was worth having.

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

/// Speaks by asking a resident engine over loopback.
///
/// **The second `impl` this trait was designed to receive**, and the reason decision 5 of the voice
/// design phrased itself as *"escolhe-se a implementação, não a arquitetura"*. Nothing outside this
/// file changed to add it.
///
/// The shape is deliberately `runner.rs`'s `OllamaRunner` and not `sidecar.rs`'s supervised children:
/// a base URL in configuration, loopback, and **no supervision**. The núcleo does not start Ollama
/// either, and for the same reason — it is a service the operator runs, whose lifetime is not this
/// daemon's business. That symmetry is also why `health.rs` does not probe it: it probes programs
/// this daemon spawns, and a row that probed one loopback service and not the other would be
/// describing the operator's setup rather than this daemon's.
///
/// The contract is Piper's HTTP server: `POST {base}/synthesize` with `{"text": …}`, WAV back. Chosen
/// over inventing one because an invented one would need an adapter, which is the same argument that
/// put the text on stdin above.
pub struct HttpSpeaker {
    base_url: String,
    /// Built once and held, so a conversation's sentences reuse connections instead of paying a
    /// handshake each. At one request per sentence that is most of what there is to save.
    client: reqwest::Client,
}

impl HttpSpeaker {
    pub fn new(base_url: String) -> Self {
        Self {
            // Trailing slashes trimmed here rather than trusted: `{base}/synthesize` against a URL
            // that already ends in one yields `//synthesize`, which some servers route and some
            // 404 — and a config file is exactly where a trailing slash gets typed.
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Speaker for HttpSpeaker {
    async fn speak(&self, text: &str) -> std::io::Result<Vec<u8>> {
        if text.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "nothing to speak",
            ));
        }

        let response = self
            .client
            .post(format!("{}/synthesize", self.base_url))
            .json(&serde_json::json!({ "text": text }))
            // The same deadline the spawning path uses, for the same reason: it has to scale with the
            // work or it is right for short input and wrong for long, every time, deterministically.
            .timeout(deadline_for_text(text.chars().count()))
            .send()
            .await
            .map_err(|error| {
                // Reported as `ConnectionRefused` and not `Other`, because this is the failure that
                // actually happens: the server is simply not running. `voice.rs` turns it into a 502
                // and the window says the speaker failed — which is true and is what to act on.
                std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("could not reach the speaker at {}: {error}", self.base_url),
                )
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(std::io::Error::other(format!(
                "the speaker answered {status}"
            )));
        }

        let audio = response.bytes().await.map_err(|error| {
            std::io::Error::other(format!("the speaker's answer broke: {error}"))
        })?;

        // The same two guards the spawning path applies, and they are not decoration here either. A
        // server that answers 200 with an error page produces bytes that are not audio, and a voice
        // whose model failed to load answers 200 with nothing at all.
        if audio.len() > MAX_AUDIO_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("speaker produced more than {MAX_AUDIO_BYTES} bytes"),
            ));
        }
        if audio.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the speaker produced no audio",
            ));
        }

        Ok(audio.to_vec())
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

    /// A trailing slash in a config file is not a typo anybody notices, and `{base}//synthesize` is
    /// routed by some servers and 404'd by others — so it is trimmed rather than trusted.
    #[tokio::test]
    async fn a_trailing_slash_in_the_url_does_not_become_a_double_one() {
        for written in [
            "http://127.0.0.1:1",
            "http://127.0.0.1:1/",
            "  http://127.0.0.1:1///  ",
        ] {
            let speaker = HttpSpeaker::new(written.to_string());
            let error = speaker.speak("olá").await.unwrap_err();
            // Nothing listens on port 1, so the request is REFUSED rather than answered — which is
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

    /// An engine that is simply not running is the failure that actually happens, and it says so.
    #[tokio::test]
    async fn a_resident_engine_that_is_not_there_reports_a_refused_connection() {
        let speaker = HttpSpeaker::new("http://127.0.0.1:1".to_string());
        let error = speaker.speak("olá").await.unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
        // The address is in the message: "the speaker failed" with no port names nothing to check.
        assert!(error.to_string().contains("127.0.0.1:1"), "{error}");
    }

    /// Both implementations refuse an empty utterance, and neither goes near the network or a spawn
    /// to do it. One trait, one meaning — a rule that held for only one `impl` would be a rule the
    /// caller cannot rely on.
    #[tokio::test]
    async fn neither_implementation_will_speak_nothing() {
        assert_eq!(
            HttpSpeaker::new("http://127.0.0.1:1".to_string())
                .speak("   ")
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            CommandSpeaker::new("no-such-program-anywhere".to_string())
                .speak("   ")
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    /// A command string with no program in it is a configuration error, not a crash.
    #[tokio::test]
    async fn an_empty_command_reports_rather_than_panics() {
        let speaker = CommandSpeaker::new("   ".to_string());
        let error = speaker.speak("olá").await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
