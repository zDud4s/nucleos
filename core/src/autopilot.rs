//! §spec agenticos-foundation-and-autopilot

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Shadow,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project_id: String,
    pub mode: Mode,
    pub project_root: Option<String>,
    pub pending: i64,
    /// Shadow-exit progress (§8.2): action classes clearing the bar, out of those exercised so far.
    pub classes_ready: i64,
    pub classes_total: i64,
    /// How many of those ready classes are ones the classifier WITHHELD (`pending_approval`/`deny`).
    /// Surfaced separately because it is the criterion a project can fail while looking finished:
    /// every class ready out of every class exercised, and still nothing proving restraint.
    pub withheld_classes_ready: i64,
    /// Whether the shell should unlock the promote-to-active control. Computed here rather than in
    /// the shell so the displayed gate and the enforced rule are the same arithmetic (`shadow.rs`).
    pub promotable: bool,
    /// WIP brake (§8.4): proposals waiting on the human, the effective ceiling (`None` = brake off),
    /// and whether the project is currently deferring new work because of it. A project can be idle
    /// purely because its queue is full, so the UI has to be able to say so.
    pub open_proposals: i64,
    pub wip_limit: Option<i64>,
    pub queue_full: bool,
    /// The last thing this project's gate said, and when it said it.
    ///
    /// The LAST verdict and not a tally over a window, because the roster's question is *is this
    /// one broken right now*. [`crate::project_readings`] already answers "how has it been going"
    /// for the one project somebody opened, and a thirty-day count in a roster column would read
    /// green for a project that broke this morning.
    ///
    /// `None` means this project has never produced one — no gate command, or no run that got far
    /// enough to reach it. `0032_run_gate.sql` argues that case: it is not a pass and not a
    /// failure, it is nobody having asked for a measurement.
    pub last_gate: Option<String>,
    pub last_gate_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopedKill {
    pub scope_type: String,
    pub scope_id: String,
    pub engaged: bool,
}

impl Mode {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Active => "active",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "shadow" => Some(Self::Shadow),
            "active" => Some(Self::Active),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ActivationError {
    NotAGitRepo,
    ProjectRootRequired,
    NotOnboarded,
    HookNotRegistered,
    Database(sqlx::Error),
}

impl fmt::Display for ActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAGitRepo => write!(formatter, "project root is not a git repository"),
            Self::ProjectRootRequired => {
                write!(formatter, "project_root is required to enable shadow mode")
            }
            Self::NotOnboarded => write!(formatter, "project is not onboarded to .ai/workflow"),
            Self::HookNotRegistered => {
                write!(formatter, "a PreToolUse hook is not registered")
            }
            Self::Database(error) => write!(formatter, "database error: {error}"),
        }
    }
}

impl std::error::Error for ActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for ActivationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

pub async fn project_mode(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Mode> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT mode FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;

    stored.map_or(Ok(Mode::Off), |value| {
        Mode::from_db_str(&value).ok_or_else(|| {
            sqlx::Error::Protocol(format!("invalid autopilot mode in database: {value}"))
        })
    })
}

pub async fn set_project_mode(
    pool: &SqlitePool,
    project_id: &str,
    mode: Mode,
    project_root: Option<&Path>,
) -> Result<(), ActivationError> {
    match mode {
        Mode::Active => {
            let project_root = project_root.ok_or(ActivationError::ProjectRootRequired)?;
            activation_prerequisites(project_root)?;
            if !project_root.join(".git").exists() {
                return Err(ActivationError::NotAGitRepo);
            }
        }
        Mode::Shadow => {
            activation_prerequisites(project_root.ok_or(ActivationError::ProjectRootRequired)?)?
        }
        Mode::Off => {}
    }

    let persisted_root = match mode {
        Mode::Shadow | Mode::Active => project_root.map(|root| root.to_string_lossy().into_owned()),
        Mode::Off => None,
    };

    sqlx::query(
        "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, ?, ?)
         ON CONFLICT(project_id) DO UPDATE SET
             mode = excluded.mode,
             project_root = excluded.project_root",
    )
    .bind(project_id)
    .bind(mode.as_db_str())
    .bind(persisted_root)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn autopilot_projects(pool: &SqlitePool) -> sqlx::Result<Vec<(String, String, Mode)>> {
    let projects: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT project_id, project_root, mode
         FROM autopilot_state
         WHERE mode IN ('shadow', 'active') AND project_root IS NOT NULL
         ORDER BY project_id",
    )
    .fetch_all(pool)
    .await?;

    projects
        .into_iter()
        .map(|(project_id, project_root, mode)| {
            let mode = Mode::from_db_str(&mode).ok_or_else(|| {
                sqlx::Error::Protocol(format!("invalid autopilot mode in database: {mode}"))
            })?;
            Ok((project_id, project_root, mode))
        })
        .collect()
}

/// One raw roster row: `(project_id, mode, project_root, pending, open_proposals, wip_override)`.
/// Named because the tuple carries six positional fields and is only readable at the destructure.
type RosterRow = (String, String, Option<String>, i64, i64, Option<i64>);

/// The most recent gate verdict for every project, in one query.
///
/// One grouped read and not one per row, for the reason `shadow_readiness` is one: the shell polls
/// the roster every three seconds, and a call per project turns a roster of twenty-five into
/// twenty-five round trips on every tick.
///
/// **This leans on a documented SQLite behaviour and says so, because it is not standard SQL.** With
/// a bare column beside `MAX(...)` in an aggregate query, SQLite takes that column from the row
/// that supplied the maximum — so `gate_status` here is the status of the newest run, not an
/// arbitrary one from the group. Every other engine is free to return any row, and this query would
/// have to be rewritten as a window function the day this stops being SQLite.
///
/// Rows with no verdict are excluded rather than counted as anything: a run that never reached its
/// gate says nothing about the code, and the last run that DID reach one is still the answer.
async fn last_gate_verdicts(
    pool: &SqlitePool,
) -> sqlx::Result<std::collections::HashMap<String, (String, String)>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT project_id, gate_status, MAX(completed_at)
         FROM runs
         WHERE project_id IS NOT NULL
           AND gate_status IS NOT NULL
           AND completed_at IS NOT NULL
         GROUP BY project_id",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(project_id, status, at)| (project_id, (status, at)))
        .collect())
}

