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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::config::{DevtimeAdaptersConfig, DevtimeRulesConfig};
use crate::devtime_parse::PARSER_VERSION;
use crate::devtime_rules_cmd::{CmdClass, CommandSet, PathMatcher};
use crate::devtime_rules_f::CrossFinding;
use crate::devtime_store::{
    AttemptMarkRow, AttemptRow, FindingRow, RuleWrite, SessionHead, SessionRows, SpanMark, SpanRow,
};
use crate::{
    devtime_rules_a, devtime_rules_b, devtime_rules_c, devtime_rules_cmd, devtime_rules_dctx,
    devtime_rules_dflow, devtime_rules_f, devtime_store, devtime_unexplained,
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

/// An RFC 3339 timestamp as epoch milliseconds; `None` when it does not parse.
pub(crate) fn parse_ms(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|moment| moment.timestamp_millis())
}

/// Epoch milliseconds as the stored UTC text, `%Y-%m-%dT%H:%M:%S%.3fZ`.
pub(crate) fn format_ms(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn json_strings(text: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(text).unwrap_or_default()
}

fn json_objects(text: &str) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(text).unwrap_or_default()
}

fn outcome_of(text: &str) -> Outcome {
    match text {
        "ok" => Outcome::Ok,
        "error" => Outcome::Error,
        "interrupted" => Outcome::Interrupted,
        "launched" => Outcome::Launched,
        _ => Outcome::Unknown,
    }
}

/// How an attempt really ended. A foreground attempt's stored outcome stands. A background one is
/// judged by its notification: `completed` with exit 0 or none is Ok, `completed` with another exit and
/// `failed` are Error, `killed` is Interrupted, and without a notification it is Unknown (unless the
/// launch itself already failed or was interrupted).
fn effective_outcome(row: &AttemptRow) -> Outcome {
    let stored = outcome_of(&row.outcome);
    if row.background == 0 {
        return stored;
    }
    match row.bg_status.as_deref() {
        Some("completed") => {
            if matches!(row.exit_code, None | Some(0)) {
                Outcome::Ok
            } else {
                Outcome::Error
            }
        }
        Some("failed") => Outcome::Error,
        Some("killed") => Outcome::Interrupted,
        _ => {
            if matches!(stored, Outcome::Error | Outcome::Interrupted) {
                stored
            } else {
                Outcome::Unknown
            }
        }
    }
}

/// One stored attempt as a fact, `None` when its start does not parse. `idx` is set by the caller
/// once the attempts are sorted.
fn attempt_fact(
    row: &AttemptRow,
    cmds: &CommandSet,
    cfg: &DevtimeRulesConfig,
    turns_by_time: &[(i64, i64)],
) -> Option<AttemptFact> {
    let started_ms = parse_ms(&row.started_at)?;
    let ended_ms = row.ended_at.as_deref().and_then(parse_ms);
    let background = row.background != 0;
    let bg_end = row.bg_ended_at.as_deref().and_then(parse_ms);
    let finished = if background {
        bg_end.or(ended_ms)
    } else {
        ended_ms
    };
    let done_ms = finished.unwrap_or(started_ms).max(started_ms);
    let outcome = effective_outcome(row);
    let program = row.cmd_program.as_deref();
    let is_shell = cmds.shell_tools.contains(&row.tool_name);
    let in_list = |list: &Vec<String>| {
        is_shell
            && program.is_some_and(|p| devtime_rules_cmd::matches_any(list, p, &cmds.interpreters))
    };
    let cmd_class = if is_shell {
        program.and_then(|p| devtime_rules_cmd::classify(cmds, p))
    } else {
        None
    };
    let turn_seq = turns_by_time
        .partition_point(|(start, _)| *start <= started_ms)
        .checked_sub(1)
        .map(|at| turns_by_time[at].1);
    Some(AttemptFact {
        idx: 0,
        attempt_id: row.attempt_id.clone(),
        lane: row.lane.clone(),
        is_main: row.lane == "main",
        message_id: row.message_id.clone(),
        turn_seq,
        kind: row.kind.clone(),
        tool_name: row.tool_name.clone(),
        agent_type: row.agent_type.clone(),
        agent_id: row.agent_id.clone(),
        role: row
            .agent_type
            .as_deref()
            .and_then(|agent_type| devtime_rules_cmd::role_of(agent_type, &cfg.roles)),
        model: row.model.clone(),
        effort: row.effort.clone(),
        started_ms,
        ended_ms,
        done_ms,
        outcome,
        exit_code: row.exit_code,
        error_class: row.error_class.clone(),
        cmd_program: row.cmd_program.clone(),
        cmd_hash: row.cmd_hash.clone(),
        cmd_class,
        is_shell,
        is_edit: cmds.edit_tools.contains(&row.tool_name),
        is_read: cmds.read_tools.contains(&row.tool_name),
        is_search: cmds.search_tools.contains(&row.tool_name),
        mutating: outcome == Outcome::Ok && in_list(&cmds.mutating),
        is_sleep: in_list(&cmds.sleep),
        is_commit: in_list(&cmds.commit),
        timeout_ms: row.timeout_ms,
        background,
        bg_status: row.bg_status.clone(),
        files: json_strings(&row.files),
        edits: json_objects(&row.edits)
            .iter()
            .filter_map(|edit| {
                let text = |key: &str| edit.get(key).and_then(Value::as_str).unwrap_or("");
                Some(EditFact {
                    path: edit.get("path")?.as_str()?.to_string(),
                    before: text("before").to_string(),
                    after: text("after").to_string(),
                })
            })
            .collect(),
        reads: json_objects(&row.reads)
            .iter()
            .filter_map(|read| {
                Some(ReadFact {
                    path: read.get("path")?.as_str()?.to_string(),
                    offset: read.get("offset").and_then(Value::as_i64).unwrap_or(0),
                })
            })
            .collect(),
        refs_in: json_strings(&row.refs_in),
        refs_out: json_strings(&row.refs_out),
        refs_known: row.parser_version >= 2,
    })
}

