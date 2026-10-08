//! §spec workspace-de-projeto
//!
//! The commands a project declares about itself, and what each of them last said.
//!
//! **Not `commands.rs`.** That module reads the slash commands a *conversation* can offer off disk
//! — `.claude/commands/`, a markdown body, expanded by the CLI. These are shell commands somebody
//! declared for a project: `gate`, `fmt`, `typecheck`. Two things named the same word, and the
//! filename is the only place the difference can be stated before somebody opens the wrong one.
//!
//! # What this holds, and what it deliberately does not
//!
//! **Commands that finish.** The design's example list is `gate`, `fmt`, `suite`, `typecheck`,
//! `dashboard`, `tauri dev` — and the last two are not the same kind of thing. They are servers:
//! they never exit. Run through a runner shaped around a verdict, `dashboard` would sit there until
//! the timeout and then report that it timed out, which is the most confusing possible answer to a
//! button that worked. Supervising a long-running process is `sidecar.rs`-shaped work — a pid, a
//! stop, liveness — and it is its own piece. So this registry holds commands with an exit code, and
//! `gate`, `fmt`, `suite` and `typecheck` are exactly that set.
//!
//! **One result, not a history.** The question a button needs answered beside it is whether the
//! thing is green *now*. The trend already exists elsewhere: `project_readings.rs` counts gate
//! verdicts across a project's runs over thirty days. A second history of ad-hoc runs would answer
//! a question nobody has asked.
//!
//! # The rule that keeps this from becoming a drawer
//!
//! §4.6 is anxious about exactly one failure, and it is the one every dashboard reaches: a row of
//! buttons at the foot of the page with no owner. Two rules keep it away. Commands that act on a
//! *thing* live in that thing — reviewing a run is an action about that slot, and it is drawn
//! inside the slot. And of what is left, **the bar shows the gates and the palette holds the rest**:
//! a gate's last verdict is a fact you want without asking, and everything else is a verb you go
//! looking for by name. Nothing lands in the bar by accumulating there.
//!
//! # The overlay
//!
//! §8: the workflow declares, the project overrides — the same shape already used for models and
//! for nodes, so one concept rather than two. [`resolve`] is that rule and it is testable now; the
//! library that installs a workflow's commands is a later slice and plugs in as ROWS.

use std::collections::HashSet;
use std::time::Duration;

use sqlx::SqlitePool;

/// The longest a project command may run before it is taken down.
///
/// A backstop and not a policy: these are checks, and a check that has been going for half an hour
/// has stopped being one. Generous on purpose — this repository's own suite takes about six
/// minutes, and a ceiling that fired on a slow machine would look like a broken command.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The longest name that will be stored. Long enough for a sentence nobody wanted, short enough
/// that the bar cannot be widened by one row.
pub const MAX_NAME: usize = 60;

/// Who may run a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnableBy {
    /// Somebody at the keyboard. The default, and the safe direction.
    Person,
    /// A person, or an autonomous run acting on its own.
    Agent,
}

impl RunnableBy {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
        }
    }

    /// Anything unrecognised reads as `Person`, which is the direction that refuses. A row written
    /// by a future version with a third value must not become permission.
    pub fn from_db_str(value: &str) -> Self {
        match value {
            "agent" => Self::Agent,
            _ => Self::Person,
        }
    }
}

/// Where a command came from — the only thing that explains two commands sharing a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Declared for this project. Wins over a workflow's row of the same name.
    Project,
    /// Brought by the installed workflow. Nothing writes these yet.
    Workflow,
}

impl Source {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Workflow => "workflow",
        }
    }

    /// Unrecognised reads as `Workflow`, which is the losing side of the overlay. A row nobody can
    /// classify must not be able to shadow one somebody wrote deliberately.
    pub fn from_db_str(value: &str) -> Self {
        match value {
            "project" => Self::Project,
            _ => Self::Workflow,
        }
    }
}

