use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ModelsConfig {
    pub claude_model: String,
    pub codex_model: String,
    /// Absent keeps the established CLI path active, which lets local triage ship dark until an
    /// operator explicitly names a model that can keep message bodies on the machine.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub local_triage_model: Option<String>,
    /// Absent leaves voice cleanup unarmed, so a transcript is delivered raw rather than not at all.
    /// Named here rather than in `.ai/voice.yaml` because pinning models is this file's whole job.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub voice_cleanup_model: Option<String>,
    /// Which agent CLI answers a run. Absent — or naming anything startup does not recognise — keeps
    /// the proven Claude path, so the second runner ships dark until an operator asks for it by
    /// name, the same posture `local_triage_model` gives local inference.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub primary_runner: Option<String>,
    /// Where a job's `plan` stage runs. Absent keeps it on `claude_model`, so a file written before
    /// this key existed routes nothing anywhere.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub plan_model: Option<String>,
    /// Where a job's `review` stage runs, on the same absent-means-unrouted posture as `plan_model`.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub review_model: Option<String>,
    /// Absent leaves chat turns from the Telegram sidecar answered by the cloud CLI, exactly as they
    /// are today. Naming a model here is what moves them onto this machine.
    ///
    /// Ship-dark like `local_triage_model` and `primary_runner`, and here it matters more than for
    /// either: this key changes the behaviour of a channel already in daily use, so an upgrade must
    /// change nothing at all until somebody asks for it by name.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub local_assistant_model: Option<String>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        ModelsConfig {
            claude_model: "claude-sonnet-5".to_string(),
            codex_model: "gpt-5.6-terra".to_string(),
            local_triage_model: None,
            voice_cleanup_model: None,
            primary_runner: None,
            plan_model: None,
            review_model: None,
            local_assistant_model: None,
        }
    }
}

fn deserialize_optional_model<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty()))
}

pub fn load_models_config(path: &Path) -> std::io::Result<ModelsConfig> {
    if !path.exists() {
        return Ok(ModelsConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    serde_yaml::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `.ai/email.yaml` (spec §3.4). Every field has a default, so a partial file is valid and an
/// absent one switches the pillar off in silence rather than blocking startup.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EmailConfig {
    /// Opt-in. The pillar reads a real mailbox, so nothing about it starts by accident.
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    /// The submission host, kept apart from `host` above because reading a mailbox and posting a
    /// message to it are two different servers as often as they are one — `imap.gmail.com` and
    /// `smtp.gmail.com` being the case most people are in. Empty is the shipped state and means
    /// sending is unconfigured: the send route answers 503 rather than guessing a host from the
    /// IMAP one, because a guessed submission server either refuses the login or, far worse,
    /// belongs to somebody else.
    pub smtp_host: String,
    /// 465 (implicit TLS) rather than 587 (STARTTLS): the pillar's premise is that a message the
    /// mailbox's owner cannot recall does not leave this machine in the clear, and only one of the
    /// two is encrypted before the first byte of the conversation.
    pub smtp_port: u16,
    pub username: String,
    pub mailbox: String,
    pub sent_mailbox: Option<String>,
    pub poll_interval_secs: u64,
    /// Which triage classes are worth interrupting a person for.
    ///
    /// An EMPTY list is a valid value meaning "notify about nothing" — only an ABSENT key gives
    /// the `["urgent"]` default. The distinction carries the rollout's first week (spec §9), where
    /// the whole point is to watch the classifier without being paged by it; treating `[]` as
    /// absent would notify from day one and burn the calibration ramp.
    pub notify_classes: Vec<String>,
    /// Hour (UTC) the daily digest is emitted. Validated to `0..=21` so its two-hour window cannot
    /// straddle midnight and emit twice (§6.3).
    pub digest_hour_utc: u8,
    /// How long a classified message keeps its body (§7.2). Defaults to 14 rather than 0 because
    /// the pillar's first state after being switched on is always the calibration week, which is
    /// exactly when the bodies are needed. Capped at 30, where row pruning removes the row anyway.
    pub retain_bodies_days: u8,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            username: String::new(),
            mailbox: "INBOX".to_string(),
            sent_mailbox: None,
            poll_interval_secs: 300,
            notify_classes: vec!["urgent".to_string()],
            digest_hour_utc: 7,
            retain_bodies_days: 14,
        }
    }
}

impl EmailConfig {
    /// Clamps the two validated fields, warning about what it changed. Out-of-range values are
    /// corrected rather than fatal, for the same reason the whole file is: nothing in this config
    /// is worth refusing to start the daemon over.
    fn validated(mut self) -> Self {
        if self.digest_hour_utc > 21 {
            tracing::warn!(
                digest_hour_utc = self.digest_hour_utc,
                "email config: digest_hour_utc must be 0..=21 so the window cannot cross midnight; using 7"
            );
            self.digest_hour_utc = 7;
        }
        if self.retain_bodies_days > 30 {
            tracing::warn!(
                retain_bodies_days = self.retain_bodies_days,
                "email config: retain_bodies_days must be 0..=30; using 30"
            );
            self.retain_bodies_days = 30;
        }
        self
    }
}

/// Reads `.ai/email.yaml`. Absent or unreadable → defaults, with a warning; never an error, so a
/// typo in an optional pillar's config cannot stop the daemon from starting.
pub fn load_email_config(path: &Path) -> EmailConfig {
    if !path.exists() {
        return EmailConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| serde_yaml::from_str::<EmailConfig>(&text)) {
        Ok(Ok(config)) => config.validated(),
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "email config: could not be parsed; the pillar stays off");
            EmailConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "email config: could not be read; the pillar stays off");
            EmailConfig::default()
        }
    }
}

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

