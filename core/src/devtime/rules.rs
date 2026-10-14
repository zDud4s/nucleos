//! The devtime rule engine (sub-project 2): the registry of rules as data, the per-session facts the
//! rule families read, family dispatch with panic isolation, the projection of findings onto spans
//! and attempts, the `rules_version` fingerprint, and the per-cycle rules pass.
//!
//! Owns no SQL: `devtime_store` reads and writes every row. Rules never create, split or delete a
//! span; they only annotate spans the lane builder already made. Thresholds, programs and vocabularies
//! live in `config::DevtimeRulesConfig`, never in rule code.
//!
//! The types and the registry below are the contract every rule family is written against. Below
//! them: `SessionFacts::build` (the stored rows parsed once into facts), the shared helpers the
//! families use instead of re-implementing them, `evaluate` (family dispatch with panic isolation,
//! the level filter, dedup and verdict merging), `project` (findings onto span and attempt
//! annotations) and `run_rules_pass` (the per-cycle driver, one failure never stops the next session).

pub mod a;
pub mod b;
pub mod c;
pub mod cmd;
pub mod dctx;
pub mod dflow;
pub mod f;
#[cfg(test)]
pub mod fixture;

mod facts;
mod pass;
mod project;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::config::{DevtimeAdaptersConfig, DevtimeRulesConfig};
use crate::devtime::parse::PARSER_VERSION;
use crate::devtime::rules::cmd::{CmdClass, CommandSet, PathMatcher};
use crate::devtime::rules::f::CrossFinding;
use crate::devtime::store::{
    AttemptMarkRow, AttemptRow, FindingRow, RuleWrite, SessionHead, SessionRows, SpanMark, SpanRow,
};
use crate::{
    devtime::rules::a as devtime_rules_a, devtime::rules::b as devtime_rules_b,
    devtime::rules::c as devtime_rules_c, devtime::rules::cmd as devtime_rules_cmd,
    devtime::rules::dctx as devtime_rules_dctx, devtime::rules::dflow as devtime_rules_dflow,
    devtime::rules::f as devtime_rules_f, devtime::store as devtime_store,
    devtime::unexplained as devtime_unexplained,
};

pub use self::facts::*;
pub(crate) use self::facts::{format_ms, parse_ms};
pub use self::pass::*;
pub use self::project::*;

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

// --- shared helpers: families use these and never re-implement them -------------------------------

/// Attempts (any lane) that changed the working tree and succeeded (`is_edit` or `mutating`, outcome
/// Ok), strictly between `from_ms` and `to_ms` by start.
#[allow(dead_code)] // the rule families are the callers
pub fn edits_between<'a>(
    facts: &'a SessionFacts,
    from_ms: i64,
    to_ms: i64,
) -> impl Iterator<Item = &'a AttemptFact> + 'a {
    facts.attempts.iter().filter(move |a| {
        (a.is_edit || a.mutating)
            && a.outcome == Outcome::Ok
            && from_ms < a.started_ms
            && a.started_ms < to_ms
    })
}

/// Whether one of [`edits_between`] touched `path` (in its edits or its written files).
#[allow(dead_code)] // the rule families are the callers
pub fn path_edited_between(facts: &SessionFacts, path: &str, from_ms: i64, to_ms: i64) -> bool {
    edits_between(facts, from_ms, to_ms).any(|a| {
        a.edits.iter().any(|edit| edit.path == path) || a.files.iter().any(|file| file == path)
    })
}

/// A failed attempt that is a plain failure: Error with class `exit_nonzero`, `tool_error` or none.
/// Timeouts, exit 75, hook blocks, permission denials, wrong shell and interrupts are B/C territory.
#[allow(dead_code)] // the rule families are the callers
pub fn a_fail(a: &AttemptFact) -> bool {
    a.outcome == Outcome::Error
        && a.error_class
            .as_deref()
            .is_none_or(|class| class == "exit_nonzero" || class == "tool_error")
}

/// An attempt that ran and succeeded: Ok with exit code 0 or none.
#[allow(dead_code)] // the rule families are the callers
pub fn passed(a: &AttemptFact) -> bool {
    a.outcome == Outcome::Ok && matches!(a.exit_code, None | Some(0))
}

/// The attempts of one lane, in order.
#[allow(dead_code)] // the rule families are the callers
pub fn lane_attempts<'a>(
    facts: &'a SessionFacts,
    lane: &'a str,
) -> impl Iterator<Item = &'a AttemptFact> + 'a {
    facts.attempts.iter().filter(move |a| a.lane == lane)
}