/// What a command last did. Five states counting the absent one, and none of them collapses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Started and not finished. Reconciled only by a daemon restart, like `runs.status`.
    Running,
    Passed,
    /// The command ran and its exit code was not the one that passes.
    Failed,
    /// The command could not be measured — it would not start, it timed out, a signal took it.
    /// Never the same as `Failed`: one says the project is broken, the other says we cannot tell.
    Errored,
}

impl Outcome {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Errored => "errored",
        }
    }

    /// `None` for anything unrecognised, INCLUDING for a command nobody has run. A command with no
    /// result is not a command that failed, and the wire keeps them apart by absence.
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "passed" => Some(Self::Passed),
            "failed" => Some(Self::Failed),
            "errored" => Some(Self::Errored),
            _ => None,
        }
    }
}

/// The last thing a command said.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LastRun {
    pub outcome: Outcome,
    pub started_at: String,
    /// `None` while it is still running, and only then.
    pub ended_at: Option<String>,
    /// `None` when there was no exit code at all — a signal, or a command that never started.
    pub exit_code: Option<i64>,
    pub output: Option<String>,
}

/// One declared command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectCommand {
    pub id: i64,
    pub name: String,
    pub command: String,
    /// Relative to the project root. `None` is the root itself.
    pub cwd: Option<String>,
    pub is_gate: bool,
    pub pass_exit_code: i64,
    pub runnable_by: RunnableBy,
    pub source: Source,
    /// `None` for a command nobody has run yet — which is not a command that failed.
    pub last: Option<LastRun>,
}

/// Why a declaration was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Invalid {
    /// A command with no name cannot be found in a palette, which is where most of them live.
    EmptyName,
    NameTooLong,
    /// The words the runner would spawn. Checked HERE rather than at run time, because a command
    /// that can only fail is not worth storing — and the person who typed it is still looking at it.
    EmptyCommand,
    /// `gate::split_command`'s own complaint, usually an unbalanced quote.
    Unsplittable(String),
    /// A `cwd` that is absolute, or that walks out of the project. The path it names is checked
    /// against the filesystem by the caller; this is the part that needs no disk.
    UnsafeCwd,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => write!(formatter, "a command needs a name"),
            Self::NameTooLong => {
                write!(formatter, "that name is longer than {MAX_NAME} characters")
            }
            Self::EmptyCommand => write!(formatter, "a command needs something to run"),
            Self::Unsplittable(reason) => write!(formatter, "{reason}"),
            Self::UnsafeCwd => write!(
                formatter,
                "the working directory must be inside the project"
            ),
        }
    }
}

/// Everything about a declaration that can be decided without a disk or a database.
///
/// Pure and separate so the refusals a person sees while typing are the same rules the row is held
/// to, rather than a second, laxer copy in a form.
pub fn validate(name: &str, command: &str, cwd: Option<&str>) -> Result<(), Invalid> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Invalid::EmptyName);
    }
    if name.chars().count() > MAX_NAME {
        return Err(Invalid::NameTooLong);
    }
    let words = crate::gate::split_command(command).map_err(Invalid::Unsplittable)?;
    if words.is_empty() {
        return Err(Invalid::EmptyCommand);
    }
    if let Some(cwd) = cwd {
        // The same spelling rule `ownership::normalise` enforces, and for the same reason: a
        // backslash names the claimed directory on Windows and a directory whose NAME contains a
        // backslash on Linux, so one request would mean two things.
        let cwd = cwd.trim();
        if cwd.contains('\\') || cwd.starts_with('/') || cwd.as_bytes().get(1) == Some(&b':') {
            return Err(Invalid::UnsafeCwd);
        }
        if cwd.split('/').any(|part| part == "..") {
            return Err(Invalid::UnsafeCwd);
        }
    }
    Ok(())
}

