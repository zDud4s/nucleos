//! §spec trabalho-noturno-e-jobs-paralelos

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;

use crate::autopilot::Mode;
use crate::config::{self, ScheduleRule};
use crate::runs::{CreateRunError, create_run_inner};
use crate::state::AppState;

/// The daemon checks schedules twice per minute so boundary detection remains prompt.
const TICK: Duration = Duration::from_secs(30);
/// A rule cannot create more than one run per minute, even if it uses second-level cron syntax.
const MIN_INTERVAL: chrono::Duration = chrono::Duration::minutes(1);
/// Phase 1 limits each project rule to 24 runs per daemon day.
pub const DAILY_CAP: u32 = 24;
/// How late a due window must be before its run counts as a catch-up. Normal scheduling lands within
/// one `TICK`; this far behind means the daemon or the machine was genuinely unavailable, not merely
/// busy — which is the difference between "a bit late" and "running on days-old assumptions".
const CATCH_UP_GRACE_MINUTES: i64 = 15;

/// Whether a due window came up so long ago that the daemon plainly was not running to serve it.
pub fn is_catch_up(due_at: DateTime<Utc>, now: DateTime<Utc>, grace: chrono::Duration) -> bool {
    now.signed_duration_since(due_at) > grace
}

/// A quota hold excuses only windows that were still within their on-time grace when it began.
pub fn lateness_origin(
    due_at: DateTime<Utc>,
    hold: Option<(DateTime<Utc>, DateTime<Utc>)>,
) -> DateTime<Utc> {
    match hold {
        // A rule still inside its on-time grace when the quota closed was delayed by the
        // quota; one already late before it was not. A rule due after reopening was not held.
        Some((start, end))
            if start <= due_at + chrono::Duration::minutes(CATCH_UP_GRACE_MINUTES)
                && due_at < end =>
        {
            end
        }
        _ => due_at,
    }
}

fn humanize_lateness(late: chrono::Duration) -> String {
    let minutes = late.num_minutes().max(0);
    match minutes {
        0..=59 => format!("{minutes} minutes"),
        60..=1439 => format!("{} hours", minutes / 60),
        _ => format!("{} days", minutes / (60 * 24)),
    }
}

/// The preamble a catch-up run carries ahead of its rule's own prompt.
///
/// Two separate warnings, because they answer different risks: being late says the surrounding work
/// may have moved on, while a moved HEAD says the plan itself may no longer apply. The run is always
/// plan-only, so the worst case is a proposal worth rejecting rather than a bad commit — but the
/// model still needs to be told, or it will confidently execute a stale plan as if it were fresh.
pub fn catch_up_preamble(late: chrono::Duration, head_moved: bool) -> String {
    let mut preamble = format!(
        "This is a CATCH-UP run: its scheduled window was missed and it is starting {} late, \
         so the assumptions behind this task may be out of date.",
        humanize_lateness(late)
    );
    if head_moved {
        preamble.push_str(
            " The repository HEAD has also moved since this window was scheduled. Re-triage before \
             planning: check the task against the current state of the repository, and if it no \
             longer makes sense, say so instead of carrying out the original plan.",
        );
    }
    preamble.push_str(" You are in plan-only mode — propose, do not act.");
    preamble
}

/// PURE: the zone a rule's cron is read in.
///
/// Absent means UTC, which is what every rule written before the field meant, so adding it moves
/// nothing. An unknown name is an error rather than a fallback to UTC: silently reading
/// `Europe/Lisbon` as UTC would fire the rule an hour off and look like it worked.
pub fn rule_timezone(rule: &ScheduleRule) -> Result<Tz, String> {
    timezone_named(rule.timezone.as_deref())
}

/// The same question asked of a loose string, for a rule that is not a `ScheduleRule` yet.
///
/// Split out rather than copied, because a second answer to "is this a zone" would be a second
/// answer to whether a rule fires at 08:00 or at 09:00.
pub fn timezone_named(name: Option<&str>) -> Result<Tz, String> {
    match name {
        None => Ok(Tz::UTC),
        Some(name) => name
            .parse::<Tz>()
            .map_err(|_| format!("'{name}' is not an IANA timezone")),
    }
}

/// PURE: the instant a one-shot rule fires at, `None` when the rule has no `at:`.
///
/// ISO 8601 with an offset is taken as written; a naive `YYYY-MM-DDTHH:MM[:SS]` is read in the
/// rule's own zone (UTC when absent). An error is returned, not guessed around, so the arming
/// check can announce it once.
pub fn at_instant(rule: &ScheduleRule) -> Option<Result<DateTime<Utc>, String>> {
    let at = rule.at.as_deref()?;
    let at = at.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(at) {
        return Some(Ok(parsed.with_timezone(&Utc)));
    }
    let naive = NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M"));
    let Ok(naive) = naive else {
        return Some(Err(format!("'{at}' is not an ISO 8601 date-time")));
    };
    let zone = match rule_timezone(rule) {
        Ok(zone) => zone,
        Err(error) => return Some(Err(error)),
    };
    Some(
        zone.from_local_datetime(&naive)
            .earliest()
            .map(|local| local.with_timezone(&Utc))
            .ok_or_else(|| format!("'{at}' does not exist in {zone}")),
    )
}

/// PURE: when a rule fires next after `since`, or why it never will.
///
/// The same parse, the same zone handling and the same anchor as `due_rules` — deliberately, because
/// this exists to be SHOWN, and a screen that computed "next at 08:00" by a second route would
/// eventually disagree with the tick that actually fires it.
///
/// The error side is the point as much as the success side. An invalid cron or an unknown timezone
/// makes `due_rules` skip the rule and log at debug, which repeats 2,880 times a day and is
/// therefore invisible: the rule simply never runs, and nothing anywhere says so. Returned here, it
/// becomes something a person can read.
pub fn next_fire(rule: &ScheduleRule, since: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    if let Some(at) = at_instant(rule) {
        let at = at?;
        return if at > since {
            Ok(at)
        } else {
            Err(format!(
                "a one-shot rule fires at most once; its at ({at}) is not after {since}"
            ))
        };
    }
    next_occurrence(&rule.cron, rule.timezone.as_deref(), since)
}

/// [`next_fire`] over loose fields, for the same reason [`timezone_named`] exists: a rule can be
/// checked before there is a rule.
///
/// "Is this cron readable" and "does this zone exist" are two of the three ways a rule never fires;
/// the third is a cron that parses and has no next occurrence (`0 0 30 2 *` — the thirtieth of
/// February), which only a search for the next one can tell you. All three arrive here as an error
/// a person can read.
pub fn next_occurrence(
    cron: &str,
    timezone: Option<&str>,
    since: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let parsed = cron
        .parse::<Cron>()
        .map_err(|error| format!("'{cron}' is not a cron expression: {error}"))?;
    let zone = timezone_named(timezone)?;
    parsed
        .find_next_occurrence(&since.with_timezone(&zone), false)
        .map(|next| next.with_timezone(&Utc))
        .map_err(|error| format!("no next occurrence for '{cron}': {error}"))
}

/// Returns each due rule paired with the occurrence that made it due, so the caller can tell a run
/// served on time from one recovering a window missed hours or days ago.
pub fn due_rules<'a>(
    rules: &'a [ScheduleRule],
    last_fired: &HashMap<String, DateTime<Utc>>,
    fires_today: &HashMap<String, u32>,
    now: DateTime<Utc>,
    min_interval: chrono::Duration,
    daily_cap: u32,
) -> Vec<(&'a ScheduleRule, DateTime<Utc>)> {
    rules
        .iter()
        .filter_map(|rule| {
            // A one-shot rule: due while armed before its instant and the instant has passed. The
            // claim stamps `last_fired_at = now >= at`, so it can never be due again.
            if let Some(at) = at_instant(rule) {
                let at = match at {
                    Ok(at) => at,
                    Err(error) => {
                        tracing::debug!(
                            rule_name = %rule.name,
                            error = %error,
                            "skipping a one-shot schedule rule with an unreadable at"
                        );
                        return None;
                    }
                };
                let last = last_fired.get(&rule.name)?;
                return (*last < at && at <= now).then_some((rule, at));
            }

            let cron = match rule.cron.parse::<Cron>() {
                Ok(cron) => cron,
                // Debug, not warn: this runs every 30 seconds for as long as the rule exists, and
                // the same fact is announced once to the feed when the rule is armed. A line
                // repeated 2,880 times a day is not a louder warning, it is a quieter log.
                Err(error) => {
                    tracing::debug!(
                        rule_name = %rule.name,
                        cron = %rule.cron,
                        error = %error,
                        "skipping invalid schedule rule"
                    );
                    return None;
                }
            };

            let zone = match rule_timezone(rule) {
                Ok(zone) => zone,
                Err(error) => {
                    tracing::debug!(
                        rule_name = %rule.name,
                        error = %error,
                        "skipping schedule rule with an unknown timezone"
                    );
                    return None;
                }
            };

            let last_fired_at = last_fired.get(&rule.name)?;

            if now.signed_duration_since(*last_fired_at) < min_interval {
                return None;
            }

            if fires_today.get(&rule.name).copied().unwrap_or(0) >= daily_cap {
                return None;
            }

            // Anchored in the rule's own zone, so `0 8 * * *` means 08:00 there — including across
            // a DST change, when the wall-clock hour the user wrote and the UTC hour it lands on
            // stop agreeing. `croner` reads the fields off whatever zone the anchor carries, so
            // this conversion is the whole mechanism.
            match cron.find_next_occurrence(&last_fired_at.with_timezone(&zone), false) {
                Ok(next) if next.with_timezone(&Utc) <= now => {
                    Some((rule, next.with_timezone(&Utc)))
                }
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(
                        rule_name = %rule.name,
                        cron = %rule.cron,
                        error = %error,
                        "could not calculate the next schedule occurrence"
                    );
                    None
                }
            }
        })
        .collect()
}

