//! Developer-time configuration and validation.

use super::*;

/// The devtime ingestion's settings.
///
/// Derives `PartialEq` and not `Eq`, because the amber rate is an `f64`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeConfig {
    pub enabled: bool,
    /// Seconds between two ingestion cycles.
    pub cycle_seconds: u64,
    /// Minutes of silence after which a session counts as idle.
    pub idle_minutes: u64,
    /// Where the transcripts live. Empty means `<home>/.claude/projects`.
    pub projects_dir: String,
    /// The share of unparseable lines above which the ingest health goes amber.
    pub parse_failure_amber_rate: f64,
    /// Fewer lines than this and the rate above is not judged.
    pub parse_failure_min_lines: u64,
    /// The rule engine's vocabularies and thresholds (sub-project 2).
    pub rules: DevtimeRulesConfig,
}

impl Default for DevtimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cycle_seconds: 60,
            idle_minutes: 15,
            projects_dir: String::new(),
            parse_failure_amber_rate: 0.02,
            parse_failure_min_lines: 200,
            rules: DevtimeRulesConfig::default(),
        }
    }
}

/// The parser's closed error-class vocabulary as `devtime.yaml` may name it in
/// `rules.vocab.error_signatures`. It lists the same nine tokens as `devtime_parse::ERROR_CLASSES`
/// (the parser's own const gains `wrong_shell` with parser v2), and a parser test holds the two equal:
/// this module must not import the parser, so the list is stated twice and checked once.
pub const RULES_ERROR_CLASSES: [&str; 9] = [
    "exit_nonzero",
    "exit_75",
    "timeout",
    "interrupted",
    "permission_denied",
    "hook_block",
    "edit_not_found",
    "wrong_shell",
    "tool_error",
];

pub(super) fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

/// The rule engine's whole configuration: every threshold, program list and vocabulary a rule reads.
/// Rule code holds no such literal (spec §3.5), so a partial file at any nesting level is valid and
/// every field has the default the spec states.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeRulesConfig {
    pub enabled: bool,
    /// Backlog cap per cycle after a `rules_version` change; sessions touched this cycle always run.
    pub sessions_per_cycle: u32,
    pub commands: DevtimeCommandsConfig,
    pub roles: DevtimeRolesConfig,
    pub vocab: DevtimeVocabConfig,
    pub thresholds: DevtimeThresholdsConfig,
    pub paths: DevtimePathsConfig,
    pub precision: DevtimePrecisionConfig,
    pub unexplained: DevtimeUnexplainedConfig,
    pub adapters: DevtimeAdaptersConfig,
}

impl Default for DevtimeRulesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sessions_per_cycle: 200,
            commands: DevtimeCommandsConfig::default(),
            roles: DevtimeRolesConfig::default(),
            vocab: DevtimeVocabConfig::default(),
            thresholds: DevtimeThresholdsConfig::default(),
            paths: DevtimePathsConfig::default(),
            precision: DevtimePrecisionConfig::default(),
            unexplained: DevtimeUnexplainedConfig::default(),
            adapters: DevtimeAdaptersConfig::default(),
        }
    }
}

/// Which programs count as a test, a build, a lint, a mutation, a sleep, a commit or a revert, and
/// which tool names are shell, edit, read or search tools.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeCommandsConfig {
    pub test: Vec<String>,
    pub build: Vec<String>,
    pub lint: Vec<String>,
    pub mutating: Vec<String>,
    pub sleep: Vec<String>,
    pub commit: Vec<String>,
    pub revert_with_paths: Vec<String>,
    pub revert_pathless: Vec<String>,
    /// First words that run a script rather than name a program (`bash scripts/gates.sh`).
    pub interpreters: Vec<String>,
    pub shell_tools: Vec<String>,
    pub edit_tools: Vec<String>,
    pub read_tools: Vec<String>,
    pub search_tools: Vec<String>,
    /// Project id to extra programs, appended to the base lists above.
    pub per_project: BTreeMap<String, DevtimeProjectCommands>,
}