impl SessionFacts {
    /// Parses a session's stored rows into facts, once. A row whose timestamp does not parse is
    /// dropped, never a panic. Attempts are sorted `(started_ms, attempt_id)` and numbered, spans
    /// `(lane, started_ms, id)`, messages `(lane, first_ms, message_id)`, markers by time, and turns by
    /// `seq`. The command set is resolved for the session's project.
    pub fn build(
        head: &SessionHead,
        rows: &SessionRows,
        spans: &[SpanRow],
        cfg: &DevtimeRulesConfig,
        sources: AdapterSources,
    ) -> SessionFacts {
        let cmds = CommandSet::for_project(cfg, &head.project_id);

        let mut turns: Vec<TurnFact> = rows
            .turns
            .iter()
            .filter_map(|turn| {
                let started_ms = parse_ms(&turn.started_at)?;
                let ended_ms = parse_ms(&turn.ended_at)
                    .unwrap_or(started_ms)
                    .max(started_ms);
                Some(TurnFact {
                    seq: turn.seq,
                    started_ms,
                    ended_ms,
                    interrupted: turn.interrupted != 0,
                    opens_with_correction: turn.opens_with_correction.map(|flag| flag != 0),
                })
            })
            .collect();
        turns.sort_by_key(|turn| turn.seq);
        let mut turns_by_time: Vec<(i64, i64)> = turns
            .iter()
            .map(|turn| (turn.started_ms, turn.seq))
            .collect();
        turns_by_time.sort_unstable();

        let mut attempts: Vec<AttemptFact> = rows
            .attempts
            .iter()
            .filter_map(|row| attempt_fact(row, &cmds, cfg, &turns_by_time))
            .collect();
        attempts.sort_by(|a, b| {
            (a.started_ms, a.attempt_id.as_str()).cmp(&(b.started_ms, b.attempt_id.as_str()))
        });
        for (idx, attempt) in attempts.iter_mut().enumerate() {
            attempt.idx = idx;
        }

        let mut span_facts: Vec<SpanFact> = spans
            .iter()
            .filter_map(|span| {
                let started_ms = parse_ms(&span.started_at)?;
                let ended_ms = parse_ms(&span.ended_at)?;
                let mut attempt_ids = json_strings(&span.attempt_ids);
                if attempt_ids.is_empty() {
                    attempt_ids.extend(span.attempt_id.clone());
                }
                Some(SpanFact {
                    id: span.id,
                    lane: span.lane.clone(),
                    kind: span.kind.clone(),
                    started_ms,
                    ended_ms: ended_ms.max(started_ms),
                    attempt_ids,
                    context_tokens: span.context_tokens,
                })
            })
            .collect();
        span_facts.sort_by(|a, b| {
            (a.lane.as_str(), a.started_ms, a.id).cmp(&(b.lane.as_str(), b.started_ms, b.id))
        });

        let mut messages: Vec<MessageFact> = rows
            .messages
            .iter()
            .filter_map(|message| {
                let first_ms = parse_ms(&message.first_at)?;
                let last_ms = parse_ms(&message.last_at)?;
                Some(MessageFact {
                    lane: message.lane.clone(),
                    message_id: message.message_id.clone(),
                    first_ms,
                    last_ms: last_ms.max(first_ms),
                    model: message.model.clone(),
                    has_tool_use: message.has_tool_use != 0,
                })
            })
            .collect();
        messages.sort_by(|a, b| {
            (a.lane.as_str(), a.first_ms, a.message_id.as_str()).cmp(&(
                b.lane.as_str(),
                b.first_ms,
                b.message_id.as_str(),
            ))
        });

        let mut markers: Vec<MarkerFact> = rows
            .markers
            .iter()
            .filter_map(|marker| {
                Some(MarkerFact {
                    lane: marker.lane.clone(),
                    ts_ms: parse_ms(&marker.ts)?,
                    kind: marker.kind.clone(),
                    reference: marker.r#ref.clone(),
                })
            })
            .collect();
        markers.sort_by_key(|marker| marker.ts_ms);

        // The session's own bounds, else the extent of what was read.
        let times = || {
            attempts
                .iter()
                .flat_map(|a| [a.started_ms, a.done_ms])
                .chain(span_facts.iter().flat_map(|s| [s.started_ms, s.ended_ms]))
                .chain(messages.iter().flat_map(|m| [m.first_ms, m.last_ms]))
                .chain(turns.iter().flat_map(|t| [t.started_ms, t.ended_ms]))
                .chain(markers.iter().map(|m| m.ts_ms))
        };
        let started_ms = head
            .started_at
            .as_deref()
            .and_then(parse_ms)
            .or_else(|| times().min())
            .unwrap_or(0);
        let ended_ms = head
            .ended_at
            .as_deref()
            .and_then(parse_ms)
            .or_else(|| times().max())
            .unwrap_or(started_ms);

        SessionFacts {
            session_id: head.session_id.clone(),
            project_id: head.project_id.clone(),
            started_ms,
            ended_ms,
            has_wait_machine: span_facts.iter().any(|span| span.kind == "wait_machine"),
            attempts,
            spans: span_facts,
            messages,
            turns,
            markers,
            sources,
            cmds,
            paths: PathMatcher::new(&cfg.paths),
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
    #[allow(dead_code)] // part of the facts contract, no family reads it yet
    pub effort: Option<String>,
    pub started_ms: i64,
    #[allow(dead_code)] // as above
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
    #[allow(dead_code)] // part of the facts contract, no family reads it yet
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
pub fn next_in_lane<'a>(
    facts: &'a SessionFacts,
    idx: usize,
    pred: impl Fn(&AttemptFact) -> bool,
) -> Option<&'a AttemptFact> {
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

// --- projection --------------------------------------------------------------------------------------

/// Span kinds that count as work: unclaimed, they are `useful`. `wait_human` and `idle` are not work
/// and stay unannotated.
const WORK_KINDS: [&str; 5] = [
    "model",
    "tool",
    "subagent",
    "wait_background",
    "wait_machine",
];

/// One finding competing to annotate a span or an attempt.
struct Claim {
    /// Position of the finding in the slice handed to `project`.
    index: usize,
    rule: &'static Rule,
    /// Position of the rule in `RULES`: the tie-break.
    order: usize,
    key: String,
}

fn waste_rank(waste: Waste) -> u8 {
    match waste {
        Waste::Avoidable => 2,
        Waste::Rework => 1,
        Waste::Useful => 0,
    }
}

/// `avoidable > rework`, then registry order.
fn outranks(candidate: &Claim, held: &Claim) -> bool {
    let (a, b) = (
        waste_rank(candidate.rule.waste),
        waste_rank(held.rule.waste),
    );
    a > b || (a == b && candidate.order < held.order)
}

/// The winning claim; between claims that tie completely, the first stands.
fn best_claim<'a>(claims: impl Iterator<Item = &'a Claim>) -> Option<&'a Claim> {
    let mut best: Option<&Claim> = None;
    for claim in claims {
        if best.is_none_or(|held| outranks(claim, held)) {
            best = Some(claim);
        }
    }
    best
}

