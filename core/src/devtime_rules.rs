//! The devtime rule engine (sub-project 2): the registry of rules as data, the per-session facts the
//! rule families read, family dispatch with panic isolation, the projection of findings onto spans
//! and attempts, the `rules_version` fingerprint, and the per-cycle rules pass.
//!
//! Owns no SQL: `devtime_store` reads and writes every row. Rules never create, split or delete a
//! span; they only annotate spans the lane builder already made. Thresholds, programs and vocabularies
//! live in `config::DevtimeRulesConfig`, never in rule code.
//!
//! The types and the registry below are the contract every rule family is written against. The
//! engine bodies (`SessionFacts::build`, `evaluate_with`, `run_rules_pass`) are filled in by a later
//! chunk and return empty values until then.

use std::collections::BTreeSet;
use std::path::Path;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::config::{DevtimeAdaptersConfig, DevtimeRulesConfig};
use crate::devtime_parse::PARSER_VERSION;
use crate::devtime_rules_cmd::{CmdClass, CommandSet, PathMatcher};
use crate::devtime_store::{SessionHead, SessionRows, SpanRow};
use crate::{
    devtime_rules_a, devtime_rules_b, devtime_rules_c, devtime_rules_dctx, devtime_rules_dflow,
};

/// Bumped when the engine itself changes what it concludes from the same rules and rows.
pub const ENGINE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------------

/// How a rule is judged: a `Base` rule is detected from the transcript alone, an `Adapter` rule needs
/// an external source, a `Deferred` rule is registered and never evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Base,
    Adapter,
    Deferred,
}

impl Level {
    /// One of `devtime_store::LEVELS`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Base => "base",
            Level::Adapter => "adapter",
            Level::Deferred => "deferred",
        }
    }
}

/// The time class a finding gives to the spans it claims (spec §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waste {
    Useful,
    Rework,
    Avoidable,
}

impl Waste {
    /// One of `devtime_store::WASTES`.
    pub fn as_str(self) -> &'static str {
        match self {
            Waste::Useful => "useful",
            Waste::Rework => "rework",
            Waste::Avoidable => "avoidable",
        }
    }
}

/// The external source an adapter rule needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    PermissionHook,
    HeavyLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Exact,
    Inferred,
}

impl Confidence {
    /// One of `devtime_store::CONFIDENCE`.
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Inferred => "inferred",
        }
    }
}

/// One rule, as data. Changing a rule's waste, levers or level is one line here plus a `version` bump,
/// and the bump alone recomputes every session (see [`rules_fingerprint`]).
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub id: &'static str,
    pub version: u32,
    pub family: char,
    /// Each one of `devtime_store::LEVERS`; `levers[0]` is the one stored on a finding.
    pub levers: &'static [&'static str],
    pub level: Level,
    pub waste: Waste,
    pub source: Option<Source>,
    /// A correlation, not a proof (spec §5.7).
    pub hypothesis: bool,
}

/// A base rule at version 1: the id, its family and levers, its level and waste.
macro_rules! rule {
    ($id:expr, $family:expr, [$($lever:expr),+], $level:ident, $waste:ident) => {
        Rule {
            id: $id,
            version: 1,
            family: $family,
            levers: &[$($lever),+],
            level: Level::$level,
            waste: Waste::$waste,
            source: None,
            hypothesis: false,
        }
    };
}