/// `.ai/voice.yaml`. Every field defaults, so a partial file is valid and an absent one leaves the
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
    pub hotkey: String,
    pub memo_hotkey: String,
    /// Dictations are a searchable record of everything said, in a pillar whose first requirement is
    /// privacy, so they expire. Memos do not: those are documents somebody asked for.
    pub retain_dictations_days: u8,
    /// Terms said often and heard badly. Applied twice on purpose — as decoding bias and as a
    /// deterministic pass — so a term is fixed even when the bias was not enough.
    pub hints: Vec<String>,
    pub cleanup_prompt: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            stt_command: String::new(),
            hotkey: "Ctrl+Alt+Space".to_string(),
            memo_hotkey: "Ctrl+Alt+M".to_string(),
            retain_dictations_days: 7,
            hints: Vec::new(),
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
        self.enabled && !self.stt_command.trim().is_empty()
    }

    fn validated(mut self) -> Self {
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

/// Reads `.ai/voice.yaml`. Absent, unreadable or malformed → defaults, with a warning; never an error.
///
/// This follows `load_email_config` rather than `load_schedule_rules`, and the choice matters in two
/// directions: a typo in a dictation aid must not stop the daemon from starting, and "off" is the
/// inert state for a file holding a command the daemon spawns.
pub fn load_voice_config(path: &Path) -> VoiceConfig {
    if !path.exists() {
        return VoiceConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| serde_yaml::from_str::<VoiceConfig>(&text)) {
        Ok(Ok(config)) => config.validated(),
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

/// The calendar's settings.
///
/// Only two things are configurable, because only two things are policy. The zone is what an event
/// means when the caller does not say; the working window is where a PROPOSAL may land — and it is
/// emphatically not when you are busy. Busy comes from real events only, and keeping the two apart
/// is what stops "I do not work Sundays" from quietly becoming "tell me nothing on Sundays".
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarConfig {
    /// An IANA name. Empty means "ask the operating system", which is right far more often than
    /// any name written into a default could be.
    pub default_tz: String,
    pub working_hours_start: String,
    pub working_hours_end: String,
    /// Lowercase three-letter English day names.
    pub working_weekdays: Vec<String>,
    /// Whether an `action`-class message may file a proposal asking for time. Off by default, for
    /// the reason spelled out on `calendar::CalendarRuntime::propose_for_actions`.
    pub propose_time_for_actions: bool,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            default_tz: String::new(),
            working_hours_start: "09:00".to_string(),
            working_hours_end: "18:00".to_string(),
            working_weekdays: ["mon", "tue", "wed", "thu", "fri"]
                .iter()
                .map(|day| (*day).to_string())
                .collect(),
            propose_time_for_actions: false,
        }
    }
}

pub fn load_calendar_config(path: &Path) -> CalendarConfig {
    if !path.exists() {
        return CalendarConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| serde_yaml::from_str::<CalendarConfig>(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "calendar config: could not be parsed; defaults apply");
            CalendarConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "calendar config: could not be read; defaults apply");
            CalendarConfig::default()
        }
    }
}

/// `.ai/web.yaml`. The web pillar's settings, including the one list in this system that decides
/// what counts as trustworthy.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WebConfig {
    /// Opt-in, like every pillar that reaches the network.
    pub enabled: bool,
    /// `brave` or `searxng`. Validated by the sidecar, which is what has to build one.
    pub provider: String,
    pub searxng_url: String,
    pub retain_pages_days: i64,
    pub max_page_bytes: i64,
    pub fetch_timeout_seconds: u32,
    /// Hosts whose extracted text may reach an agent as written — and only then when the owner
    /// asked (`trust.rs`, spec §5.2).
    ///
    /// Empty by default, and that is the load-bearing choice in this struct. Every other field's
    /// default is a convenience; this one's is a refusal. A `.ai/web.yaml` that is missing,
    /// unreadable, or malformed therefore trusts NOTHING rather than falling back to a list nobody
    /// can see — the opposite direction from `VoiceConfig`, whose defaults are all benign.
    pub trusted_hosts: Vec<String>,
    /// Whether a pillar may search on its own, with nobody watching (spec §10.5).
    ///
    /// Separate from `enabled`, and off by default, because the query leaves the machine: enriching
    /// a correspondent means searching a person's name, and nobody decided to share that.
    ///
    /// It lives in the file format and NOT yet on `WebRuntime`, because nothing consumes it: no
    /// pillar reaches the web in this version. Carrying it into the runtime would be a switch that
    /// grants nothing, and a switch that grants nothing is one somebody later assumes is working.
    /// The first pillar to search is what moves it across.
    pub pillar_search_enabled: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "brave".to_string(),
            searxng_url: String::new(),
            retain_pages_days: 30,
            max_page_bytes: 2_000_000,
            fetch_timeout_seconds: 20,
            trusted_hosts: Vec::new(),
            pillar_search_enabled: false,
        }
    }
}

/// Reads `.ai/web.yaml`. Absent, unreadable or malformed → defaults, with a warning.
///
/// The failure mode is deliberately asymmetric with the rest of this module: falling back to
/// defaults here means falling back to an EMPTY allowlist, so a broken file costs fidelity (more
/// pages go through the local model) and never costs safety. A loader that errored instead would
/// stop the daemon over a typo in a convenience list; one that guessed a permissive list would be
/// the worst of both.
pub fn load_web_config(path: &Path) -> WebConfig {
    if !path.exists() {
        return WebConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| serde_yaml::from_str::<WebConfig>(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "web config: could not be parsed; the pillar stays off and nothing is trusted");
            WebConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "web config: could not be read; the pillar stays off and nothing is trusted");
            WebConfig::default()
        }
    }
}