/// The overlay: a project's command shadows the workflow's of the same name.
///
/// Gates first and then alphabetical, which is the order both surfaces want — the bar takes the
/// head of the list and the palette shows all of it, so one ordering serves both and they cannot
/// disagree about which command is "the first one".
pub fn resolve(rows: Vec<ProjectCommand>) -> Vec<ProjectCommand> {
    let overridden: HashSet<String> = rows
        .iter()
        .filter(|row| row.source == Source::Project)
        .map(|row| row.name.clone())
        .collect();
    let mut kept: Vec<ProjectCommand> = rows
        .into_iter()
        .filter(|row| row.source == Source::Project || !overridden.contains(&row.name))
        .collect();
    kept.sort_by(|a, b| b.is_gate.cmp(&a.is_gate).then_with(|| a.name.cmp(&b.name)));
    kept
}

/// A finished run, ready to be written back.
#[derive(Debug, PartialEq, Eq)]
pub struct Finished {
    pub outcome: Outcome,
    pub exit_code: Option<i64>,
    pub output: Option<String>,
}

/// `gate::run_gate`'s answer, read against THIS command's rule about what passing means.
///
/// Three things this must get right, and each of them is a state the design forbids collapsing:
///
/// - **A measurement that did not happen is never a pass.** `Errored` stays `Errored` whatever
///   `pass_exit_code` says. A process killed by a signal has no exit code at all, so no value can
///   match it — that is `classify_exit`'s property, and the test below asserts it rather than
///   trusting it.
/// - **Exit 0 is not automatically a pass.** For a command that counts 1 as passing — a linter that
///   exits 1 for warnings — a zero exit is a failure, and it is reported as one.
/// - **That last case loses its output, and the sentence says so.** `GateOutcome::Passed` carries
///   no tail: `run_gate` keeps output only for a non-zero exit, because for a gate zero is success
///   and success has nothing to read. Rather than pretend, the record carries a line saying what
///   happened and that there is nothing to show.
pub fn verdict(pass_exit_code: i64, outcome: crate::gate::GateOutcome) -> Finished {
    match outcome {
        crate::gate::GateOutcome::Errored { reason } => Finished {
            outcome: Outcome::Errored,
            exit_code: None,
            output: Some(reason),
        },
        crate::gate::GateOutcome::Passed => {
            if pass_exit_code == 0 {
                Finished {
                    outcome: Outcome::Passed,
                    exit_code: Some(0),
                    output: None,
                }
            } else {
                Finished {
                    outcome: Outcome::Failed,
                    exit_code: Some(0),
                    output: Some(format!(
                        "exited 0, which this command does not count as passing ({pass_exit_code} does); \
                         no output is kept for a zero exit"
                    )),
                }
            }
        }
        crate::gate::GateOutcome::Failed { exit_code, output } => Finished {
            outcome: if i64::from(exit_code) == pass_exit_code {
                Outcome::Passed
            } else {
                Outcome::Failed
            },
            exit_code: Some(i64::from(exit_code)),
            output: Some(output),
        },
    }
}

/// Whether this key is unambiguously an agent's rather than a person's.
///
/// **`Control` is deliberately absent, and that is a real limit rather than an oversight.** An
/// orchestrator turn carries the human's key by design, because it acts for the user — so no check
/// at this layer can tell a person from an orchestrator turn, and one that claimed to would be
/// worse than none. What it CAN do is refuse the keys that belong to nobody but an agent.
///
/// `Run` cannot reach this route at all today — it opens exactly one route, the safety gate — so
/// this is belt and braces. It is worth having anyway: the day a run is given a way in, the field
/// is already load-bearing rather than decorative.
pub fn caller_is_an_agent(scope: &crate::auth::Scope) -> bool {
    matches!(
        scope,
        crate::auth::Scope::Run(_)
            | crate::auth::Scope::TeamRun(_)
            | crate::auth::Scope::Service(_)
    )
}

/* ------------------------------------------------------------------- SQL -- */

const COLUMNS: &str = "id, name, command, cwd, is_gate, pass_exit_code, runnable_by, source, \
                       last_started_at, last_ended_at, last_outcome, last_exit_code, last_output";