fn claims_span(finding: &Finding, span: &SpanFact) -> bool {
    match &finding.policy {
        SpanPolicy::Interval => {
            span.lane == finding.lane
                && span.started_ms >= finding.started_ms
                && span.ended_ms <= finding.ended_ms
        }
        SpanPolicy::Attempts => span
            .attempt_ids
            .iter()
            .any(|id| finding.attempt_ids.contains(id)),
        SpanPolicy::Lane(lane) => &span.lane == lane,
        SpanPolicy::Spans(ids) => ids.contains(&span.id),
        SpanPolicy::Signal => false,
    }
}

/// Puts the findings and verdicts onto the session's spans and attempts.
///
/// - A span goes to the winning finding that claims it (`avoidable` over `rework`, then registry
///   order); a span of work no finding claims is `useful`; `wait_human` and `idle` stay unmarked.
/// - An attempt gets a mark when a non-signal finding names it, when it is an agent attempt (its
///   `verified` defaults to `not_measured`) or when a verdict names it.
///
/// Time totals are meant to be summed from these annotations, never from a finding's `cost_ms`, so
/// overlapping findings never count twice.
pub fn project(
    facts: &SessionFacts,
    findings: &[Finding],
    verdicts: &[Verdict],
) -> (Vec<SpanMark>, Vec<AttemptMarkRow>) {
    let claims: Vec<Claim> = findings
        .iter()
        .enumerate()
        .filter(|(_, finding)| !matches!(finding.policy, SpanPolicy::Signal))
        .filter_map(|(index, finding)| {
            let registered = rule(finding.rule_id)?;
            Some(Claim {
                index,
                rule: registered,
                order: RULES
                    .iter()
                    .position(|r| r.id == registered.id)
                    .unwrap_or(usize::MAX),
                key: finding_key(&facts.session_id, finding),
            })
        })
        .collect();

    let mut span_marks = Vec::new();
    for span in &facts.spans {
        let winner = best_claim(
            claims
                .iter()
                .filter(|claim| claims_span(&findings[claim.index], span)),
        );
        match winner {
            Some(claim) => span_marks.push(SpanMark {
                span_id: span.id,
                waste: Some(claim.rule.waste.as_str().to_string()),
                rule_id: Some(claim.rule.id.to_string()),
                lever: Some(claim.rule.levers[0].to_string()),
                finding_key: Some(claim.key.clone()),
            }),
            None if WORK_KINDS.contains(&span.kind.as_str()) => span_marks.push(SpanMark {
                span_id: span.id,
                waste: Some(Waste::Useful.as_str().to_string()),
                ..Default::default()
            }),
            None => {}
        }
    }

    let merged = merge_verdicts(verdicts.to_vec());
    let verdict_of: HashMap<&str, &Verdict> = merged
        .iter()
        .map(|verdict| (verdict.attempt_id.as_str(), verdict))
        .collect();
    let mut attempt_marks = Vec::new();
    for attempt in &facts.attempts {
        let winner = best_claim(claims.iter().filter(|claim| {
            findings[claim.index]
                .attempt_ids
                .contains(&attempt.attempt_id)
        }));
        let verdict = verdict_of.get(attempt.attempt_id.as_str()).copied();
        if winner.is_none() && verdict.is_none() && attempt.kind != "agent" {
            continue;
        }
        attempt_marks.push(AttemptMarkRow {
            attempt_id: attempt.attempt_id.clone(),
            session_id: facts.session_id.clone(),
            waste: winner.map(|claim| claim.rule.waste.as_str().to_string()),
            rule_id: winner.map(|claim| claim.rule.id.to_string()),
            lever: winner.map(|claim| claim.rule.levers[0].to_string()),
            finding_key: winner.map(|claim| claim.key.clone()),
            verified: verdict
                .map_or(Verified::NotMeasured, |v| v.verified)
                .as_str()
                .to_string(),
            verified_by: verdict.map(|v| v.rule_id.to_string()),
        });
    }
    (span_marks, attempt_marks)
}