/// `.ai/browser.yaml`. The browser pillar's switch and its ceilings (spec §8).
///
/// The site lists are deliberately NOT here, and that omission is the pillar's central invariant:
/// they live in `browser_sites` and grow only when a person finishes a login (spec §5.2). A field in
/// this file would be a way to grant a profile access to a host by editing a gitignored YAML, which
/// is exactly the path §5.2 exists to close.
///
/// There is also no field that widens the fence. Spec §6.4: the boundary of §6.2 is not
/// configurable, because a switch to loosen it is a switch somebody eventually finds a reason to
/// flip at 2am.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    /// Opt-in, like every pillar that reaches the network. Ships off (spec §14.2).
    pub enabled: bool,
    /// Live tabs. They compete with the local model for the same card, so this is a ceiling rather
    /// than a target.
    pub max_sessions: u32,
    pub cache_size_mb: u32,
    /// Persistent profiles. Above this the sidecar refuses and names one to forget.
    pub max_profiles: u32,
    /// The whole profiles directory, ephemeral included. Reaching it sweeps first and refuses second.
    pub disk_budget_mb: u32,
    pub load_timeout_seconds: u32,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        // The numbers spec §8 ships, so the documented file and an absent file behave alike.
        Self {
            enabled: false,
            max_sessions: 2,
            cache_size_mb: 100,
            max_profiles: 20,
            disk_budget_mb: 3000,
            load_timeout_seconds: 30,
        }
    }
}

/// Reads `.ai/browser.yaml`. Absent, unreadable or malformed → defaults, with a warning.
///
/// Defaults mean the pillar is OFF, so a broken file costs a capability and never grants one — the
/// same asymmetry [`load_web_config`] has, and here it is easier to justify: there is nothing in
/// this file whose default is more permissive than what somebody would have written.
pub fn load_browser_config(path: &Path) -> BrowserConfig {
    if !path.exists() {
        return BrowserConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| serde_yaml::from_str::<BrowserConfig>(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "browser config: could not be parsed; the pillar stays off");
            BrowserConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "browser config: could not be read; the pillar stays off");
            BrowserConfig::default()
        }
    }
}

/// How many seats one council may hold.
///
/// Eight, from the Python orchestrator this pillar was ported out of, where it was the parallelism
/// cap. Here it is the roster cap and the parallelism cap at once, because the roster IS the
/// parallelism: every seat of phase 1 is launched together, so a ninth seat would be a ninth agent
/// process and a ninth cloud bill for one question. A number in this file may lower the fan-out and
/// may not raise it, exactly as `MAX_ITEMS_CEILING` may not — `.ai/` is gitignored, so nobody
/// reviews what is written here.
pub const MAX_COUNCIL_SEATS: usize = 8;

/// The wall clock one seat gets, when the file does not say.
///
/// 600 seconds, the same default the Python council ran on. It is per SEAT and not per council: the
/// seats of phase 1 run concurrently, so a council of eight is still bounded by one of these plus
/// phase 2 plus phase 3.
pub const DEFAULT_COUNCIL_TIMEOUT_SECONDS: u64 = 600;

/// The ceiling on that clock, whatever the file asks for.
///
/// A council holds no worktree and takes no concurrency slot (that is decision 7 of the design), so
/// nothing else in the daemon would ever end one. The clock is therefore the only thing that does,
/// and an unbounded one is a council that stays `running` for as long as the daemon lives.
pub const MAX_COUNCIL_TIMEOUT_SECONDS: u64 = 3_600;

/// Where one seat's answer comes from.
///
/// Two variants and no `Auto`. Which machine a question leaves — or does not leave — is the whole
/// reason a mixed roster is a feature, and a variant that decided it for the operator would make
/// the roster stop being the statement of that.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SeatKind {
    /// An agent CLI, through `runner.rs`.
    Cloud,
    /// A model on this machine, through `local_agent.rs`.
    Local,
}

impl SeatKind {
    pub fn as_db_str(self) -> &'static str {
        match self {
            SeatKind::Cloud => "cloud",
            SeatKind::Local => "local",
        }
    }
}

/// One seat of a roster: where it runs and which model answers.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CouncilSeat {
    pub kind: SeatKind,
    /// Spelled `ref` in the file, because that is what the design and the Python it came from call
    /// it, and `model_ref` in Rust, because `ref` is a keyword.
    #[serde(rename = "ref")]
    pub model_ref: String,
}

/// `.ai/council.yaml`. Absent means there is no council — this pillar has no useful default,
/// because a roster nobody chose is a list of models nobody agreed to pay for.
///
/// `deny_unknown_fields`, and here it is load-bearing rather than tidy: `member:` for `members:`
/// would otherwise parse into a council with no seats, which is a council that answers every
/// question with the chairman's own opinion while looking like it deliberated.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CouncilConfig {
    #[serde(default = "default_council_timeout")]
    pub timeout_seconds: u64,
    pub chairman: CouncilSeat,
    pub members: Vec<CouncilSeat>,
}

fn default_council_timeout() -> u64 {
    DEFAULT_COUNCIL_TIMEOUT_SECONDS
}

impl CouncilConfig {
    /// Everything wrong with a roster, said as one list rather than as the first thing noticed.
    ///
    /// An operator editing this file by hand is going to get more than one thing wrong at once, and
    /// a loader that reports only the first turns a single correction into three restarts.
    fn faults(&self, local_available: bool) -> Vec<String> {
        let mut faults = Vec::new();

        if self.members.is_empty() {
            faults.push("the roster has no members".to_string());
        }
        if self.members.len() > MAX_COUNCIL_SEATS {
            faults.push(format!(
                "the roster has {} members, above the ceiling of {MAX_COUNCIL_SEATS}",
                self.members.len()
            ));
        }
        for (index, seat) in self.seats().enumerate() {
            let who = if index == 0 {
                "the chairman".to_string()
            } else {
                format!("seat {}", index - 1)
            };
            if seat.model_ref.trim().is_empty() {
                faults.push(format!("{who} names no model"));
            }
            // Refused rather than quietly re-routed to the cloud. An operator who wrote `local`
            // asked for a question that does not leave this machine, and answering it in the cloud
            // anyway is the one failure this check exists to prevent.
            if seat.kind == SeatKind::Local && !local_available {
                faults.push(format!(
                    "{who} asks for a local model and no local model is configured"
                ));
            }
        }

        faults
    }