type Row = (
    i64,
    String,
    String,
    Option<String>,
    i64,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

fn from_row(row: Row) -> ProjectCommand {
    let (
        id,
        name,
        command,
        cwd,
        is_gate,
        pass_exit_code,
        runnable_by,
        source,
        started_at,
        ended_at,
        outcome,
        exit_code,
        output,
    ) = row;
    // A result needs BOTH a start and a recognised outcome. A row with one and not the other is a
    // half-written record, and reporting it as a run would be reporting a measurement nobody has.
    let last = match (
        started_at,
        outcome.as_deref().and_then(Outcome::from_db_str),
    ) {
        (Some(started_at), Some(outcome)) => Some(LastRun {
            outcome,
            started_at,
            ended_at,
            exit_code,
            output,
        }),
        _ => None,
    };
    ProjectCommand {
        id,
        name,
        command,
        cwd,
        is_gate: is_gate != 0,
        pass_exit_code,
        runnable_by: RunnableBy::from_db_str(&runnable_by),
        source: Source::from_db_str(&source),
        last,
    }
}

/// Every command this project offers, overlay applied.
pub async fn list(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<ProjectCommand>> {
    // `AssertSqlSafe` because sqlx 0.9 only trusts `&'static str` by default, and the same
    // argument `shadow.rs` makes applies: the one interpolated fragment is the private `COLUMNS`
    // const above, and the project id stays a bound parameter — no caller input reaches the text.
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_commands WHERE project_id = ?"
    )))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(resolve(rows.into_iter().map(from_row).collect()))
}

/// One command of this project's, by id. `None` when it is not there or belongs to another project.
pub async fn get(
    pool: &SqlitePool,
    project_id: &str,
    id: i64,
) -> sqlx::Result<Option<ProjectCommand>> {
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM project_commands WHERE id = ? AND project_id = ?"
    )))
    .bind(id)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(from_row))
}

/// What a caller asked to declare.
pub struct Declaration {
    pub name: String,
    pub command: String,
    pub cwd: Option<String>,
    pub is_gate: bool,
    pub pass_exit_code: i64,
    pub runnable_by: RunnableBy,
}