impl Default for DevtimeCommandsConfig {
    fn default() -> Self {
        Self {
            test: owned(&[
                "cargo test",
                "go test",
                "npm test",
                "npx vitest",
                "npx jest",
                "pytest",
                "dotnet test",
                "make test",
            ]),
            build: owned(&[
                "cargo build",
                "cargo check",
                "go build",
                "go vet",
                "npx tsc",
                "mvn",
                "gradle",
                "dotnet build",
                "make",
            ]),
            lint: owned(&["cargo fmt", "cargo clippy", "npx eslint", "gofmt"]),
            mutating: owned(&[
                "cargo fmt",
                "gofmt",
                "git checkout",
                "git restore",
                "git stash",
                "git reset",
                "git apply",
                "git merge",
                "git rebase",
                "git pull",
                "sed",
                "patch",
            ]),
            sleep: owned(&["sleep", "Start-Sleep", "timeout"]),
            commit: owned(&["git commit"]),
            revert_with_paths: owned(&["git checkout", "git restore"]),
            revert_pathless: owned(&["git reset", "git stash"]),
            interpreters: owned(&[
                "bash",
                "sh",
                "python",
                "python3",
                "node",
                "pwsh",
                "powershell",
            ]),
            shell_tools: owned(&["Bash", "PowerShell"]),
            edit_tools: owned(&["Edit", "Write", "MultiEdit", "NotebookEdit"]),
            read_tools: owned(&["Read"]),
            search_tools: owned(&["Grep", "Glob"]),
            per_project: BTreeMap::new(),
        }
    }
}

/// One project's extra test, build and lint programs.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeProjectCommands {
    pub test: Vec<String>,
    pub build: Vec<String>,
    pub lint: Vec<String>,
}

/// Case-insensitive globs (`*` only) over a subagent's `agentType`. The reviewer list is tried first.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeRolesConfig {
    pub reviewer: Vec<String>,
    pub implementer: Vec<String>,
}

impl Default for DevtimeRolesConfig {
    fn default() -> Self {
        Self {
            reviewer: owned(&["*review*"]),
            implementer: owned(&["*executor*", "*implement*", "general-purpose"]),
        }
    }
}

/// The words the parser and the rules match in memory. Only the derived flag or token is ever stored.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeVocabConfig {
    /// A prompt that opens with one of these counts as a correction of the previous answer.
    pub correction_openers: Vec<String>,
    /// Model families, weakest first.
    pub model_strength: Vec<String>,
    /// Error class to extra substrings that classify a failed tool result into it.
    pub error_signatures: BTreeMap<String, Vec<String>>,
}

impl Default for DevtimeVocabConfig {
    fn default() -> Self {
        let mut error_signatures = BTreeMap::new();
        error_signatures.insert(
            "wrong_shell".to_string(),
            owned(&[
                "is not recognized as the name of a cmdlet",
                "is not a valid statement separator",
                "ParserError",
                "Windows Subsystem for Linux",
                "C:/Program Files/Git/",
            ]),
        );
        error_signatures.insert(
            "hook_block".to_string(),
            owned(&["pre-commit hook", "hook rejected", "hook declined"]),
        );
        error_signatures.insert(
            "edit_not_found".to_string(),
            owned(&[
                "matches of the string to replace",
                "File has not been read yet",
            ]),
        );
        error_signatures.insert("permission_denied".to_string(), Vec::new());
        Self {
            correction_openers: owned(&[
                "não",
                "nao",
                "não era isso",
                "nao era isso",
                "errado",
                "está errado",
                "isso não",
                "that's not",
                "that is not",
                "not what i",
                "wrong",
                "no,",
                "nope",
            ]),
            model_strength: owned(&["haiku", "sonnet", "opus"]),
            error_signatures,
        }
    }
}

/// Every numeric threshold a rule compares against.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeThresholdsConfig {
    /// D3: a Grep/Glob streak without an edit.
    pub d3_search_burst: u32,
    /// FLAILING: a streak without a Read or an Edit.
    pub d3_flailing_burst: u32,
    pub d5_foreground_agent_seconds: u64,
    pub d6_foreground_command_seconds: u64,
    /// D1 / THRASH.
    pub thrash_repeats: u32,
    pub thrash_reset_calls: u32,
    /// D14 / LATE SCOPE.
    pub late_scope_fraction: f64,
    pub late_scope_min_files: u32,
    pub d8_poll_repeats: u32,
    pub d9_context_tokens: i64,
    pub f_min_sessions: u32,
    pub f_sequence_len: u32,
    pub f_window_days: u32,
}

impl Default for DevtimeThresholdsConfig {
    fn default() -> Self {
        Self {
            d3_search_burst: 6,
            d3_flailing_burst: 4,
            d5_foreground_agent_seconds: 120,
            d6_foreground_command_seconds: 120,
            thrash_repeats: 3,
            thrash_reset_calls: 40,
            late_scope_fraction: 0.30,
            late_scope_min_files: 3,
            d8_poll_repeats: 3,
            d9_context_tokens: 250_000,
            f_min_sessions: 3,
            f_sequence_len: 3,
            f_window_days: 30,
        }
    }
}

