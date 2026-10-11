//! Voice transcription configuration and hotkeys.

use super::*;

/// What the local model is told to do with a raw transcript.
///
/// The four prohibitions are the load-bearing part, and each answers a measured failure of a small
/// model rather than a hypothetical one. Local triage found a 4B model inventing deadlines nobody
/// wrote and inverting who was asking whom, which two forbidding sentences fixed. The third is
/// specific to dictation: a dictated sentence is often a question, and a small model handed a question
/// ANSWERS it — without that line, saying "will this compile?" pastes an opinion about compilation
/// instead of the words that were said.
///
/// The fourth came out of §14.6, from measurement rather than review: the first three forbid adding,
/// distorting and answering, and none of them forbids REWRITING. Asked to tidy "prune dictations
/// after 7 days", the model returned "pruning dictations after seven days" — reproducibly, three
/// times out of three. It had added nothing and changed no meaning, so every existing rule was
/// satisfied; it had also restructured the sentence and spelled out the numeral, which for dictation
/// is the whole failure. Someone dictating a config value, a version, a time or a path needs the
/// characters they said, not a well-phrased paraphrase of them.
pub const DEFAULT_CLEANUP_PROMPT: &str =
    "Tidy the transcript below. Fix punctuation, capitalisation, \
and obvious speech-to-text errors. Remove filler words and false starts.
Do NOT add any information that is not in the transcript.
Do NOT change the meaning, the tone, or who is asking whom.
Do NOT answer, summarise, or comment on the content — you are an editor, not a reader.
Do NOT rephrase or restructure sentences that are already clear, and keep numbers, dates, \
versions, units and paths exactly as they were said — digits stay digits.
Keep the original language. Return only the corrected text.";

/// The chords the pillar ships with when `~/.nucleos/voice.yaml` names none, per platform.
///
/// macOS is the reason this is a constant rather than three literals in `Default`: the
/// `Ctrl+Alt` family is not free there. Cmd+Space is Spotlight, Ctrl+Space switches the input
/// source, Cmd+Option+Space opens Finder search, Ctrl+Cmd+Space is the Character Viewer, and
/// Ctrl+Option is the modifier pair VoiceOver reserves for itself; a global Cmd+Shift+letter
/// would steal an application shortcut in every application at once. Three modifiers held
/// together reach no default macOS binding, which is why that arm adds Command — a chord the
/// desktop already owns does not fail loudly, it simply never reaches this app, and on screen
/// that is indistinguishable from dictation being broken.
///
/// The spelling is `Super` and not `Cmd` because `global-hotkey`'s parser (`hotkey.rs:205`)
/// accepts "COMMAND" | "CMD" | "SUPER" as one and the same modifier, so this name parses and
/// reads the same on every host.
///
/// **UNVERIFIED on a real Mac.** The collision list above is read from Apple's documented
/// shortcuts, not pressed: CI compiles this arm, and only a person on a Mac can confirm that
/// the four chords it ships are free.
#[cfg(target_os = "macos")]
pub(super) const DEFAULT_HOTKEYS: [&str; 4] = [
    "Ctrl+Alt+Super+Space",
    "Ctrl+Alt+Super+M",
    "Ctrl+Alt+Super+C",
    "Ctrl+Alt+Super+N",
];

/// Windows and Linux leave the `Ctrl+Alt` family alone, so the shorter chords stay — see the
/// macOS arm above for why that platform needs a third modifier.
#[cfg(not(target_os = "macos"))]
pub(super) const DEFAULT_HOTKEYS: [&str; 4] =
    ["Ctrl+Alt+Space", "Ctrl+Alt+M", "Ctrl+Alt+C", "Ctrl+Alt+N"];

