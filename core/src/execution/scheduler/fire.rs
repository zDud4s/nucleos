//! Scheduled command execution and job-start helpers.
//! This module owns command concurrency, result recording, and fire-time translation.

use super::*;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};

use crate::config::{self, ScheduleRule};
use crate::state::AppState;

/// Hands a claimed window back, so a busy project retries on the next tick rather than skipping its
/// schedule for the day.
///
/// A compare-and-set on the value just written, so a concurrent tick that has already claimed the
/// next window is not clobbered. Only ever called where nothing was started: past the point where a
/// run row or a job row exists, the window stays spent, because re-firing on a maybe is the
/// duplicate the claim-first ordering exists to prevent.
///
/// The daily allowance is given back in the same statement the claim spent it in: a window that
/// never ran must not count against `DAILY_CAP`, or a rule on a busy project burns the whole day's
/// allowance in retries and then fires as a catch-up after midnight. The claim stamped today's
/// `fires_date` and added one, so subtracting one (floored at zero) undoes exactly that.
pub(super) async fn release_window(
    state: &AppState,
    project_id: &str,
    rule_name: &str,
    previous_fired_at: Option<&str>,
    previous_head_sha: Option<&str>,
    now: DateTime<Utc>,
) {
    let released = sqlx::query(
        "UPDATE scheduler_state
         SET last_fired_at = ?, last_head_sha = ?,
             fires_today = MAX(fires_today - 1, 0)
         WHERE project_id = ? AND rule_name = ? AND last_fired_at = ?",
    )
    .bind(previous_fired_at)
    .bind(previous_head_sha)
    .bind(project_id)
    .bind(rule_name)
    .bind(now.to_rfc3339())
    .execute(&state.pool)
    .await;
    if let Err(error) = released {
        tracing::warn!(
            project_id = %project_id,
            rule_name = %rule_name,
            %error,
            "could not give the scheduler window back; it stays spent"
        );
    }
}

/// Turns a due `graph:` rule into the shape `job::start` takes.
///
/// All that is left here is the translation from "a rule fired" to "a job was asked for": the
/// sequence itself lives in `job.rs`, where the route can reach it too. What stays is the one
/// decision that is genuinely the scheduler's — which number becomes `max_items`.
pub(super) async fn start_job(
    state: &AppState,
    project_id: &str,
    project_root: &str,
    rule: &ScheduleRule,
    graph: &config::GraphConfig,
    head_sha: Option<&str>,
) -> crate::execution::job::JobStart {
    crate::execution::job::start(
        state,
        &crate::execution::job::StartRequest {
            project_id,
            project_root,
            rule_name: Some(&rule.name),
            prompt: &rule.prompt,
            // The daemon's ceiling, not the file's number. The project's `autopilot.yaml` is
            // per-developer
            // and gitignored, so nobody reviews what it asks for; it may lower the fan-out and
            // never raise it.
            max_items: graph.max_items() as i64,
            gate_each: graph.gate_after_each_item,
            review: graph.review,
            // The accessor and never the field, for the same reason `max_items` reads one: the raw
            // number is what the gitignored file asked for, and a retry is a whole run.
            gate_retries: graph.gate_retries() as i64,
            head_sha,
            // A scheduled job asks for neither, which keeps it at one round under the house limit —
            // exactly what a `graph:` rule did before rounds existed. Rounds are opt-in per request,
            // not something a rule already in somebody's `autopilot.yaml` acquires overnight.
            max_rounds: None,
            // The file's number, and here that is safe where `max_rounds` above is not. Both would
            // come from the same unreviewed, per-developer `autopilot.yaml`; the
            // difference is direction. A budget runs UNDER the house limit rather than instead of
            // it, so the only thing this key can do is tighten what the job may spend — there is no
            // value it could hold that buys the job more than the daemon already allows. Rounds
            // loosen: a number there raises how much the daemon will do, which is exactly the kind
            // of decision an unreviewed file does not get to make.
            budget_usd: graph.budget_usd,
            // The file's again, and safe for the same reason the budget is: a team raises no
            // ceiling the daemon enforces. What it changes is who does the work and how much of it
            // happens at once, and the second is still bounded by the project's slot count and by
            // the free-disk floor, both applied per checkout.
            //
            // Absent from every rule written so far, which is what keeps every night already
            // scheduled running exactly as it ran yesterday.
            team_id: graph.team.as_deref(),
        },
    )
    .await
}

/// How much of a command's output is kept on `scheduler_state` and shown in the rules view.
pub(super) const COMMAND_OUTPUT_TAIL_BYTES: usize = 4096;

/// The `(project root, project, rule)` triples whose command is running right now. In memory on
/// purpose: it dies with the daemon, and so do the children (`kill_on_drop` plus the process-tree
/// killer), so there is nothing a restart could leave stale. The root is part of the key so that two
/// independent daemons (or test states) in one process never share a slot, while one daemon still
/// holds exactly one slot per rule.
static COMMANDS_IN_FLIGHT: OnceLock<Mutex<HashSet<(String, String, String)>>> = OnceLock::new();

fn in_flight() -> std::sync::MutexGuard<'static, HashSet<(String, String, String)>> {
    COMMANDS_IN_FLIGHT
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn command_in_flight(project_root: &str, project_id: &str, rule_name: &str) -> bool {
    in_flight().contains(&(
        project_root.to_string(),
        project_id.to_string(),
        rule_name.to_string(),
    ))
}

/// Holds a rule's slot in [`COMMANDS_IN_FLIGHT`] and gives it back however the task ends.
struct InFlight((String, String, String));