pub async fn project_roster(pool: &SqlitePool) -> sqlx::Result<Vec<ProjectSummary>> {
    let projects: Vec<RosterRow> = sqlx::query_as(
        "SELECT state.project_id, state.mode, state.project_root,
                (SELECT COUNT(*)
                 FROM shadow_decisions
                 JOIN runs ON shadow_decisions.run_id = runs.id
                 WHERE runs.project_id = state.project_id
                   AND shadow_decisions.human_verdict IS NULL
                   AND runs.mode = 'shadow')
                +
                (SELECT COUNT(*)
                 FROM runs
                 WHERE runs.project_id = state.project_id
                   AND runs.status = 'awaiting_approval') AS pending,
                -- Same arithmetic as `wip::open_proposals`, deliberately: the flag the shell renders
                -- and the gate the daemon enforces must not be able to disagree. Shadow decisions
                -- count because a shadow run mints no proposal, so counting proposals alone left
                -- the brake invisible in the mode that generates the most review work. Scoped to
                -- `runs.mode = 'shadow'`: a `worktree`-mode decision was already enforced (no
                -- verdict left to give it) or already became a proposal the first term counts, so
                -- an unfiltered count would either double-count or count enforced work as backlog.
                -- Both copies of this subquery must carry the same filter, or this display number
                -- and the number the daemon enforces (`wip::open_proposals`) would disagree.
                (SELECT COUNT(*)
                 FROM proposals
                 WHERE proposals.project_id = state.project_id
                   AND proposals.status = 'pending')
                +
                (SELECT COUNT(*)
                 FROM shadow_decisions
                 JOIN runs ON shadow_decisions.run_id = runs.id
                 WHERE runs.project_id = state.project_id
                   AND shadow_decisions.human_verdict IS NULL
                   AND runs.mode = 'shadow') AS open_proposals,
                state.wip_limit AS wip_limit
         FROM autopilot_state AS state
         ORDER BY state.project_id",
    )
    .fetch_all(pool)
    .await?;

    // One grouped query for every project's readiness, rather than a scoreboard call per row — the
    // shell polls this endpoint every 3s. Same reason the WIP ceiling is resolved from one global
    // read plus the per-row override already selected above.
    let readiness = crate::shadow::shadow_readiness(pool).await?;
    let global_wip_limit = crate::wip::global_wip_limit(pool).await?;
    let gates = last_gate_verdicts(pool).await?;

    projects
        .into_iter()
        .map(
            |(project_id, mode, project_root, pending, open_proposals, wip_override)| {
                let mode = Mode::from_db_str(&mode).ok_or_else(|| {
                    sqlx::Error::Protocol(format!("invalid autopilot mode in database: {mode}"))
                })?;
                let (classes_ready, classes_total, withheld_classes_ready) =
                    readiness.get(&project_id).copied().unwrap_or((0, 0, 0));
                let wip_limit = wip_override.or(global_wip_limit);
                let (last_gate, last_gate_at) = match gates.get(&project_id) {
                    Some((status, at)) => (Some(status.clone()), Some(at.clone())),
                    None => (None, None),
                };
                Ok(ProjectSummary {
                    project_id,
                    mode,
                    project_root,
                    pending,
                    classes_ready,
                    classes_total,
                    withheld_classes_ready,
                    promotable: crate::shadow::promotable(
                        classes_ready,
                        classes_total,
                        withheld_classes_ready,
                    ),
                    open_proposals,
                    wip_limit,
                    queue_full: crate::wip::queue_full(open_proposals, wip_limit),
                    last_gate,
                    last_gate_at,
                })
            },
        )
        .collect()
}

/// The hook script that makes a project's tool calls reach this daemon. Written with forward
/// slashes because it is compared against a JSON command string, where that is the spelling.
const HOOK_SCRIPT: &str = ".claude/hooks/ask_daemon.py";

/// This daemon's classifier hook, carried inside the binary so it can be installed anywhere.
///
/// **Under `core/hooks/` and not under `.claude/`, which is where it used to live.** This is the
/// source of an artefact the daemon SHIPS — it is written into every project that asks for tools —
/// and `.claude/` is a directory this repository's own pre-commit guard refuses to let anything be
/// staged into. A build input that cannot be changed is not a build input, and the contradiction was
/// not theoretical: the daemon wiring THIS repository overwrites the copy under `.claude/`, which
/// then shows as a modified tracked file that nothing is allowed to commit.
///
/// `include_str!` rather than a path resolved at runtime: the daemon that ANSWERS the hook and the
/// script that ASKS it are two halves of one protocol. A copy read off disk at install time could
/// be any version — including one left behind by a daemon that is no longer running — and the two
/// disagreeing is a gate that fails open or a session that cannot act, neither of which announces
/// itself.
const HOOK_SOURCE: &str = include_str!("../hooks/ask_daemon.py");

/// The interpreter the classifier hook is registered under, per platform.
///
/// `python3` off Windows: macOS has shipped no `python` since 12.3 and Debian/Ubuntu ship only
/// `python3`. A missing interpreter does not fail closed — the shell exits 127, Claude Code treats
/// every exit but 2 as a non-blocking error, and the tool call goes ahead unclassified; all the
/// fail-closed care inside `ask_daemon.py` never runs because the script never starts.
/// `python` on Windows and NOT `python3`: on a default install `python3` is the Microsoft Store's
/// App Execution Alias, which opens the Store instead of running the script — the same hole from
/// the other side. Existing entries are recognised by script path, so changing this orphans none.
pub(crate) const HOOK_INTERPRETER: &str = if cfg!(windows) { "python" } else { "python3" };

/// How the hook is registered, spelled exactly as `classifier_hook_is_wired` looks for it.
///
/// `${CLAUDE_PROJECT_DIR}` and not an absolute path: the same settings file is read from worktrees
/// and from copies of the project, and a path baked in at install time would point at whichever one
/// happened to be wired first.
///
/// The SAME entry is registered under every event `wire_classifier_hook` wires — `PreToolUse`,
/// `PostToolUse`, `PostToolUseFailure` — because it is one script that reads `hook_event_name` out
/// of its own payload to tell them apart (`core/hooks/ask_daemon.py`'s `main`). Nothing here needs
/// to know which event it is being registered under.
fn hook_entry() -> serde_json::Value {
    serde_json::json!({
        "matcher": "*",
        "hooks": [{
            "type": "command",
            "command": format!("{HOOK_INTERPRETER} \"${{CLAUDE_PROJECT_DIR}}/.claude/hooks/ask_daemon.py\"")
        }]
    })
}