/// What one rules pass did, for the caller's log. No health row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RulesPassStats {
    pub sessions_ruled: usize,
    pub sessions_failed: usize,
    pub families_panicked: usize,
}

/// The per-cycle rules pass over the sessions this cycle touched plus a capped backlog of stale ones,
/// skipping `skip` (sessions whose lane rebuild failed). Touched sessions always run; the backlog (a
/// session ruled under another `rules_version`, or never ruled) is capped by `sessions_per_cycle`.
/// One session's error or panic is logged and counted, and the next session goes on. Afterwards each
/// project a session was ruled for gets its cross-session findings replaced.
pub async fn run_rules_pass(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    sources: AdapterSources,
    touched: &BTreeSet<String>,
    skip: &BTreeSet<String>,
) -> RulesPassStats {
    run_rules_pass_with(pool, cfg, sources, touched, skip, FAMILIES).await
}

/// What ruling one session produced, for the pass's tally.
struct Ruled {
    project_id: String,
    families_panicked: usize,
}

/// Rules one session: reads its rows, builds the facts, evaluates, projects and writes the result in
/// one transaction. The pure part runs under `catch_unwind`, so a panic anywhere in it fails only this
/// session.
async fn rule_session(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    sources: AdapterSources,
    families: &[(char, FamilyFn)],
    fingerprint: &str,
    session_id: &str,
) -> Result<Ruled, String> {
    let head = devtime_store::session_head(pool, session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the session has no row".to_string())?;
    let rows = devtime_store::session_rows(pool, session_id)
        .await
        .map_err(|error| error.to_string())?;
    let spans = devtime_store::session_spans(pool, session_id)
        .await
        .map_err(|error| error.to_string())?;
    let (write, families_panicked) = catch_unwind(AssertUnwindSafe(|| {
        let facts = SessionFacts::build(&head, &rows, &spans, cfg, sources);
        let (output, panicked) = evaluate_counted(&facts, cfg, families);
        let (span_marks, attempt_marks) = project(&facts, &output.findings, &output.verdicts);
        let turn_stats = devtime_unexplained::turn_stats(&facts, &span_marks, cfg);
        let findings = finding_rows(&facts, &output.findings, fingerprint);
        (
            RuleWrite {
                findings,
                span_marks,
                attempt_marks,
                turn_stats,
            },
            panicked,
        )
    }))
    .map_err(|_| "the rule engine panicked".to_string())?;
    devtime_store::write_rule_results(pool, &head, &write, fingerprint)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Ruled {
        project_id: head.project_id,
        families_panicked,
    })
}