/// What counts as an external path, and which file extensions are code.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimePathsConfig {
    /// Globs, case-insensitive, with `\` read as `/`.
    pub external: Vec<String>,
    pub code_extensions: Vec<String>,
}

impl Default for DevtimePathsConfig {
    fn default() -> Self {
        Self {
            external: owned(&["*/temp/*", "*/tmp/*", "*scratchpad*", "*.log", "*.output"]),
            code_extensions: owned(&[
                "rs", "go", "ts", "tsx", "js", "jsx", "mjs", "py", "java", "kt", "cs", "c", "cc",
                "cpp", "h", "hpp", "swift", "rb", "php", "sql", "sh", "ps1",
            ]),
        }
    }
}

/// How a rule's precision is judged (spec §7).
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimePrecisionConfig {
    /// A rule below this precision is flagged out of phase 2.
    pub floor: f64,
    /// Fewer judged cases than this and the precision is not trusted.
    pub min_cases: u32,
    /// Reported while a rule has fewer than `min_cases` cases.
    pub prior_default: f64,
    /// Rule id to its starting precision, when it is not `prior_default`.
    pub priors: BTreeMap<String, f64>,
}

impl Default for DevtimePrecisionConfig {
    fn default() -> Self {
        let mut priors = BTreeMap::new();
        // Spec §5: C2 "arranca com precisão baixa".
        priors.insert("C2".to_string(), 0.2);
        Self {
            floor: 0.8,
            min_cases: 20,
            prior_default: 0.5,
            priors,
        }
    }
}

/// When a slow turn that no rule explains is worth listing.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeUnexplainedConfig {
    pub median_multiple: f64,
    pub min_turn_seconds: u64,
    pub max_explained_fraction: f64,
    /// Main-lane calls per turn that split turns into classes `c0..cN`.
    pub class_bounds: Vec<u32>,
    /// A class with fewer turns has no median, and nothing in it is listed.
    pub min_class_turns: u32,
    pub window_days: u32,
}

impl Default for DevtimeUnexplainedConfig {
    fn default() -> Self {
        Self {
            median_multiple: 3.0,
            min_turn_seconds: 300,
            max_explained_fraction: 0.2,
            class_bounds: vec![0, 5, 20],
            min_class_turns: 10,
            window_days: 30,
        }
    }
}

/// Where the optional adapter sources live. Empty means absent.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeAdaptersConfig {
    pub permission_log: String,
    pub heavy_log: String,
}

/// What the grammar alone cannot say: a signature for a class the parser does not have, a fraction
/// outside [0, 1], or a count that must be at least one. A file that fails this is refused whole.
pub fn validate_devtime(config: &DevtimeConfig) -> Result<(), String> {
    let rules = &config.rules;
    for class in rules.vocab.error_signatures.keys() {
        if !RULES_ERROR_CLASSES.contains(&class.as_str()) {
            return Err(format!(
                "rules.vocab.error_signatures names `{class}`, which is not an error class"
            ));
        }
    }
    let fractions = [
        (
            "rules.thresholds.late_scope_fraction",
            rules.thresholds.late_scope_fraction,
        ),
        ("rules.precision.floor", rules.precision.floor),
        (
            "rules.precision.prior_default",
            rules.precision.prior_default,
        ),
        (
            "rules.unexplained.max_explained_fraction",
            rules.unexplained.max_explained_fraction,
        ),
    ];
    for (name, value) in fractions {
        if !(0.0..=1.0).contains(&value) {
            return Err(format!("{name} must be within [0, 1], got {value}"));
        }
    }
    for (rule, prior) in &rules.precision.priors {
        if !(0.0..=1.0).contains(prior) {
            return Err(format!(
                "rules.precision.priors.{rule} must be within [0, 1], got {prior}"
            ));
        }
    }
    let counts = [
        (
            "rules.thresholds.thrash_repeats",
            rules.thresholds.thrash_repeats,
        ),
        (
            "rules.thresholds.d3_search_burst",
            rules.thresholds.d3_search_burst,
        ),
        (
            "rules.thresholds.d3_flailing_burst",
            rules.thresholds.d3_flailing_burst,
        ),
        (
            "rules.thresholds.f_min_sessions",
            rules.thresholds.f_min_sessions,
        ),
        (
            "rules.thresholds.f_sequence_len",
            rules.thresholds.f_sequence_len,
        ),
        (
            "rules.thresholds.f_window_days",
            rules.thresholds.f_window_days,
        ),
    ];
    for (name, value) in counts {
        if value == 0 {
            return Err(format!("{name} must be at least 1"));
        }
    }
    Ok(())
}

