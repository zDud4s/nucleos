use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
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
    match rule.timezone.as_deref() {
        None => Ok(Tz::UTC),
        Some(name) => name
            .parse::<Tz>()
            .map_err(|_| format!("'{name}' is not an IANA timezone")),
    }
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
    let cron = rule
        .cron
        .parse::<Cron>()
        .map_err(|error| format!("'{}' is not a cron expression: {error}", rule.cron))?;
    let zone = rule_timezone(rule)?;
    cron.find_next_occurrence(&since.with_timezone(&zone), false)
        .map(|next| next.with_timezone(&Utc))
        .map_err(|error| format!("no next occurrence for '{}': {error}", rule.cron))
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
            // The daemon's ceiling, not the file's number. `.ai/autopilot.yaml` is per-developer
            // and gitignored, so nobody reviews what it asks for; it may lower the fan-out and
            // never raise it.
            max_items: graph.max_items() as i64,
            gate_each: graph.gate_after_each_item,
            review: graph.review,
            head_sha,
            // A scheduled job asks for neither, which keeps it at one round under the house limit —
            // exactly what a `graph:` rule did before rounds existed. Rounds are opt-in per request,
            // not something a rule already in somebody's `.ai/autopilot.yaml` acquires overnight.
            max_rounds: None,
            budget_usd: None,
        },
    )
    .await
}

pub(crate) async fn scheduler_tick(state: &AppState, now: DateTime<Utc>) {
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

    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::budget::budget_permits_new_run(&state.pool, now).await
    {
        tracing::info!(reason = %reason, "budget exhausted; scheduler paused this tick");
        return;
    }

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

        let rules = match config::load_schedule_rules(Path::new(&project_root)) {
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
            let unreadable = rule
                .cron
                .parse::<Cron>()
                .err()
                .map(|error| format!("an unreadable cron ({}): {error}", rule.cron))
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
                )
                .await;
            }

            // Armed regardless, so this branch runs once rather than every tick. A rule that cannot
            // be read is still a rule the user wrote, and forgetting it would only mean announcing
            // it again in 30 seconds.
            if let Err(error) = sqlx::query(
                "INSERT INTO scheduler_state (project_id, rule_name, last_fired_at, last_head_sha)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&project_id)
            .bind(&rule.name)
            .bind(now.to_rfc3339())
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

            // A catch-up is demoted to plan-only whatever the project's mode: it carries assumptions
            // as old as the window it missed, so the most it may produce is a proposal.
            let catch_up = is_catch_up(
                due_at,
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
                Err(error @ (CreateRunError::Busy | CreateRunError::Invalid(_))) => {
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

    async fn test_state(delay: Option<Duration>) -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        AppState {
            token: Token("test-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner {
                delay: Mutex::new(delay),
                ..Default::default()
            }),
            triage_runner: None,
            local_triage_disabled: None,
            local_assistant: None,
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: DEFAULT_RUN_TIMEOUT,
        }
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

    fn write_schedule(project_root: &FsPath) {
        let ai_dir = project_root.join(".ai");
        std::fs::create_dir_all(&ai_dir).expect("create .ai directory");
        std::fs::write(
            ai_dir.join("autopilot.yaml"),
            "schedules:\n  - name: r1\n    cron: \"* * * * *\"\n    prompt: \"go\"\n",
        )
        .expect("write autopilot schedule");
    }

    /// The same rule with a `graph:` block on it, so a test can compare like with like.
    fn write_graph_schedule(project_root: &FsPath) {
        std::fs::write(
            project_root.join(".ai").join("autopilot.yaml"),
            "schedules:\n  - name: r1\n    cron: \"* * * * *\"\n    prompt: \"go\"\n    graph:\n      max_items: 3\n",
        )
        .expect("write autopilot schedule with a graph block");
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
        let state = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "shadow", &old).await;
        write_graph_schedule(project.path());

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
        let state = test_state(None).await;
        // Due at 10:01; the daemon only came back four hours later.
        let now = timestamp("2026-07-18T14:00:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        write_graph_schedule(project.path());

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
        let state = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, project.path(), "active", &old).await;
        write_graph_schedule(project.path());
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
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
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
        let state = test_state(None).await;
        let now = timestamp("2026-07-18T10:10:00Z");
        let old = timestamp("2026-07-18T10:00:00Z").to_rfc3339();
        seed_project(&state, &repo, "active", &old).await;
        write_graph_schedule(&repo);

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
        write_schedule(project_root);
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
        let container = space_free_tempdir("nucleos-scheduler-badcron-");
        let repo = container.path().join("repo");
        std::fs::create_dir_all(repo.join(".ai")).unwrap();
        std::fs::write(
            repo.join(".ai").join("autopilot.yaml"),
            "schedules:\n  - name: r1\n    cron: \"not a cron\"\n    prompt: \"go\"\n",
        )
        .unwrap();
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
        }
    }

    fn rule_in(name: &str, cron: &str, timezone: &str) -> ScheduleRule {
        ScheduleRule {
            timezone: Some(timezone.to_string()),
            ..rule(name, cron)
        }
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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

    #[tokio::test]
    async fn a_catch_up_over_a_moved_head_is_told_to_re_triage() {
        let container = space_free_tempdir("nucleos-scheduler-catchup-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(Some(Duration::from_millis(100))).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
        let state = test_state(None).await;
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
