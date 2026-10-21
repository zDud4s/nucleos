//! Configuration for the email, calendar, verification, web, browser, and GitHub pillars.

use super::*;

/// `~/.nucleos/email.yaml` (spec §3.4). Every field has a default, so a partial file is valid and an
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
    pub(super) fn validated(mut self) -> Self {
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

/// `~/.nucleos/email.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_email_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in an optional pillar's config cannot stop the daemon from starting. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_email_config(contents: &str) -> Result<EmailConfig, String> {
    serde_yaml::from_str::<EmailConfig>(contents)
        .map(EmailConfig::validated)
        .map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/email.yaml`. Absent or unreadable → defaults, with a warning; never an error, so a
/// typo in an optional pillar's config cannot stop the daemon from starting.
pub fn load_email_config(path: &Path) -> EmailConfig {
    if !path.exists() {
        return EmailConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_email_config(&text)) {
        Ok(Ok(config)) => config,
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

/// `~/.nucleos/calendar.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_calendar_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in two policy strings cannot stop the daemon from starting. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_calendar_config(contents: &str) -> Result<CalendarConfig, String> {
    serde_yaml::from_str::<CalendarConfig>(contents).map_err(|error| error.to_string())
}

pub fn load_calendar_config(path: &Path) -> CalendarConfig {
    if !path.exists() {
        return CalendarConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_calendar_config(&text)) {
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

/// The verification executor's machine settings, `~/.nucleos/verify.yaml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct VerifyConfig {
    /// Weight units that may run at once across the whole machine.
    pub capacity: u32,
    /// A queued request that has waited this long moves up one priority level.
    pub aging_seconds: u64,
    /// The wall clock one unit may take before it is killed.
    pub unit_timeout_seconds: u64,
    /// The ceiling, in GB, on the whole machine's warm state.
    pub disk_cap_gb: u64,
    /// The argv prefixed to every unit when it runs (a machine's broker). Empty means none.
    pub broker_prefix: Vec<String>,
    /// Days a green unit stays reusable by the cache (spec §5.3).
    pub cache_days: u64,
    /// The admission book bounding how many heavy commands runs launch at once.
    pub admission: AdmissionConfig,
}

/// The longest a run may be made to wait for admission, in seconds. A heavy command's hook has a
/// ceiling of its own, and a wait that outlives it would be cut off by the caller instead.
pub const ADMISSION_MAX_WAIT_CEILING_SECONDS: u64 = 3000;

/// `verify.yaml`'s `admission:` block: the bound on heavy commands launched at once.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AdmissionConfig {
    /// Off means every heavy command is admitted at once.
    pub enabled: bool,
    /// Weight units that may be held at once. Absent means half the physical cores, at least one.
    pub capacity: Option<u32>,
    /// How long a waiting command is held before it is let through without a grant.
    pub max_wait_seconds: u64,
    /// The weight of an ordinary heavy command (build, check, a filtered test).
    pub heavy_weight: u32,
    /// The weight of an unfiltered full test suite.
    pub full_suite_weight: u32,
    /// A grant nobody released is reclaimed after this long.
    pub grant_ttl_seconds: u64,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            capacity: None,
            max_wait_seconds: 900,
            heavy_weight: 1,
            full_suite_weight: 2,
            grant_ttl_seconds: 7200,
        }
    }
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            capacity: 4,
            aging_seconds: 300,
            unit_timeout_seconds: 1800,
            disk_cap_gb: 60,
            broker_prefix: Vec::new(),
            cache_days: 7,
            admission: AdmissionConfig::default(),
        }
    }
}

/// `~/.nucleos/verify.yaml`'s grammar, and the only place that decides what a valid one is.
/// Refuses unknown keys, malformed YAML and a zero capacity, aging or timeout;
/// [`load_verify_config`] turns that refusal into defaults.
pub fn parse_verify_config(contents: &str) -> Result<VerifyConfig, String> {
    // An empty file (or only comments) is "say nothing", which is the defaults.
    if contents.trim().is_empty() {
        return Ok(VerifyConfig::default());
    }
    let config =
        serde_yaml::from_str::<VerifyConfig>(contents).map_err(|error| error.to_string())?;
    if config.capacity < 1 {
        return Err("capacity must be at least 1".to_string());
    }
    if config.aging_seconds < 1 {
        return Err("aging_seconds must be at least 1".to_string());
    }
    if config.unit_timeout_seconds < 1 {
        return Err("unit_timeout_seconds must be at least 1".to_string());
    }
    if config.disk_cap_gb < 1 {
        return Err("disk_cap_gb must be at least 1".to_string());
    }
    if config.cache_days < 1 {
        return Err("cache_days must be at least 1".to_string());
    }
    let admission = &config.admission;
    if admission.capacity == Some(0) {
        return Err("admission.capacity must be at least 1".to_string());
    }
    if admission.heavy_weight < 1 || admission.full_suite_weight < 1 {
        return Err("admission weights must be at least 1".to_string());
    }
    if admission.max_wait_seconds < 1
        || admission.max_wait_seconds > ADMISSION_MAX_WAIT_CEILING_SECONDS
    {
        return Err(format!(
            "admission.max_wait_seconds must be between 1 and {ADMISSION_MAX_WAIT_CEILING_SECONDS}"
        ));
    }
    if admission.grant_ttl_seconds < 60 {
        return Err("admission.grant_ttl_seconds must be at least 60".to_string());
    }
    Ok(config)
}