    /// The chairman first, then the members, which is the order `faults` numbers them in.
    fn seats(&self) -> impl Iterator<Item = &CouncilSeat> {
        std::iter::once(&self.chairman).chain(self.members.iter())
    }

    fn validated(mut self) -> Self {
        if self.timeout_seconds == 0 {
            tracing::warn!(
                "council config: timeout_seconds must be above zero; using {DEFAULT_COUNCIL_TIMEOUT_SECONDS}"
            );
            self.timeout_seconds = DEFAULT_COUNCIL_TIMEOUT_SECONDS;
        }
        if self.timeout_seconds > MAX_COUNCIL_TIMEOUT_SECONDS {
            tracing::warn!(
                timeout_seconds = self.timeout_seconds,
                "council config: timeout_seconds is capped at {MAX_COUNCIL_TIMEOUT_SECONDS}"
            );
            self.timeout_seconds = MAX_COUNCIL_TIMEOUT_SECONDS;
        }
        self
    }
}

/// Reads `.ai/council.yaml`. Absent, unreadable, malformed or invalid → `None`, with a warning.
///
/// This follows `load_web_config` and NOT `load_models_config`, and the direction was chosen rather
/// than inherited. Erroring would stop the daemon — mail, autopilot, voice and the API with it —
/// over a typo in a list of model names, and the fallback here is the feature being OFF rather than
/// a permissive one. A council nobody can start costs the operator a council; a daemon that will
/// not boot costs them everything else.
///
/// `local_available` is passed in rather than read, because whether this machine can answer locally
/// is not configuration — startup PROVES it by probing the model, and only `main.rs` holds that
/// answer.
pub fn load_council_config(path: &Path, local_available: bool) -> Option<CouncilConfig> {
    if !path.exists() {
        return None;
    }
    let config = match std::fs::read_to_string(path).map(|text| serde_yaml::from_str(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "council config: could not be parsed; there is no council");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "council config: could not be read; there is no council");
            return None;
        }
    };

    let faults = CouncilConfig::faults(&config, local_available);
    if !faults.is_empty() {
        tracing::warn!(
            path = %path.display(),
            faults = %faults.join("; "),
            "council config: the roster is not usable; there is no council"
        );
        return None;
    }

    Some(config.validated())
}

/// `deny_unknown_fields` on every rule type and on the file itself: without it a typo like
/// `schedule:` for `schedules:` parses cleanly into an empty ruleset, and all autonomy for that
/// project silently stops. That direction is fail-closed, which is precisely why nobody notices —
/// the contract this module advertises is "error on malformed YAML rather than guess", and a
/// misspelt key is malformed.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRule {
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub cwd: Option<String>,
    /// The IANA zone the cron is read in — `Europe/Lisbon`, `America/New_York`.
    ///
    /// Absent means UTC, which is what every rule written before this field already meant, so no
    /// existing schedule moves by adding it. It exists because UTC is the one answer that is wrong
    /// twice a year for most of the world: `0 8 * * *` is 08:00 local in winter and 09:00 in
    /// summer, and the daily task that quietly starts an hour late for half the year is a worse
    /// failure than one that never runs, because nothing about it looks broken.
    pub timezone: Option<String>,
    /// Absent means the rule keeps today's behaviour: one run, one gate. Opt-in per rule.
    pub graph: Option<GraphConfig>,
}

/// The ceiling the daemon puts on a rule's fan-out, whatever the file asks for.
///
/// `.ai/` is gitignored and travels with nobody, so `autopilot.yaml` is per-developer configuration
/// that no review ever sees. A number in it therefore cannot be the only thing standing between one
/// trigger and an unbounded number of runs — the file may lower the fan-out, never raise it.
pub const MAX_ITEMS_CEILING: usize = 5;

/// The ceiling the daemon puts on how many ROUNDS one job may run.
///
/// The symmetric argument to `MAX_ITEMS_CEILING`'s, against a different threat. That one guards a
/// number in a per-developer file no review ever sees; this guards a number in an HTTP body that a
/// model filled in from a conversation, which is reviewed less still — an assistant asked to "keep
/// going until it's done" can write 10 000 as easily as 10.
///
/// `MAX_ITEMS_CEILING` deliberately does NOT rise to meet it. Five stays the ceiling PER ROUND, and
/// depth comes from rounds, which are counted in the database where a restart cannot lose them.
/// Together they are 100 items in the worst case, each with its own gate — which is a lot, and is
/// exactly why the per-job budget rather than either counter is the brake expected to fire first.
pub const MAX_ROUNDS_CEILING: i64 = 20;

/// The rounds actually allowed, after the daemon's own ceiling.
///
/// A free function rather than an accessor on a struct, because unlike `max_items` this number
/// arrives loose in a request body and there is no struct to hang it on that a caller could not
/// sidestep. Same purpose though: a caller that used the asked-for number directly would honour what
/// the model wrote and leave the ceiling decorative.
///
/// `None` — nobody asked for rounds — resolves to ONE, never to the ceiling. Resolving it upward
/// would switch rounds on for every `graph:` rule already scheduled, silently.
pub fn rounds_allowed(asked: Option<i64>) -> i64 {
    asked.unwrap_or(1).clamp(1, MAX_ROUNDS_CEILING)
}

#[cfg(test)]
mod rounds_ceiling_tests {
    use super::*;