/// Registers one hook entry under `event_name` in `hooks`, deduped the same way for every event.
///
/// A free function rather than the chain inlined three times, because `.entry("hooks")…
/// .entry("PreToolUse")` used to CONSUME the map's `as_object_mut()` borrow to reach the one
/// event's array — there is no way to chain a second `.entry(...)` off the end of that without
/// borrowing `hooks` again from scratch. Re-`entry()`ing from `hooks` once per event, here, is
/// what lets a second and third event be appended instead of only the first one ever landing.
///
/// Compared the way `classifier_hook_is_wired` compares: against the script's path anywhere in
/// the entry. The invocation around it is the user's business — `python`, `py -3`, a venv — and
/// re-registering ours beside theirs would ask the daemon about every tool call twice.
///
/// **This dedupe does not look at which event it is scanning, and that is only safe because
/// `hook_entry()` returns the identical command for every event it is wired under.** If the
/// command ever came to differ by event — say, a flag naming the event so the script did not have
/// to sniff `hook_event_name` out of its own payload — a `PostToolUse` entry already on disk could
/// satisfy this check while wiring `PreToolUse`, and the barrier that matters most would silently
/// stop being (re-)registered. Splitting the check by event, at that point, is not optional.
fn wire_event(
    hooks: &mut serde_json::Map<String, serde_json::Value>,
    event_name: &str,
    entry: serde_json::Value,
) -> Result<(), String> {
    let list = hooks
        .entry(event_name)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("`{event_name}` is not a list"))?;

    let already = list.iter().any(|existing| {
        serde_json::to_string(existing).is_ok_and(|text| text.contains(HOOK_SCRIPT))
    });
    if !already {
        list.push(entry);
    }
    Ok(())
}

/// Puts this daemon's classifier hook into `dir`, so a conversation continued there may act.
///
/// This is what stands between continuing a coding session and continuing it with a model that
/// cannot open a file: `tool_policy_for` hands a turn the project's tools only where this gate is
/// satisfied, and a directory without it — every fresh worktree, since `.claude/` is not committed
/// — falls back to the MCP server alone.
///
/// Idempotent, and deliberately narrow: it adds one entry and rewrites one script. Everything else
/// in the settings file belongs to whoever wrote it and is carried across untouched.
///
/// Nothing is written until the existing settings have been read AND parsed. A file this cannot
/// understand is somebody's work in progress, or a version of the CLI this daemon has never seen,
/// and the cost of guessing at it is settings nobody asked to lose.
pub(crate) fn wire_classifier_hook(dir: &Path) -> Result<(), String> {
    let claude = dir.join(".claude");
    let settings_path = claude.join("settings.json");

    let mut settings: serde_json::Value = match std::fs::read_to_string(&settings_path) {
        Ok(text) if text.trim().is_empty() => serde_json::json!({}),
        Ok(text) => serde_json::from_str(&text).map_err(|error| {
            format!(
                "{} is not JSON this daemon can read ({error}); it was left alone",
                settings_path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => {
            return Err(format!(
                "could not read {}: {error}",
                settings_path.display()
            ));
        }
    };

    let object = settings
        .as_object_mut()
        .ok_or_else(|| format!("{} does not hold an object", settings_path.display()))?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{}: `hooks` is not an object", settings_path.display()))?;

    // `PreToolUse` is the barrier `classifier_hook_is_wired` gates activation on, and it is wired
    // first for that reason alone — order does not matter to the settings file, only to a reader
    // of this diff. `PostToolUse` and `PostToolUseFailure` are the outcome half wired beside it:
    // the CLI fires the first after a successful call and the second after a failed one, never
    // both, so wiring only one of them would make this ledger blind to every tool call that did
    // NOT succeed.
    for event_name in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
        wire_event(hooks, event_name, hook_entry())
            .map_err(|error| format!("{}: {error}", settings_path.display()))?;
    }

    let body = serde_json::to_string_pretty(&settings)
        .map_err(|error| format!("could not write the settings back: {error}"))?;

    std::fs::create_dir_all(claude.join("hooks"))
        .map_err(|error| format!("could not make {}: {error}", claude.display()))?;
    std::fs::write(claude.join("hooks").join("ask_daemon.py"), HOOK_SOURCE)
        .map_err(|error| format!("could not write the hook script: {error}"))?;
    std::fs::write(
        &settings_path,
        format!(
            "{body}
"
        ),
    )
    .map_err(|error| format!("could not write {}: {error}", settings_path.display()))?;

    Ok(())
}

/// Whether THIS daemon's classifier hook is both registered in `dir` and executable there.
///
/// Two callers ask this, and they must never diverge. Activation asks it before letting a project
/// act autonomously at all. `runs.rs` asks it before opening the CLI's own permission surface for
/// an unattended run — because the classifier is what governs that run's actions, and a run whose
/// permissions are opened without one is the same mistake as a project activated without one.
///
/// `dir` is the directory the CLI will actually run in, which for a worktree run is the worktree
/// and not the project root: settings are read from where the process starts.
///
/// **Checks `PreToolUse` only, deliberately, even though `wire_classifier_hook` now registers two
/// more events.** The barrier the gate exists to guarantee — a run cannot act without a tool call
/// being classified first — is entirely the `PreToolUse` half; `PostToolUse`/`PostToolUseFailure`
/// only ever RECORD an outcome after the tool has already run, and cannot be the thing standing
/// between a stranger's text and a shell. Requiring all three here would also break every project
/// already wired before this pair existed, refusing activation to a project whose actual barrier
/// is intact until its `.claude/settings.json` happens to be rewritten. Do not "complete" this
/// check without re-reading this paragraph first.
pub(crate) fn classifier_hook_is_wired(dir: &Path) -> bool {
    let Ok(settings) = std::fs::read_to_string(dir.join(".claude/settings.json")) else {
        return false;
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&settings) else {
        return false;
    };
    // "Some PreToolUse hook exists" was never the property worth checking: a formatter satisfied it
    // just as well as ours, and activation then let a project act autonomously with nothing
    // classifying its actions. What has to be true is that OUR hook script is the one wired up,
    // and that it is actually on disk — a registered command that cannot execute classifies
    // nothing either.
    //
    // Matching on the script's path rather than the whole command line, because the invocation
    // around it is the user's business (`python`, `py -3`, a venv, extra flags) and only the
    // script identifies the hook as this daemon's.
    let registered = settings
        .get("hooks")
        .and_then(|hooks| hooks.get("PreToolUse"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|inner| {
                        inner.iter().any(|hook| {
                            hook.get("command")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|command| command.contains(HOOK_SCRIPT))
                        })
                    })
            })
        });

    registered && dir.join(HOOK_SCRIPT).is_file()
}