/// The first attempt after position `idx` in the same lane that satisfies `pred`.
#[allow(dead_code)] // the rule families are the callers
pub fn next_in_lane(
    facts: &SessionFacts,
    idx: usize,
    pred: impl Fn(&AttemptFact) -> bool,
) -> Option<&AttemptFact> {
    let from = facts.attempts.get(idx)?;
    facts
        .attempts
        .iter()
        .skip(idx + 1)
        .find(|a| a.lane == from.lane && pred(a))
}

/// How long an attempt took: `done_ms - started_ms`, never negative.
#[allow(dead_code)] // the rule families are the callers
pub fn duration(a: &AttemptFact) -> i64 {
    (a.done_ms - a.started_ms).max(0)
}

// --- keys and rows ---------------------------------------------------------------------------------

/// `sha16("{rule_id}|{scope}|{anchor}")`, where `scope` is the session id (a session finding) or the
/// project id (a cross-session one).
pub(crate) fn key_for(rule_id: &str, scope: &str, anchor: &str) -> String {
    sha16(&format!("{rule_id}|{scope}|{anchor}"))
}

/// A session finding's stable key: its rule, its session and its anchor (the first attempt, or the
/// start time when it names none). The same data recomputed gives the same key, and feedback keys on it.
pub fn finding_key(session_id: &str, finding: &Finding) -> String {
    let anchor = finding
        .attempt_ids
        .first()
        .cloned()
        .unwrap_or_else(|| finding.started_ms.to_string());
    key_for(finding.rule_id, session_id, &anchor)
}