/// Declares a project's own command, replacing one of the same name.
///
/// An upsert rather than a create-or-409, because the name IS the identity here: a person editing
/// `gate` is editing the `gate` they already have, and making them delete it first would be
/// ceremony over a form. `source = 'project'` is not a parameter — nothing outside the workflow
/// library may write a workflow's row, and offering the choice would be offering a way to forge
/// one.
///
/// The last result is deliberately NOT cleared on re-declaration. Editing what `gate` runs does not
/// unhappen the last time it ran, and blanking the verdict would turn an edit into a silent "not
/// measured" on the panel that answers *is this green*.
pub async fn declare(
    pool: &SqlitePool,
    project_id: &str,
    declaration: Declaration,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO project_commands
             (project_id, name, command, cwd, is_gate, pass_exit_code, runnable_by, source, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(project_id, source, name) DO UPDATE SET
             command = excluded.command,
             cwd = excluded.cwd,
             is_gate = excluded.is_gate,
             pass_exit_code = excluded.pass_exit_code,
             runnable_by = excluded.runnable_by
         RETURNING id",
    )
    .bind(project_id)
    .bind(declaration.name.trim())
    .bind(&declaration.command)
    .bind(&declaration.cwd)
    .bind(i64::from(declaration.is_gate))
    .bind(declaration.pass_exit_code)
    .bind(declaration.runnable_by.as_db_str())
    // Bound from the enum rather than written as a literal, so the value here and the value
    // `from_db_str` reads back cannot drift apart in a later edit.
    .bind(Source::Project.as_db_str())
    .bind(&now)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Forgets one of this project's commands. `false` when there was none to forget.
pub async fn remove(pool: &SqlitePool, project_id: &str, id: i64) -> sqlx::Result<bool> {
    let done = sqlx::query("DELETE FROM project_commands WHERE id = ? AND project_id = ?")
        .bind(id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Claims a command for a run. `false` when it is already running.
///
/// The claim is the `WHERE`, not a read followed by a write: two clicks landing together would both
/// pass a check-then-set and spawn the suite twice in one directory.
pub async fn mark_running(pool: &SqlitePool, project_id: &str, id: i64) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let claimed = sqlx::query(
        "UPDATE project_commands
            SET last_started_at = ?, last_ended_at = NULL, last_outcome = 'running',
                last_exit_code = NULL, last_output = NULL
          WHERE id = ? AND project_id = ?
            AND (last_outcome IS NULL OR last_outcome <> 'running')",
    )
    .bind(&now)
    .bind(id)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(claimed.rows_affected() > 0)
}

/// Writes back what a command said.
pub async fn finish(pool: &SqlitePool, id: i64, finished: Finished) -> sqlx::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE project_commands
            SET last_ended_at = ?, last_outcome = ?, last_exit_code = ?, last_output = ?
          WHERE id = ?",
    )
    .bind(&now)
    .bind(finished.outcome.as_db_str())
    .bind(finished.exit_code)
    .bind(&finished.output)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks every command still `running` as `errored` — once at startup, beside
/// `runs::reconcile_orphaned_runs` and for the same reason.
///
/// `errored` and not `failed`, which is the whole point: a daemon that stopped mid-command tells us
/// nothing about the project. A row left saying `running` for ever would be worse still — the
/// button would refuse every future click with "already running", against a process that has not
/// existed since the last restart.
pub async fn reconcile_orphaned_commands(pool: &SqlitePool) -> sqlx::Result<u64> {
    let now = chrono::Utc::now().to_rfc3339();
    let done = sqlx::query(
        "UPDATE project_commands
            SET last_ended_at = ?, last_outcome = 'errored',
                last_output = 'the daemon restarted while this was running, so nothing was measured'
          WHERE last_outcome = 'running'",
    )
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::GateOutcome;

    fn command(name: &str, source: Source, is_gate: bool) -> ProjectCommand {
        ProjectCommand {
            id: 1,
            name: name.to_owned(),
            command: "cargo test".to_owned(),
            cwd: None,
            is_gate,
            pass_exit_code: 0,
            runnable_by: RunnableBy::Person,
            source,
            last: None,
        }
    }

    /* ------------------------------------------------------------ overlay -- */

    /// The overlay in one assertion: a project's row shadows the workflow's of the same name, and
    /// leaves the ones it does not name alone.
    #[test]
    fn a_projects_command_shadows_the_workflows_of_the_same_name() {
        let resolved = resolve(vec![
            command("gate", Source::Workflow, true),
            command("gate", Source::Project, true),
            command("fmt", Source::Workflow, false),
        ]);

        let names: Vec<&str> = resolved.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["gate", "fmt"]);
        assert_eq!(resolved[0].source, Source::Project);
        // The one nobody overrode is still the workflow's, not silently reattributed.
        assert_eq!(resolved[1].source, Source::Workflow);
    }

    /// Gates lead, then alphabetical. One ordering for both surfaces, so the bar and the palette
    /// cannot come to disagree about which command is first.
    #[test]
    fn gates_lead_and_the_rest_are_alphabetical() {
        let resolved = resolve(vec![
            command("typecheck", Source::Project, false),
            command("suite", Source::Project, true),
            command("fmt", Source::Project, false),
            command("gate", Source::Project, true),
        ]);
        let names: Vec<&str> = resolved.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["gate", "suite", "fmt", "typecheck"]);
    }

    /* ---------------------------------------------------------- verdicts -- */

    /// A measurement that did not happen is never a pass, whatever the row says passing is.
    ///
    /// The sharp case is a signal: the OOM killer reaping a suite leaves no exit code at all, and
    /// `classify_exit` turns that into `Errored`. This asserts the property rather than trusting
    /// it, because the alternative — a `pass_exit_code` that could somehow match "no code" — would
    /// turn a machine running out of memory into a green gate.
    #[test]
    fn a_measurement_that_did_not_happen_is_never_a_pass() {
        for pass_code in [0, 1, -1] {
            let finished = verdict(
                pass_code,
                GateOutcome::Errored {
                    reason: "gate command timed out".to_owned(),
                },
            );
            assert_eq!(finished.outcome, Outcome::Errored, "pass code {pass_code}");
            assert_eq!(finished.exit_code, None);
            assert!(finished.output.unwrap().contains("timed out"));
        }
    }

    /// The ordinary case, both ways round.
    #[test]
    fn zero_passes_a_command_that_counts_zero_and_fails_one_that_does_not() {
        let passed = verdict(0, GateOutcome::Passed);
        assert_eq!(passed.outcome, Outcome::Passed);
        assert_eq!(passed.exit_code, Some(0));
        assert_eq!(passed.output, None);

        // A linter that exits 1 for warnings and 0 for "nothing to say" is not what this command
        // was set up to accept, and the record says so instead of quietly agreeing.
        let refused = verdict(1, GateOutcome::Passed);
        assert_eq!(refused.outcome, Outcome::Failed);
        assert_eq!(refused.exit_code, Some(0));
        assert!(
            refused
                .output
                .unwrap()
                .contains("does not count as passing")
        );
    }

    /// A non-zero exit passes when it is the one this command named, and fails when it is not — and
    /// the output survives either way, which is the half that makes a failure diagnosable.
    #[test]
    fn a_named_exit_code_passes_and_its_output_is_kept() {
        let passed = verdict(
            1,
            GateOutcome::Failed {
                exit_code: 1,
                output: "3 warnings".to_owned(),
            },
        );
        assert_eq!(passed.outcome, Outcome::Passed);
        assert_eq!(passed.output.as_deref(), Some("3 warnings"));

        let failed = verdict(
            1,
            GateOutcome::Failed {
                exit_code: 2,
                output: "could not parse".to_owned(),
            },
        );
        assert_eq!(failed.outcome, Outcome::Failed);
        assert_eq!(failed.exit_code, Some(2));
        assert_eq!(failed.output.as_deref(), Some("could not parse"));
    }

    /// A verdict is never `Running`. That state is written by the claim and cleared by the finish,
    /// and a path that could produce it here would be a command that finished and still said it
    /// had not.
    #[test]
    fn a_finished_command_is_never_still_running() {
        for outcome in [
            GateOutcome::Passed,
            GateOutcome::Failed {
                exit_code: 1,
                output: String::new(),
            },
            GateOutcome::Errored {
                reason: String::new(),
            },
        ] {
            assert_ne!(verdict(0, outcome).outcome, Outcome::Running);
        }
    }

    /* -------------------------------------------------------- validation -- */

    #[test]
    fn a_command_needs_a_name_and_something_to_run() {
        assert_eq!(validate("", "cargo test", None), Err(Invalid::EmptyName));
        assert_eq!(validate("   ", "cargo test", None), Err(Invalid::EmptyName));
        assert_eq!(
            validate(&"x".repeat(MAX_NAME + 1), "cargo test", None),
            Err(Invalid::NameTooLong)
        );
        assert_eq!(validate("gate", "", None), Err(Invalid::EmptyCommand));
        assert_eq!(validate("gate", "   ", None), Err(Invalid::EmptyCommand));
        assert!(validate("gate", "cargo test", None).is_ok());
    }

    /// An unbalanced quote is caught while the person who typed it is still looking at it, rather
    /// than becoming a button that errors the first time somebody presses it.
    #[test]
    fn a_command_the_runner_could_not_split_is_refused_when_it_is_written() {
        let refusal = validate("gate", "bash -c \"cargo test", None).expect_err("unbalanced quote");
        assert!(matches!(refusal, Invalid::Unsplittable(_)), "{refusal:?}");
    }

    /// A working directory outside the project is refused, and a backslash is refused with it — for
    /// the reason `ownership::normalise` gives: it names the claimed directory on Windows and a
    /// directory whose NAME contains a backslash on Linux, so one request would mean two things.
    #[test]
    fn a_working_directory_must_stay_inside_the_project() {
        for cwd in [
            "../elsewhere",
            "shell/../../elsewhere",
            "/etc",
            "C:/Windows",
            "shell\\src",
        ] {
            assert_eq!(
                validate("dev", "npm run dev", Some(cwd)),
                Err(Invalid::UnsafeCwd),
                "{cwd}"
            );
        }
        assert!(validate("dev", "npm run dev", Some("shell")).is_ok());
        assert!(validate("dev", "npm run dev", Some("shell/src")).is_ok());
    }

    /* ------------------------------------------------------------- scope -- */

    /// The keys that are nobody's but an agent's, and the one that is deliberately not on the list.
    #[test]
    fn only_a_key_that_is_nobodys_but_an_agents_is_treated_as_one() {
        use crate::auth::{ApiTokenLevel, Scope};

        assert!(caller_is_an_agent(&Scope::Run(7)));
        assert!(caller_is_an_agent(&Scope::TeamRun("run-1".to_owned())));

        // The human's key, and the limit worth writing down: an orchestrator turn carries it too,
        // by design, because it acts for the user. Nothing at this layer can tell them apart, and a
        // check that claimed to would be worse than none.
        assert!(!caller_is_an_agent(&Scope::Control));
        assert!(!caller_is_an_agent(&Scope::ApiToken(ApiTokenLevel::Admin)));
    }

    /// An unrecognised value in either enum falls to the side that refuses. A row written by a
    /// newer version must not become permission, and must not be able to shadow one somebody wrote.
    #[test]
    fn an_unrecognised_row_falls_to_the_refusing_side() {
        assert_eq!(RunnableBy::from_db_str("agent"), RunnableBy::Agent);
        assert_eq!(RunnableBy::from_db_str("person"), RunnableBy::Person);
        assert_eq!(RunnableBy::from_db_str("everyone"), RunnableBy::Person);
        assert_eq!(RunnableBy::from_db_str(""), RunnableBy::Person);

        assert_eq!(Source::from_db_str("project"), Source::Project);
        assert_eq!(Source::from_db_str("bundle"), Source::Workflow);
    }

    /* ---------------------------------------------------------------- db -- */

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn project(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'shadow')")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Declaring the same name twice edits the command rather than making a second one — the name
    /// is the identity — **and does not unhappen the last time it ran.**
    #[tokio::test]
    async fn a_second_declaration_edits_the_first_and_keeps_its_last_result() {
        let pool = pool().await;
        project(&pool, "alpha").await;

        let id = declare(
            &pool,
            "alpha",
            Declaration {
                name: "gate".to_owned(),
                command: "cargo test".to_owned(),
                cwd: None,
                is_gate: true,
                pass_exit_code: 0,
                runnable_by: RunnableBy::Person,
            },
        )
        .await
        .unwrap();

        assert!(mark_running(&pool, "alpha", id).await.unwrap());
        finish(
            &pool,
            id,
            Finished {
                outcome: Outcome::Passed,
                exit_code: Some(0),
                output: None,
            },
        )
        .await
        .unwrap();

        let again = declare(
            &pool,
            "alpha",
            Declaration {
                name: "gate".to_owned(),
                command: "cargo clippy".to_owned(),
                cwd: Some("core".to_owned()),
                is_gate: true,
                pass_exit_code: 0,
                runnable_by: RunnableBy::Agent,
            },
        )
        .await
        .unwrap();
        assert_eq!(again, id, "the name is the identity");

        let rows = list(&pool, "alpha").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].command, "cargo clippy");
        assert_eq!(rows[0].cwd.as_deref(), Some("core"));
        assert_eq!(rows[0].runnable_by, RunnableBy::Agent);
        // Editing what `gate` runs does not unhappen the last time it ran. Blanking this would put
        // "not measured" on the panel that answers *is this green*.
        assert_eq!(rows[0].last.as_ref().unwrap().outcome, Outcome::Passed);
    }

    /// The claim is the `WHERE`. Two clicks landing together must not both spawn the suite in one
    /// directory, and a check-then-set would let them.
    #[tokio::test]
    async fn a_command_already_running_cannot_be_started_again() {
        let pool = pool().await;
        project(&pool, "alpha").await;
        let id = declare(
            &pool,
            "alpha",
            Declaration {
                name: "gate".to_owned(),
                command: "cargo test".to_owned(),
                cwd: None,
                is_gate: true,
                pass_exit_code: 0,
                runnable_by: RunnableBy::Person,
            },
        )
        .await
        .unwrap();

        assert!(mark_running(&pool, "alpha", id).await.unwrap());
        assert!(!mark_running(&pool, "alpha", id).await.unwrap());

        finish(
            &pool,
            id,
            Finished {
                outcome: Outcome::Failed,
                exit_code: Some(101),
                output: Some("2 tests failed".to_owned()),
            },
        )
        .await
        .unwrap();
        // Finished, so it can go again.
        assert!(mark_running(&pool, "alpha", id).await.unwrap());
    }

    /// Another project's id does not reach this project's command — the guard that makes an integer
    /// id in a URL safe.
    #[tokio::test]
    async fn a_command_belongs_to_its_project_and_to_no_other() {
        let pool = pool().await;
        project(&pool, "alpha").await;
        project(&pool, "beta").await;
        let id = declare(
            &pool,
            "alpha",
            Declaration {
                name: "gate".to_owned(),
                command: "cargo test".to_owned(),
                cwd: None,
                is_gate: false,
                pass_exit_code: 0,
                runnable_by: RunnableBy::Person,
            },
        )
        .await
        .unwrap();

        assert!(get(&pool, "beta", id).await.unwrap().is_none());
        assert!(!mark_running(&pool, "beta", id).await.unwrap());
        assert!(!remove(&pool, "beta", id).await.unwrap());
        assert!(get(&pool, "alpha", id).await.unwrap().is_some());
        assert!(remove(&pool, "alpha", id).await.unwrap());
    }

    /// A restart settles a row that says `running`, as `errored` — because a daemon that stopped
    /// mid-command measured nothing. Left alone it would refuse every future click with "already
    /// running", against a process that has not existed since the last restart.
    #[tokio::test]
    async fn a_restart_settles_a_command_that_was_running() {
        let pool = pool().await;
        project(&pool, "alpha").await;
        let id = declare(
            &pool,
            "alpha",
            Declaration {
                name: "suite".to_owned(),
                command: "cargo test".to_owned(),
                cwd: None,
                is_gate: true,
                pass_exit_code: 0,
                runnable_by: RunnableBy::Person,
            },
        )
        .await
        .unwrap();
        assert!(mark_running(&pool, "alpha", id).await.unwrap());

        assert_eq!(reconcile_orphaned_commands(&pool).await.unwrap(), 1);
        let row = get(&pool, "alpha", id).await.unwrap().unwrap();
        let last = row.last.unwrap();
        assert_eq!(last.outcome, Outcome::Errored);
        assert!(last.ended_at.is_some());
        assert!(last.output.unwrap().contains("daemon restarted"));

        // And it can be started again, which is the point of settling it.
        assert!(mark_running(&pool, "alpha", id).await.unwrap());
    }

    /// A command nobody has run has no result — not a failed one, and not a zeroed one.
    #[tokio::test]
    async fn a_command_nobody_has_run_reports_no_result_at_all() {
        let pool = pool().await;
        project(&pool, "alpha").await;
        declare(
            &pool,
            "alpha",
            Declaration {
                name: "fmt".to_owned(),
                command: "cargo fmt --check".to_owned(),
                cwd: None,
                is_gate: false,
                pass_exit_code: 0,
                runnable_by: RunnableBy::Person,
            },
        )
        .await
        .unwrap();

        let rows = list(&pool, "alpha").await.unwrap();
        assert_eq!(rows[0].last, None);
    }
}