/// Every rule of the catalogue, in the order that breaks ties between findings of equal weight.
pub static RULES: &[Rule] = &[
    rule!("A1", 'A', ["model", "spec"], Base, Rework),
    rule!("A2", 'A', ["model"], Base, Rework),
    rule!("A3", 'A', ["verification"], Base, Avoidable),
    rule!("A4", 'A', ["model", "verification"], Base, Rework),
    rule!("A5", 'A', ["model", "spec"], Base, Rework),
    rule!("A6", 'A', ["model"], Base, Rework),
    rule!("A7", 'A', ["dispatch_scope"], Base, Rework),
    rule!("A8", 'A', ["machine"], Base, Rework),
    rule!("B1", 'B', ["precision"], Base, Avoidable),
    rule!("B2", 'B', ["project_knowledge"], Base, Avoidable),
    rule!("B3", 'B', ["environment_knowledge"], Base, Avoidable),
    rule!("B4", 'B', ["estimation"], Base, Avoidable),
    rule!("B5", 'B', ["machine"], Base, Rework),
    rule!("B6", 'B', ["permissions"], Base, Rework),
    rule!("B7", 'B', ["precision"], Base, Avoidable),
    rule!("C1", 'C', ["spec", "clarity"], Base, Rework),
    rule!("C2", 'C', ["spec", "clarity"], Base, Rework),
    rule!("C3", 'C', ["spec", "model"], Base, Rework),
    rule!("C4", 'C', ["discipline"], Deferred, Avoidable),
    rule!("D1", 'D', ["context"], Base, Avoidable),
    rule!("D2", 'D', ["delegation"], Base, Avoidable),
    rule!("D3", 'D', ["delegation", "model"], Base, Avoidable),
    rule!("D4", 'D', ["parallelism"], Base, Avoidable),
    rule!("D5", 'D', ["parallelism"], Base, Avoidable),
    rule!("D6", 'D', ["parallelism"], Base, Avoidable),
    rule!("D7", 'D', ["verification"], Base, Avoidable),
    rule!("D8", 'D', ["waiting"], Base, Avoidable),
    rule!("D9", 'D', ["context"], Base, Avoidable),
    rule!("D10", 'D', ["context"], Base, Avoidable),
    rule!("D11", 'D', ["model"], Deferred, Avoidable),
    Rule {
        id: "D12",
        version: 1,
        family: 'D',
        levers: &["model"],
        level: Level::Base,
        waste: Waste::Rework,
        source: None,
        hypothesis: true,
    },
    rule!("D13", 'D', ["verification"], Base, Avoidable),
    rule!("D14", 'D', ["spec", "discipline"], Base, Avoidable),
    rule!("E1", 'E', ["autonomy"], Deferred, Avoidable),
    Rule {
        id: "E2",
        version: 1,
        family: 'E',
        levers: &["permissions"],
        level: Level::Adapter,
        waste: Waste::Avoidable,
        source: Some(Source::PermissionHook),
        hypothesis: false,
    },
    rule!("F1", 'F', ["script_skill"], Base, Avoidable),
    rule!("F2", 'F', ["gotcha"], Base, Avoidable),
];

pub fn rule(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|rule| rule.id == id)
}

// ---------------------------------------------------------------------------------------------
// Session facts: what a rule family reads. All times are epoch milliseconds.
// ---------------------------------------------------------------------------------------------

/// Which optional adapter sources exist on this machine. Detected once per cycle, by file existence
/// alone, and passed down; a rule family never looks at the filesystem.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdapterSources {
    pub permission_hook: bool,
    pub heavy_log: bool,
}

impl AdapterSources {
    /// A source is present when its configured path is non-empty and exists.
    pub fn detect(cfg: &DevtimeAdaptersConfig) -> Self {
        let present = |path: &str| !path.trim().is_empty() && Path::new(path.trim()).exists();
        Self {
            permission_hook: present(&cfg.permission_log),
            heavy_log: present(&cfg.heavy_log),
        }
    }
}

/// Everything about one session that a rule may read, parsed once from the stored rows. A row with an
/// unparseable timestamp is dropped by `build`, never a panic.
#[derive(Debug, Clone, Default)]
pub struct SessionFacts {
    pub session_id: String,
    pub project_id: String,
    /// `devtime_sessions.started_at` / `ended_at`.
    pub started_ms: i64,
    pub ended_ms: i64,
    /// All lanes, sorted `(started_ms, attempt_id)`; `idx` is the position.
    pub attempts: Vec<AttemptFact>,
    /// All lanes, sorted `(lane, started_ms)`.
    pub spans: Vec<SpanFact>,
    /// Sorted `(lane, first_ms)`.
    pub messages: Vec<MessageFact>,
    /// Main lane, by `seq`.
    pub turns: Vec<TurnFact>,
    /// Sorted by `ts_ms`.
    pub markers: Vec<MarkerFact>,
    pub sources: AdapterSources,
    pub has_wait_machine: bool,
    /// Resolved for `project_id`.
    pub cmds: CommandSet,
    pub paths: PathMatcher,
}