    /// The number arrives in a request body a model filled in from a conversation. An assistant
    /// asked to "keep going until it's done" writes 10 000 as easily as 10, and a ceiling applied
    /// anywhere other than the way in is one a forgetful caller walks past.
    #[test]
    fn the_rounds_a_caller_asks_for_are_cut_to_the_daemons_ceiling() {
        // Nobody asked: one round, never the ceiling. Resolving upward would switch rounds on for
        // every `graph:` rule already scheduled, silently.
        assert_eq!(rounds_allowed(None), 1);
        assert_eq!(rounds_allowed(Some(5)), 5);
        assert_eq!(rounds_allowed(Some(10_000)), MAX_ROUNDS_CEILING);
        // Below the floor is a job that could never do anything, which is not what any caller meant.
        assert_eq!(rounds_allowed(Some(0)), 1);
        assert_eq!(rounds_allowed(Some(-3)), 1);
    }

    /// The per-round ceiling deliberately does NOT rise to meet the round ceiling. Depth comes from
    /// rounds, which are counted in the database where a restart cannot lose them; fan-out stays
    /// where the argument for it was written.
    #[test]
    fn the_per_round_fan_out_is_unchanged_by_rounds_existing() {
        assert_eq!(MAX_ITEMS_CEILING, 5);
        assert_eq!(MAX_ROUNDS_CEILING, 20);
    }
}

fn default_max_items() -> usize {
    MAX_ITEMS_CEILING
}

fn default_true() -> bool {
    true
}

/// Turns one scheduled rule into a job: a sequence of runs over one shared worktree, rather than a
/// single run capped by one context window.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    #[serde(default = "default_max_items")]
    max_items: usize,
    #[serde(default = "default_true")]
    pub gate_after_each_item: bool,
    #[serde(default = "default_true")]
    pub review: bool,
}