/// `~/.nucleos/devtime.yaml`'s grammar, and the only place that decides what a valid one is.
/// Refuses unknown keys and malformed YAML; [`load_devtime_config`] turns that refusal into defaults.
pub fn parse_devtime_config(contents: &str) -> Result<DevtimeConfig, String> {
    let config =
        serde_yaml::from_str::<DevtimeConfig>(contents).map_err(|error| error.to_string())?;
    validate_devtime(&config)?;
    Ok(config)
}

pub fn load_devtime_config(path: &Path) -> DevtimeConfig {
    if !path.exists() {
        return DevtimeConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_devtime_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "devtime config: could not be parsed; defaults apply");
            DevtimeConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "devtime config: could not be read; defaults apply");
            DevtimeConfig::default()
        }
    }
}

/// Where the thresholds a findings request ran under came from. Reported to the page so a typo in
/// `devtime.yaml` is visible there instead of silently showing the default precision thresholds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdsSource {
    /// The file parsed and applies.
    #[default]
    File,
    /// The file is currently invalid; the last version that parsed applies.
    LastGood,
    /// No file, or an invalid one and no good version ever seen.
    Default,
}

/// One file's cache slot: the (length, mtime) of the last stat that was read, the last config that
/// parsed, and whether that read was a refusal (so an unchanged bad file is not re-read, or
/// re-logged, on every request).
pub(super) struct DevtimeCacheEntry {
    len: u64,
    modified: Option<std::time::SystemTime>,
    last_good: Option<DevtimeConfig>,
    invalid: bool,
}

pub(super) type DevtimeCache =
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, DevtimeCacheEntry>>;

/// A config and where it came from.
pub type ResolvedDevtime = (DevtimeConfig, ThresholdsSource);

/// The devtime config at `path`, re-read only when the file's length or modification time moved.
///
/// Findings and mark requests call this per request; `load_devtime_config` is the startup read.
/// An invalid edit keeps the last version that parsed (defaults only if none ever did) and is
/// logged once per file change, not once per request. A vanished file is the defaults, as for the
/// schedule rules: deleting the file is how an owner reverts to the defaults, so the old content
/// must not outlive it.
pub fn devtime_config_now(path: &Path) -> ResolvedDevtime {
    static CACHE: std::sync::OnceLock<DevtimeCache> = std::sync::OnceLock::new();
    cached_devtime_config(path, CACHE.get_or_init(Default::default))
}

/// The stat-checked read behind [`devtime_config_now`], apart from the static so a test can drive it.
pub(super) fn cached_devtime_config(path: &Path, cache: &DevtimeCache) -> ResolvedDevtime {
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(meta) = std::fs::metadata(path) else {
        cache.remove(path);
        return (DevtimeConfig::default(), ThresholdsSource::Default);
    };
    let (len, modified) = (meta.len(), meta.modified().ok());
    // An unreadable mtime never hits: with only the length to go on, a same-length edit would be
    // served stale.
    let hit = cache
        .get(path)
        .filter(|hit| modified.is_some() && hit.len == len && hit.modified == modified)
        .and_then(devtime_answer);
    if let Some(answer) = hit {
        return answer;
    }
    let loaded = std::fs::read_to_string(path)
        .map_err(|error| error.to_string())
        .and_then(|text| parse_devtime_config(&text));
    let last_good = cache.get(path).and_then(|hit| hit.last_good.clone());
    match loaded {
        Ok(config) => {
            cache.insert(
                path.to_path_buf(),
                DevtimeCacheEntry {
                    len,
                    modified,
                    last_good: Some(config.clone()),
                    invalid: false,
                },
            );
            (config, ThresholdsSource::File)
        }
        Err(error) => {
            tracing::error!(
                %error,
                path = %path.display(),
                "devtime config: could not be loaded; keeping the last good thresholds"
            );
            let entry = DevtimeCacheEntry {
                len,
                modified,
                last_good,
                invalid: true,
            };
            let answer = devtime_answer(&entry);
            cache.insert(path.to_path_buf(), entry);
            answer.unwrap_or((DevtimeConfig::default(), ThresholdsSource::Default))
        }
    }
}

/// What a cache slot answers while its file is unchanged. `None` only for a slot that has nothing
/// to answer with, which cannot be a hit.
fn devtime_answer(entry: &DevtimeCacheEntry) -> Option<ResolvedDevtime> {
    match (&entry.last_good, entry.invalid) {
        (Some(config), false) => Some((config.clone(), ThresholdsSource::File)),
        (Some(config), true) => Some((config.clone(), ThresholdsSource::LastGood)),
        (None, true) => Some((DevtimeConfig::default(), ThresholdsSource::Default)),
        (None, false) => None,
    }
}