/// `~/.nucleos/voice.yaml`. Every field defaults, so a partial file is valid and an absent one leaves the
/// pillar off without comment.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VoiceConfig {
    /// Opt-in, like the email pillar: this one opens a microphone.
    pub enabled: bool,
    /// Split into program + args, with the audio path appended last — the contract the Telegram
    /// sidecar's transcriber already uses. Whitespace separates; double quotes protect a path that
    /// contains spaces, which anything installed under `C:\Program Files` needs. Empty means there is
    /// no transcriber, which is indistinguishable from the pillar being off and is treated as such.
    pub stt_command: String,
    /// A resident transcriber on loopback, e.g. `http://127.0.0.1:5018` for whisper.cpp's own
    /// `whisper-server`.
    ///
    /// **Preferred over `stt_command` when both are set**, for the reason `tts_url` is preferred over
    /// `tts_command`, and by a wider margin than that one. Measured here 2026-09-20 with
    /// `ggml-small` on an RTX 3060: a 1.3 s clip costs 1250 ms spawned and 96 ms resident, a 5.6 s
    /// clip 1210 ms against 191 ms. The floor is identical for a one-second clip and a five-second
    /// one because what the spawning path pays for is loading 487 MB of model and waking CUDA, not
    /// transcribing. `transcribe.rs` carries the table.
    ///
    /// That floor is why this key exists at all: progressive dictation re-transcribes the sentence
    /// in flight as it is spoken, and a 1.2 s floor per revision is not progressive.
    pub stt_url: String,
    /// Split into program + args exactly as `stt_command` is, but the text goes on STDIN and a WAV
    /// comes back on STDOUT — `speak.rs` explains why the two contracts differ. Empty means the
    /// núcleo has no voice, which is a smaller loss than having no transcriber: conversation still
    /// works, it just answers in writing.
    pub tts_command: String,
    /// A resident engine on loopback, e.g. `http://127.0.0.1:5017` for Piper's own HTTP server.
    ///
    /// **Preferred over `tts_command` when both are set**, because the difference is not marginal:
    /// measured here, spawning costs ~2.8 s of model loading per sentence against ~0.2 s for the
    /// resident server. `speak.rs` carries the numbers. Somebody who configured both meant the one
    /// that works, so this wins rather than erroring — but it says so in the log, because silently
    /// ignoring a line somebody wrote is how a config file stops being believed.
    pub tts_url: String,
    pub hotkey: String,
    pub memo_hotkey: String,
    /// Toggles hands-free conversation mode. A third chord and not a mode of the first, because the
    /// two do opposite things with the same recording: dictation pastes it into whatever had focus,
    /// conversation sends it to the agent. A single key that guessed between them would guess wrong
    /// in the direction that types a question into a terminal.
    pub conversation_hotkey: String,
    /// Opens the Brain capture box, where the owner types a note into the knowledge store. A chord
    /// of its own because it neither records nor talks: folding it into one of the other three
    /// would make a keystroke that only shows a text box start a microphone.
    pub capture_hotkey: String,
    /// Dictations are a searchable record of everything said, in a pillar whose first requirement is
    /// privacy, so they expire. Memos do not: those are documents somebody asked for.
    pub retain_dictations_days: u8,
    /// Terms said often and heard badly. Applied twice on purpose — as decoding bias and as a
    /// deterministic pass — so a term is fixed even when the bias was not enough.
    pub hints: Vec<String>,
    /// What ends a turn, now that silence does not. A list and not a literal because the spelling a
    /// transcriber returns is a measurement, not a decision: `-l auto` picks a language per segment,
    /// so the same spoken word comes back differently depending on what whisper thought it heard.
    /// Phase 1 measures those spellings and they are added here.
    pub closing_words: Vec<String>,
    /// What throws the accumulated turn away. Two tokens, matched as two — `risca` alone is a verb
    /// somebody says about code.
    pub discard_phrase: String,
    /// What confirms a discard. Throwing away three minutes of thinking on one misheard phrase is
    /// the expensive mistake in this pair, so it takes two utterances and not one.
    pub confirm_words: Vec<String>,
    pub cleanup_prompt: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            stt_command: String::new(),
            stt_url: String::new(),
            tts_command: String::new(),
            tts_url: String::new(),
            hotkey: DEFAULT_HOTKEYS[0].to_string(),
            memo_hotkey: DEFAULT_HOTKEYS[1].to_string(),
            conversation_hotkey: DEFAULT_HOTKEYS[2].to_string(),
            capture_hotkey: DEFAULT_HOTKEYS[3].to_string(),
            retain_dictations_days: 7,
            hints: Vec::new(),
            closing_words: vec!["câmbio".into()],
            discard_phrase: "risca isso".into(),
            confirm_words: vec!["sim".into()],
            cleanup_prompt: DEFAULT_CLEANUP_PROMPT.to_string(),
        }
    }
}

impl VoiceConfig {
    /// Whether there is a working capability here, as opposed to a file that merely exists.
    ///
    /// `enabled: true` with no transcriber is not half-on, it is off: the hotkey would record and then
    /// have nowhere to send the audio, which presents as the feature being broken rather than absent.
    pub fn armed(&self) -> bool {
        // Either engine arms it. Naming only `stt_command` here would leave a machine pointed at a
        // resident server -- the configuration that is six times faster -- reporting no voice at all,
        // which is the regression `a_resident_engine_is_a_voice_even_with_no_command` already had to
        // be written for on the speaking side.
        self.enabled && !(self.stt_command.trim().is_empty() && self.stt_url.trim().is_empty())
    }

    /// Whether this machine can say anything out loud.
    ///
    /// Deliberately NOT folded into `armed`, and the asymmetry is the design. A pillar with no
    /// transcriber is off, because every entry point starts with a recording. A pillar with no
    /// speaker still works: the question is heard, the agent answers, and the answer is read rather
    /// than spoken. Collapsing the two would take a conversation away from someone who has an STT
    /// engine and no TTS one — which is every machine on the day this ships.
    ///
    /// Gated on `armed` all the same: a voice with nothing to say it in response to is not a
    /// capability, and reporting it as one would put a control in the window for a pillar that is off.
    pub fn speaks(&self) -> bool {
        self.armed() && (!self.tts_url.trim().is_empty() || !self.tts_command.trim().is_empty())
    }

    pub(super) fn validated(mut self) -> Self {
        if self.retain_dictations_days > 30 {
            tracing::warn!(
                retain_dictations_days = self.retain_dictations_days,
                "voice config: retain_dictations_days must be 0..=30; using 30"
            );
            self.retain_dictations_days = 30;
        }
        // A blank prompt would ask the model to do anything it liked with the transcript. Deleting the
        // key is the documented way to reset, so emptying it resolves to the same default.
        if self.cleanup_prompt.trim().is_empty() {
            self.cleanup_prompt = DEFAULT_CLEANUP_PROMPT.to_string();
        }
        self
    }
}

/// `~/.nucleos/voice.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_voice_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in a dictation aid cannot stop the daemon from starting. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_voice_config(contents: &str) -> Result<VoiceConfig, String> {
    serde_yaml::from_str::<VoiceConfig>(contents)
        .map(VoiceConfig::validated)
        .map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/voice.yaml`. Absent, unreadable or malformed → defaults, with a warning; never an error.
///
/// This follows `load_email_config` rather than `load_schedule_rules`, and the choice matters in two
/// directions: a typo in a dictation aid must not stop the daemon from starting, and "off" is the
/// inert state for a file holding a command the daemon spawns.
pub fn load_voice_config(path: &Path) -> VoiceConfig {
    if !path.exists() {
        return VoiceConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_voice_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "voice config: could not be parsed; the pillar stays off");
            VoiceConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "voice config: could not be read; the pillar stays off");
            VoiceConfig::default()
        }
    }
}
