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
}

impl Default for ModelsConfig {
    fn default() -> Self {
        ModelsConfig {
            claude_model: "claude-sonnet-5".to_string(),
            codex_model: "gpt-5.6-terra".to_string(),
            local_triage_model: None,
            voice_cleanup_model: None,
            primary_runner: None,
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
