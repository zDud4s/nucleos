//! Per-session facts used by the development-time rules.

use super::*;

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

pub(super) fn json_strings(text: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(text).unwrap_or_default()
}

pub(super) fn json_objects(text: &str) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(text).unwrap_or_default()
}

pub(super) fn outcome_of(text: &str) -> Outcome {
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
pub(super) fn effective_outcome(row: &AttemptRow) -> Outcome {
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
pub(super) fn attempt_fact(
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