fn activation_prerequisites(project_root: &Path) -> Result<(), ActivationError> {
    if !project_root.join(".ai/workflow/workflow.md").is_file() {
        return Err(ActivationError::NotOnboarded);
    }
    if !classifier_hook_is_wired(project_root) {
        return Err(ActivationError::HookNotRegistered);
    }
    Ok(())
}

pub async fn kill_switch_engaged(pool: &SqlitePool) -> sqlx::Result<bool> {
    let value: i64 = sqlx::query_scalar("SELECT kill_switch FROM autopilot_global LIMIT 1")
        .fetch_one(pool)
        .await?;
    Ok(value != 0)
}

pub async fn set_kill_switch(pool: &SqlitePool, engaged: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE autopilot_global SET kill_switch = ?")
        .bind(engaged)
        .execute(pool)
        .await?;
    Ok(())
}

/// Whether the granular kill switch for a given scope (e.g. scope_type "project"/"trigger") is engaged.
/// An absent row means not engaged.
pub async fn scoped_kill_engaged(
    pool: &SqlitePool,
    scope_type: &str,
    scope_id: &str,
) -> sqlx::Result<bool> {
    let value: Option<i64> = sqlx::query_scalar(
        "SELECT engaged FROM scoped_kill_switches WHERE scope_type = ? AND scope_id = ?",
    )
    .bind(scope_type)
    .bind(scope_id)
    .fetch_optional(pool)
    .await?;
    Ok(value.unwrap_or(0) != 0)
}

/// Engage or disengage the granular kill switch for a scope (upsert).
pub async fn set_scoped_kill(
    pool: &SqlitePool,
    scope_type: &str,
    scope_id: &str,
    engaged: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO scoped_kill_switches (scope_type, scope_id, engaged) VALUES (?, ?, ?)
         ON CONFLICT(scope_type, scope_id) DO UPDATE SET engaged = excluded.engaged",
    )
    .bind(scope_type)
    .bind(scope_id)
    .bind(engaged)
    .execute(pool)
    .await?;
    Ok(())
}