fn json_array(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

/// The `devtime_findings` rows of one session's findings, stamped with `fingerprint`.
pub(crate) fn finding_rows(
    facts: &SessionFacts,
    findings: &[Finding],
    fingerprint: &str,
) -> Vec<FindingRow> {
    findings
        .iter()
        .filter_map(|finding| {
            let registered = rule(finding.rule_id)?;
            Some(FindingRow {
                finding_key: finding_key(&facts.session_id, finding),
                project_id: facts.project_id.clone(),
                session_id: facts.session_id.clone(),
                scope: "session".to_string(),
                rule_id: registered.id.to_string(),
                rule_version: i64::from(registered.version),
                level: registered.level.as_str().to_string(),
                waste: registered.waste.as_str().to_string(),
                lever: registered.levers[0].to_string(),
                confidence: finding.confidence.as_str().to_string(),
                lane: finding.lane.clone(),
                started_at: format_ms(finding.started_ms),
                ended_at: format_ms(finding.ended_ms),
                cost_ms: finding.cost_ms,
                count: finding.count,
                attempt_ids: json_array(&finding.attempt_ids),
                sessions: "[]".to_string(),
                rules_version: fingerprint.to_string(),
                parser_version: PARSER_VERSION,
            })
        })
        .collect()
}

/// The `devtime_findings` rows of a project's cross-session findings, keyed by rule, project and
/// anchor, one per key.
pub(crate) fn cross_rows(
    project_id: &str,
    found: &[CrossFinding],
    fingerprint: &str,
) -> Vec<FindingRow> {
    let mut seen = HashSet::new();
    found
        .iter()
        .filter_map(|item| {
            let registered = rule(item.rule_id)?;
            let key = key_for(registered.id, project_id, &item.anchor);
            if !seen.insert(key.clone()) {
                return None;
            }
            Some(FindingRow {
                finding_key: key,
                project_id: project_id.to_string(),
                session_id: item.latest_session.clone(),
                scope: "cross".to_string(),
                rule_id: registered.id.to_string(),
                rule_version: i64::from(registered.version),
                level: registered.level.as_str().to_string(),
                waste: registered.waste.as_str().to_string(),
                lever: registered.levers[0].to_string(),
                confidence: Confidence::Exact.as_str().to_string(),
                lane: "main".to_string(),
                started_at: format_ms(item.started_ms),
                ended_at: format_ms(item.ended_ms),
                cost_ms: item.cost_ms,
                count: item.count,
                attempt_ids: json_array(&item.attempt_ids),
                sessions: json_array(&item.sessions),
                rules_version: fingerprint.to_string(),
                parser_version: PARSER_VERSION,
            })
        })
        .collect()
}

// --- evaluation --------------------------------------------------------------------------------------

/// Whether an adapter rule's source is present for this session. `HeavyLog` also needs a
/// `wait_machine` span, since without one there is nothing for the log to explain.
fn source_present(source: Source, facts: &SessionFacts) -> bool {
    match source {
        Source::PermissionHook => facts.sources.permission_hook,
        Source::HeavyLog => facts.sources.heavy_log && facts.has_wait_machine,
    }
}

/// Whether a rule's findings are kept: base rules always, adapter rules when their source is present,
/// deferred and unknown ids never.
fn rule_active(id: &str, facts: &SessionFacts) -> bool {
    let Some(registered) = rule(id) else {
        return false;
    };
    match registered.level {
        Level::Base => true,
        Level::Deferred => false,
        Level::Adapter => registered
            .source
            .is_some_and(|source| source_present(source, facts)),
    }
}

/// One verdict per attempt: `Failed` beats `Passed`, which beats `NotMeasured`; between equals the
/// first stands. The order is that of each attempt's first verdict.
fn merge_verdicts(verdicts: Vec<Verdict>) -> Vec<Verdict> {
    fn rank(verified: Verified) -> u8 {
        match verified {
            Verified::Failed => 2,
            Verified::Passed => 1,
            Verified::NotMeasured => 0,
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut best: HashMap<String, Verdict> = HashMap::new();
    for verdict in verdicts {
        let current = best.get(&verdict.attempt_id).map(|v| rank(v.verified));
        match current {
            None => {
                order.push(verdict.attempt_id.clone());
                best.insert(verdict.attempt_id.clone(), verdict);
            }
            Some(held) if rank(verdict.verified) > held => {
                best.insert(verdict.attempt_id.clone(), verdict);
            }
            Some(_) => {}
        }
    }
    order
        .into_iter()
        .filter_map(|id| best.remove(&id))
        .collect()
}

/// Runs every family over one session: panic isolation per family, the level filter, dedup by
/// `finding_key` (the first stands) and verdicts merged per attempt.
#[allow(dead_code)] // reached through `run_rules_pass`, which names the injectable form
pub fn evaluate(facts: &SessionFacts, cfg: &DevtimeRulesConfig) -> RuleOutput {
    evaluate_with(facts, cfg, FAMILIES)
}

fn evaluate_with(
    facts: &SessionFacts,
    cfg: &DevtimeRulesConfig,
    families: &[(char, FamilyFn)],
) -> RuleOutput {
    evaluate_counted(facts, cfg, families).0
}

/// [`evaluate_with`], and how many families panicked. A panic is logged and the family contributes
/// nothing; the others still run (spec §8: the ingest loop never dies).
fn evaluate_counted(
    facts: &SessionFacts,
    cfg: &DevtimeRulesConfig,
    families: &[(char, FamilyFn)],
) -> (RuleOutput, usize) {
    let mut panicked = 0;
    let mut raw_findings: Vec<Finding> = Vec::new();
    let mut raw_verdicts: Vec<Verdict> = Vec::new();
    for (family, run) in families {
        match catch_unwind(AssertUnwindSafe(|| run(facts, cfg))) {
            Ok(output) => {
                raw_findings.extend(output.findings);
                raw_verdicts.extend(output.verdicts);
            }
            Err(_) => {
                panicked += 1;
                tracing::warn!(
                    session = %facts.session_id,
                    family = %family,
                    "devtime: a rule family panicked"
                );
            }
        }
    }
    let mut keys = HashSet::new();
    let findings: Vec<Finding> = raw_findings
        .into_iter()
        .filter(|finding| rule_active(finding.rule_id, facts))
        .filter(|finding| keys.insert(finding_key(&facts.session_id, finding)))
        .collect();
    let verdicts = merge_verdicts(
        raw_verdicts
            .into_iter()
            .filter(|verdict| rule_active(verdict.rule_id, facts))
            .collect(),
    );
    (RuleOutput { findings, verdicts }, panicked)
}

/// A technical constant: milliseconds in a day.
const MS_PER_DAY: i64 = 86_400_000;

/// The rules whose findings span sessions (family F); they are replaced wholesale per project.
const CROSS_RULE_IDS: &[&str] = &["F1", "F2"];

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
    use crate::devtime::store::{LEVERS, WASTES};

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

    // --- the engine ------------------------------------------------------------------------------

    use crate::devtime::rules::fixture::{Script, at, attempt, script, test_pool};

    fn finding(
        rule_id: &'static str,
        lane: &str,
        from: i64,
        to: i64,
        attempts: &[&str],
        policy: SpanPolicy,
    ) -> Finding {
        Finding {
            rule_id,
            lane: lane.to_string(),
            started_ms: from,
            ended_ms: to,
            cost_ms: to - from,
            attempt_ids: attempts.iter().map(|id| id.to_string()).collect(),
            confidence: Confidence::Exact,
            count: 1,
            policy,
        }
    }

    /// A failing test run, then a second run.
    fn two_runs() -> Script {
        script(
            "
            turn main 0
            bash main 2+3 prog=\"cargo test\" out=error exit=1 aid=a1
            bash main 6+2 prog=\"cargo test\" aid=a2
            ",
        )
    }

    /// A finding on the session's first attempt, for each rule id it is asked for.
    fn on_first_attempt(facts: &SessionFacts, ids: &[&'static str]) -> RuleOutput {
        let first = &facts.attempts[0];
        RuleOutput {
            findings: ids
                .iter()
                .map(|id| {
                    finding(
                        id,
                        "main",
                        first.started_ms,
                        first.done_ms,
                        &[first.attempt_id.as_str()],
                        SpanPolicy::Attempts,
                    )
                })
                .collect(),
            verdicts: Vec::new(),
        }
    }

    fn rule_ids(output: &RuleOutput) -> Vec<&'static str> {
        output.findings.iter().map(|f| f.rule_id).collect()
    }

    #[test]
    fn facts_effective_outcome_for_background_uses_bg_status() {
        let facts = script(
            "
            turn main 0
            bash main 1+0 bg=1 bg_status=completed bg_end=10 aid=done
            bash main 2+0 bg=1 bg_status=completed exit=3 aid=badexit
            bash main 3+0 bg=1 bg_status=failed aid=failed
            bash main 4+0 bg=1 bg_status=killed aid=killed
            bash main 5+0 bg=1 aid=silent
            bash main 6+1 aid=foreground
            bash main 8+1 bg=1 out=error aid=refused
            ",
        )
        .facts(&DevtimeRulesConfig::default());
        let outcome = |aid: &str| attempt(&facts, aid).outcome;
        assert_eq!(outcome("done"), Outcome::Ok);
        assert_eq!(attempt(&facts, "done").done_ms, at(10), "bg_ended_at");
        assert_eq!(
            outcome("badexit"),
            Outcome::Error,
            "completed with a non-zero exit"
        );
        assert_eq!(outcome("failed"), Outcome::Error);
        assert_eq!(outcome("killed"), Outcome::Interrupted);
        assert_eq!(outcome("silent"), Outcome::Unknown, "no notification yet");
        assert_eq!(outcome("foreground"), Outcome::Ok);
        assert_eq!(
            outcome("refused"),
            Outcome::Error,
            "a launch that itself failed stays an error"
        );
        assert_eq!(
            attempt(&facts, "silent").done_ms,
            at(5),
            "no end recorded: its start"
        );
        assert_eq!(
            attempt(&facts, "killed").bg_status.as_deref(),
            Some("killed")
        );
    }

    #[test]
    fn facts_drop_rows_with_a_bad_timestamp_and_never_panic() {
        let parsed = two_runs();
        let mut rows = parsed.rows();
        rows.attempts[1].started_at = "not a time".to_string();
        rows.turns[0].started_at = "also not".to_string();
        let mut spans = parsed.spans_default();
        spans[0].ended_at = "nope".to_string();
        let kept_spans = spans.len() - 1;
        let facts = SessionFacts::build(
            &parsed.head(),
            &rows,
            &spans,
            &DevtimeRulesConfig::default(),
            AdapterSources::default(),
        );
        assert_eq!(facts.attempts.len(), 1);
        assert!(facts.turns.is_empty());
        assert_eq!(facts.spans.len(), kept_spans);
        assert_eq!(facts.attempts[0].turn_seq, None, "no turn to belong to");
    }

    #[test]
    fn helpers_read_the_facts_as_the_plan_says() {
        let facts = script(
            "
            turn main 0
            bash main 1+1 prog=\"cargo test\" out=error exit=1 aid=red
            edit main 3+1 path=a.rs aid=ed
            bash main 5+1 prog=sed aid=mut
            bash main 7+1 prog=\"cargo test\" out=error err=timeout aid=slow
            bash main 9+1 prog=\"cargo test\" aid=green
            ",
        )
        .facts(&DevtimeRulesConfig::default());
        let red = attempt(&facts, "red");
        assert!(a_fail(red) && !passed(red));
        assert!(
            !a_fail(attempt(&facts, "slow")),
            "a timeout is not a plain failure"
        );
        assert!(passed(attempt(&facts, "green")));
        assert!(
            attempt(&facts, "mut").mutating,
            "sed succeeded: it counts as an edit"
        );

        let between = |from: i64, to: i64| -> Vec<String> {
            edits_between(&facts, at(from), at(to))
                .map(|a| a.attempt_id.clone())
                .collect()
        };
        assert_eq!(between(2, 8), ["ed", "mut"]);
        assert!(between(3, 5).is_empty(), "both bounds are exclusive");
        assert!(path_edited_between(&facts, "a.rs", at(2), at(4)));
        assert!(!path_edited_between(&facts, "b.rs", at(2), at(4)));
        assert!(!path_edited_between(&facts, "a.rs", at(4), at(9)));

        assert_eq!(lane_attempts(&facts, "main").count(), 5);
        assert_eq!(lane_attempts(&facts, "agent:x").count(), 0);
        let next_green = next_in_lane(&facts, red.idx, |a| a.cmd_class.is_some() && passed(a));
        assert_eq!(next_green.map(|a| a.attempt_id.as_str()), Some("green"));
        assert!(next_in_lane(&facts, 99, |_| true).is_none());
        assert_eq!(duration(red), 1000);
    }

    #[test]
    fn finding_key_is_stable_and_anchored_on_the_first_attempt() {
        let f = finding("A1", "main", 10, 20, &["a1", "a2"], SpanPolicy::Attempts);
        assert_eq!(finding_key("s1", &f), key_for("A1", "s1", "a1"));
        assert_eq!(finding_key("s1", &f), finding_key("s1", &f));
        assert_ne!(finding_key("s1", &f), finding_key("s2", &f));
        let bare = finding("A1", "main", 10, 20, &[], SpanPolicy::Signal);
        assert_eq!(finding_key("s1", &bare), key_for("A1", "s1", "10"));
        assert_eq!(finding_key("s1", &f).len(), 16);
    }

    fn mixed(facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
        on_first_attempt(facts, &["B1", "C4", "E2", "Z9"])
    }

    #[test]
    fn deferred_and_sourceless_adapter_findings_are_dropped() {
        let cfg = DevtimeRulesConfig::default();
        let parsed = two_runs();
        let families: &[(char, FamilyFn)] = &[('X', mixed as FamilyFn)];

        let bare = parsed.facts(&cfg);
        assert_eq!(
            rule_ids(&evaluate_with(&bare, &cfg, families)),
            ["B1"],
            "C4 is deferred, E2 has no source and Z9 is not a rule"
        );

        let hooked = parsed.facts_with(
            &cfg,
            AdapterSources {
                permission_hook: true,
                heavy_log: false,
            },
        );
        assert_eq!(
            rule_ids(&evaluate_with(&hooked, &cfg, families)),
            ["B1", "E2"],
            "E2 is kept once the permission hook is present"
        );

        // A heavy-log source also needs a wait_machine span to have anything to explain.
        assert!(!source_present(Source::HeavyLog, &bare));
        let mut with_log = bare.clone();
        with_log.sources.heavy_log = true;
        assert!(!source_present(Source::HeavyLog, &with_log));
        with_log.has_wait_machine = true;
        assert!(source_present(Source::HeavyLog, &with_log));
    }

    fn boom(_facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
        panic!("a rule family blew up (expected in this test)")
    }

    fn just_b1(facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
        on_first_attempt(facts, &["B1"])
    }

    #[test]
    fn a_panicking_family_is_isolated() {
        let cfg = DevtimeRulesConfig::default();
        let facts = two_runs().facts(&cfg);
        let families: &[(char, FamilyFn)] = &[('A', boom as FamilyFn), ('B', just_b1 as FamilyFn)];
        let output = evaluate_with(&facts, &cfg, families);
        assert_eq!(
            rule_ids(&output),
            ["B1"],
            "exactly the surviving family's finding"
        );
        assert_eq!(evaluate_counted(&facts, &cfg, families).1, 1);

        // The same finding returned twice is one finding: the key is the identity.
        let twice: &[(char, FamilyFn)] = &[('B', just_b1 as FamilyFn), ('B', just_b1 as FamilyFn)];
        assert_eq!(rule_ids(&evaluate_with(&facts, &cfg, twice)), ["B1"]);
    }

    fn span_at<'a>(facts: &'a SessionFacts, kind: &str, start: i64) -> &'a SpanFact {
        facts
            .spans
            .iter()
            .find(|s| s.kind == kind && s.started_ms == at(start))
            .unwrap_or_else(|| panic!("no {kind} span at {start}"))
    }

    #[test]
    fn projection_prefers_avoidable_then_registry_order_and_marks_unclaimed_work_useful() {
        let cfg = DevtimeRulesConfig::default();
        let facts = script(
            "
            turn main 0
            msg  main 1+1  id=m1
            bash main 2+3  msg=m1 aid=t1
            msg  main 5+1  id=m2
            bash main 6+4  msg=m2 aid=t2
            msg  main 10+1 id=m3 tools=0
            turn main 100
            msg  main 101+1 id=m4 tools=0
            turn main 2000
            msg  main 2001+1 id=m5 tools=0
            ",
        )
        .facts(&cfg);
        let span = |kind: &str, start: i64| span_at(&facts, kind, start);
        let t1 = span("tool", 2);
        let t2 = span("tool", 6);

        let findings = vec![
            // Rework, on t1's attempt.
            finding("A1", "main", at(2), at(5), &["t1"], SpanPolicy::Attempts),
            // Avoidable, over the whole first stretch: wins t1's span over A1's rework.
            finding("D7", "main", at(0), at(11), &["t2"], SpanPolicy::Interval),
            // Avoidable like D7 and earlier in the registry: wins t2's span.
            finding("B1", "main", at(6), at(10), &["t2"], SpanPolicy::Attempts),
            // A signal claims nothing, not even the attempt it names.
            finding("D1", "main", at(0), at(0), &["t1"], SpanPolicy::Signal),
        ];
        let (marks, attempt_marks) = project(&facts, &findings, &[]);
        let mark = |id: i64| marks.iter().find(|m| m.span_id == id);

        let on_t1 = mark(t1.id).unwrap();
        assert_eq!(
            on_t1.rule_id.as_deref(),
            Some("D7"),
            "avoidable beats rework"
        );
        assert_eq!(on_t1.waste.as_deref(), Some("avoidable"));
        assert_eq!(on_t1.lever.as_deref(), Some("verification"));
        assert_eq!(on_t1.finding_key, Some(finding_key("s1", &findings[1])));
        let on_t2 = mark(t2.id).unwrap();
        assert_eq!(
            on_t2.rule_id.as_deref(),
            Some("B1"),
            "registry order breaks the tie"
        );
        assert_eq!(on_t2.lever.as_deref(), Some("precision"));
        assert_eq!(
            mark(span("model", 0).id).unwrap().rule_id.as_deref(),
            Some("D7"),
            "an interval claims the spans inside it"
        );

        for (kind, start) in [("model", 100), ("model", 2000)] {
            let useful = mark(span(kind, start).id).unwrap();
            assert_eq!(useful.waste.as_deref(), Some("useful"), "{kind} at {start}");
            assert!(
                useful.rule_id.is_none() && useful.lever.is_none() && useful.finding_key.is_none()
            );
        }
        for (kind, start) in [("wait_human", 11), ("wait_human", 102), ("idle", 1002)] {
            assert!(
                mark(span(kind, start).id).is_none(),
                "{kind} at {start} stays NULL"
            );
        }
        assert_eq!(marks.len(), 7, "five claimed spans and two of useful work");

        // Attempts: one mark per attempt a non-signal finding names, by the same rule.
        assert_eq!(attempt_marks.len(), 2);
        let on = |id: &str| attempt_marks.iter().find(|m| m.attempt_id == id).unwrap();
        assert_eq!(
            on("t1").rule_id.as_deref(),
            Some("A1"),
            "the signal does not count"
        );
        assert_eq!(on("t1").waste.as_deref(), Some("rework"));
        assert_eq!(on("t2").rule_id.as_deref(), Some("B1"));
        assert_eq!(on("t2").verified, "not_measured");
        assert_eq!(on("t2").session_id, "s1");
    }

    fn verdicts(_facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
        let v = |attempt_id: &str, verified: Verified, rule_id: &'static str| Verdict {
            attempt_id: attempt_id.to_string(),
            verified,
            rule_id,
        };
        RuleOutput {
            findings: Vec::new(),
            verdicts: vec![
                v("a1", Verified::Passed, "A3"),
                v("a1", Verified::Failed, "A4"),
                v("a2", Verified::NotMeasured, "A3"),
                v("a2", Verified::Passed, "A4"),
                v("a2", Verified::NotMeasured, "A3"),
                v("a3", Verified::Failed, "C4"),
            ],
        }
    }

    #[test]
    fn verdicts_merge_failed_over_passed() {
        let cfg = DevtimeRulesConfig::default();
        let facts = script(
            "
            turn  main 0
            agent main 1+5  type=wf-executor id=g1 aid=a1
            agent main 7+5  type=wf-executor id=g2 aid=a2
            agent main 13+5 type=Explore id=g3 aid=a3
            bash  main 20+1 aid=b1
            ",
        )
        .facts(&cfg);
        let families: &[(char, FamilyFn)] = &[('A', verdicts as FamilyFn)];
        let output = evaluate_with(&facts, &cfg, families);
        let merged: Vec<(&str, Verified, &str)> = output
            .verdicts
            .iter()
            .map(|v| (v.attempt_id.as_str(), v.verified, v.rule_id))
            .collect();
        assert_eq!(
            merged,
            [
                ("a1", Verified::Failed, "A4"),
                ("a2", Verified::Passed, "A4")
            ],
            "failed beats passed beats not_measured; a deferred rule's verdict is dropped"
        );

        let (_, attempt_marks) = project(&facts, &output.findings, &output.verdicts);
        let verified = |id: &str| {
            let mark = attempt_marks.iter().find(|m| m.attempt_id == id).unwrap();
            (mark.verified.as_str(), mark.verified_by.as_deref())
        };
        assert_eq!(verified("a1"), ("failed", Some("A4")));
        assert_eq!(verified("a2"), ("passed", Some("A4")));
        assert_eq!(
            verified("a3"),
            ("not_measured", None),
            "an agent defaults to not measured"
        );
        assert!(
            attempt_marks.iter().all(|m| m.attempt_id != "b1"),
            "a plain call with nothing to say gets no row"
        );
    }

    fn first_test_failure(facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
        let findings = facts
            .attempts
            .iter()
            .filter(|a| a.cmd_class == Some(CmdClass::Test) && a_fail(a))
            .take(1)
            .map(|a| {
                finding(
                    "A1",
                    &a.lane,
                    a.started_ms,
                    a.done_ms,
                    &[a.attempt_id.as_str()],
                    SpanPolicy::Attempts,
                )
            })
            .collect();
        RuleOutput {
            findings,
            verdicts: Vec::new(),
        }
    }

    type Row6 = (
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    /// Everything a rules pass writes about session `s1`, as text, to compare two passes.
    async fn snapshot(pool: &SqlitePool) -> Vec<String> {
        let mut out = Vec::new();
        let findings: Vec<(String, String, String, String, String, i64)> = sqlx::query_as(
            "SELECT finding_key, rule_id, waste, lever, attempt_ids, cost_ms
             FROM devtime_findings WHERE session_id = 's1' ORDER BY finding_key",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        out.extend(findings.iter().map(|row| format!("finding {row:?}")));
        let spans: Vec<Row6> = sqlx::query_as(
            "SELECT id, kind, waste, rule_id, lever, finding_key
             FROM devtime_spans WHERE session_id = 's1' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        out.extend(spans.iter().map(|row| format!("span {row:?}")));
        let marks: Vec<(String, Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT attempt_id, waste, rule_id, verified
             FROM devtime_attempt_marks WHERE session_id = 's1' ORDER BY attempt_id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        out.extend(marks.iter().map(|row| format!("mark {row:?}")));
        out
    }

    async fn dirty_of(pool: &SqlitePool, session: &str) -> i64 {
        sqlx::query_scalar("SELECT dirty FROM devtime_sessions WHERE session_id = ?")
            .bind(session)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn stamp_of(pool: &SqlitePool, session: &str) -> Option<String> {
        sqlx::query_scalar("SELECT rules_version FROM devtime_sessions WHERE session_id = ?")
            .bind(session)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn pass(
        pool: &SqlitePool,
        cfg: &DevtimeRulesConfig,
        touched: &BTreeSet<String>,
        families: &[(char, FamilyFn)],
    ) -> RulesPassStats {
        run_rules_pass_with(
            pool,
            cfg,
            AdapterSources::default(),
            touched,
            &BTreeSet::new(),
            families,
        )
        .await
    }

    #[tokio::test]
    async fn rules_pass_is_idempotent_and_recomputes_on_fingerprint_change() {
        let pool = test_pool().await;
        let cfg = DevtimeRulesConfig::default();
        two_runs().persist(&pool, "p1").await.unwrap();
        let families: &[(char, FamilyFn)] = &[('A', first_test_failure as FamilyFn)];
        let touched: BTreeSet<String> = ["s1".to_string()].into_iter().collect();
        let none = BTreeSet::new();

        assert_eq!(dirty_of(&pool, "s1").await, 1, "ingested, not yet ruled");
        let first_run = pass(&pool, &cfg, &touched, families).await;
        assert_eq!(
            first_run,
            RulesPassStats {
                sessions_ruled: 1,
                sessions_failed: 0,
                families_panicked: 0
            }
        );
        let first = snapshot(&pool).await;
        assert_eq!(
            first
                .iter()
                .filter(|line| line.starts_with("finding"))
                .count(),
            1,
            "{first:?}"
        );
        assert!(
            first
                .iter()
                .any(|line| line.starts_with("span") && line.contains("\"A1\"")),
            "the finding annotated a span: {first:?}"
        );
        assert!(
            first.iter().any(|line| line.starts_with("mark")),
            "{first:?}"
        );
        assert_eq!(dirty_of(&pool, "s1").await, 0);
        assert_eq!(stamp_of(&pool, "s1").await, Some(rules_fingerprint(&cfg)));

        // Touched again: ruled again, and the result is the same rows.
        assert_eq!(
            pass(&pool, &cfg, &touched, families).await.sessions_ruled,
            1
        );
        assert_eq!(snapshot(&pool).await, first, "idempotent");
        assert_eq!(dirty_of(&pool, "s1").await, 0);

        // Nothing touched and nothing stale: the pass has no work.
        assert_eq!(pass(&pool, &cfg, &none, families).await.sessions_ruled, 0);

        // A changed threshold changes the fingerprint, so the session is owed work again.
        let mut changed = cfg.clone();
        changed.thresholds.d3_search_burst += 1;
        let redone = pass(&pool, &changed, &none, families).await;
        assert_eq!(redone.sessions_ruled, 1, "selected again by the backlog");
        assert_eq!(
            stamp_of(&pool, "s1").await,
            Some(rules_fingerprint(&changed))
        );
        assert_eq!(
            pass(&pool, &changed, &none, families).await.sessions_ruled,
            0
        );
    }

    #[tokio::test]
    async fn one_failing_session_does_not_stop_the_pass() {
        let pool = test_pool().await;
        let cfg = DevtimeRulesConfig::default();
        two_runs().persist(&pool, "p1").await.unwrap();
        script(
            "
            turn main 0
            bash main 2+3 prog=\"cargo test\" out=error exit=1 aid=x1
            ",
        )
        .session("s2")
        .persist(&pool, "p1")
        .await
        .unwrap();
        // One stored span has a timestamp nobody can read: the facts drop it and the session is ruled.
        sqlx::query(
            "UPDATE devtime_spans SET started_at = 'garbage'
             WHERE session_id = 's2'
               AND id = (SELECT MIN(id) FROM devtime_spans WHERE session_id = 's2')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let families: &[(char, FamilyFn)] = &[('A', first_test_failure as FamilyFn)];
        // `ghost` is touched but has no session row at all: it fails, and the ones after it go on.
        let touched: BTreeSet<String> = ["ghost", "s1", "s2"]
            .iter()
            .map(|id| id.to_string())
            .collect();
        let stats = run_rules_pass_with(
            &pool,
            &cfg,
            AdapterSources::default(),
            &touched,
            &BTreeSet::new(),
            families,
        )
        .await;
        assert_eq!(stats.sessions_ruled, 2);
        assert_eq!(stats.sessions_failed, 1);
        let fp = rules_fingerprint(&cfg);
        assert_eq!(stamp_of(&pool, "s1").await, Some(fp.clone()));
        assert_eq!(stamp_of(&pool, "s2").await, Some(fp));
        let found: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devtime_findings WHERE session_id IN ('s1', 's2')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(found, 2, "both sessions got their finding");

        // A skipped session (its lane rebuild failed) is not ruled, touched or not.
        let skip: BTreeSet<String> = ["s1".to_string()].into_iter().collect();
        let touched: BTreeSet<String> = ["s1".to_string()].into_iter().collect();
        let skipped = run_rules_pass_with(
            &pool,
            &cfg,
            AdapterSources::default(),
            &touched,
            &skip,
            families,
        )
        .await;
        assert_eq!(skipped, RulesPassStats::default());
    }
}