impl SessionFacts {
    /// Parses a session's stored rows into facts. Stub: carries the identity, the adapter sources,
    /// the project's command set and the path matcher, and leaves every collection empty.
    pub fn build(
        head: &SessionHead,
        _rows: &SessionRows,
        _spans: &[SpanRow],
        cfg: &DevtimeRulesConfig,
        sources: AdapterSources,
    ) -> SessionFacts {
        SessionFacts {
            session_id: head.session_id.clone(),
            project_id: head.project_id.clone(),
            sources,
            cmds: CommandSet::for_project(cfg, &head.project_id),
            paths: PathMatcher::new(&cfg.paths),
            ..Default::default()
        }
    }
}

/// How an attempt ended, effective: a background attempt's comes from its notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
    Interrupted,
    Launched,
    Unknown,
}

/// A subagent's role, by the glob its `agentType` matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Implementer,
    Reviewer,
}

#[derive(Debug, Clone)]
pub struct AttemptFact {
    pub idx: usize,
    pub attempt_id: String,
    pub lane: String,
    pub is_main: bool,
    pub message_id: Option<String>,
    /// The main-lane turn whose `[start, next start)` holds `started_ms`.
    pub turn_seq: Option<i64>,
    /// `tool` or `agent`.
    pub kind: String,
    pub tool_name: String,
    pub agent_type: Option<String>,
    pub agent_id: Option<String>,
    pub role: Option<Role>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    /// A background attempt's `bg_ended_at`, else `ended_at`, else `started_ms`.
    pub done_ms: i64,
    /// Effective: for a background attempt, from `bg_status` (`completed` with exit 0 or none is Ok,
    /// `failed` is Error, `killed` is Interrupted, none is Unknown).
    pub outcome: Outcome,
    pub exit_code: Option<i64>,
    pub error_class: Option<String>,
    pub cmd_program: Option<String>,
    pub cmd_hash: Option<String>,
    /// Test, Build or Lint, by `devtime_rules_cmd::classify`.
    pub cmd_class: Option<CmdClass>,
    pub is_shell: bool,
    pub is_edit: bool,
    pub is_read: bool,
    pub is_search: bool,
    /// A shell call whose program is in the mutating list and which succeeded: counts as an edit.
    pub mutating: bool,
    pub is_sleep: bool,
    pub is_commit: bool,
    pub timeout_ms: Option<i64>,
    pub background: bool,
    pub bg_status: Option<String>,
    pub files: Vec<String>,
    pub edits: Vec<EditFact>,
    pub reads: Vec<ReadFact>,
    pub refs_in: Vec<String>,
    pub refs_out: Vec<String>,
    /// True when the row was written by parser v2 or later, which is when refs exist at all.
    pub refs_known: bool,
}

#[derive(Debug, Clone)]
pub struct EditFact {
    pub path: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone)]