impl InFlight {
    /// `None` when the rule's command is already running.
    fn take(project_root: &str, project_id: &str, rule_name: &str) -> Option<Self> {
        let key = (
            project_root.to_string(),
            project_id.to_string(),
            rule_name.to_string(),
        );
        in_flight().insert(key.clone()).then_some(Self(key))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        in_flight().remove(&self.0);
    }
}

/// The last `max` bytes of `text`, moved forward to a character boundary.
fn tail_of(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Writes a command rule's outcome onto its `scheduler_state` row.
pub(super) async fn record_command_result(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    rule_name: &str,
    outcome: &str,
    exit_code: Option<i64>,
    output: &str,
) {
    let output = crate::governance::redact::redact_secrets(output);
    // Redact the whole output first, then cut: a secret straddling the cut would otherwise leave
    // its in-tail suffix too short for the redactor to recognise.
    let output = tail_of(&output, COMMAND_OUTPUT_TAIL_BYTES);
    if let Err(error) = sqlx::query(
        "UPDATE scheduler_state
         SET command_outcome = ?, command_exit_code = ?, command_output = ?, command_ended_at = ?
         WHERE project_id = ? AND rule_name = ?",
    )
    .bind(outcome)
    .bind(exit_code)
    .bind(output)
    .bind(Utc::now().to_rfc3339())
    .bind(project_id)
    .bind(rule_name)
    .execute(pool)
    .await
    {
        tracing::warn!(
            project_id = %project_id,
            rule_name = %rule_name,
            %error,
            "failed to record a scheduled command's result"
        );
    }
}

/// Judges a `command:` rule's command, then runs it on a spawned task so the tick never waits on
/// it. No agent run and no job is created. The window is already claimed when this is called.
///
/// Refused unless the classifier says `allow`: the project's shell rules are the owner's lever for
/// a script the classifier does not know, and an unreadable rules table refuses too.
pub(super) async fn fire_command(
    state: &AppState,
    project_id: &str,
    project_root: &str,
    rule: &ScheduleRule,
    command: &str,
    catch_up: bool,
    late: chrono::Duration,
) {
    let cwd = match rule.cwd.as_deref() {
        Some(relative) => Path::new(project_root).join(relative),
        None => Path::new(project_root).to_path_buf(),
    };
    let lateness = if catch_up {
        format!(" (catch-up, {} late)", humanize_lateness(late))
    } else {
        String::new()
    };

    let refusal = match crate::project::policy::shell_rules(&state.pool, project_id).await {
        Ok(rules) => {
            let judged = crate::governance::classifier::classify(
                "Bash",
                &serde_json::json!({ "command": command }),
                Some(&cwd),
                &crate::integrations::github::Policy::empty(),
                &rules,
                crate::governance::classifier::Unrecognized::AsksAPerson,
            );
            if judged.decision.decision == "allow" {
                None
            } else {
                Some((
                    judged.action_class.to_string(),
                    format!(
                        "{}/{}: {}",
                        judged.decision.decision, judged.action_class, judged.reason
                    ),
                ))
            }
        }
        Err(error) => Some((
            "unreadable-shell-rules".to_string(),
            format!("the project's shell rules could not be read: {error}"),
        )),
    };
    if let Some((action_class, detail)) = refusal {
        record_command_result(
            &state.pool,
            project_id,
            &rule.name,
            "refused",
            None,
            &detail,
        )
        .await;
        let _ = crate::feed::append(
            &state.pool,
            Some(project_id),
            "command_finished",
            &format!(
                "scheduled command '{}' was refused ({action_class}) and did not run{lateness}; \
                 to let it run unattended, declare it allowed for this project \
                 (POST /projects/{project_id}/shell-rules)",
                rule.name
            ),
            None,
            None,
        )
        .await;
        return;
    }

    // Built BEFORE the task and moved into it, so the slot is released however the task ends.
    let Some(guard) = InFlight::take(project_root, project_id, &rule.name) else {
        tracing::info!(
            project_id = %project_id,
            rule_name = %rule.name,
            "a scheduled command is still running; not starting it again"
        );
        return;
    };
    let pool = state.pool.clone();
    let project_id = project_id.to_string();
    let rule_name = rule.name.clone();
    let command = command.to_string();
    tokio::spawn(async move {
        let _guard = guard;
        let (outcome, exit_code, output, summary) = match crate::gate::split_command(&command) {
            Err(error) => (
                "errored",
                None,
                error.clone(),
                format!("could not be measured: {error}"),
            ),
            Ok(words) => {
                let ran = crate::gate::run_argv(
                    &words,
                    &cwd,
                    &[],
                    crate::project::commands::COMMAND_TIMEOUT,
                )
                .await;
                if ran.timed_out {
                    (
                        "errored",
                        None,
                        format!(
                            "timed out after {} seconds\n{}",
                            crate::project::commands::COMMAND_TIMEOUT.as_secs(),
                            ran.tail
                        ),
                        "could not be measured: it timed out".to_string(),
                    )
                } else if let Some(error) = ran.error {
                    (
                        "errored",
                        None,
                        error.clone(),
                        format!("could not be measured: {error}"),
                    )
                } else {
                    match ran.exit_code {
                        Some(0) => ("passed", Some(0), ran.tail, "passed".to_string()),
                        Some(code) => (
                            "failed",
                            Some(i64::from(code)),
                            ran.tail,
                            format!("failed with exit {code}"),
                        ),
                        None => (
                            "errored",
                            None,
                            format!("killed by a signal\n{}", ran.tail),
                            "could not be measured: it was killed by a signal".to_string(),
                        ),
                    }
                }
            }
        };
        record_command_result(&pool, &project_id, &rule_name, outcome, exit_code, &output).await;
        let _ = crate::feed::append(
            &pool,
            Some(&project_id),
            "command_finished",
            &format!("scheduled command '{rule_name}' {summary}{lateness}"),
            None,
            None,
        )
        .await;
    });
}