/// [`run_rules_pass`] with the families injected, so a test can run the pass over its own.
async fn run_rules_pass_with(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    sources: AdapterSources,
    touched: &BTreeSet<String>,
    skip: &BTreeSet<String>,
    families: &[(char, FamilyFn)],
) -> RulesPassStats {
    let fingerprint = rules_fingerprint(cfg);
    let mut stats = RulesPassStats::default();

    let mut work: Vec<String> = touched
        .iter()
        .filter(|id| !skip.contains(*id))
        .cloned()
        .collect();
    let mut queued: HashSet<String> = work.iter().cloned().collect();
    match devtime_store::sessions_needing_rules(pool, &fingerprint, cfg.sessions_per_cycle).await {
        Ok(backlog) => {
            for id in backlog {
                if !skip.contains(&id) && queued.insert(id.clone()) {
                    work.push(id);
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "devtime: could not list the sessions the rules owe work");
        }
    }

    let mut projects: BTreeSet<String> = BTreeSet::new();
    for session_id in &work {
        match rule_session(pool, cfg, sources, families, &fingerprint, session_id).await {
            Ok(ruled) => {
                stats.sessions_ruled += 1;
                stats.families_panicked += ruled.families_panicked;
                projects.insert(ruled.project_id);
            }
            Err(error) => {
                stats.sessions_failed += 1;
                tracing::warn!(session = %session_id, %error, "devtime: ruling a session failed");
            }
        }
    }

    let since = format_ms(
        Utc::now().timestamp_millis() - i64::from(cfg.thresholds.f_window_days) * MS_PER_DAY,
    );
    for project_id in &projects {
        let rows = match devtime_store::cross_session_rows(pool, project_id, &since).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(project = %project_id, %error, "devtime: could not read the cross-session rows");
                continue;
            }
        };
        let found = match catch_unwind(AssertUnwindSafe(|| devtime_rules_f::run_cross(&rows, cfg)))
        {
            Ok(found) => found,
            Err(_) => {
                stats.families_panicked += 1;
                tracing::warn!(project = %project_id, family = "F", "devtime: a rule family panicked");
                continue;
            }
        };
        let written = cross_rows(project_id, &found, &fingerprint);
        if let Err(error) =
            devtime_store::replace_cross_findings(pool, project_id, CROSS_RULE_IDS, &written).await
        {
            tracing::warn!(project = %project_id, %error, "devtime: could not store the cross-session findings");
        }
    }
    stats
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

    // --- the engine ------------------------------------------------------------------------------

    use crate::devtime_rules_fixture::{Script, at, attempt, script, test_pool};

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
                        *id,
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