pub struct ReadFact {
    pub path: String,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct SpanFact {
    pub id: i64,
    pub lane: String,
    pub kind: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub attempt_ids: Vec<String>,
    pub context_tokens: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct MessageFact {
    pub lane: String,
    pub message_id: String,
    pub first_ms: i64,
    pub last_ms: i64,
    pub model: Option<String>,
    pub has_tool_use: bool,
}

#[derive(Debug, Clone)]
pub struct TurnFact {
    pub seq: i64,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub interrupted: bool,
    /// `None` is "not evaluated" (a row written before parser v2), not "no".
    pub opens_with_correction: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct MarkerFact {
    pub lane: String,
    pub ts_ms: i64,
    pub kind: String,
    pub reference: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// What a family returns
// ---------------------------------------------------------------------------------------------

/// Which spans a finding claims.
#[derive(Debug, Clone)]
pub enum SpanPolicy {
    /// Spans of the finding's lane inside `[started_ms, ended_ms]`.
    Interval,
    /// Spans that carry any of the finding's attempts.
    Attempts,
    /// Every span of one lane.
    Lane(String),
    /// Exactly these span ids.
    Spans(Vec<i64>),
    /// Claims nothing: a flag on the session, counted but with no time of its own.
    Signal,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub rule_id: &'static str,
    pub lane: String,
    /// The claimed interval; equal for a `Signal`.
    pub started_ms: i64,
    pub ended_ms: i64,
    /// The finding's own claim, used to rank cases (0 for a `Signal`). Time totals are summed from
    /// span annotations, never from this, so overlapping findings never double count.
    pub cost_ms: i64,
    /// `attempt_ids[0]` anchors the `finding_key`.
    pub attempt_ids: Vec<String>,
    pub confidence: Confidence,
    /// Repeats, rounds or files; 1 by default.
    pub count: i64,
    pub policy: SpanPolicy,
}

/// The three-valued verification result of an agent attempt (spec §3.5): a check that ran and passed,
/// one that ran and failed, or no check to judge by. Never sent anywhere (spec §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verified {
    Passed,
    Failed,
    NotMeasured,
}

impl Verified {
    /// One of `devtime_store::VERIFIED`.
    pub fn as_str(self) -> &'static str {
        match self {
            Verified::Passed => "passed",
            Verified::Failed => "failed",
            Verified::NotMeasured => "not_measured",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub attempt_id: String,
    pub verified: Verified,
    pub rule_id: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct RuleOutput {
    pub findings: Vec<Finding>,
    pub verdicts: Vec<Verdict>,
}

/// A rule family's entry point. One family may be several files (`D` is two).
pub type FamilyFn = fn(&SessionFacts, &DevtimeRulesConfig) -> RuleOutput;

pub static FAMILIES: &[(char, FamilyFn)] = &[
    ('A', devtime_rules_a::run),
    ('B', devtime_rules_b::run),
    ('C', devtime_rules_c::run),
    ('D', devtime_rules_dctx::run),
    ('D', devtime_rules_dflow::run),
];

// ---------------------------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------------------------

/// Runs every family over one session. Stub: returns nothing.
pub fn evaluate(facts: &SessionFacts, cfg: &DevtimeRulesConfig) -> RuleOutput {
    evaluate_with(facts, cfg, FAMILIES)
}

fn evaluate_with(
    _facts: &SessionFacts,
    _cfg: &DevtimeRulesConfig,
    _families: &[(char, FamilyFn)],
) -> RuleOutput {
    RuleOutput::default()
}

/// What one rules pass did, for the caller's log. No health row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RulesPassStats {
    pub sessions_ruled: usize,
    pub sessions_failed: usize,
    pub families_panicked: usize,
}

/// The per-cycle rules pass over the sessions this cycle touched plus a capped backlog of stale ones,
/// skipping `skip` (sessions whose lane rebuild failed). Stub: does nothing.
pub async fn run_rules_pass(
    _pool: &SqlitePool,
    _cfg: &DevtimeRulesConfig,
    _sources: AdapterSources,
    _touched: &BTreeSet<String>,
    _skip: &BTreeSet<String>,
) -> RulesPassStats {
    RulesPassStats::default()
}

/// The first 16 hex chars of the sha256 of `text`.
pub(crate) fn sha16(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fingerprint_of(cfg: &DevtimeRulesConfig, rules: &[Rule]) -> String {
    let mut text = format!("{ENGINE_VERSION};{PARSER_VERSION};");
    for rule in rules {
        text.push_str(&format!("{}@{};", rule.id, rule.version));
    }
    text.push_str(&serde_json::to_string(cfg).unwrap_or_default());
    sha16(&text)
}

/// A stamp of everything that decides what the rules conclude: the engine, the parser, every rule's
/// version and the rules configuration. A session ruled under another stamp is stale, which is how a
/// changed rule recomputes instead of mixing eras. The configuration serialises deterministically
/// (the structs derive `Serialize` and keep their maps in `BTreeMap`).
pub fn rules_fingerprint(cfg: &DevtimeRulesConfig) -> String {
    fingerprint_of(cfg, RULES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_store::{LEVERS, WASTES};

    const EXPECTED_IDS: [&str; 37] = [
        "A1", "A2", "A3", "A4", "A5", "A6", "A7", "A8", "B1", "B2", "B3", "B4", "B5", "B6", "B7",
        "C1", "C2", "C3", "C4", "D1", "D2", "D3", "D4", "D5", "D6", "D7", "D8", "D9", "D10", "D11",
        "D12", "D13", "D14", "E1", "E2", "F1", "F2",
    ];

    #[test]
    fn registry_is_complete_and_well_formed() {
        assert_eq!(RULES.len(), EXPECTED_IDS.len());
        for id in EXPECTED_IDS {
            assert_eq!(
                RULES.iter().filter(|rule| rule.id == id).count(),
                1,
                "{id} must be registered exactly once"
            );
        }
        for rule in RULES {
            assert!(!rule.levers.is_empty(), "{} has no lever", rule.id);
            for lever in rule.levers {
                assert!(LEVERS.contains(lever), "{}: unknown lever {lever}", rule.id);
            }
            assert!(
                WASTES.contains(&rule.waste.as_str()),
                "{}: unknown waste",
                rule.id
            );
            assert_eq!(
                Some(rule.family),
                rule.id.chars().next(),
                "{}: the family letter is the id's first letter",
                rule.id
            );
            assert!(rule.version >= 1, "{}", rule.id);
            assert_eq!(
                rule.hypothesis,
                rule.id == "D12",
                "only D12 is a hypothesis: {}",
                rule.id
            );
        }
        for id in ["C4", "D11", "E1"] {
            assert_eq!(rule(id).unwrap().level, Level::Deferred, "{id}");
        }
        let e2 = rule("E2").unwrap();
        assert_eq!(e2.level, Level::Adapter);
        assert_eq!(e2.source, Some(Source::PermissionHook));
        assert!(
            RULES
                .iter()
                .filter(|rule| rule.id != "E2")
                .all(|rule| rule.source.is_none()),
            "only E2 names a source"
        );
        assert!(rule("D12").unwrap().hypothesis);
        assert!(rule("Z9").is_none());
        // The families the registry names are the families the engine dispatches to.
        for rule in RULES.iter().filter(|rule| rule.level == Level::Base) {
            assert!(
                rule.family == 'F' || FAMILIES.iter().any(|(family, _)| *family == rule.family),
                "{}: no family function for {}",
                rule.id,
                rule.family
            );
        }
    }

    #[test]
    fn fingerprint_changes_with_rule_version_and_config_and_is_stable() {
        let cfg = DevtimeRulesConfig::default();
        let base = rules_fingerprint(&cfg);
        assert_eq!(base.len(), 16);
        assert_eq!(base, rules_fingerprint(&cfg), "stable across calls");
        assert_eq!(
            base,
            rules_fingerprint(&cfg.clone()),
            "stable across clones"
        );

        let mut changed = cfg.clone();
        changed.thresholds.d3_search_burst += 1;
        assert_ne!(base, rules_fingerprint(&changed), "a threshold changes it");
        let mut changed = cfg.clone();
        changed.precision.priors.insert("A1".to_string(), 0.9);
        assert_ne!(base, rules_fingerprint(&changed), "a map entry changes it");

        let mut bumped = RULES.to_vec();
        bumped[0].version += 1;
        assert_eq!(base, fingerprint_of(&cfg, RULES));
        assert_ne!(
            base,
            fingerprint_of(&cfg, &bumped),
            "a rule version changes it"
        );
    }

    #[test]
    fn adapter_sources_need_a_configured_path_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("permissions.log");
        std::fs::write(&log, "x").unwrap();
        let cfg = DevtimeAdaptersConfig {
            permission_log: log.to_string_lossy().into_owned(),
            heavy_log: dir.path().join("absent.log").to_string_lossy().into_owned(),
        };
        let sources = AdapterSources::detect(&cfg);
        assert!(sources.permission_hook);
        assert!(!sources.heavy_log, "a path that does not exist is absent");
        assert_eq!(
            AdapterSources::detect(&DevtimeAdaptersConfig::default()),
            AdapterSources::default(),
            "an empty path is absent"
        );
    }
}