pub fn load_verify_config(path: &Path) -> VerifyConfig {
    if !path.exists() {
        return VerifyConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_verify_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "verify config: could not be parsed; defaults apply");
            VerifyConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "verify config: could not be read; defaults apply");
            VerifyConfig::default()
        }
    }
}

/// `~/.nucleos/web.yaml`. The web pillar's settings, including the one list in this system that decides
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
    /// default is a convenience; this one's is a refusal. A `~/.nucleos/web.yaml` that is missing,
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

/// `~/.nucleos/web.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_web_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a broken file costs fidelity and never safety. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_web_config(contents: &str) -> Result<WebConfig, String> {
    serde_yaml::from_str::<WebConfig>(contents).map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/web.yaml`. Absent, unreadable or malformed → defaults, with a warning.
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
    match std::fs::read_to_string(path).map(|text| parse_web_config(&text)) {
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

/// `~/.nucleos/browser.yaml`. The browser pillar's switch and its ceilings (spec §8).
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

/// `~/.nucleos/browser.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_browser_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a broken file costs a capability and never grants one. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_browser_config(contents: &str) -> Result<BrowserConfig, String> {
    serde_yaml::from_str::<BrowserConfig>(contents).map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/browser.yaml`. Absent, unreadable or malformed → defaults, with a warning.
///
/// Defaults mean the pillar is OFF, so a broken file costs a capability and never grants one — the
/// same asymmetry [`load_web_config`] has, and here it is easier to justify: there is nothing in
/// this file whose default is more permissive than what somebody would have written.
pub fn load_browser_config(path: &Path) -> BrowserConfig {
    if !path.exists() {
        return BrowserConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_browser_config(&text)) {
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

/// A blank or whitespace-only string reads as `None`, mirroring `chats.rs`'s
/// `instructions.filter(|text| !text.trim().is_empty())` for the same reason: a doctrine of empty
/// spaces would spend an argv slot saying nothing.
pub(crate) fn deserialize_blank_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|text| !text.trim().is_empty()))
}

/// `~/.nucleos/github.yaml`. The GitHub pillar's switch and the two lists that decide what runs without
/// anybody watching.
///
/// **`enabled` defaults to TRUE, which is the opposite of every other pillar that reaches the
/// network, and the asymmetry is the whole design rather than an oversight.** `WebConfig` and
/// `BrowserConfig` ship off because for them "off" and "grants nothing" are the same state. Here
/// they are not: what a missing file has to produce is a pillar *capable of everything and
/// autonomous in nothing* — a person can still approve any operation, and no operation runs without
/// one. That is what the two empty lists below deliver, and switching the pillar off as well would
/// take away the capability the owner asked for in order to withhold an autonomy the empty lists had
/// already withheld.
///
/// So the refusal in this struct lives in `autonomous_reads` and `autonomous_actions`, exactly where
/// `WebConfig::trusted_hosts` puts its own: every other field's default is a convenience, and those
/// two are a refusal. A file that is missing, unreadable or malformed asks a human about every last
/// `gh` invocation.
///
/// Neither list is authoritative on its own. `github::Policy` intersects both with a compiled
/// ceiling, so what is written here can only ever NARROW what the code already allows — which is
/// what makes a per-developer gitignored YAML a defensible place for this at all.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GithubConfig {
    pub enabled: bool,
    /// `gh` prefixes a run may execute through Bash without asking. Subset of
    /// `github::READ_CEILING`; an entry outside it is dropped with a warning.
    pub autonomous_reads: Vec<String>,
    /// `github::ActOp` kinds the núcleo executes without asking. Subset of
    /// `github::ACTION_CEILING`, which does not contain `api_read` and never will.
    pub autonomous_actions: Vec<String>,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            autonomous_reads: Vec::new(),
            autonomous_actions: Vec::new(),
        }
    }
}

/// `~/.nucleos/github.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_github_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in a convenience list cannot take the daemon down. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_github_config(contents: &str) -> Result<GithubConfig, String> {
    serde_yaml::from_str::<GithubConfig>(contents).map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/github.yaml`. Absent, unreadable or malformed -> defaults, with a warning.
///
/// The same asymmetry `load_web_config` has and the same reason: falling back to defaults here means
/// falling back to two EMPTY lists, so a broken file costs convenience — everything starts asking —
/// and never costs safety. A loader that errored would take the daemon down over a typo in a
/// convenience list; one that guessed would be the worst of both.
///
/// The warning says what was lost in the words the owner needs, because "could not parse" alone
/// reads like a pillar that stopped working and this one has not.
pub fn load_github_config(path: &Path) -> GithubConfig {
    if !path.exists() {
        return GithubConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_github_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "github config: could not be parsed; the pillar stays capable and stops being autonomous");
            GithubConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "github config: could not be read; the pillar stays capable and stops being autonomous");
            GithubConfig::default()
        }
    }
}