impl GraphConfig {
    /// The fan-out actually allowed, after the daemon's own ceiling.
    ///
    /// Private field plus this accessor on purpose: a caller that read `max_items` straight off the
    /// struct would silently honour whatever the file said, and the ceiling would be advisory.
    pub fn max_items(&self) -> usize {
        self.max_items.min(MAX_ITEMS_CEILING)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepoTrigger {
    pub name: String,
    pub branch: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutopilotRules {
    #[serde(default)]
    pub schedules: Vec<ScheduleRule>,
    #[serde(default)]
    pub repo_triggers: Vec<RepoTrigger>,
    #[serde(default)]
    pub gate_command: Option<String>,
}

pub fn load_schedule_rules(project_root: &Path) -> std::io::Result<AutopilotRules> {
    let path = project_root.join(".ai").join("autopilot.yaml");
    if !path.exists() {
        return Ok(AutopilotRules::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    // A file with no YAML document in it -- empty, or nothing but comments -- is a fourth state, and
    // it must land with "absent" rather than with "unreadable". serde_yaml returns EndOfStream here,
    // which would otherwise become `GateConfig::Unreadable` and report `gate errored` on every
    // completed run. The way an operator switches a gate off for an afternoon is to comment the
    // `gate_command:` line out; in this repository's own config that leaves comments only.
    if contents.trim().is_empty() {
        return Ok(AutopilotRules::default());
    }
    serde_yaml::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_from(yaml: &str) -> std::io::Result<AutopilotRules> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(dir.path().join(".ai").join("autopilot.yaml"), yaml).unwrap();
        load_schedule_rules(dir.path())
    }

    #[test]
    fn a_rule_without_a_graph_block_keeps_todays_behaviour() {
        let rules =
            rules_from("schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n")
                .expect("a rule with no graph block still parses");
        assert_eq!(rules.schedules[0].graph, None);
    }

    #[test]
    fn a_graph_block_defaults_to_gating_each_item_and_reviewing() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph: {}\n",
        )
        .expect("an empty graph block is valid and fully defaulted");
        let graph = rules.schedules[0].graph.as_ref().expect("graph present");
        assert_eq!(graph.max_items(), MAX_ITEMS_CEILING);
        assert!(graph.gate_after_each_item);
        assert!(graph.review);
    }

    #[test]
    fn the_daemon_ceiling_wins_over_the_file() {
        // The file is per-developer and gitignored, so nobody reviews this number. It may lower the
        // fan-out; it may not raise it past what the daemon is willing to run in one trigger.
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_items: 500\n",
        )
        .expect("an oversized max_items parses");
        assert_eq!(rules.schedules[0].graph.as_ref().unwrap().max_items(), 5);
    }

    #[test]
    fn a_lower_max_items_is_honoured() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_items: 2\n",
        )
        .expect("a smaller max_items parses");
        assert_eq!(rules.schedules[0].graph.as_ref().unwrap().max_items(), 2);
    }

    #[test]
    fn a_misspelt_graph_key_is_malformed_rather_than_ignored() {
        // Same fail-closed contract the rest of this module keeps: a typo that parsed cleanly would
        // silently drop the setting, and the rule would run a shape nobody asked for.
        let error = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_item: 2\n",
        )
        .expect_err("an unknown key inside graph is an error");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    fn email_config_from(yaml: &str) -> EmailConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("email.yaml");
        std::fs::write(&path, yaml).unwrap();
        load_email_config(&path)
    }

    #[test]
    fn an_absent_email_config_leaves_the_pillar_off() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_email_config(&dir.path().join("email.yaml"));
        assert_eq!(config, EmailConfig::default());
        assert!(!config.enabled);
    }

    /// A typo in an optional pillar's config must not stop the daemon from starting.
    #[test]
    fn an_unparseable_email_config_leaves_the_pillar_off() {
        assert_eq!(
            email_config_from("enabled: [this is not a bool"),
            EmailConfig::default()
        );
    }

    #[test]
    fn a_partial_email_config_keeps_the_defaults_for_the_rest() {
        let config = email_config_from("enabled: true\nhost: imap.gmail.com\nusername: me@x.com\n");
        assert!(config.enabled);
        assert_eq!(config.host, "imap.gmail.com");
        assert_eq!(config.port, 993);
        assert_eq!(config.mailbox, "INBOX");
        assert_eq!(config.notify_classes, vec!["urgent".to_string()]);
        assert_eq!(config.retain_bodies_days, 14);
    }

    #[test]
    fn sem_pasta_de_enviados_o_pilar_arranca() {
        let config = email_config_from("enabled: true\nhost: imap.example.com\n");

        assert!(
            config.enabled,
            "the rest of the email config must still load"
        );
        assert_eq!(config.sent_mailbox, None);
    }

    /// The distinction the rollout's first week depends on: an EMPTY list means "notify about
    /// nothing", and only an absent key means "urgent". Collapsing the two would page the user from
    /// day one and burn the calibration ramp.
    #[test]
    fn an_empty_notify_list_is_a_real_setting() {
        assert_eq!(
            email_config_from("notify_classes: []\n").notify_classes,
            Vec::<String>::new()
        );
        assert_eq!(
            email_config_from("enabled: true\n").notify_classes,
            vec!["urgent".to_string()]
        );
    }

    /// A window starting after 21:00 UTC would cross midnight and emit two digests (§6.3).
    #[test]
    fn a_digest_hour_that_would_cross_midnight_is_corrected() {
        assert_eq!(
            email_config_from("digest_hour_utc: 23\n").digest_hour_utc,
            7
        );
        assert_eq!(
            email_config_from("digest_hour_utc: 21\n").digest_hour_utc,
            21
        );
        assert_eq!(email_config_from("digest_hour_utc: 0\n").digest_hour_utc, 0);
    }

    /// Above 30 days the row itself is gone, so a larger value would keep nothing extra (§7.2).
    #[test]
    fn retention_is_capped_at_the_row_pruning_horizon() {
        assert_eq!(
            email_config_from("retain_bodies_days: 90\n").retain_bodies_days,
            30
        );
        assert_eq!(
            email_config_from("retain_bodies_days: 0\n").retain_bodies_days,
            0
        );
    }

    fn council_config_from(yaml: &str, local_available: bool) -> Option<CouncilConfig> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("council.yaml");
        std::fs::write(&path, yaml).unwrap();
        load_council_config(&path, local_available)
    }

    const A_GOOD_ROSTER: &str = "chairman: { kind: cloud, ref: claude-opus-4-8 }\n\
                                 members:\n\
                                 \x20\x20- { kind: cloud, ref: claude-opus-4-8 }\n\
                                 \x20\x20- { kind: cloud, ref: gpt-5.6-terra }\n";

    #[test]
    fn a_council_roster_parses_with_its_default_clock() {
        let config = council_config_from(A_GOOD_ROSTER, false).expect("a cloud-only roster loads");
        assert_eq!(config.timeout_seconds, DEFAULT_COUNCIL_TIMEOUT_SECONDS);
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.chairman.kind, SeatKind::Cloud);
        assert_eq!(config.members[1].model_ref, "gpt-5.6-terra");
    }

    #[test]
    fn an_absent_council_config_means_there_is_no_council() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_council_config(&dir.path().join("council.yaml"), true).is_none());
    }

    /// Absent and malformed reach the same inert state, and neither is an error.
    ///
    /// The direction is the one `load_web_config` takes and not `load_models_config`'s: erroring
    /// would stop the daemon — mail, autopilot, voice and the API with it — over a typo in a list of
    /// model names. The `member:` case is the one worth pinning, because `deny_unknown_fields` is
    /// the only thing standing between that typo and a council that answers every question with the
    /// chairman's own opinion while looking like it deliberated.
    #[test]
    fn a_malformed_council_yaml_leaves_the_feature_off() {
        assert!(council_config_from("chairman: [this is not a seat", true).is_none());
        assert!(council_config_from("", true).is_none());
        assert!(
            council_config_from(
                "chairman: { kind: cloud, ref: m }\nmember:\n  - { kind: cloud, ref: m }\n",
                true
            )
            .is_none(),
            "`member:` for `members:` must not parse into a seatless council"
        );
        assert!(
            council_config_from("chairman: { kind: sideways, ref: m }\nmembers: []\n", true)
                .is_none(),
            "a `kind` that is neither cloud nor local is not a seat"
        );
    }

    /// An operator who wrote `local` asked for a question that does not leave this machine.
    /// Answering it in the cloud anyway is the one failure this check exists to prevent, so the
    /// council is refused rather than re-routed.
    #[test]
    fn a_local_seat_without_a_local_model_is_refused() {
        const MIXED: &str = "chairman: { kind: cloud, ref: claude-opus-4-8 }\n\
                             members:\n\
                             \x20\x20- { kind: cloud, ref: claude-opus-4-8 }\n\
                             \x20\x20- { kind: local, ref: qwen3.5:4b }\n";

        assert!(council_config_from(MIXED, false).is_none());
        assert_eq!(
            council_config_from(MIXED, true)
                .expect("the same roster loads once a local model exists")
                .members
                .len(),
            2
        );

        // The chairman is a seat too, and is checked on the same rule.
        assert!(
            council_config_from(
                "chairman: { kind: local, ref: qwen3.5:4b }\nmembers:\n  - { kind: cloud, ref: m }\n",
                false
            )
            .is_none()
        );
    }

    #[test]
    fn a_roster_that_is_empty_or_oversized_is_refused() {
        assert!(
            council_config_from("chairman: { kind: cloud, ref: m }\nmembers: []\n", true).is_none(),
            "a council with no members is the chairman talking to itself"
        );

        let mut oversized = "chairman: { kind: cloud, ref: m }\nmembers:\n".to_string();
        for _ in 0..=MAX_COUNCIL_SEATS {
            oversized.push_str("  - { kind: cloud, ref: m }\n");
        }
        assert!(council_config_from(&oversized, true).is_none());

        let mut at_the_ceiling = "chairman: { kind: cloud, ref: m }\nmembers:\n".to_string();
        for _ in 0..MAX_COUNCIL_SEATS {
            at_the_ceiling.push_str("  - { kind: cloud, ref: m }\n");
        }
        assert_eq!(
            council_config_from(&at_the_ceiling, true)
                .expect("the ceiling itself is allowed")
                .members
                .len(),
            MAX_COUNCIL_SEATS
        );
    }

    #[test]
    fn a_seat_that_names_no_model_is_refused() {
        assert!(
            council_config_from(
                "chairman: { kind: cloud, ref: m }\nmembers:\n  - { kind: cloud, ref: \"  \" }\n",
                true
            )
            .is_none()
        );
    }

    /// Nothing else in the daemon would ever end a council — it holds no worktree and takes no
    /// concurrency slot — so the clock is the only thing that does, and an unbounded one is a
    /// council that stays `running` for as long as the daemon lives.
    #[test]
    fn the_seat_clock_is_bounded_at_both_ends() {
        let long = format!("timeout_seconds: 99999\n{A_GOOD_ROSTER}");
        assert_eq!(
            council_config_from(&long, true).unwrap().timeout_seconds,
            MAX_COUNCIL_TIMEOUT_SECONDS
        );

        let zero = format!("timeout_seconds: 0\n{A_GOOD_ROSTER}");
        assert_eq!(
            council_config_from(&zero, true).unwrap().timeout_seconds,
            DEFAULT_COUNCIL_TIMEOUT_SECONDS
        );

        let chosen = format!("timeout_seconds: 120\n{A_GOOD_ROSTER}");
        assert_eq!(
            council_config_from(&chosen, true).unwrap().timeout_seconds,
            120
        );
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        assert_eq!(load_models_config(&path).unwrap(), ModelsConfig::default());
    }

    #[test]
    fn parses_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();

        let config = load_models_config(&path).unwrap();
        assert_eq!(config.claude_model, "claude-opus-4-8");
        assert_eq!(config.codex_model, "gpt-5.6-sol");
    }

    /// Local inference must remain an explicit opt-in: an absent or blank model keeps the proven
    /// CLI path active, while a real name is preserved for startup to construct the local runner.
    #[test]
    fn local_triage_model_is_optional_and_absent_means_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.claude_model, "claude-opus-4-8");
        assert_eq!(absent.local_triage_model, None);

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nlocal_triage_model: qwen3.5:4b\n",
        )
        .unwrap();
        assert_eq!(
            load_models_config(&path).unwrap().local_triage_model,
            Some("qwen3.5:4b".to_string())
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nlocal_triage_model: \"\"\n",
        )
        .unwrap();
        assert_eq!(load_models_config(&path).unwrap().local_triage_model, None);
    }

    /// Per-role models are how a plan turn is priced apart from the implement turns that follow it.
    /// Both keys keep the `local_triage_model` posture: absent or blank means the role is NOT routed
    /// anywhere, so a file written before these keys existed changes nothing about what runs. The
    /// blank case is the one worth pinning — a key left in the file with its value deleted reads as
    /// "turn this off", and `Some("")` would instead pass an empty string to `--model`.
    #[test]
    fn models_config_reads_the_per_role_keys_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nplan_model: claude-opus-4-8\nreview_model: claude-haiku-4-5\n",
        )
        .unwrap();
        let named = load_models_config(&path).unwrap();
        assert_eq!(named.plan_model, Some("claude-opus-4-8".to_string()));
        assert_eq!(named.review_model, Some("claude-haiku-4-5".to_string()));

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.plan_model, None, "an older file routes nothing");
        assert_eq!(absent.review_model, None, "an older file routes nothing");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nplan_model: \"\"\nreview_model: \"   \"\n",
        )
        .unwrap();
        let blank = load_models_config(&path).unwrap();
        assert_eq!(blank.plan_model, None, "a blanked key means off, not empty");
        assert_eq!(
            blank.review_model, None,
            "a blanked key means off, not empty"
        );
    }

    /// Absent and malformed must reach the SAME inert state, and neither may be an error.
    ///
    /// Written in the negative because the failure this guards is a daemon that will not start: this
    /// reader deliberately follows `load_email_config`, not `load_schedule_rules`, so a typo in a
    /// dictation aid cannot take down mail, autopilot and the API with it. The malformed case also has
    /// to land on `armed() == false` rather than merely on defaults — the file holds a command the
    /// daemon spawns, and "off" is the only safe reading of a file it could not understand.
    #[test]
    fn voice_config_absent_and_malformed_both_mean_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.yaml");

        let absent = load_voice_config(&path);
        assert!(!absent.armed());
        assert_eq!(absent.cleanup_prompt, DEFAULT_CLEANUP_PROMPT);

        std::fs::write(&path, "enabled: [this is not a bool\n  stt_command:\n").unwrap();
        let malformed = load_voice_config(&path);
        assert!(!malformed.armed());
        assert_eq!(malformed, VoiceConfig::default());

        std::fs::write(
            &path,
            "enabled: true\nstt_command: whisper-cli -m model.bin\n",
        )
        .unwrap();
        assert!(load_voice_config(&path).armed());
    }

    /// `enabled: true` with nothing to transcribe with is off, not half-on.
    ///
    /// The direction matters: the alternative is a hotkey that records audio and then has nowhere to
    /// send it, which reads as a broken feature rather than an absent one. A blank prompt resolves to
    /// the default for the same reason deleting the key does — that is the documented reset.
    #[test]
    fn voice_enabled_without_a_transcriber_is_not_armed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.yaml");

        std::fs::write(&path, "enabled: true\nstt_command: \"   \"\n").unwrap();
        assert!(!load_voice_config(&path).armed());

        std::fs::write(
            &path,
            "enabled: true\nstt_command: whisper-cli\ncleanup_prompt: \"  \"\nretain_dictations_days: 99\n",
        )
        .unwrap();
        let clamped = load_voice_config(&path);
        assert_eq!(clamped.cleanup_prompt, DEFAULT_CLEANUP_PROMPT);
        assert_eq!(clamped.retain_dictations_days, 30);
    }

    /// The second runner ships dark, so what this key parses to is what decides whether a run is
    /// answered by the proven CLI or by one nobody asked for.
    ///
    /// The unrecognised case is the one worth a test: startup maps anything but `codex` back to the
    /// Claude runner, and it can only do that if loading SUCCEEDS and hands it the name. Were the
    /// deserializer to reject an unknown value instead, a typo in one key would take the whole
    /// daemon down — email, scheduler and all — rather than costing the operator the runner they
    /// misspelled.
    #[test]
    fn primary_runner_is_optional_and_an_unknown_name_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.codex_model, "gpt-5.6-sol");
        assert_eq!(
            absent.primary_runner, None,
            "an absent key must not opt a daemon into the second runner"
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nprimary_runner: codex\n",
        )
        .unwrap();
        assert_eq!(
            load_models_config(&path).unwrap().primary_runner,
            Some("codex".to_string()),
            "the one recognised name must survive parsing for startup to act on"
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nprimary_runner: gemini\n",
        )
        .unwrap();
        let unknown = load_models_config(&path)
            .expect("an unrecognised runner must cost the operator a warning, not the daemon");
        assert_eq!(
            unknown.primary_runner,
            Some("gemini".to_string()),
            "startup needs the name it did not recognise in order to warn about it"
        );
        assert_ne!(
            unknown.primary_runner.as_deref(),
            Some("codex"),
            "nothing but the exact name may reach the codex branch of the runner match"
        );
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(&path, "not: [valid, yaml for this struct").unwrap();
        assert!(load_models_config(&path).is_err());
    }

    #[test]
    fn schedule_rules_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_schedule_rules(dir.path()).unwrap(),
            AutopilotRules::default()
        );
    }

    #[test]
    fn schedule_rules_parses_two_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "schedules:\n\
             \x20\x20- name: nightly-build\n\
             \x20\x20\x20\x20cron: \"0 2 * * *\"\n\
             \x20\x20\x20\x20prompt: \"run the nightly build\"\n\
             \x20\x20\x20\x20cwd: /repo\n\
             \x20\x20- name: morning-report\n\
             \x20\x20\x20\x20cron: \"0 8 * * *\"\n\
             \x20\x20\x20\x20prompt: \"summarize overnight activity\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(dir.path()).unwrap();
        assert_eq!(rules.schedules.len(), 2);

        assert_eq!(rules.schedules[0].name, "nightly-build");
        assert_eq!(rules.schedules[0].cron, "0 2 * * *");
        assert_eq!(rules.schedules[0].prompt, "run the nightly build");
        assert_eq!(rules.schedules[0].cwd, Some("/repo".to_string()));

        assert_eq!(rules.schedules[1].name, "morning-report");
        assert_eq!(rules.schedules[1].cron, "0 8 * * *");
        assert_eq!(rules.schedules[1].prompt, "summarize overnight activity");
    }

    #[test]
    fn schedule_rules_entry_without_cwd_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "schedules:\n\
             \x20\x20- name: morning-report\n\
             \x20\x20\x20\x20cron: \"0 8 * * *\"\n\
             \x20\x20\x20\x20prompt: \"summarize overnight activity\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(dir.path()).unwrap();
        assert_eq!(rules.schedules[0].cwd, None);
    }

    /// The state between "no file" and "broken file". serde_yaml reports a document-less stream as
    /// EndOfStream, which reads as malformed — so without this, commenting out the `gate_command:`
    /// line (the obvious way to switch a gate off) turns every completed worktree run into
    /// `gate errored`. Both spellings, because a comments-only file is the realistic one.
    #[test]
    fn schedule_rules_with_no_yaml_document_is_not_a_gate_rather_than_an_error() {
        for contents in ["", "   \n\n", "# just a comment\n# and another\n"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
            std::fs::write(dir.path().join(".ai").join("autopilot.yaml"), contents).unwrap();

            let rules = load_schedule_rules(dir.path())
                .unwrap_or_else(|e| panic!("{contents:?} must not be an error, got {e}"));
            assert_eq!(rules.gate_command, None);
            assert!(rules.schedules.is_empty());
            assert!(rules.repo_triggers.is_empty());
        }
    }

    #[test]
    fn schedule_rules_malformed_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "schedules: [not, valid, for this struct",
        )
        .unwrap();

        assert!(load_schedule_rules(dir.path()).is_err());
    }

    #[test]
    fn repo_triggers_parse_and_default_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(
            dir.path().join(".ai").join("autopilot.yaml"),
            "repo_triggers:\n\
             \x20\x20- name: review-main\n\
             \x20\x20\x20\x20branch: main\n\
             \x20\x20\x20\x20prompt: \"review new commits on main\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(dir.path()).unwrap();
        assert!(rules.schedules.is_empty());
        assert_eq!(rules.repo_triggers.len(), 1);
        assert_eq!(rules.repo_triggers[0].name, "review-main");
        assert_eq!(rules.repo_triggers[0].branch, "main");
        assert_eq!(rules.repo_triggers[0].prompt, "review new commits on main");
    }
}