/// All granular kill-switch rows, ordered by (scope_type, scope_id).
pub async fn list_scoped_kills(pool: &SqlitePool) -> sqlx::Result<Vec<ScopedKill>> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT scope_type, scope_id, engaged FROM scoped_kill_switches
         ORDER BY scope_type, scope_id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(scope_type, scope_id, engaged)| ScopedKill {
            scope_type,
            scope_id,
            engaged: engaged != 0,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script the daemon SHIPS knows the verdict the daemon GIVES.
    ///
    /// `hooks.rs` answers `asking` for a tool call somebody has to allow, and comes back to
    /// `/hooks/ask-wait` for the answer. A script that does not know either word fails closed on an
    /// unrecognised verdict — a refusal with a worse message than the one it replaced, and no way
    /// for anybody to say yes.
    ///
    /// The two halves live in different files and are only true together. Asserted here rather than
    /// left to whoever next edits one of them.
    #[test]
    fn the_shipped_hook_knows_how_to_wait_for_an_answer() {
        assert!(
            HOOK_SOURCE.contains("asking"),
            "the shipped hook does not know the verdict this daemon gives"
        );
        assert!(
            HOOK_SOURCE.contains("/hooks/ask-wait"),
            "the shipped hook does not know where to wait for an answer"
        );
    }
    use std::fs;
    use tempfile::TempDir;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn write_workflow(root: &TempDir) {
        let workflow_dir = root.path().join(".ai/workflow");
        fs::create_dir_all(&workflow_dir).unwrap();
        fs::write(workflow_dir.join("workflow.md"), "# Workflow").unwrap();
    }

    fn write_settings(root: &TempDir, contents: &str) {
        let claude_dir = root.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(claude_dir.join("settings.json"), contents).unwrap();
    }

    /// Settings wired to THIS daemon's hook, plus the script on disk — both of which activation
    /// now requires. Anything less specific would pass a formatter off as the safety gate.
    fn write_nucleos_hook(root: &TempDir) {
        write_settings(
            root,
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"python \"${CLAUDE_PROJECT_DIR}/.claude/hooks/ask_daemon.py\""}]}]}}"#,
        );
        let hooks_dir = root.path().join(".claude/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        fs::write(hooks_dir.join("ask_daemon.py"), "# hook").unwrap();
    }

    #[test]
    fn the_hook_is_registered_under_this_platforms_interpreter() {
        let entry = hook_entry();
        let command = entry["hooks"][0]["command"].as_str().unwrap();
        #[cfg(windows)]
        let expected_prefix = "python \"";
        #[cfg(not(windows))]
        let expected_prefix = "python3 \"";

        assert!(command.starts_with(expected_prefix), "{command}");
        assert!(command.ends_with(&format!("/{HOOK_SCRIPT}\"")), "{command}");
    }

    #[test]
    fn an_entry_under_any_interpreter_is_recognised_and_never_duplicated() {
        for interpreter in ["python", "python3", "py -3"] {
            let root = TempDir::new().unwrap();
            let command = format!("{interpreter} \"${{CLAUDE_PROJECT_DIR}}/{HOOK_SCRIPT}\"");
            write_settings(
                &root,
                &serde_json::json!({
                    "hooks": {
                        "PreToolUse": [{
                            "matcher": "*",
                            "hooks": [{"type": "command", "command": command}]
                        }]
                    }
                })
                .to_string(),
            );
            let hooks_dir = root.path().join(".claude/hooks");
            fs::create_dir_all(&hooks_dir).unwrap();
            fs::write(hooks_dir.join("ask_daemon.py"), "# hook").unwrap();

            assert!(classifier_hook_is_wired(root.path()), "{interpreter}");
            wire_classifier_hook(root.path()).unwrap();

            let settings: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(root.path().join(".claude/settings.json")).unwrap(),
            )
            .unwrap();
            let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
            let ours = entries
                .iter()
                .filter(|entry| serde_json::to_string(entry).unwrap().contains(HOOK_SCRIPT))
                .count();
            assert_eq!(ours, 1, "{interpreter}: {entries:?}");
        }
    }

    fn git_init(root: &TempDir) {
        fs::create_dir_all(root.path().join(".git")).unwrap();
    }

    #[tokio::test]
    async fn project_roster_is_empty_without_autopilot_state_rows() {
        let pool = test_pool().await;

        assert_eq!(
            project_roster(&pool).await.unwrap(),
            Vec::<ProjectSummary>::new()
        );
    }

    #[tokio::test]
    async fn project_roster_lists_every_mode_ordered_by_project_id() {
        let pool = test_pool().await;

        for (project_id, mode) in [
            ("project-shadow", "shadow"),
            ("project-off", "off"),
            ("project-active", "active"),
        ] {
            sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, ?)")
                .bind(project_id)
                .bind(mode)
                .execute(&pool)
                .await
                .unwrap();
        }

        assert_eq!(
            project_roster(&pool).await.unwrap(),
            vec![
                ProjectSummary {
                    project_id: "project-active".to_owned(),
                    mode: Mode::Active,
                    project_root: None,
                    pending: 0,
                    classes_ready: 0,
                    classes_total: 0,
                    withheld_classes_ready: 0,
                    promotable: false,
                    open_proposals: 0,
                    wip_limit: Some(3),
                    queue_full: false,
                    last_gate: None,
                    last_gate_at: None,
                },
                ProjectSummary {
                    project_id: "project-off".to_owned(),
                    mode: Mode::Off,
                    project_root: None,
                    pending: 0,
                    classes_ready: 0,
                    classes_total: 0,
                    withheld_classes_ready: 0,
                    promotable: false,
                    open_proposals: 0,
                    wip_limit: Some(3),
                    queue_full: false,
                    last_gate: None,
                    last_gate_at: None,
                },
                ProjectSummary {
                    project_id: "project-shadow".to_owned(),
                    mode: Mode::Shadow,
                    project_root: None,
                    pending: 0,
                    classes_ready: 0,
                    classes_total: 0,
                    withheld_classes_ready: 0,
                    promotable: false,
                    open_proposals: 0,
                    wip_limit: Some(3),
                    queue_full: false,
                    last_gate: None,
                    last_gate_at: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn project_roster_returns_project_root_when_present() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) \
             VALUES ('project-rooted', 'shadow', '/some/root')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('project-off', 'off')")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            project_roster(&pool).await.unwrap(),
            vec![
                ProjectSummary {
                    project_id: "project-off".to_owned(),
                    mode: Mode::Off,
                    project_root: None,
                    pending: 0,
                    classes_ready: 0,
                    classes_total: 0,
                    withheld_classes_ready: 0,
                    promotable: false,
                    open_proposals: 0,
                    wip_limit: Some(3),
                    queue_full: false,
                    last_gate: None,
                    last_gate_at: None,
                },
                ProjectSummary {
                    project_id: "project-rooted".to_owned(),
                    mode: Mode::Shadow,
                    project_root: Some("/some/root".to_owned()),
                    pending: 0,
                    classes_ready: 0,
                    classes_total: 0,
                    withheld_classes_ready: 0,
                    promotable: false,
                    open_proposals: 0,
                    wip_limit: Some(3),
                    queue_full: false,
                    last_gate: None,
                    last_gate_at: None,
                },
            ]
        );
    }

    /// The LAST verdict, and the roster's whole gate column rests on it being the last one.
    ///
    /// Three runs, out of chronological insert order on purpose: a query that returned "some row
    /// from the group" would pass this half the time, and the SQLite bare-column rule this leans on
    /// is exactly what is being pinned.
    #[tokio::test]
    async fn project_roster_reports_the_newest_gate_verdict_and_not_an_older_one() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('alpha', 'shadow')")
            .execute(&pool)
            .await
            .unwrap();

        for (status, completed_at) in [
            ("failed", "2026-08-01T09:00:00Z"),
            ("passed", "2026-08-03T09:00:00Z"),
            ("errored", "2026-08-02T09:00:00Z"),
        ] {
            sqlx::query(
                "INSERT INTO runs (project_id, prompt, status, created_at, mode, gate_status, completed_at)
                 VALUES ('alpha', 'p', 'completed', '2026-08-01T08:00:00Z', 'shadow', ?, ?)",
            )
            .bind(status)
            .bind(completed_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        let roster = project_roster(&pool).await.unwrap();
        assert_eq!(roster[0].last_gate.as_deref(), Some("passed"));
        assert_eq!(
            roster[0].last_gate_at.as_deref(),
            Some("2026-08-03T09:00:00Z")
        );
    }

    /// A run that never reached its gate says nothing about the code, so it cannot be the answer.
    ///
    /// The newest run here has no verdict at all, and the roster must still report the last one that
    /// did — otherwise a project goes blank in the column the moment anything crashes early.
    #[tokio::test]
    async fn project_roster_ignores_runs_that_never_reached_a_gate() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('alpha', 'shadow')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, created_at, mode, gate_status, completed_at)
             VALUES ('alpha', 'p', 'completed', '2026-08-01T08:00:00Z', 'shadow', 'failed', '2026-08-01T09:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, created_at, mode, completed_at)
             VALUES ('alpha', 'p', 'failed', '2026-08-05T08:00:00Z', 'shadow', '2026-08-05T09:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            project_roster(&pool).await.unwrap()[0].last_gate.as_deref(),
            Some("failed")
        );
    }

    /// **No gate is not a pass.** `0032_run_gate.sql` spends its comment on this: a project with no
    /// gate command has no definition of green, and a default verdict would invent a check.
    #[tokio::test]
    async fn project_roster_reports_no_verdict_for_a_project_that_has_never_run_a_gate() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('alpha', 'shadow')")
            .execute(&pool)
            .await
            .unwrap();

        let roster = project_roster(&pool).await.unwrap();
        assert_eq!(roster[0].last_gate, None);
        assert_eq!(roster[0].last_gate_at, None);
    }

    #[tokio::test]
    async fn project_roster_counts_unreviewed_shadow_and_awaiting_approval() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, ?)")
            .bind("project-pending")
            .bind("active")
            .execute(&pool)
            .await
            .unwrap();

        let shadow_run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, created_at, mode) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("project-pending")
        .bind("classify this")
        .bind("completed")
        .bind("2026-07-20T10:00:00Z")
        .bind("shadow")
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        for (human_verdict, created_at) in [
            (None, "2026-07-20T10:01:00Z"),
            (Some("approve"), "2026-07-20T10:02:00Z"),
        ] {
            sqlx::query(
                "INSERT INTO shadow_decisions \
                 (run_id, tool_name, decision, action_class, classifier_version, human_verdict, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(shadow_run_id)
            .bind("Bash")
            .bind("allow")
            .bind("read")
            .bind(1_i64)
            .bind(human_verdict)
            .bind(created_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        for prompt in ["approve one", "approve two"] {
            sqlx::query(
                "INSERT INTO runs (project_id, prompt, status, created_at, mode) VALUES (?, ?, ?, ?, ?)",
            )
            .bind("project-pending")
            .bind(prompt)
            .bind("awaiting_approval")
            .bind("2026-07-20T10:03:00Z")
            .bind("real")
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(
            project_roster(&pool).await.unwrap(),
            vec![ProjectSummary {
                project_id: "project-pending".to_owned(),
                mode: Mode::Active,
                project_root: None,
                pending: 3,
                classes_ready: 0,
                classes_total: 1,
                withheld_classes_ready: 0,
                promotable: false,
                // The one unreviewed shadow decision seeded above. It counts here as well as in
                // `pending`, because the WIP brake throttles on everything waiting for a human and
                // a shadow run mints no proposal to stand for it. The overlap is deliberate:
                // `pending` is what the person is shown, `open_proposals` is what the brake
                // measures, and both have to see the same backlog.
                open_proposals: 1,
                wip_limit: Some(3),
                queue_full: false,
                last_gate: None,
                last_gate_at: None,
            }]
        );
    }

    #[tokio::test]
    async fn project_roster_keeps_projects_with_zero_pending() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, ?)")
            .bind("project-idle")
            .bind("off")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            project_roster(&pool).await.unwrap(),
            vec![ProjectSummary {
                project_id: "project-idle".to_owned(),
                mode: Mode::Off,
                project_root: None,
                pending: 0,
                classes_ready: 0,
                classes_total: 0,
                withheld_classes_ready: 0,
                promotable: false,
                open_proposals: 0,
                wip_limit: Some(3),
                queue_full: false,
                last_gate: None,
                last_gate_at: None,
            }]
        );
    }

    #[tokio::test]
    async fn project_roster_reports_a_full_approval_queue() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, wip_limit)
             VALUES ('project-a', 'active', 2)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode) VALUES ('project-b', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();

        for (run_id, project_id, status) in [
            (1_i64, "project-a", "pending"),
            (2, "project-a", "pending"),
            // Already decided, so it must not count against the ceiling.
            (3, "project-a", "approved"),
            (4, "project-b", "pending"),
        ] {
            sqlx::query(
                "INSERT INTO proposals (kind, status, run_id, project_id, reasoning, created_at)
                 VALUES ('action-approval', ?, ?, ?, 'test', '2026-07-27T00:00:00Z')",
            )
            .bind(status)
            .bind(run_id)
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        let roster = project_roster(&pool).await.unwrap();

        // project-a overrides the ceiling to 2 and has exactly 2 waiting.
        assert_eq!(
            (
                roster[0].open_proposals,
                roster[0].wip_limit,
                roster[0].queue_full
            ),
            (2, Some(2), true)
        );
        // project-b inherits the global default of 3 and is nowhere near it.
        assert_eq!(
            (
                roster[1].open_proposals,
                roster[1].wip_limit,
                roster[1].queue_full
            ),
            (1, Some(3), false)
        );
    }

    /// The roster's `open_proposals` and `wip::open_proposals` are two hand-written copies of the
    /// same query — nothing else stops them drifting apart. Compares the two NUMBERS, not two
    /// hardcoded literals, so a change to one copy that is not mirrored in the other fails this
    /// test rather than silently making the shell disagree with the daemon.
    #[tokio::test]
    async fn o_roster_e_o_portao_contam_o_mesmo() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode) VALUES ('project-a', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO proposals (kind, status, run_id, project_id, reasoning, created_at)
             VALUES ('action-approval', 'pending', 1, 'project-a', 'test', '2026-08-24T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // A shadow-mode unreviewed decision: must count in both readers.
        let shadow_run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-a', 'shadow work', 'completed', 'shadow', '2026-08-24T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO shadow_decisions
             (run_id, tool_name, decision, action_class, classifier_version, created_at)
             VALUES (?, 'Bash', 'allow', 'read-local', 2, '2026-08-24T00:01:00Z')",
        )
        .bind(shadow_run_id)
        .execute(&pool)
        .await
        .unwrap();

        // Five worktree-mode unreviewed decisions: already enforced, must count in NEITHER reader.
        let worktree_run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-a', 'worktree work', 'completed', 'worktree', '2026-08-24T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        for index in 0..5 {
            sqlx::query(
                "INSERT INTO shadow_decisions
                 (run_id, tool_name, decision, action_class, classifier_version, created_at)
                 VALUES (?, 'Bash', 'allow', 'read-local', 2, ?)",
            )
            .bind(worktree_run_id)
            .bind(format!("2026-08-24T00:0{index}:00Z"))
            .execute(&pool)
            .await
            .unwrap();
        }

        let roster = project_roster(&pool).await.unwrap();
        let gate = crate::wip::open_proposals(&pool, "project-a")
            .await
            .unwrap();

        assert_eq!(roster.len(), 1);
        assert_eq!(
            roster[0].open_proposals, gate,
            "the roster's open_proposals must never disagree with the daemon's wip gate"
        );
        assert_eq!(
            gate, 2,
            "1 pending proposal + 1 shadow decision; the 5 worktree decisions must not count"
        );
    }

    #[tokio::test]
    async fn project_mode_defaults_to_off_without_a_row() {
        let pool = test_pool().await;

        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn shadow_mode_round_trips_when_both_prerequisites_exist() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_nucleos_hook(&root);

        set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap();

        assert_eq!(
            project_mode(&pool, "project-a").await.unwrap(),
            Mode::Shadow
        );
    }

    #[tokio::test]
    async fn missing_workflow_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_nucleos_hook(&root);

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::NotOnboarded));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn missing_settings_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn malformed_settings_json_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(&root, "{ this is not valid json ");

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();
        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn settings_without_pretooluse_key_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(&root, r#"{"hooks":{"PostToolUse":[{"command":"x"}]}}"#);

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();
        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    /// The prerequisite is supposed to prove that tool calls will be classified. It only proved
    /// that SOME PreToolUse hook existed — so a formatter, a linter, or anything else at all
    /// satisfied it, and autonomy was activated for a project whose actions nothing would gate.
    #[tokio::test]
    async fn a_pretooluse_hook_that_is_not_ours_rejects_shadow() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(
            &root,
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"npx prettier --write ."}]}]}}"#,
        );

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn the_real_hook_satisfies_the_prerequisite() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(
            &root,
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"python \"${CLAUDE_PROJECT_DIR}/.claude/hooks/ask_daemon.py\""}]}]}}"#,
        );
        std::fs::create_dir_all(root.path().join(".claude/hooks")).unwrap();
        std::fs::write(root.path().join(".claude/hooks/ask_daemon.py"), "#").unwrap();

        set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .expect("the nucleos hook is registered and present");
        assert_eq!(
            project_mode(&pool, "project-a").await.unwrap(),
            Mode::Shadow
        );
    }

    /// Registered but missing from disk is the same as not registered: the CLI would run a hook
    /// command that cannot execute, and a hook that cannot run classifies nothing.
    #[tokio::test]
    async fn a_registered_hook_whose_script_is_absent_rejects_shadow() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(
            &root,
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"python \"${CLAUDE_PROJECT_DIR}/.claude/hooks/ask_daemon.py\""}]}]}}"#,
        );

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::HookNotRegistered));
    }

    #[tokio::test]
    async fn empty_pretooluse_rejects_shadow_and_leaves_mode_off() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_settings(&root, r#"{"hooks":{"PreToolUse":[]}}"#);

        let error = set_project_mode(&pool, "project-a", Mode::Shadow, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::HookNotRegistered));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn active_mode_round_trips_with_all_prerequisites() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_nucleos_hook(&root);
        git_init(&root);

        set_project_mode(&pool, "project-a", Mode::Active, Some(root.path()))
            .await
            .unwrap();

        assert_eq!(
            project_mode(&pool, "project-a").await.unwrap(),
            Mode::Active
        );
        assert_eq!(
            autopilot_projects(&pool).await.unwrap(),
            vec![(
                "project-a".to_owned(),
                root.path().to_string_lossy().into_owned(),
                Mode::Active,
            )]
        );
    }

    #[tokio::test]
    async fn active_mode_rejected_when_not_a_git_repo() {
        let pool = test_pool().await;
        let root = tempfile::tempdir().unwrap();
        write_workflow(&root);
        write_nucleos_hook(&root);

        let error = set_project_mode(&pool, "project-a", Mode::Active, Some(root.path()))
            .await
            .unwrap_err();

        assert!(matches!(error, ActivationError::NotAGitRepo));
        assert_eq!(project_mode(&pool, "project-a").await.unwrap(), Mode::Off);
    }

    #[tokio::test]
    async fn active_mode_still_requires_onboarding_and_hook() {
        let pool = test_pool().await;
        let missing_workflow_root = tempfile::tempdir().unwrap();
        // Everything else in place, so the failure can only be the missing onboarding.
        write_nucleos_hook(&missing_workflow_root);
        git_init(&missing_workflow_root);

        let error = set_project_mode(
            &pool,
            "missing-workflow",
            Mode::Active,
            Some(missing_workflow_root.path()),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ActivationError::NotOnboarded));

        let missing_hook_root = tempfile::tempdir().unwrap();
        write_workflow(&missing_hook_root);
        write_settings(&missing_hook_root, r#"{"hooks":{"PreToolUse":[]}}"#);
        git_init(&missing_hook_root);

        let error = set_project_mode(
            &pool,
            "missing-hook",
            Mode::Active,
            Some(missing_hook_root.path()),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ActivationError::HookNotRegistered));
    }

    #[tokio::test]
    async fn autopilot_projects_lists_shadow_and_active() {
        let pool = test_pool().await;
        let shadow_root = tempfile::tempdir().unwrap();
        write_workflow(&shadow_root);
        write_nucleos_hook(&shadow_root);
        let active_root = tempfile::tempdir().unwrap();
        write_workflow(&active_root);
        write_nucleos_hook(&active_root);
        git_init(&active_root);

        set_project_mode(
            &pool,
            "project-shadow",
            Mode::Shadow,
            Some(shadow_root.path()),
        )
        .await
        .unwrap();
        set_project_mode(
            &pool,
            "project-active",
            Mode::Active,
            Some(active_root.path()),
        )
        .await
        .unwrap();
        set_project_mode(&pool, "project-off", Mode::Off, None)
            .await
            .unwrap();

        assert_eq!(
            autopilot_projects(&pool).await.unwrap(),
            vec![
                (
                    "project-active".to_owned(),
                    active_root.path().to_string_lossy().into_owned(),
                    Mode::Active,
                ),
                (
                    "project-shadow".to_owned(),
                    shadow_root.path().to_string_lossy().into_owned(),
                    Mode::Shadow,
                ),
            ]
        );
    }

    #[tokio::test]
    async fn kill_switch_round_trips() {
        let pool = test_pool().await;

        assert!(!kill_switch_engaged(&pool).await.unwrap());
        set_kill_switch(&pool, true).await.unwrap();
        assert!(kill_switch_engaged(&pool).await.unwrap());
        set_kill_switch(&pool, false).await.unwrap();
        assert!(!kill_switch_engaged(&pool).await.unwrap());
    }

    #[tokio::test]
    async fn scoped_kill_absent_scope_is_not_engaged() {
        let pool = test_pool().await;
        assert!(!scoped_kill_engaged(&pool, "project", "p1").await.unwrap());
    }

    #[tokio::test]
    async fn scoped_kill_set_then_get_round_trips() {
        let pool = test_pool().await;
        set_scoped_kill(&pool, "project", "p1", true).await.unwrap();
        assert!(scoped_kill_engaged(&pool, "project", "p1").await.unwrap());
        set_scoped_kill(&pool, "project", "p1", false)
            .await
            .unwrap();
        assert!(!scoped_kill_engaged(&pool, "project", "p1").await.unwrap());
    }

    #[tokio::test]
    async fn scoped_kills_are_independent_and_listed_ordered() {
        let pool = test_pool().await;
        set_scoped_kill(&pool, "project", "p1", true).await.unwrap();
        set_scoped_kill(&pool, "trigger", "scheduled", true)
            .await
            .unwrap();

        // p2 was never set -> not engaged; the others are independent.
        assert!(!scoped_kill_engaged(&pool, "project", "p2").await.unwrap());
        assert!(scoped_kill_engaged(&pool, "project", "p1").await.unwrap());
        assert!(
            scoped_kill_engaged(&pool, "trigger", "scheduled")
                .await
                .unwrap()
        );

        assert_eq!(
            list_scoped_kills(&pool).await.unwrap(),
            vec![
                ScopedKill {
                    scope_type: "project".into(),
                    scope_id: "p1".into(),
                    engaged: true
                },
                ScopedKill {
                    scope_type: "trigger".into(),
                    scope_id: "scheduled".into(),
                    engaged: true
                },
            ]
        );
    }

    /// Wiring is asserted through the READER, never against the bytes it wrote.
    ///
    /// The gate that decides whether a session may act is `classifier_hook_is_wired`, and a writer
    /// tested against its own output would pass just as happily while writing something that gate
    /// rejects — which is the one failure that matters, and it fails silently: the conversation
    /// simply continues with no tools and nobody is told why.
    #[test]
    fn wiring_a_project_makes_the_gate_say_it_is_wired() {
        let root = TempDir::new().unwrap();
        assert!(!classifier_hook_is_wired(root.path()));

        wire_classifier_hook(root.path()).unwrap();

        assert!(classifier_hook_is_wired(root.path()));
        // And the script is this daemon's, not a stub: the gate checks the file exists, and a hook
        // that cannot answer classifies nothing.
        let written = fs::read_to_string(root.path().join(".claude/hooks/ask_daemon.py")).unwrap();
        assert!(
            written.contains("PreToolUse"),
            "the hook script was not written"
        );
    }

    /// The outcome half of the ledger is wired the moment `PreToolUse` is: without `PostToolUse`
    /// and `PostToolUseFailure` registered too, the CLI never reports what a tool call did, and
    /// `shadow::record_outcome` has nothing arriving to complete.
    #[test]
    fn wiring_a_project_registers_all_three_events() {
        let root = TempDir::new().unwrap();

        wire_classifier_hook(root.path()).unwrap();

        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
            let entries = settings["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("`{event}` was not wired: {settings}"));
            assert!(
                entries
                    .iter()
                    .any(|entry| serde_json::to_string(entry).unwrap().contains(HOOK_SCRIPT)),
                "our hook is missing from `{event}`: {entries:?}"
            );
        }
    }

    #[test]
    fn wiring_keeps_whatever_else_the_settings_already_said() {
        let root = TempDir::new().unwrap();
        write_settings(
            &root,
            r#"{"model":"opus","hooks":{"PostToolUse":[{"matcher":"Edit","hooks":[]}]}}"#,
        );

        wire_classifier_hook(root.path()).unwrap();

        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        // Somebody else's settings file. Taking the tools is not a licence to rewrite the rest of
        // it, and a wiring that ate a person's PostToolUse hook would be a worse bug than the one
        // it fixes.
        assert_eq!(settings["model"], "opus");
        let post_tool_use = settings["hooks"]["PostToolUse"].as_array().unwrap();
        // Their entry survives...
        assert!(
            post_tool_use
                .iter()
                .any(|entry| entry["matcher"] == "Edit"
                    && entry["hooks"].as_array().unwrap().is_empty()),
            "the user's own PostToolUse entry must survive: {post_tool_use:?}"
        );
        // ...and gains ours BESIDE it, because `PostToolUse` is now one of the events this daemon
        // wires too. A writer that treated an existing array as "already handled" and skipped it,
        // or one that replaced it outright, would either leave this ledger blind or eat the
        // user's own hook — the two failures this assertion tells apart.
        assert!(
            post_tool_use
                .iter()
                .any(|entry| serde_json::to_string(entry).unwrap().contains(HOOK_SCRIPT)),
            "our hook must be registered beside theirs: {post_tool_use:?}"
        );
        assert!(classifier_hook_is_wired(root.path()));
    }

    /// A project wired before `PostToolUse`/`PostToolUseFailure` existed has neither entry at
    /// all, and the gate must still say it is wired: the barrier it guarantees was, and remains,
    /// the `PreToolUse` one alone (see the doc comment on `classifier_hook_is_wired`).
    #[test]
    fn a_project_wired_before_the_outcome_pair_existed_still_passes_the_gate() {
        let root = TempDir::new().unwrap();
        write_nucleos_hook(&root);

        assert!(classifier_hook_is_wired(root.path()));
    }

    #[test]
    fn wiring_a_project_twice_leaves_one_entry() {
        let root = TempDir::new().unwrap();

        wire_classifier_hook(root.path()).unwrap();
        wire_classifier_hook(root.path()).unwrap();

        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
        let ours = entries
            .iter()
            .filter(|entry| serde_json::to_string(entry).unwrap().contains(HOOK_SCRIPT))
            .count();
        // Idempotent because it will be pressed twice: the button says what is true now, and
        // pressing it on a project already wired must be a no-op rather than a second hook that
        // asks the daemon about every tool call all over again.
        assert_eq!(ours, 1, "{entries:?}");
    }

    #[test]
    fn settings_that_are_not_json_are_refused_rather_than_replaced() {
        let root = TempDir::new().unwrap();
        write_settings(&root, "{ this is not json");

        let refused = wire_classifier_hook(root.path());

        assert!(refused.is_err());
        // Untouched. A file this cannot parse is a file somebody is in the middle of editing, or
        // one written by a version of the CLI this daemon has never seen — and replacing it would
        // destroy settings nobody asked to lose in order to add one hook.
        assert_eq!(
            fs::read_to_string(root.path().join(".claude/settings.json")).unwrap(),
            "{ this is not json"
        );
    }
}