/// One rule's persisted scheduler state, as the tick reads it.
#[derive(sqlx::FromRow)]
struct RuleState {
    rule_name: String,
    /// Kept verbatim rather than parsed, because it is the value the claim compare-and-sets
    /// against: a round trip through `DateTime` can change the spelling (offset form, sub-second
    /// digits) without changing the instant, and then the claim matches nothing.
    last_fired_at: String,
    last_head_sha: Option<String>,
    /// The day `fires_today` counts for. A count carrying another day's date is a count of nothing,
    /// which is what makes the daily reset a comparison rather than a sweep somebody has to run at
    /// midnight.
    fires_date: Option<String>,
    fires_today: i64,
}

pub async fn run_scheduler(state: AppState) {
    let mut interval = tokio::time::interval(TICK);
    loop {
        interval.tick().await;
        scheduler_tick(&state, Utc::now()).await;
    }
}

/// Hands a claimed window back, so a busy project retries on the next tick rather than skipping its
/// schedule for the day.
///
/// A compare-and-set on the value just written, so a concurrent tick that has already claimed the
/// next window is not clobbered. Only ever called where nothing was started: past the point where a
/// run row or a job row exists, the window stays spent, because re-firing on a maybe is the
/// duplicate the claim-first ordering exists to prevent.
async fn release_window(
    state: &AppState,
    project_id: &str,
    rule_name: &str,
    previous_fired_at: Option<&str>,
    previous_head_sha: Option<&str>,
    now: DateTime<Utc>,
) {
    let released = sqlx::query(
        "UPDATE scheduler_state
         SET last_fired_at = ?, last_head_sha = ?
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
async fn start_job(
    state: &AppState,
    project_id: &str,
    project_root: &str,
    rule: &ScheduleRule,
    graph: &config::GraphConfig,
    head_sha: Option<&str>,
) -> crate::job::JobStart {
    crate::job::start(
        state,
        &crate::job::StartRequest {
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
const COMMAND_OUTPUT_TAIL_BYTES: usize = 4096;

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

fn command_in_flight(project_root: &str, project_id: &str, rule_name: &str) -> bool {
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
async fn record_command_result(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    rule_name: &str,
    outcome: &str,
    exit_code: Option<i64>,
    output: &str,
) {
    let output = crate::redact::redact_secrets(output);
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
async fn fire_command(
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

    let refusal = match crate::project_policy::shell_rules(&state.pool, project_id).await {
        Ok(rules) => {
            let judged = crate::classifier::classify(
                "Bash",
                &serde_json::json!({ "command": command }),
                Some(&cwd),
                &crate::github::Policy::empty(),
                &rules,
                crate::classifier::Unrecognized::AsksAPerson,
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
                    crate::project_commands::COMMAND_TIMEOUT,
                )
                .await;
                if ran.timed_out {
                    (
                        "errored",
                        None,
                        format!(
                            "timed out after {} seconds\n{}",
                            crate::project_commands::COMMAND_TIMEOUT.as_secs(),
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

pub async fn scheduler_tick(state: &AppState, now: DateTime<Utc>) {
    if crate::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return;
    }

    if crate::autopilot::scoped_kill_engaged(&state.pool, "trigger", "scheduled")
        .await
        .unwrap_or(true)
    {
        return;
    }

    match crate::quota::permits_new_run(state, now).await {
        crate::budget::BudgetDecision::Pause { reason, source, .. } => {
            if source == crate::quota::PAUSE_SOURCE {
                let _ = sqlx::query(
                    "UPDATE autopilot_global
                     SET quota_hold_started_at = ?, quota_hold_ended_at = NULL
                     WHERE quota_hold_started_at IS NULL OR quota_hold_ended_at IS NOT NULL",
                )
                .bind(now.to_rfc3339())
                .execute(&state.pool)
                .await;
                tracing::info!(reason = %reason, "quota exhausted; scheduler paused this tick");
            } else {
                tracing::info!(reason = %reason, "budget exhausted; scheduler paused this tick");
            }
            return;
        }
        crate::budget::BudgetDecision::Allow => {
            let _ = sqlx::query(
                "UPDATE autopilot_global SET quota_hold_ended_at = ?
                 WHERE quota_hold_started_at IS NOT NULL AND quota_hold_ended_at IS NULL",
            )
            .bind(now.to_rfc3339())
            .execute(&state.pool)
            .await;
        }
    }

    let quota_hold: Option<(String, String)> = sqlx::query_as(
        "SELECT quota_hold_started_at, quota_hold_ended_at FROM autopilot_global
         WHERE quota_hold_started_at IS NOT NULL AND quota_hold_ended_at IS NOT NULL LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let quota_hold = quota_hold.and_then(|(start, end)| {
        Some((
            DateTime::parse_from_rfc3339(&start)
                .ok()?
                .with_timezone(&Utc),
            DateTime::parse_from_rfc3339(&end).ok()?.with_timezone(&Utc),
        ))
    });

    if let crate::attention::AttentionDecision::Defer { reason, scope } =
        crate::attention::attention_permits_new_run(&state.pool, &state.run_handles, "", now).await
    {
        tracing::info!(
            attention_scope = ?scope,
            reason = %reason,
            "owner attention brake refused global scheduled work; scheduler paused this tick"
        );
        return;
    }

    let projects = match crate::autopilot::autopilot_projects(&state.pool).await {
        Ok(projects) => projects,
        Err(error) => {
            tracing::warn!(%error, "failed to load autopilot projects for scheduler tick");
            return;
        }
    };

    for (project_id, project_root, project_mode) in projects {
        if crate::autopilot::scoped_kill_engaged(&state.pool, "project", &project_id)
            .await
            .unwrap_or(true)
        {
            continue;
        }

        // Per-project, so a full queue on one project never stalls the others.
        if let crate::wip::WipDecision::Defer { reason } =
            crate::wip::wip_permits_new_run(&state.pool, &project_id).await
        {
            tracing::info!(
                project_id = %project_id,
                reason = %reason,
                "approval queue full; deferring this project's scheduled work"
            );
            continue;
        }

        if let crate::attention::AttentionDecision::Defer { reason, scope } =
            crate::attention::attention_permits_new_run(
                &state.pool,
                &state.run_handles,
                &project_id,
                now,
            )
            .await
        {
            tracing::info!(
                project_id = %project_id,
                attention_scope = ?scope,
                reason = %reason,
                "owner attention brake refused this project's scheduled work"
            );
            if matches!(scope, crate::attention::AttentionScope::Global) {
                return;
            }
            continue;
        }

        let rules =
            match config::load_schedule_rules(state.machine_config_root.as_deref(), &project_id) {
                Ok(rules) => rules.schedules,
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        project_root = %project_root,
                        %error,
                        "failed to load project schedule rules"
                    );
                    continue;
                }
            };

        let rows: Vec<RuleState> = match sqlx::query_as(
            "SELECT rule_name, last_fired_at, last_head_sha, fires_date, fires_today
             FROM scheduler_state
             WHERE project_id = ?",
        )
        .bind(&project_id)
        .fetch_all(&state.pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(
                    project_id = %project_id,
                    %error,
                    "failed to load persisted scheduler state"
                );
                continue;
            }
        };

        let persisted_names: HashSet<&str> =
            rows.iter().map(|row| row.rule_name.as_str()).collect();
        // The HEAD each rule was last armed/fired at, so a catch-up can tell whether the repo moved
        // under it. Absent for rules armed before migration 0018 — treated as "cannot tell", which
        // is not the same as "did not move", so those simply skip the re-triage warning.
        let last_head_shas: HashMap<&str, &str> = rows
            .iter()
            .filter_map(|row| Some((row.rule_name.as_str(), row.last_head_sha.as_deref()?)))
            .collect();
        let last_fired_raw: HashMap<&str, &str> = rows
            .iter()
            .map(|row| (row.rule_name.as_str(), row.last_fired_at.as_str()))
            .collect();
        let mut last_fired = HashMap::new();
        for RuleState {
            rule_name,
            last_fired_at: value,
            ..
        } in &rows
        {
            match DateTime::parse_from_rfc3339(value) {
                Ok(timestamp) => {
                    last_fired.insert(rule_name.clone(), timestamp.with_timezone(&Utc));
                }
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule_name,
                        last_fired_at = %value,
                        %error,
                        "re-arming malformed scheduler timestamp"
                    );
                    if let Err(update_error) = sqlx::query(
                        "UPDATE scheduler_state
                         SET last_fired_at = ?
                         WHERE project_id = ? AND rule_name = ?",
                    )
                    .bind(now.to_rfc3339())
                    .bind(&project_id)
                    .bind(rule_name)
                    .execute(&state.pool)
                    .await
                    {
                        tracing::warn!(
                            project_id = %project_id,
                            rule_name = %rule_name,
                            error = %update_error,
                            "failed to re-arm malformed scheduler timestamp"
                        );
                    }
                }
            }
        }

        // From the row, not from a counter the loop holds: the map reset on every daemon start, and
        // the Task Scheduler registration restarts the daemon three times on failure. A crashing
        // daemon is the one situation this cap is named for, and it was the one that rearmed it.
        let today = now.date_naive().to_string();
        let project_fires: HashMap<String, u32> = rows
            .iter()
            .filter(|row| row.fires_date.as_deref() == Some(today.as_str()))
            .map(|row| (row.rule_name.clone(), row.fires_today.max(0) as u32))
            .collect();
        let due = due_rules(
            &rules,
            &last_fired,
            &project_fires,
            now,
            MIN_INTERVAL,
            DAILY_CAP,
        );

        // Read HEAD once per project, and only when something is actually about to be armed or
        // fired — this loop runs every 30s and spawning git for an idle project would be pure waste.
        let unarmed = rules
            .iter()
            .any(|rule| !persisted_names.contains(rule.name.as_str()));
        let head_sha = if unarmed || !due.is_empty() {
            crate::repo_trigger::current_branch_sha(Path::new(&project_root), "HEAD", false).await
        } else {
            None
        };

        for rule in &rules {
            if persisted_names.contains(rule.name.as_str()) {
                continue;
            }

            // Said once, here, where a rule is first seen. A rule that cannot be read simply never
            // comes due, so the only sign of a typo was a `warn!` every 30 seconds for as long as
            // the rule existed — which is the same as no sign at all, in a log nobody is watching
            // while they wonder why their automation stopped. The feed is where a person looks.
            let unreadable = match rule.at.as_deref() {
                // A one-shot whose instant is already behind us cannot be told apart from a typo,
                // and firing it "now" could replay something the owner meant for last week.
                Some(at) => match at_instant(rule) {
                    Some(Err(error)) => Some(format!("an unreadable at ({error})")),
                    Some(Ok(instant)) if instant <= now => {
                        Some(format!("an at already in the past ({at})"))
                    }
                    _ => None,
                },
                None => rule
                    .cron
                    .parse::<Cron>()
                    .err()
                    .map(|error| format!("an unreadable cron ({}): {error}", rule.cron)),
            }
            .or_else(|| rule_timezone(rule).err());
            if let Some(problem) = unreadable {
                let _ = crate::feed::append(
                    &state.pool,
                    Some(&project_id),
                    "schedule_rule_invalid",
                    &format!(
                        "schedule rule '{}' has {problem} — it will never fire",
                        rule.name
                    ),
                    None,
                    None,
                )
                .await;
            }

            // Armed regardless, so this branch runs once rather than every tick. A rule that cannot
            // be read is still a rule the user wrote, and forgetting it would only mean announcing
            // it again in 30 seconds. A one-shot whose `at` was already behind us is armed AT that
            // instant rather than at `now`: `last_fired_at == at` is never due (due needs
            // `last < at`) and, unlike `now`, still tells it apart from a rule that fired (the
            // claim stamps a time after `at`), so the rules view can say it never fires.
            let armed_at = match at_instant(rule) {
                Some(Ok(instant)) if instant <= now => instant,
                _ => now,
            };
            if let Err(error) = sqlx::query(
                "INSERT INTO scheduler_state (project_id, rule_name, last_fired_at, last_head_sha)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&project_id)
            .bind(&rule.name)
            .bind(armed_at.to_rfc3339())
            .bind(head_sha.as_deref())
            .execute(&state.pool)
            .await
            {
                tracing::warn!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    %error,
                    "failed to arm new schedule rule"
                );
            }
        }

        let mut fired_this_tick = HashSet::new();
        for (rule, due_at) in due {
            if project_fires.get(&rule.name).copied().unwrap_or(0) >= DAILY_CAP {
                tracing::warn!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    daily_cap = DAILY_CAP,
                    "scheduler runaway guard suppressed a scheduled run"
                );
                continue;
            }
            if !fired_this_tick.insert(rule.name.clone()) {
                tracing::warn!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    "duplicate schedule rule name suppressed within scheduler tick"
                );
                continue;
            }
            // Before the claim, so a still-running command spends no window: the next tick sees it
            // due again and asks again.
            if rule.command.is_some() && command_in_flight(&project_root, &project_id, &rule.name) {
                tracing::info!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    "a scheduled command is still running; not starting it again"
                );
                continue;
            }

            // A catch-up is demoted to plan-only whatever the project's mode: it carries assumptions
            // as old as the window it missed, so the most it may produce is a proposal.
            let catch_up = is_catch_up(
                lateness_origin(due_at, quota_hold),
                now,
                chrono::Duration::minutes(CATCH_UP_GRACE_MINUTES),
            );
            let (cwd, run_mode) = match project_mode {
                Mode::Off => continue,
                Mode::Active if !catch_up => (project_root.clone(), "worktree"),
                // Shadow always, and Active demoted by a catch-up: plan-only, in place, no worktree.
                Mode::Shadow | Mode::Active => (
                    rule.cwd.clone().unwrap_or_else(|| project_root.clone()),
                    "shadow",
                ),
            };

            let prompt = if catch_up {
                let head_moved = match (last_head_shas.get(rule.name.as_str()), head_sha.as_deref())
                {
                    (Some(recorded), Some(current)) => *recorded != current,
                    // No recorded sha (armed before migration 0018) or no readable HEAD: we cannot
                    // tell, which is not the same as knowing it stayed put — so warn about lateness
                    // only, rather than asserting the repo is unchanged.
                    _ => false,
                };
                tracing::info!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    late_minutes = now.signed_duration_since(due_at).num_minutes(),
                    head_moved,
                    "serving a missed window as a plan-only catch-up run"
                );
                format!(
                    "{}\n\n{}",
                    catch_up_preamble(now.signed_duration_since(due_at), head_moved),
                    rule.prompt
                )
            } else {
                rule.prompt.clone()
            };

            // Re-read the emergency stop immediately before committing to this run. The tick's
            // preamble checked it once and then iterates every project and every due rule, and one
            // tick can spend minutes in `git worktree add` — so a switch thrown during that fan-out
            // did not stop the rules that had not been reached yet. "Stop" that keeps starting work
            // for another minute is not a stop.
            //
            // Fails closed, like the preamble: an unreadable switch stops the tick.
            if crate::autopilot::kill_switch_engaged(&state.pool)
                .await
                .unwrap_or(true)
            {
                tracing::info!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    "kill switch engaged mid-tick; not firing"
                );
                return;
            }

            // Claim the window BEFORE starting anything, compare-and-set against the timestamp this
            // tick read. Firing first and persisting afterwards made every failure mode duplicate
            // work: a crash in between left the rule still due, so the next start ran the same
            // prompt again, and a persistent write failure re-fired it every tick while the run
            // itself kept succeeding. For an autonomous run that is not a retry — it is the same
            // mutation applied twice.
            //
            // The trade is deliberate and the safe direction: if the run then fails to start, this
            // window is spent and the rule waits for its next one. A missed window is visible and
            // recoverable; a duplicated commit is neither.
            //
            // The sha rides along with the timestamp because both describe the state the NEXT
            // window is scheduled from, so a later catch-up would otherwise compare against a HEAD
            // from the wrong moment.
            //
            // The daily allowance is spent in the same statement, for the same reason: two writes
            // would leave a window where the run has been claimed and not counted, and a daemon
            // that dies there would come back with the allowance intact. `fires_date` carries the
            // day the count belongs to, so a stale count from yesterday resets rather than
            // accumulating — there is no midnight sweep to miss.
            let previous_fired_at = last_fired_raw.get(rule.name.as_str()).copied();
            let claim = sqlx::query(
                "UPDATE scheduler_state
                 SET last_fired_at = ?,
                     last_head_sha = ?,
                     fires_today = CASE WHEN fires_date = ? THEN fires_today + 1 ELSE 1 END,
                     fires_date = ?
                 WHERE project_id = ? AND rule_name = ? AND last_fired_at IS ?",
            )
            .bind(now.to_rfc3339())
            .bind(head_sha.as_deref())
            .bind(&today)
            .bind(&today)
            .bind(&project_id)
            .bind(&rule.name)
            .bind(previous_fired_at)
            .execute(&state.pool)
            .await;
            match claim {
                Ok(result) if result.rows_affected() == 1 => {}
                Ok(_) => {
                    // Another tick already moved this rule on. Not an error: the window is served.
                    tracing::info!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        "scheduler window already claimed; not firing"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        %error,
                        "could not claim the scheduler window; not firing"
                    );
                    continue;
                }
            }

            // A `command:` rule starts no agent: it is judged, then run as an argv with a timeout.
            // The mode match above already skipped Off; a command ignores `cwd:`'s shadow meaning
            // (it is resolved against the project root inside `fire_command`) and `run_mode`.
            if let Some(command) = rule.command.as_deref() {
                fire_command(
                    state,
                    &project_id,
                    &project_root,
                    rule,
                    command,
                    catch_up,
                    now.signed_duration_since(due_at),
                )
                .await;
                continue;
            }

            // A rule with a `graph:` block starts a job instead of a run — but only when the run it
            // would otherwise start is a real one.
            //
            // Not in shadow. Shadow runs are `--permission-mode plan` (`runs.rs` derives
            // `plan_only` from the mode), so the plan node could not write the `plan.json` that
            // §5.2 of the design takes the queue from, and every shadow job would fail at its first
            // node, deterministically. §6.5 wanted the implement nodes' classifier samples and
            // flagged its own dependency on plan-only mode; this is that dependency coming back
            // false. A `graph:` rule in shadow therefore behaves exactly as it does today.
            //
            // Not on a catch-up either: a catch-up is demoted to plan-only precisely because its
            // assumptions are as old as the window it missed, and a job is the largest thing this
            // daemon can start.
            if let (Mode::Active, false, Some(graph)) =
                (project_mode, catch_up, rule.graph.as_ref())
            {
                match start_job(
                    state,
                    &project_id,
                    &project_root,
                    rule,
                    graph,
                    head_sha.as_deref(),
                )
                .await
                {
                    crate::job::JobStart::Started(job_id) => tracing::info!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        job_id,
                        "fired a scheduled job"
                    ),
                    // No slot was free. Nothing was started, so the window goes back — the same
                    // trade `CreateRunError::Busy` gets below, and for the same reason: a busy
                    // project should retry next tick rather than skip its schedule.
                    crate::job::JobStart::NoRoom(_) => {
                        release_window(
                            state,
                            &project_id,
                            &rule.name,
                            previous_fired_at,
                            last_head_shas.get(rule.name.as_str()).copied(),
                            now,
                        )
                        .await;
                    }
                    // The rule names a team the catalogue does not have. Nothing was created, and
                    // the window stays spent anyway — unlike `NoRoom`, retrying next tick would
                    // fail identically for as long as the file and the catalogue disagree, which
                    // is every tick until somebody edits one of them. Waiting for this rule's next
                    // window is the right noise level for a misconfiguration.
                    //
                    // Said out loud, because it is the only place it CAN be said. There is no job
                    // row for `fail_early` to retire and no feed entry for anybody to read, so a
                    // rule that silently never fires again is the exact shape of failure a team is
                    // supposed to make visible.
                    crate::job::JobStart::NoTeam(reason) => tracing::warn!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        %reason,
                        "a scheduled job asked for a team that does not exist and was not started"
                    ),
                    // Past the INSERT: the job row exists and this cannot tell how far provisioning
                    // got. The window stays spent, because re-firing on a maybe is the duplicate
                    // the whole claim-first ordering exists to prevent.
                    crate::job::JobStart::Failed => {}
                }
                continue;
            }

            match create_run_inner(
                state,
                prompt,
                Some(project_id.clone()),
                Some(cwd),
                run_mode,
                false,
            )
            .await
            {
                Ok(run_id) => {
                    // +1 for the claim above, which is where the allowance was actually spent.
                    let spent = project_fires.get(&rule.name).copied().unwrap_or(0) + 1;
                    tracing::info!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        run_id,
                        run_mode,
                        "fired scheduled run"
                    );
                    if spent == DAILY_CAP {
                        tracing::warn!(
                            project_id = %project_id,
                            rule_name = %rule.name,
                            daily_cap = DAILY_CAP,
                            "scheduler runaway guard reached; suppressing further runs today"
                        );
                    }
                }
                // `Busy` and `Invalid` are both decided before or by the runs INSERT itself — the
                // unique violation that means another worktree run holds the project, and the
                // argument check ahead of it — so no row exists and nothing ran. Those give the
                // window back, which is what makes a busy project retry on the next tick rather
                // than silently skip its schedule.
                //
                // Every other error is kept spent, because past the INSERT the row exists and this
                // cannot tell how far provisioning got. Re-firing on a maybe is the duplicate this
                // whole ordering exists to prevent.
                //
                // Released with a compare-and-set on the value just written, so a concurrent tick
                // that has already claimed the next window is not clobbered.
                //
                // A disk below the floor is the same trade: refused before any tree, a condition
                // that passes. The log line's `%error` is the refusal's own sentence, so it says
                // the disk rather than a slot.
                Err(
                    error @ (CreateRunError::Busy
                    | CreateRunError::NoRoomOnDisk(_)
                    | CreateRunError::Invalid(_)),
                ) => {
                    release_window(
                        state,
                        &project_id,
                        &rule.name,
                        previous_fired_at,
                        last_head_shas.get(rule.name.as_str()).copied(),
                        now,
                    )
                    .await;
                    tracing::info!(
                        project_id = %project_id,
                        rule_name = %rule.name,
                        %error,
                        "no run was created; window released for the next tick"
                    );
                }
                Err(error) => tracing::warn!(
                    project_id = %project_id,
                    rule_name = %rule.name,
                    run_mode,
                    %error,
                    "failed to create scheduled run — this window is spent"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::FakeCommandRunner;
    use crate::state::{AppState, DEFAULT_RUN_TIMEOUT};
    use std::path::{Path as FsPath, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use std::time::Duration;

    const DAILY_CAP: u32 = 24;
    const WORKTREE_ROOT_ENV: &str = "NUCLEOS_WORKTREE_ROOT";
    const ACTIVE_TEST_CHILD_ENV: &str = "NUCLEOS_SCHEDULER_ACTIVE_TEST_CHILD";
    const ACTIVE_TEST_REPO_ENV: &str = "NUCLEOS_SCHEDULER_ACTIVE_TEST_REPO";

    /// A state whose `machine_config_root` is a temporary directory standing in for `~/.nucleos`,
    /// returned beside it so the directory lives exactly as long as the test that holds it.
    async fn test_state(delay: Option<Duration>) -> (AppState, tempfile::TempDir) {
        let home = tempfile::tempdir().expect("create a stand-in home");
        let pool = crate::testdb::fresh_pool().await;

        let state = AppState {
            token: Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: Arc::new(FakeCommandRunner {
                delay: Mutex::new(delay),
                ..Default::default()
            }),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: Some(home.path().to_path_buf()),
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: DEFAULT_RUN_TIMEOUT,
        };
        (state, home)
    }

    fn space_free_tempdir(prefix: &str) -> tempfile::TempDir {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(base)
            .expect("create space-free tempdir")
    }

    fn initialize_repo(repo: &FsPath) {
        std::fs::create_dir_all(repo).expect("create repository directory");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@x"],
            vec!["config", "user.name", "test"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args(args)
                    .status()
                    .expect("git should start")
                    .success()
            );
        }
        std::fs::write(repo.join("seed.txt"), "seed\n").expect("write seed file");
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["add", "-A"])
                .status()
                .expect("git should start")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["commit", "-m", "seed"])
                .status()
                .expect("git should start")
                .success()
        );
    }

    /// Writes `project_id`'s rules where the scheduler reads them: its directory under the state's
    /// stand-in home.
    fn write_rules(state: &AppState, project_id: &str, contents: &str) {
        crate::project_state::write_for_test(
            state
                .machine_config_root
                .as_deref()
                .expect("the test state has a stand-in home"),
            project_id,
            crate::project_state::AUTOPILOT_FILE,
            contents,
        );
    }

    fn write_schedule(state: &AppState, project_id: &str) {
        write_rules(
            state,
            project_id,
            "schedules:\n  - name: r1\n    cron: \"* * * * *\"\n    prompt: \"go\"\n",
        );
    }

    /// The same rule with a `graph:` block on it, so a test can compare like with like.
    fn write_graph_schedule(state: &AppState) {
        write_rules(
            state,
            "proj",
            "schedules:\n  - name: r1\n    cron: \"* * * * *\"\n    prompt: \"go\"\n    graph:\n      max_items: 3\n",
        );
    }

    /// The same graph rule again, this time naming a ceiling of its own. The number is deliberately
    /// nothing any default or house limit would produce, so a row carrying it can only have got it
    /// from this file.
    fn write_budgeted_graph_schedule(state: &AppState) {
        write_rules(
            state,
            "proj",
            "schedules:\n  - name: r1\n    cron: \"* * * * *\"\n    prompt: \"go\"\n    graph:\n      max_items: 3\n      budget_usd: 2.5\n      team: crew\n",
        );
    }

    /// A team the schedule above can name. `foreign_keys` is on, so the agent comes first.
    async fn seed_team(state: &AppState, team_id: &str) {
        let now = "2026-07-18T09:00:00Z";
        sqlx::query(
            "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                 created_at, updated_at)
             VALUES ('dir', 'Dir', 'directing', 'lead', 'claude', 'inherit', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES (?, 'Crew', 'ship it', 'dir', 3, 2, ?, ?)",
        )
        .bind(team_id)
        .bind(now)
        .bind(now)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn job_count(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn run_count(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    /// Shadow runs are `--permission-mode plan`, so the plan node could not write the `plan.json`
    /// the queue is read from, and every shadow job would fail at its first node — deterministically,
    /// on every project in shadow. §6.5 of the design wanted the implement nodes' classifier samples
    /// and flagged its own dependency on plan-only mode; this is that dependency coming back false.
    /// A `graph:` rule in shadow therefore behaves exactly as it does today.
    #[tokio::test]
    async fn a_graph_rule_in_shadow_still_fires_one_ordinary_run() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        write_graph_schedule(&state);

        scheduler_tick(&state, now).await;

        assert_eq!(job_count(&state).await, 0);
        let mode: String = sqlx::query_scalar("SELECT mode FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(mode, "shadow");
    }

    /// A catch-up is demoted to plan-only precisely because its assumptions are as old as the window
    /// it missed. A job is the largest thing this daemon can start, so it is the last thing a stale
    /// window should get.
    #[tokio::test]
    async fn a_catch_up_never_starts_a_job() {
        let project = tempfile::tempdir().expect("create active project");
        let (state, _home) = test_state(None).await;
        // Due at 10:01; the daemon only came back four hours later.
        let now = timestamp("2026-07-18T14:00:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        write_graph_schedule(&state);

        scheduler_tick(&state, now).await;

        assert_eq!(job_count(&state).await, 0);
        let (mode, prompt): (String, String) = sqlx::query_as("SELECT mode, prompt FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(mode, "shadow");
        assert!(prompt.contains("CATCH-UP"), "got: {prompt}");
    }

    /// Nothing was started, so the window goes back — the same trade `CreateRunError::Busy` gets,
    /// and for the same reason: a busy project should retry on the next tick rather than skip its
    /// schedule for the day.
    #[tokio::test]
    async fn a_project_with_no_slot_free_keeps_its_window() {
        let project = tempfile::tempdir().expect("create active project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        write_graph_schedule(&state);
        // A ceiling of one, which is what this used to get from `one_live_job_per_project`. The
        // seeded job then has to hold the slot as well as the row: `insert_job` alone claims
        // nothing, because claiming is `start`'s job and this is seeding, not starting.
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = 1")
            .execute(&state.pool)
            .await
            .unwrap();
        crate::job::insert_job(
            &state.pool,
            &crate::job::NewJob {
                project_id: "proj",
                project_root: &project.path().to_string_lossy(),
                rule_name: Some("r1"),
                prompt: "go",
                max_items: 3,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        .expect("a job is already live for this project");
        crate::concurrency::claim(&state.pool, "proj", crate::worktree::Owner::Job(1))
            .await
            .expect("the live job holds the project's only slot");

        scheduler_tick(&state, now).await;

        // Refused by the slot ceiling, not by a check in the tick: the INSERT is still the lock.
        assert_eq!(job_count(&state).await, 1);
        assert_eq!(run_count(&state).await, 0);
        let stored: String = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'r1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(stored, old, "the window must go back for the next tick");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_graph_rule_starts_a_job_that_owns_its_worktree() {
        // Re-executed in a child process, like the other worktree-provisioning scheduler test:
        // `NUCLEOS_WORKTREE_ROOT` is process-wide, and a real `git worktree add` runs here.
        if std::env::var_os(ACTIVE_TEST_CHILD_ENV).is_none() {
            let _lock = env_lock();
            let repo_container = space_free_tempdir("nucleos-scheduler-job-");
            let repo = repo_container.path().join("repo");
            initialize_repo(&repo);
            let worktree_root = space_free_tempdir("nucleos-wt-test-job-");
            let status = Command::new(std::env::current_exe().expect("resolve test executable"))
                .args([
                    "--exact",
                    "scheduler::tests::a_graph_rule_starts_a_job_that_owns_its_worktree",
                    "--nocapture",
                ])
                .env(ACTIVE_TEST_CHILD_ENV, "1")
                .env(ACTIVE_TEST_REPO_ENV, &repo)
                .env(WORKTREE_ROOT_ENV, worktree_root.path())
                .status()
                .expect("start isolated scheduler test process");
            assert!(status.success(), "isolated scheduler test process failed");
            return;
        }

        let repo = PathBuf::from(
            std::env::var_os(ACTIVE_TEST_REPO_ENV).expect("active test repository is set"),
        );
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, &repo, "active", &old).await;
        write_graph_schedule(&state);

        scheduler_tick(&state, now).await;

        // A job, and no run: the nodes are started by the job tick, not by this one.
        assert_eq!(run_count(&state).await, 0);
        let (job_id, status, max_items, prompt): (i64, String, i64, String) =
            sqlx::query_as("SELECT id, status, max_items, prompt FROM jobs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(status, "planning");
        assert_eq!(max_items, 3);
        // The rule's prompt is copied onto the job, not re-read at the plan node: the file can be
        // edited between now and then, and a job that changed shape mid-flight would plan something
        // nobody asked this job to do.
        assert_eq!(prompt, "go");

        // The worktree belongs to the JOB. A run owning it would let the GC collect the tree the
        // moment that one node finished, with the rest of the queue still to run in it.
        let worktree_path: String = sqlx::query_scalar(
            "SELECT path FROM worktrees WHERE owner_kind = 'job' AND owner_id = ?",
        )
        .bind(job_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(worktree_path.ends_with(&format!("job-{job_id}")));

        let _ = crate::worktree::remove(&repo, &PathBuf::from(worktree_path), &[]).await;
    }

    /// The autonomous path is the one that needed this. A job started from `POST /jobs` has had a
    /// `budget_usd` since the column existed, but a job fired by a SCHEDULE could not be given one —
    /// `start_job` hard-coded `None` — and the overnight run with nobody watching is exactly the
    /// case where a job that goes wrong spends the whole house allowance before anyone sees it.
    ///
    /// Asserted on the stored ROW and not on the request struct, because the row is what
    /// `job::brakes` reads on every node: a number that reached `StartRequest` and stopped there
    /// would brake nothing, and a test that watched the struct would call that a pass.
    ///
    /// Re-executed in a child process like its two neighbours: `NUCLEOS_WORKTREE_ROOT` is
    /// process-wide, and a real `git worktree add` runs here.
    #[tokio::test(flavor = "current_thread")]
    async fn a_scheduled_job_carries_the_budget_its_rule_asked_for() {
        if std::env::var_os(ACTIVE_TEST_CHILD_ENV).is_none() {
            let _lock = env_lock();
            let repo_container = space_free_tempdir("nucleos-scheduler-budget-");
            let repo = repo_container.path().join("repo");
            initialize_repo(&repo);
            let worktree_root = space_free_tempdir("nucleos-wt-test-budget-");
            let status = Command::new(std::env::current_exe().expect("resolve test executable"))
                .args([
                    "--exact",
                    "scheduler::tests::a_scheduled_job_carries_the_budget_its_rule_asked_for",
                    "--nocapture",
                ])
                .env(ACTIVE_TEST_CHILD_ENV, "1")
                .env(ACTIVE_TEST_REPO_ENV, &repo)
                .env(WORKTREE_ROOT_ENV, worktree_root.path())
                .status()
                .expect("start isolated scheduler test process");
            assert!(status.success(), "isolated scheduler test process failed");
            return;
        }

        let repo = PathBuf::from(
            std::env::var_os(ACTIVE_TEST_REPO_ENV).expect("active test repository is set"),
        );
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, &repo, "active", &old).await;
        seed_team(&state, "crew").await;
        write_budgeted_graph_schedule(&state);

        scheduler_tick(&state, now).await;

        let (job_id, budget_usd, team_id): (i64, Option<f64>, Option<String>) =
            sqlx::query_as("SELECT id, budget_usd, team_id FROM jobs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            budget_usd,
            Some(2.5),
            "the ceiling the rule asked for must be on the row the brakes read"
        );
        // The other half of what a rule may now ask for, and the one with no symptom when it goes
        // missing: a night that runs sequentially looks exactly like a night that was never asked
        // to do anything else.
        assert_eq!(
            team_id.as_deref(),
            Some("crew"),
            "the team the rule asked for must be on the row `load_view` reads"
        );

        let worktree_path: String = sqlx::query_scalar(
            "SELECT path FROM worktrees WHERE owner_kind = 'job' AND owner_id = ?",
        )
        .bind(job_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let _ = crate::worktree::remove(&repo, &PathBuf::from(worktree_path), &[]).await;
    }

    async fn seed_project(
        state: &AppState,
        project_root: &FsPath,
        mode: &str,
        last_fired_at: &str,
    ) {
        seed_named_project(state, "proj", project_root, mode, last_fired_at).await;
    }

    async fn seed_named_project(
        state: &AppState,
        project_id: &str,
        project_root: &FsPath,
        mode: &str,
        last_fired_at: &str,
    ) {
        write_schedule(state, project_id);
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, ?, ?)",
        )
        .bind(project_id)
        .bind(mode)
        .bind(project_root.to_string_lossy().as_ref())
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO scheduler_state (project_id, rule_name, last_fired_at)
             VALUES (?, 'r1', ?)",
        )
        .bind(project_id)
        .bind(last_fired_at)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// The runaway guard used to be a `HashMap` the scheduler loop held, so it emptied every time
    /// the daemon started — and the Task Scheduler registration restarts the daemon three times on
    /// failure. A crashing daemon is the one situation this cap is named for, and it was the
    /// situation that rearmed it. `scheduler_tick` is called directly here, which is exactly what a
    /// restart looks like: no loop state survives between the calls.
    #[tokio::test]
    async fn the_daily_cap_survives_a_daemon_restart() {
        let (state, _home) = test_state(None).await;
        let container = space_free_tempdir("nucleos-scheduler-cap-");
        let repo = container.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        seed_project(
            &state,
            &repo,
            "shadow",
            &timestamp("2026-07-18T09:00:00Z").to_rfc3339(),
        )
        .await;

        // A rule that has already spent its allowance today, recorded where a restart cannot lose
        // it. Nothing else in this test carries state between the ticks below.
        sqlx::query("UPDATE scheduler_state SET fires_date = '2026-07-18', fires_today = ?")
            .bind(i64::from(DAILY_CAP))
            .execute(&state.pool)
            .await
            .unwrap();

        scheduler_tick(&state, timestamp("2026-07-18T10:10:00Z")).await;

        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "a spent allowance must survive the restart");

        // And it is spent for that day only — the date is what resets it, so there is no midnight
        // sweep to miss.
        scheduler_tick(&state, timestamp("2026-07-19T10:10:00Z")).await;
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 1, "the next day starts with a fresh allowance");

        let (date, count): (Option<String>, i64) =
            sqlx::query_as("SELECT fires_date, fires_today FROM scheduler_state")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(date.as_deref(), Some("2026-07-19"));
        assert_eq!(count, 1, "the stale count resets rather than accumulating");
    }

    /// A cron nobody can parse is a rule that never comes due, so the only evidence of a typo was a
    /// log line every 30 seconds — which is where you look after you already know something is
    /// wrong, not how you find out. Said once, to the feed, when the rule is first seen.
    #[tokio::test]
    async fn an_unreadable_cron_is_reported_once_where_a_person_looks() {
        let (state, _home) = test_state(None).await;
        let container = space_free_tempdir("nucleos-scheduler-badcron-");
        let repo = container.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        write_rules(
            &state,
            "proj",
            "schedules:\n  - name: r1\n    cron: \"not a cron\"\n    prompt: \"go\"\n",
        );
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root)
             VALUES ('proj', 'shadow', ?)",
        )
        .bind(repo.to_string_lossy().as_ref())
        .execute(&state.pool)
        .await
        .unwrap();

        // Three ticks: the rule is armed on the first and merely re-read on the rest.
        for minute in ["10:00:00", "10:00:30", "10:01:00"] {
            scheduler_tick(&state, timestamp(&format!("2026-07-18T{minute}Z"))).await;
        }

        let entries: Vec<String> = sqlx::query_scalar(
            "SELECT summary FROM feed WHERE kind = 'schedule_rule_invalid' ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(entries.len(), 1, "once, not once per tick: {entries:?}");
        assert!(entries[0].contains("r1"), "{}", entries[0]);
        assert!(entries[0].contains("never fire"), "{}", entries[0]);

        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "an unreadable rule must not fire either");
    }

    /// Seeds a project whose only rule is the one-shot `rule_yaml` (named `once`). `armed` is the
    /// `scheduler_state.last_fired_at` the rule was armed with; `None` leaves no row, as for a rule
    /// the scheduler has never seen.
    async fn seed_one_shot(
        state: &AppState,
        project_root: &FsPath,
        mode: &str,
        rule_yaml: &str,
        armed: Option<&str>,
    ) {
        write_rules(state, "proj", &format!("schedules:\n{rule_yaml}"));
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('proj', ?, ?)",
        )
        .bind(mode)
        .bind(project_root.to_string_lossy().as_ref())
        .execute(&state.pool)
        .await
        .unwrap();
        if let Some(armed) = armed {
            sqlx::query(
                "INSERT INTO scheduler_state (project_id, rule_name, last_fired_at)
                 VALUES ('proj', 'once', ?)",
            )
            .bind(armed)
            .execute(&state.pool)
            .await
            .unwrap();
        }
    }

    const ONE_SHOT_PROMPT: &str =
        "  - name: once\n    at: '2026-07-18T10:05:00Z'\n    prompt: go\n";

    /// `last_fired_at` is the whole memory of a one-shot: the claim stamps it at or after `at`, and
    /// `scheduler_tick` is called again with no loop state, which is what a restart looks like.
    #[tokio::test]
    async fn a_one_shot_rule_fires_once_and_a_restart_does_not_fire_it_again() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        seed_one_shot(
            &state,
            project.path(),
            "shadow",
            ONE_SHOT_PROMPT,
            Some(&timestamp("2026-07-18T10:00:00Z").to_rfc3339()),
        )
        .await;

        scheduler_tick(&state, timestamp("2026-07-18T10:06:00Z")).await;
        assert_eq!(run_count(&state).await, 1, "the instant passed: it fires");

        scheduler_tick(&state, timestamp("2026-07-18T10:07:00Z")).await;
        scheduler_tick(&state, timestamp("2026-07-19T10:07:00Z")).await;
        assert_eq!(
            run_count(&state).await,
            1,
            "a one-shot never fires a second time, whatever the later ticks"
        );
    }

    /// The daemon was down across the instant: the first tick after it starts runs the rule late,
    /// and an active project is demoted to a plan-only catch-up exactly as a missed cron window is.
    #[tokio::test]
    async fn a_missed_one_shot_fires_on_the_first_tick_after_start_as_a_catch_up() {
        let project = tempfile::tempdir().expect("create active project");
        let (state, _home) = test_state(None).await;
        seed_one_shot(
            &state,
            project.path(),
            "active",
            ONE_SHOT_PROMPT,
            Some(&timestamp("2026-07-18T10:00:00Z").to_rfc3339()),
        )
        .await;

        scheduler_tick(&state, timestamp("2026-07-18T14:00:00Z")).await;

        assert_eq!(run_count(&state).await, 1);
        let (mode, prompt): (String, String) = sqlx::query_as("SELECT mode, prompt FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(mode, "shadow", "a catch-up may only propose");
        assert!(prompt.contains("CATCH-UP"), "got: {prompt}");
        assert!(prompt.ends_with("go"), "got: {prompt}");
    }

    /// An `at` that is already behind us when the rule is first seen cannot be told apart from a
    /// typo, so it is reported once where a person looks and never fires.
    #[tokio::test]
    async fn an_at_already_past_when_first_seen_is_reported_and_never_fires() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        seed_one_shot(
            &state,
            project.path(),
            "shadow",
            "  - name: once\n    at: '2026-07-18T09:00:00Z'\n    prompt: go\n",
            None,
        )
        .await;

        for time in ["10:00:00", "10:01:00", "10:02:00"] {
            scheduler_tick(&state, timestamp(&format!("2026-07-18T{time}Z"))).await;
        }

        let entries: Vec<String> = sqlx::query_scalar(
            "SELECT summary FROM feed WHERE kind = 'schedule_rule_invalid' ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(entries.len(), 1, "once, not once per tick: {entries:?}");
        assert!(entries[0].contains("once"), "{}", entries[0]);
        assert!(entries[0].contains("never fire"), "{}", entries[0]);
        assert_eq!(run_count(&state).await, 0, "a past at must not fire");
    }

    /// Polls `command_ended_at` because the command runs on a spawned task, not inside the tick.
    async fn wait_for_command_result(
        state: &AppState,
    ) -> (Option<String>, Option<i64>, Option<String>) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let row: (Option<String>, Option<i64>, Option<String>, Option<String>) =
                sqlx::query_as(
                    "SELECT command_outcome, command_exit_code, command_output, command_ended_at
                 FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'once'",
                )
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if row.3.is_some() {
                return (row.0, row.1, row.2);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the scheduled command never recorded a result"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    const ONE_SHOT_COMMAND: &str =
        "  - name: once\n    at: '2026-07-18T10:05:00Z'\n    command: git --version\n";

    /// A command rule starts no agent: no `runs` row, and its result is what the rules view shows.
    #[tokio::test]
    async fn a_command_rule_runs_without_creating_an_agent_run() {
        let container = space_free_tempdir("nucleos-scheduler-command-");
        let (state, _home) = test_state(None).await;
        seed_one_shot(
            &state,
            container.path(),
            "shadow",
            ONE_SHOT_COMMAND,
            Some(&timestamp("2026-07-18T10:00:00Z").to_rfc3339()),
        )
        .await;

        scheduler_tick(&state, timestamp("2026-07-18T10:06:00Z")).await;

        let (outcome, exit_code, output) = wait_for_command_result(&state).await;
        assert_eq!(outcome.as_deref(), Some("passed"));
        assert_eq!(exit_code, Some(0));
        assert!(
            output
                .as_deref()
                .is_some_and(|tail| tail.contains("git version")),
            "got: {output:?}"
        );
        assert_eq!(run_count(&state).await, 0, "a command is not an agent run");
        let finished: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'command_finished'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(finished >= 1, "the result must reach the feed");
    }

    /// The command is judged before it is spawned. A project deny rule refuses it: nothing runs and
    /// the refusal is recorded, synchronously, so no polling is needed.
    #[tokio::test]
    async fn a_refused_scheduled_command_does_not_run_and_is_recorded_as_refused() {
        let container = space_free_tempdir("nucleos-scheduler-refused-");
        let (state, _home) = test_state(None).await;
        seed_one_shot(
            &state,
            container.path(),
            "shadow",
            ONE_SHOT_COMMAND,
            Some(&timestamp("2026-07-18T10:00:00Z").to_rfc3339()),
        )
        .await;
        crate::project_policy::declare_shell_rule(
            &state.pool,
            "proj",
            None,
            "git --version",
            crate::project_policy::Verdict::Deny,
            None,
        )
        .await
        .unwrap();

        scheduler_tick(&state, timestamp("2026-07-18T10:06:00Z")).await;

        let (outcome, exit_code, output): (Option<String>, Option<i64>, Option<String>) =
            sqlx::query_as(
                "SELECT command_outcome, command_exit_code, command_output
                 FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'once'",
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(outcome.as_deref(), Some("refused"));
        assert_eq!(exit_code, None, "a refused command has no exit code");
        assert!(
            !output.unwrap_or_default().contains("git version"),
            "a refused command must not have run"
        );
        assert_eq!(run_count(&state).await, 0);
    }

    /// A past `at` is armed at its own instant, not at the arming time: that is what lets the rules
    /// view tell "never fires" (`last_fired_at == at`) from "fired" (`last_fired_at > at`).
    #[tokio::test]
    async fn a_past_at_seen_first_is_armed_at_its_own_instant() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        seed_one_shot(
            &state,
            project.path(),
            "shadow",
            "  - name: once\n    at: '2026-07-18T09:00:00Z'\n    prompt: go\n",
            None,
        )
        .await;

        scheduler_tick(&state, timestamp("2026-07-18T10:00:00Z")).await;

        let armed: Option<String> = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'once'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let armed = chrono::DateTime::parse_from_rfc3339(&armed.expect("the rule must be armed"))
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(armed, timestamp("2026-07-18T09:00:00Z"));
        assert_eq!(run_count(&state).await, 0, "a past at must not fire");
    }

    /// The output is redacted before it is cut to its tail: cut first, a token straddling the cut
    /// loses its prefix and the surviving suffix no longer looks like a secret.
    #[tokio::test]
    async fn a_secret_straddling_the_output_cut_is_redacted_whole() {
        let container = space_free_tempdir("nucleos-scheduler-straddle-");
        let (state, _home) = test_state(None).await;
        seed_one_shot(
            &state,
            container.path(),
            "shadow",
            ONE_SHOT_COMMAND,
            Some(&timestamp("2026-07-18T10:00:00Z").to_rfc3339()),
        )
        .await;
        // `ghp_` plus 36 letters is whole-token redacted; its last 20 letters alone match nothing.
        let body = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";
        let secret = format!("ghp_{body}");
        // 100 bytes of padding, the 40-byte secret, then 4066 bytes: the 4096-byte tail starts 10
        // bytes into the secret, so 30 of its characters survive the cut.
        let output = format!("{}{secret}{}", ".".repeat(100), " ".repeat(4066));
        assert_eq!(output.len() - COMMAND_OUTPUT_TAIL_BYTES, 110);

        record_command_result(&state.pool, "proj", "once", "passed", Some(0), &output).await;

        let stored: Option<String> = sqlx::query_scalar(
            "SELECT command_output FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'once'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let stored = stored.unwrap_or_default();
        assert!(
            !stored.contains(&body[16..]),
            "the in-tail suffix of the secret leaked: {stored:?}"
        );
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn rule(name: &str, cron: &str) -> ScheduleRule {
        ScheduleRule {
            name: name.to_string(),
            cron: cron.to_string(),
            prompt: "test prompt".to_string(),
            cwd: None,
            timezone: None,
            graph: None,
            at: None,
            command: None,
        }
    }

    fn rule_in(name: &str, cron: &str, timezone: &str) -> ScheduleRule {
        ScheduleRule {
            timezone: Some(timezone.to_string()),
            ..rule(name, cron)
        }
    }

    /// A one-shot rule is due exactly while it is armed before its instant and the instant has
    /// passed. `last_fired_at` is the only memory: once the claim stamps it at or after `at`, the
    /// rule can never be due again, and that is what survives a restart.
    #[test]
    fn an_at_rule_is_due_once_and_never_after_it_fired() {
        let rules = vec![ScheduleRule {
            cron: String::new(),
            at: Some("2026-10-22T09:00:00Z".to_string()),
            ..rule("once", "")
        }];
        let fires_today = HashMap::new();
        let at = timestamp("2026-10-22T09:00:00Z");

        // Armed on 10-08, the instant has passed a minute ago: due, paired with its instant.
        let mut last_fired = HashMap::new();
        last_fired.insert("once".to_string(), timestamp("2026-10-08T00:00:00Z"));
        let due = due_rules(
            &rules,
            &last_fired,
            &fires_today,
            timestamp("2026-10-22T09:01:00Z"),
            MIN_INTERVAL,
            DAILY_CAP,
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].1, at);

        // Already fired (the claim stamped last_fired_at after `at`): never due again, however
        // late the check.
        let mut last_fired = HashMap::new();
        last_fired.insert("once".to_string(), timestamp("2026-10-22T09:01:00Z"));
        let due = due_rules(
            &rules,
            &last_fired,
            &fires_today,
            timestamp("2026-10-23T09:00:00Z"),
            MIN_INTERVAL,
            DAILY_CAP,
        );
        assert!(due.is_empty());

        // Before the instant: not due yet.
        let mut last_fired = HashMap::new();
        last_fired.insert("once".to_string(), timestamp("2026-10-08T00:00:00Z"));
        let due = due_rules(
            &rules,
            &last_fired,
            &fires_today,
            timestamp("2026-10-22T08:59:00Z"),
            MIN_INTERVAL,
            DAILY_CAP,
        );
        assert!(due.is_empty());
    }

    /// UTC is the one answer that is wrong twice a year for most of the world. `0 8 * * *` written
    /// by someone in Lisbon means 08:00 there — 08:00 UTC in winter and 07:00 UTC in summer — and
    /// reading it as UTC year-round makes the task start an hour late for half of it. That failure
    /// is worse than one that never runs, because nothing about it looks broken.
    #[test]
    fn a_rule_with_a_timezone_keeps_its_wall_clock_hour_across_a_dst_change() {
        let fires_today = HashMap::new();
        let rules = vec![rule_in("morning", "0 8 * * *", "Europe/Lisbon")];

        // Winter: Lisbon is UTC+0, so 08:00 local is 08:00 UTC.
        let mut last_fired = HashMap::new();
        last_fired.insert("morning".to_string(), timestamp("2026-01-14T08:00:00Z"));
        let due = due_rules(
            &rules,
            &last_fired,
            &fires_today,
            timestamp("2026-01-15T08:05:00Z"),
            MIN_INTERVAL,
            DAILY_CAP,
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].1, timestamp("2026-01-15T08:00:00Z"));

        // Summer: Lisbon is UTC+1, so the same rule is due an hour earlier in UTC. The wall clock
        // the user wrote has not moved; the instant it names has.
        let mut last_fired = HashMap::new();
        last_fired.insert("morning".to_string(), timestamp("2026-07-14T07:00:00Z"));
        let due = due_rules(
            &rules,
            &last_fired,
            &fires_today,
            timestamp("2026-07-15T07:05:00Z"),
            MIN_INTERVAL,
            DAILY_CAP,
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].1, timestamp("2026-07-15T07:00:00Z"));

        // And without the field, the same rule is read in UTC — which is what every schedule
        // written before this existed already meant, so none of them move. Same July date, same
        // cron: only the absence of the zone changes where it lands.
        let utc_rules = vec![rule("morning", "0 8 * * *")];
        let mut last_fired = HashMap::new();
        last_fired.insert("morning".to_string(), timestamp("2026-07-14T08:00:00Z"));
        let due = due_rules(
            &utc_rules,
            &last_fired,
            &fires_today,
            timestamp("2026-07-15T08:05:00Z"),
            MIN_INTERVAL,
            DAILY_CAP,
        );
        assert_eq!(due[0].1, timestamp("2026-07-15T08:00:00Z"));
    }

    /// A zone nobody recognises must not quietly become UTC: that fires the rule an hour off and
    /// looks like it worked.
    #[test]
    fn an_unknown_timezone_stops_the_rule_rather_than_defaulting_to_utc() {
        let mut last_fired = HashMap::new();
        last_fired.insert("morning".to_string(), timestamp("2026-07-14T08:00:00Z"));
        let rules = vec![rule_in("morning", "0 8 * * *", "Europe/Lisboa")];
        let due = due_rules(
            &rules,
            &last_fired,
            &HashMap::new(),
            timestamp("2026-07-15T08:05:00Z"),
            MIN_INTERVAL,
            DAILY_CAP,
        );
        assert!(due.is_empty());
    }

    #[test]
    fn missed_cron_boundary_is_due() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("every-minute", "* * * * *")];
        let last_fired = HashMap::from([(
            "every-minute".to_string(),
            timestamp("2026-07-18T10:00:00Z"),
        )]);

        let due = due_rules(
            &rules,
            &last_fired,
            &HashMap::new(),
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0.name, "every-minute");
        // The occurrence that came due, not `now` — this is what tells a catch-up from a fresh fire.
        assert_eq!(due[0].1, timestamp("2026-07-18T10:01:00Z"));
    }

    #[test]
    fn min_interval_suppresses_just_fired_rule() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("every-second", "* * * * * *")];
        let last_fired = HashMap::from([(
            "every-second".to_string(),
            timestamp("2026-07-18T10:01:50Z"),
        )]);

        let due = due_rules(
            &rules,
            &last_fired,
            &HashMap::new(),
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert!(due.is_empty());
    }

    #[test]
    fn invalid_cron_is_skipped() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("invalid", "not a cron")];
        let last_fired =
            HashMap::from([("invalid".to_string(), timestamp("2026-07-18T09:00:00Z"))]);

        let due = due_rules(
            &rules,
            &last_fired,
            &HashMap::new(),
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert!(due.is_empty());
    }

    #[test]
    fn daily_cap_suppresses_due_rule() {
        let now = timestamp("2026-07-18T10:02:00Z");
        let rules = vec![rule("every-minute", "* * * * *")];
        let last_fired = HashMap::from([(
            "every-minute".to_string(),
            timestamp("2026-07-18T10:00:00Z"),
        )]);
        let fires_today = HashMap::from([("every-minute".to_string(), DAILY_CAP)]);

        let due = due_rules(
            &rules,
            &last_fired,
            &fires_today,
            now,
            chrono::Duration::minutes(1),
            DAILY_CAP,
        );

        assert!(due.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tick_fires_a_worktree_run_for_an_active_project() {
        if std::env::var_os(ACTIVE_TEST_CHILD_ENV).is_none() {
            let _lock = env_lock();
            let repo_container = space_free_tempdir("nucleos-scheduler-active-");
            let repo = repo_container.path().join("repo");
            initialize_repo(&repo);
            let worktree_root = space_free_tempdir("nucleos-wt-test-scheduler-");
            let status = Command::new(std::env::current_exe().expect("resolve test executable"))
                .args([
                    "--exact",
                    "scheduler::tests::tick_fires_a_worktree_run_for_an_active_project",
                    "--nocapture",
                ])
                .env(ACTIVE_TEST_CHILD_ENV, "1")
                .env(ACTIVE_TEST_REPO_ENV, &repo)
                .env(WORKTREE_ROOT_ENV, worktree_root.path())
                .status()
                .expect("start isolated scheduler test process");
            assert!(status.success(), "isolated scheduler test process failed");
            return;
        }

        let repo = PathBuf::from(
            std::env::var_os(ACTIVE_TEST_REPO_ENV).expect("active test repository is set"),
        );
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, &repo, "active", &old).await;

        scheduler_tick(&state, now).await;

        let (run_id, mode, project_id): (i64, String, Option<String>) =
            sqlx::query_as("SELECT id, mode, project_id FROM runs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mode, "worktree");
        assert_eq!(project_id.as_deref(), Some("proj"));
        let worktree_path: String = sqlx::query_scalar(
            "SELECT path FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let stored: String = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'r1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(timestamp(&stored), now);

        let _ = crate::worktree::remove(&repo, &PathBuf::from(worktree_path), &[]).await;
    }

    #[test]
    fn a_window_is_a_catch_up_only_once_it_is_properly_late() {
        let due = timestamp("2026-07-18T10:00:00Z");
        let grace = chrono::Duration::minutes(15);

        // Served within a tick or two: ordinary, not a catch-up.
        assert!(!is_catch_up(due, timestamp("2026-07-18T10:00:30Z"), grace));
        // Exactly at the grace boundary is still not late enough — the daemon may just be busy.
        assert!(!is_catch_up(due, timestamp("2026-07-18T10:15:00Z"), grace));
        assert!(is_catch_up(due, timestamp("2026-07-18T10:15:01Z"), grace));
        // The case this exists for: the machine was off overnight.
        assert!(is_catch_up(due, timestamp("2026-07-19T09:00:00Z"), grace));
    }

    #[test]
    fn the_catch_up_preamble_warns_about_lateness_and_only_re_triages_when_head_moved() {
        let steady = catch_up_preamble(chrono::Duration::hours(4), false);
        assert!(steady.contains("CATCH-UP"), "got: {steady}");
        assert!(steady.contains("4 hours"), "got: {steady}");
        assert!(steady.contains("plan-only"), "got: {steady}");
        // Nothing is claimed about the repository when we have no evidence it moved.
        assert!(!steady.contains("Re-triage"), "got: {steady}");

        let moved = catch_up_preamble(chrono::Duration::minutes(90), true);
        assert!(moved.contains("Re-triage"), "got: {moved}");
        assert!(moved.contains("1 hours"), "got: {moved}");

        // Lateness reads in the largest whole unit that fits.
        assert!(catch_up_preamble(chrono::Duration::minutes(20), false).contains("20 minutes"));
        assert!(catch_up_preamble(chrono::Duration::days(3), false).contains("3 days"));
    }

    #[tokio::test]
    async fn a_missed_window_is_demoted_to_a_plan_only_catch_up_on_an_active_project() {
        let project = tempfile::tempdir().expect("create active project");
        let (state, _home) = test_state(None).await;
        // The window came due at 10:01; the daemon only ran again four hours later.
        let now = timestamp("2026-07-18T14:00:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;

        scheduler_tick(&state, now).await;

        let (mode, prompt): (String, String) = sqlx::query_as("SELECT mode, prompt FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        // An active project would normally get a worktree run; a catch-up may only propose.
        assert_eq!(mode, "shadow");
        assert!(prompt.contains("CATCH-UP"), "got: {prompt}");
        assert!(prompt.contains("plan-only"), "got: {prompt}");
        // The rule's own prompt still rides at the end, unaltered.
        assert!(prompt.ends_with("go"), "got: {prompt}");
    }

    #[test]
    fn quota_brake_hold_does_not_excuse_a_window_missed_before_it() {
        assert_eq!(
            lateness_origin(
                timestamp("2026-07-18T02:00:00Z"),
                Some((
                    timestamp("2026-07-18T09:00:00Z"),
                    timestamp("2026-07-18T09:10:00Z"),
                )),
            ),
            timestamp("2026-07-18T02:00:00Z"),
        );
        assert_eq!(
            lateness_origin(
                timestamp("2026-07-18T10:00:00Z"),
                Some((
                    timestamp("2026-07-18T10:05:00Z"),
                    timestamp("2026-07-18T13:55:00Z"),
                )),
            ),
            timestamp("2026-07-18T13:55:00Z"),
        );
        assert_eq!(
            lateness_origin(
                timestamp("2026-07-18T09:00:00Z"),
                Some((
                    timestamp("2026-07-18T09:15:00Z"),
                    timestamp("2026-07-18T10:00:00Z"),
                )),
            ),
            timestamp("2026-07-18T10:00:00Z"),
        );
        assert_eq!(
            lateness_origin(
                timestamp("2026-07-18T09:00:00Z"),
                Some((
                    timestamp("2026-07-18T09:16:00Z"),
                    timestamp("2026-07-18T10:00:00Z"),
                )),
            ),
            timestamp("2026-07-18T09:00:00Z"),
        );
        assert_eq!(
            lateness_origin(
                timestamp("2026-07-18T11:00:00Z"),
                Some((
                    timestamp("2026-07-18T09:00:00Z"),
                    timestamp("2026-07-18T10:00:00Z"),
                )),
            ),
            timestamp("2026-07-18T11:00:00Z"),
        );
        assert_eq!(
            lateness_origin(timestamp("2026-07-18T11:00:00Z"), None),
            timestamp("2026-07-18T11:00:00Z"),
        );
    }

    #[tokio::test]
    async fn quota_brake_delay_does_not_demote_a_scheduled_rule() {
        let project = tempfile::tempdir().expect("create active project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T14:00:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        sqlx::query(
            "UPDATE autopilot_global
             SET quota_hold_started_at = ?, quota_hold_ended_at = ?",
        )
        .bind(timestamp("2026-07-18T10:05:00Z").to_rfc3339())
        .bind(timestamp("2026-07-18T13:55:00Z").to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        scheduler_tick(&state, now).await;

        let (mode, prompt): (String, String) = sqlx::query_as("SELECT mode, prompt FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(mode, "worktree");
        assert!(!prompt.contains("CATCH-UP"), "got: {prompt}");
    }

    #[tokio::test]
    async fn a_catch_up_over_a_moved_head_is_told_to_re_triage() {
        let container = space_free_tempdir("nucleos-scheduler-catchup-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T14:00:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, &repo, "shadow", &old).await;
        // The rule was armed at a HEAD the repository has since left behind.
        sqlx::query(
            "UPDATE scheduler_state SET last_head_sha = 'deadbeef' WHERE project_id = 'proj'",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        scheduler_tick(&state, now).await;

        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(prompt.contains("Re-triage"), "got: {prompt}");

        // Firing re-anchors the sha, so the next window is compared against the right moment.
        let recorded: Option<String> = sqlx::query_scalar(
            "SELECT last_head_sha FROM scheduler_state WHERE project_id = 'proj'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(
            recorded.is_some_and(|sha| sha != "deadbeef"),
            "the fired rule should record the HEAD it was scheduled from"
        );
    }

    #[tokio::test]
    async fn a_punctual_window_carries_no_catch_up_preamble() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        // Due at 10:01, served at 10:10 — late, but well inside the grace.
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;

        scheduler_tick(&state, now).await;

        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(prompt, "go");
    }

    #[tokio::test]
    async fn tick_fires_a_shadow_run_for_a_shadow_project() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;

        scheduler_tick(&state, now).await;

        let (mode, project_id): (String, Option<String>) =
            sqlx::query_as("SELECT mode, project_id FROM runs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mode, "shadow");
        assert_eq!(project_id.as_deref(), Some("proj"));
    }

    /// "Busy" used to mean the attention brake: a run in flight in this project deferred it. That
    /// question moved to `concurrency.rs` (spec §7.3), which can say "two" where attention could
    /// only ever say "one", so busy now means the slots are taken.
    #[tokio::test]
    async fn tick_defers_when_no_slot_is_free() {
        let project = tempfile::tempdir().expect("create active project");
        let (state, _home) = test_state(Some(Duration::from_millis(100))).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = 1")
            .execute(&state.pool)
            .await
            .unwrap();
        let running = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('proj', 'already running', 'running', 'worktree', ?)",
        )
        .bind(timestamp("2026-07-18T10:05:00Z").to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        // Seeded rows claim nothing on their own; `create_run_inner` is what claims, and this row
        // did not go through it.
        crate::concurrency::claim(&state.pool, "proj", crate::worktree::Owner::Run(running))
            .await
            .expect("the running node holds the project's only slot");

        scheduler_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
        let stored: String = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'r1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(stored, old);
    }

    #[tokio::test]
    async fn tick_rearms_a_malformed_timestamp() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        seed_project(&state, project.path(), "shadow", "not-a-date").await;

        scheduler_tick(&state, now).await;

        let stored: String = sqlx::query_scalar(
            "SELECT last_fired_at FROM scheduler_state WHERE project_id = 'proj' AND rule_name = 'r1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(timestamp(&stored), now);
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn tick_does_nothing_when_kill_switch_engaged() {
        let project = tempfile::tempdir().expect("create active project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        crate::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        scheduler_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn a_project_heartbeat_only_stops_that_projects_scheduled_rule() {
        let projects = tempfile::tempdir().expect("create project roots");
        let project_a = projects.path().join("project-a");
        let project_b = projects.path().join("project-b");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_named_project(&state, "project-a", &project_a, "shadow", &old).await;
        seed_named_project(&state, "project-b", &project_b, "shadow", &old).await;
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Project("project-a".to_string()),
            now,
        )
        .await
        .unwrap();

        scheduler_tick(&state, now).await;

        let fired_projects: Vec<String> =
            sqlx::query_scalar("SELECT project_id FROM runs ORDER BY project_id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(fired_projects, ["project-b"]);
    }

    #[tokio::test]
    async fn a_global_heartbeat_stops_all_scheduled_rules() {
        let projects = tempfile::tempdir().expect("create project roots");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_named_project(
            &state,
            "project-a",
            &projects.path().join("project-a"),
            "shadow",
            &old,
        )
        .await;
        seed_named_project(
            &state,
            "project-b",
            &projects.path().join("project-b"),
            "shadow",
            &old,
        )
        .await;
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Global,
            now,
        )
        .await
        .unwrap();

        scheduler_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn no_heartbeat_leaves_scheduled_firing_unchanged() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;

        scheduler_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
    }

    #[tokio::test]
    async fn tick_does_nothing_when_over_budget() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;

        // A $1 monthly ceiling with $5 of prior autonomous spend this window -> over budget.
        crate::budget::set_budget_config(
            &state.pool,
            &crate::budget::BudgetConfig {
                limit_usd: Some(1.0),
                period: crate::budget::BudgetPeriod::Monthly,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, cost_usd, created_at, completed_at)
             VALUES ('proj', 'prior spend', 'completed', 'worktree', 5.0, ?, ?)",
        )
        .bind(timestamp("2026-07-05T09:00:00Z").to_rfc3339())
        .bind(timestamp("2026-07-05T09:10:00Z").to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        scheduler_tick(&state, now).await;

        // Only the pre-existing spend row remains; the over-budget scheduler fired no new run.
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
    }

    #[tokio::test]
    async fn tick_skips_a_project_when_its_kill_is_engaged() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        crate::autopilot::set_scoped_kill(&state.pool, "project", "proj", true)
            .await
            .unwrap();

        scheduler_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }

    #[tokio::test]
    async fn tick_still_fires_when_a_different_project_is_killed() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        crate::autopilot::set_scoped_kill(&state.pool, "project", "some-other-project", true)
            .await
            .unwrap();

        scheduler_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 1);
    }

    #[tokio::test]
    async fn tick_skips_all_when_the_scheduled_trigger_kill_is_engaged() {
        let project = tempfile::tempdir().expect("create shadow project");
        let (state, _home) = test_state(None).await;
        let now = timestamp("2026-07-20T10:10:00Z");
        let old = timestamp("2026-07-20T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        crate::autopilot::set_scoped_kill(&state.pool, "trigger", "scheduled", true)
            .await
            .unwrap();

        scheduler_tick(&state, now).await;

        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_count, 0);
    }
}
