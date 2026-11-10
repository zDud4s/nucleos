//! Bounded retries of the tree driver: what it does when a gate keeps measuring nothing, a landing
//! keeps failing or waits on a resolver nobody runs, and a green branch waits on a base that will
//! not merge. Each of these used to repeat on every tick without end and without a word; here each
//! has an allowance persisted in the manifest, and past it the owner is asked once.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::decisions::{ROOT_PARK_KEY, STATUS_OPEN};
use super::driver::{GateVerdict, NotMeasured, Park, RESOLVER_GRACE};
use super::final_verify::{FINAL_FIX_KEY, is_gate_red};
use super::ladder::{self, Kind, Spent, Step};
use super::landing::{LandSettlement, apply_gate_verdict, claim_land};
use super::store::{self, TreeJob};
use crate::gate::GateOutcome;
use crate::verify::ScopeVerdict;

/// Gates of one branch in a row that may end without measuring anything before the owner is asked:
/// the first, and the ladder's retries of it. The code asks the ladder itself; tests count with this.
#[cfg(test)]
pub(super) const GATE_ATTEMPTS: i64 = ladder::retry_cap(Kind::Gate) + 1;
/// The key of `Branch.extra` that counts those gates.
pub(super) const GATE_FAILURES: &str = "gate_failures";
/// Landings of one job that may fail in the queue or be refused before the owner is asked: the
/// first, and the ladder's retries of it. The landing queue paces its own retries, and there is no
/// per-job "not before" for them, so this kind has no backoff of its own.
pub(super) const LAND_ATTEMPTS: i64 = ladder::retry_cap(Kind::Land) + 1;
/// The key of `Manifest.extra` that counts them: `{count, at, reason, decision_id?}`.
pub(super) const LAND_ATTEMPTS_KEY: &str = "land_attempts";
/// The key of `Manifest.extra` stamping (RFC 3339) when an escalated landing was first seen
/// waiting on the conflict resolver.
pub(super) const LAND_ESCALATED_SINCE: &str = "land_escalated_since";
/// The key of `Branch.extra` naming the base's park decision a green dependent was announced for.
pub(super) const BASE_WAIT_KEY: &str = "base_wait_noted";

/// The key of `Branch.extra` counting gates in a row that ended unmeasured because of load.
pub(super) const GATE_LOAD_WAITS: &str = "gate_load_waits";
/// The key of `Branch.extra` stamping (RFC 3339) until when the branch's gate waits.
pub(super) const GATE_BACKOFF_UNTIL: &str = "gate_backoff_until";
/// The key of `Branch.extra` with the last load reason, for whoever reads the manifest.
pub(super) const GATE_LOAD_REASON: &str = "gate_load_reason";
/// The key of `Branch.extra` summing (in seconds) the backoffs the branch's gate has waited out.
pub(super) const GATE_LOAD_WAITED_SECS: &str = "gate_load_waited_secs";
/// The key of `Branch.extra` counting the gates in a row whose units hit the time ceiling.
pub(super) const GATE_CEILING_HITS: &str = "gate_ceiling_hits";
/// The key of `Branch.extra` with the runnable units the last ceiling hit measured.
pub(super) const GATE_CEILING_UNITS: &str = "gate_ceiling_units";
/// The most a re-measured unit may be given, as a multiple of the configured unit ceiling (1800 s
/// by default, so 2 h): twice the post-merge gate's own ceiling, beyond which a gate that still
/// times out is not slow but stuck, and the 6 h wait cap hands it to the owner.
pub(super) const CEILING_MAX_FACTOR: u64 = 4;

/// The per-unit ceiling, in seconds, for the gate that follows `hits` time-ceiling hits over a
/// scope of `units` runnable units, on top of the configured `base_secs`. 0 (keep the configured
/// one) before any hit. The factor is one step per two units in the scope (a scope of many test
/// units runs them longer in one process tree) plus one step per hit already seen, so a gate that
/// hit the ceiling always gets more than before; it is held between 2 and [`CEILING_MAX_FACTOR`].
pub(super) fn scoped_ceiling_secs(base_secs: u64, units: u64, hits: u64) -> u64 {
    if hits == 0 {
        return 0;
    }
    let factor = units
        .div_ceil(2)
        .max(1)
        .saturating_add(hits)
        .clamp(2, CEILING_MAX_FACTOR);
    base_secs.saturating_mul(factor)
}

/// The unit ceiling a branch's next gate asks for, from what its last gates left on the branch.
pub(super) fn branch_ceiling_secs(extra: &BTreeMap<String, Value>, base_secs: u64) -> u64 {
    let get = |key: &str| extra.get(key).and_then(Value::as_u64).unwrap_or(0);
    scoped_ceiling_secs(base_secs, get(GATE_CEILING_UNITS), get(GATE_CEILING_HITS))
}

/// How long a gate may wait in total on infrastructure before it leaves the retry rung of the
/// ladder (autonomy-trees spec A1); a gate has no agent to diagnose it, so the next rung is the park.
pub(super) const GATE_LOAD_CAP_SECS: i64 = ladder::LOAD_CAP_SECS;

/// Whether an error says a process can never be spawned the way it was asked to be, however often
/// it is tried: Windows `os error 206` (the file name or command line is too long) and `267` (the
/// directory name is invalid). A wait does not undo either, so none of the retry loops may take
/// them for a passing fault: they fail the branch, or ask the owner, at the first occurrence.
pub(super) fn is_permanent_spawn_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    ["os error 206", "os error 267"]
        .iter()
        .any(|marker| error.contains(marker))
}

/// Whether a reason for an unmeasured gate says the machine was too busy, not that the gate is broken.
pub(super) fn is_load_reason(reason: &str) -> bool {
    // "failed to spawn ... (os error 206)" would otherwise read as load and be waited out for ever.
    if is_permanent_spawn_error(reason) {
        return false;
    }
    let reason = reason.to_ascii_lowercase();
    [
        "pool timed out",
        "broker is full",
        "broker full",
        "disk pressure",
        "disk-pressure",
        "failed to spawn",
        "spawn failure",
        "sccache",
        "infrastructure failure",
        // The verify queue's own storage and verdict plumbing (autonomy-trees spec A1): the code
        // was not measured, so the gate is neither attempted nor counted.
        "database is locked",
        "queue exhausted",
        "queue is exhausted",
        "ended errored",
        "without a verdict",
        // A unit cut off at its time ceiling measured nothing either; the gate is re-measured with
        // a ceiling sized to the scope (`scoped_ceiling_secs`), not counted as a red or an attempt.
        crate::verify::TIME_CEILING_MARKER,
    ]
    .iter()
    .any(|marker| reason.contains(marker))
}

/// Whether a failed unit's output is infrastructure, not the branch's: cargo says it could not
/// compile and that a process did not exit successfully, but neither rustc nor a test runner said
/// anything about the code (no `error[..]` or other `error:` line, no failed or panicked test).
/// Answers the reason to give the gate, which `is_load_reason` reads as load.
pub(super) fn infrastructure_failure(output: &str) -> Option<String> {
    if !output.contains("could not compile") || !output.contains("process didn't exit successfully")
    {
        return None;
    }
    let diagnosed = output.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("error[")
            || line.contains("panicked at")
            || line.contains("FAILED")
            || (line.starts_with("error:")
                && !line.contains("could not compile")
                && !line.contains("process didn't exit successfully"))
    });
    if diagnosed {
        return None;
    }
    Some(
        "infrastructure failure: a compiler process exited non-zero without any diagnostic \
         (cargo could not compile, no rustc error), so nothing was measured"
            .to_owned(),
    )
}

fn clear_load_wait(extra: &mut BTreeMap<String, Value>) {
    extra.remove(GATE_LOAD_WAITS);
    extra.remove(GATE_BACKOFF_UNTIL);
    extra.remove(GATE_LOAD_REASON);
    extra.remove(GATE_LOAD_WAITED_SECS);
    extra.remove(GATE_CEILING_HITS);
    extra.remove(GATE_CEILING_UNITS);
}

/// What a gate starved by infrastructure does next, after `waits` earlier waits that added up to
/// `waited_secs`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LoadStep {
    /// Wait this long, then gate again.
    Wait(Duration),
    /// The accumulated wait would pass [`GATE_LOAD_CAP_SECS`]: ask the owner.
    CapReached,
}

pub(super) fn load_step(waits: i64, waited_secs: i64) -> LoadStep {
    let spent = Spent {
        retries: waits,
        waited_secs,
        diagnosed: false,
    };
    match ladder::next_step(Kind::GateLoadWait, spent) {
        Step::Retry { after } => LoadStep::Wait(after),
        // No agent can diagnose a starved gate, so the rung after the retries is the park.
        Step::Diagnose | Step::Park => LoadStep::CapReached,
    }
}

/// Whether the branch's gate is still inside a load backoff.
pub(super) fn gate_backing_off(extra: &BTreeMap<String, Value>) -> bool {
    extra
        .get(GATE_BACKOFF_UNTIL)
        .and_then(Value::as_str)
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .is_some_and(|until| until > Utc::now())
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

async fn persist(pool: &SqlitePool, job: &mut TreeJob) -> Result<(), String> {
    job.updated_at = now();
    store::update(pool, job).await.map_err(|e| e.to_string())
}

// ---- the gate ----------------------------------------------------------------------------------

/// Records what a finished gate task answered for `branch`, under the job's lock (the caller holds
/// it). Green and red are applied and reset the count of unmeasured gates. `NothingRan` is green
/// unmeasured: no unit agrees with the diff, so nothing could fail, and the entry says so.
/// `Inherited` is green too, with the base's incident on the entry: its red is master's, so it is
/// neither blamed on the branch nor spent from the fix cap. A gate
/// that ended without measuring counts one; at `GATE_ATTEMPTS` the answer is a park, which the
/// caller asks outside the lock. A ticket still running is never counted. A branch that moved on
/// while the gate ran (parked, dropped) takes nothing.
pub(super) async fn apply_gate_answer(
    pool: &SqlitePool,
    job_id: &str,
    branch: &str,
    answer: Result<GateVerdict, NotMeasured>,
) -> Result<Option<Park>, String> {
    let scope = match &answer {
        Ok(GateVerdict::Green | GateVerdict::NothingRan | GateVerdict::Inherited { .. }) => {
            ScopeVerdict::Measured(GateOutcome::Passed)
        }
        Ok(GateVerdict::Red { summary }) => ScopeVerdict::Measured(GateOutcome::Failed {
            exit_code: 1,
            output: summary.clone(),
        }),
        Err(NotMeasured::Running(_)) => return Ok(None),
        Err(NotMeasured::Failed(_)) => ScopeVerdict::Unavailable,
    };
    let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let Some(held) = job.manifest.branches.get_mut(branch) else {
        return Ok(None);
    };
    if !matches!(held.state.as_str(), "building" | "fixing" | "reviewing") {
        return Ok(None);
    }
    let mut park = None;
    match answer {
        // The machine was too busy to measure: wait with backoff, spend nothing, ask nobody.
        Err(NotMeasured::Failed(reason)) if is_load_reason(&reason) => {
            let waits = held
                .extra
                .get(GATE_LOAD_WAITS)
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let waited = held
                .extra
                .get(GATE_LOAD_WAITED_SECS)
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if let Some((_, units)) = crate::verify::parse_time_ceiling(&reason) {
                let hits = held
                    .extra
                    .get(GATE_CEILING_HITS)
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                held.extra
                    .insert(GATE_CEILING_HITS.to_owned(), json!(hits + 1));
                held.extra
                    .insert(GATE_CEILING_UNITS.to_owned(), json!(units));
            }
            match load_step(waits, waited) {
                LoadStep::Wait(delay) => {
                    let until = Utc::now()
                        + chrono::Duration::from_std(delay)
                            .unwrap_or_else(|_| chrono::Duration::minutes(10));
                    held.extra
                        .insert(GATE_LOAD_WAITS.to_owned(), json!(waits + 1));
                    held.extra.insert(
                        GATE_LOAD_WAITED_SECS.to_owned(),
                        json!(waited + i64::try_from(delay.as_secs()).unwrap_or(0)),
                    );
                    held.extra.insert(
                        GATE_BACKOFF_UNTIL.to_owned(),
                        json!(until.to_rfc3339_opts(SecondsFormat::Secs, false)),
                    );
                    held.extra
                        .insert(GATE_LOAD_REASON.to_owned(), json!(reason));
                }
                LoadStep::CapReached => {
                    // The owner's answer is a fresh allowance, so the wait starts again.
                    clear_load_wait(&mut held.extra);
                    if let Some(node) = held.node_id.clone() {
                        park = Some((
                            node,
                            format!(
                                "The gate of branch {branch} waited {} h in total on infrastructure                                  without measuring anything (the cap is {} h); last reason: {reason}",
                                waited / 3600,
                                GATE_LOAD_CAP_SECS / 3600
                            ),
                        ));
                    }
                }
            }
        }
        Err(NotMeasured::Failed(reason)) => {
            clear_load_wait(&mut held.extra);
            let count = held
                .extra
                .get(GATE_FAILURES)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + 1;
            // `count` includes the gate that just ended; the retries so far are one fewer.
            let step = ladder::next_step(
                Kind::Gate,
                Spent {
                    retries: count - 1,
                    ..Spent::default()
                },
            );
            match step {
                Step::Retry { after } => {
                    // The next gate waits minutes, so a verify queue that is down is not hammered.
                    let until = Utc::now()
                        + chrono::Duration::from_std(after)
                            .unwrap_or_else(|_| chrono::Duration::minutes(30));
                    held.extra.insert(
                        GATE_BACKOFF_UNTIL.to_owned(),
                        json!(until.to_rfc3339_opts(SecondsFormat::Secs, false)),
                    );
                    held.extra.insert(GATE_FAILURES.to_owned(), json!(count));
                }
                // No agent can diagnose a gate that measures nothing: the diagnose rung is a park.
                Step::Diagnose | Step::Park => {
                    // The owner's answer is a fresh allowance, so the count starts again.
                    held.extra.remove(GATE_FAILURES);
                    if let Some(node) = held.node_id.clone() {
                        park = Some((
                            node,
                            format!(
                                "The gate of branch {branch} ended {count} times in a row without measuring anything; last reason: {reason}"
                            ),
                        ));
                    }
                }
            }
        }
        answer => {
            clear_load_wait(&mut held.extra);
            held.extra.remove(GATE_FAILURES);
            apply_gate_verdict(held, &scope).map_err(|e| e.to_string())?;
            let entry = held.gates.last_mut().and_then(Value::as_object_mut);
            match (answer, entry) {
                (Ok(GateVerdict::NothingRan), Some(entry)) => {
                    entry.insert("unmeasured".to_owned(), json!(true));
                    entry.insert(
                        "reason".to_owned(),
                        json!("no test unit agrees with the branch's diff, so nothing ran"),
                    );
                }
                (Ok(GateVerdict::Inherited { note }), Some(entry)) => {
                    entry.insert("inherited".to_owned(), json!(note));
                }
                _ => {}
            }
        }
    }
    persist(pool, &mut job).await?;
    Ok(park)
}

// ---- landing -----------------------------------------------------------------------------------

/// The landing's attempt record, or an empty one.
fn land_record(job: &TreeJob) -> serde_json::Map<String, Value> {
    job.manifest
        .extra
        .get(LAND_ATTEMPTS_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn land_question(count: i64, reason: &str) -> String {
    if is_gate_red(reason) {
        return format!(
            "The final verify of the tree is red at the tip being landed, so nothing was published; {reason}
Fix the integrated branch, then answer to let the landing try again."
        );
    }
    format!(
        "The landing of the tree did not go through after {count} attempt(s); last reason: {reason}. Answer to let it try again."
    )
}

/// Counts one landing that failed in the queue or was refused, persists it, and past
/// [`LAND_ATTEMPTS`] asks the owner on the root.
pub(super) async fn count_land_attempt(
    pool: &SqlitePool,
    job: &mut TreeJob,
    reason: &str,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    // A red final verify is about a tip that has not changed: trying again repeats it, so the
    // landing is not counted. A fix run of the integrated tip is asked for instead, and the owner
    // only once the ladder's rungs for it are spent (`final_verify::step`).
    if is_gate_red(reason) {
        super::final_verify::request(job, reason);
        return persist(pool, job).await;
    }
    let count = land_record(job)
        .get("count")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    set_land_record(job, count, reason);
    persist(pool, job).await?;
    if count >= LAND_ATTEMPTS {
        parks.push((job.root_node_id.clone(), land_question(count, reason)));
    }
    Ok(())
}

pub(super) fn set_land_record(job: &mut TreeJob, count: i64, reason: &str) {
    job.manifest.extra.insert(
        LAND_ATTEMPTS_KEY.to_owned(),
        json!({ "count": count, "at": now(), "reason": reason }),
    );
}

async fn decision_status(pool: &SqlitePool, id: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT status FROM decisions WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())
}

/// Whether the job may submit a landing now. Past the cap it waits for the owner: the decision the
/// root was asked on is remembered in the record, and once it is answered the record is dropped,
/// which is a fresh allowance. Until then the park is asked again (a no-op while it is open).
pub(super) async fn land_allowed(
    pool: &SqlitePool,
    job: &mut TreeJob,
    parks: &mut Vec<Park>,
) -> Result<bool, String> {
    let mut record = land_record(job);
    let count = record.get("count").and_then(Value::as_i64).unwrap_or(0);
    if count < LAND_ATTEMPTS {
        return Ok(true);
    }
    let reason = record
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let asked = record
        .get("decision_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(asked) = asked {
        match decision_status(pool, &asked).await?.as_deref() {
            Some(STATUS_OPEN) => {}
            Some(_) => {
                job.manifest.extra.remove(LAND_ATTEMPTS_KEY);
                // The answer is a fresh allowance for the fix of the integrated tip too.
                job.manifest.extra.remove(FINAL_FIX_KEY);
                persist(pool, job).await?;
                return Ok(true);
            }
            None => {
                // The decision is gone: ask again.
                record.remove("decision_id");
                job.manifest
                    .extra
                    .insert(LAND_ATTEMPTS_KEY.to_owned(), Value::Object(record));
                persist(pool, job).await?;
                parks.push((job.root_node_id.clone(), land_question(count, &reason)));
            }
        }
        return Ok(false);
    }
    let root_decision = job
        .manifest
        .extra
        .get(ROOT_PARK_KEY)
        .and_then(|park| park.get("decision_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(decision) = root_decision
        && decision_status(pool, &decision).await?.as_deref() == Some(STATUS_OPEN)
    {
        record.insert("decision_id".to_owned(), json!(decision));
        job.manifest
            .extra
            .insert(LAND_ATTEMPTS_KEY.to_owned(), Value::Object(record));
        persist(pool, job).await?;
        return Ok(false);
    }
    parks.push((job.root_node_id.clone(), land_question(count, &reason)));
    Ok(false)
}

/// Whether `since` (RFC 3339) lies further back than `grace`.
fn overdue(since: &str, grace: Duration) -> bool {
    chrono::DateTime::parse_from_rfc3339(since).is_ok_and(|since| {
        (Utc::now() - since.with_timezone(&Utc))
            .to_std()
            .is_ok_and(|waited| waited > grace)
    })
}

/// Acts on what `settle_land` read. A landing that failed (its id dropped) is counted. An
/// escalated landing keeps its id for the conflict resolver: the wait is stamped, and past
/// [`RESOLVER_GRACE`] the request is claimed (so a late resolver never spends on it), the
/// allowance is spent and the owner is asked. A landing still pending (a resolution running)
/// clears the stamp, so the grace starts again once it ends.
pub(super) async fn after_settle(
    pool: &SqlitePool,
    job: &mut TreeJob,
    settled: LandSettlement,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let waiting = job.manifest.extra.contains_key("land_id");
    match settled {
        LandSettlement::NotLanded { reason } if waiting => {
            let since = job
                .manifest
                .extra
                .get(LAND_ESCALATED_SINCE)
                .and_then(Value::as_str)
                .map(str::to_owned);
            match since {
                None => {
                    job.manifest
                        .extra
                        .insert(LAND_ESCALATED_SINCE.to_owned(), json!(now()));
                    persist(pool, job).await
                }
                Some(since) if overdue(&since, RESOLVER_GRACE) => {
                    match claim_land(pool, job).await.map_err(|e| e.to_string())? {
                        LandSettlement::NotLanded { .. } => {
                            job.manifest.extra.remove(LAND_ESCALATED_SINCE);
                            let reason = format!(
                                "the landing conflicts and no conflict resolver picked it up within {} minutes: {reason}",
                                RESOLVER_GRACE.as_secs() / 60
                            );
                            set_land_record(job, LAND_ATTEMPTS, &reason);
                            persist(pool, job).await?;
                            parks.push((
                                job.root_node_id.clone(),
                                land_question(LAND_ATTEMPTS, &reason),
                            ));
                            Ok(())
                        }
                        // The resolver claimed it first, or it landed meanwhile.
                        LandSettlement::Pending | LandSettlement::Finished { .. } => {
                            job.manifest.extra.remove(LAND_ESCALATED_SINCE);
                            persist(pool, job).await
                        }
                    }
                }
                Some(_) => Ok(()),
            }
        }
        LandSettlement::NotLanded { reason } => {
            job.manifest.extra.remove(LAND_ESCALATED_SINCE);
            count_land_attempt(pool, job, &reason, parks).await
        }
        LandSettlement::Pending => {
            if job.manifest.extra.remove(LAND_ESCALATED_SINCE).is_some() {
                persist(pool, job).await?;
            }
            Ok(())
        }
        LandSettlement::Finished { .. } => Ok(()),
    }
}

// ---- a green branch on a base that does not merge ----------------------------------------------

/// A green branch that may not merge because its base is not merged. A base parked on a decision
/// is announced once per decision (the dependent waits on the owner's answer to it); a dropped base
/// will never merge, so the dependent is parked on a question of its own. Any other base is still
/// on its way and says nothing.
pub(super) async fn note_blocked_base(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(());
    };
    let Some(base_name) = branch.base.clone() else {
        return Ok(());
    };
    let Some(base) = job.manifest.branches.get(&base_name) else {
        return Ok(());
    };
    match base.state.as_str() {
        "parked" => {
            let decision = base
                .park
                .as_ref()
                .and_then(|park| park.get("decision_id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            if branch.extra.get(BASE_WAIT_KEY).and_then(Value::as_str) == Some(decision.as_str()) {
                return Ok(());
            }
            let line = format!(
                "{}: branch {name} is green but waits on its base {base_name}, parked on decision {decision}",
                job.goal
            );
            crate::feed::append(
                pool,
                job.project_id.as_deref(),
                "work_tree_parked",
                &line,
                None,
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
            if let Some(held) = job.manifest.branches.get_mut(name) {
                held.extra.insert(BASE_WAIT_KEY.to_owned(), json!(decision));
            }
            persist(pool, job).await
        }
        "dropped" => {
            if let Some(node) = branch.node_id.clone() {
                parks.push((
                    node,
                    format!(
                        "Branch {name} is green but stands on {base_name}, which was dropped, so it can never merge; how should it go on?"
                    ),
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infrastructure_failures_are_load_reasons() {
        for reason in [
            "database is locked",
            "error returned from database: (code: 5) database is locked",
            "the verify queue exhausted: queue exhausted",
            "scope verification ended errored",
            "scope verification ended without a verdict",
        ] {
            assert!(is_load_reason(reason), "{reason}");
        }
        assert!(is_load_reason(&crate::verify::time_ceiling_reason(1800, 3)));
        assert!(!is_load_reason("the scope gate could not be submitted"));
        assert!(!is_load_reason(
            "scope verification ended errored (os error 206)"
        ));
    }

    #[test]
    fn a_gate_that_hit_the_time_ceiling_is_remeasured_with_a_bigger_one() {
        // Nothing to size before the first hit: the configured ceiling stands.
        assert_eq!(scoped_ceiling_secs(1800, 6, 0), 0);
        // One unit, one hit: twice the base. Many units start higher. Never past the maximum.
        assert_eq!(scoped_ceiling_secs(1800, 1, 1), 3600);
        assert_eq!(scoped_ceiling_secs(1800, 4, 1), 5400);
        assert_eq!(scoped_ceiling_secs(1800, 40, 1), 1800 * CEILING_MAX_FACTOR);
        // Each further hit asks for more, up to the maximum.
        assert!(scoped_ceiling_secs(1800, 1, 2) > scoped_ceiling_secs(1800, 1, 1));
        assert_eq!(scoped_ceiling_secs(1800, 1, 9), 1800 * CEILING_MAX_FACTOR);
    }

    #[test]
    fn the_ceiling_a_branch_asks_for_follows_the_hits_on_its_extra() {
        let mut extra = BTreeMap::new();
        assert_eq!(branch_ceiling_secs(&extra, 1800), 0);
        extra.insert(GATE_CEILING_HITS.to_owned(), json!(1));
        extra.insert(GATE_CEILING_UNITS.to_owned(), json!(2));
        assert_eq!(branch_ceiling_secs(&extra, 1800), 3600);
        clear_load_wait(&mut extra);
        assert_eq!(branch_ceiling_secs(&extra, 1800), 0);
    }

    #[test]
    fn the_wait_stops_at_six_hours_accumulated() {
        assert_eq!(load_step(0, 0), LoadStep::Wait(Duration::from_secs(30)));
        // Backoff is capped at 10 minutes, so ten-minute steps add up to the cap.
        let mut waited = 0;
        let mut waits = 0;
        while let LoadStep::Wait(delay) = load_step(waits, waited) {
            waited += i64::try_from(delay.as_secs()).unwrap();
            waits += 1;
            assert!(waits < 100, "the wait must end");
        }
        assert!(waited <= GATE_LOAD_CAP_SECS);
        assert!(waited > GATE_LOAD_CAP_SECS - 600, "{waited}");
        assert_eq!(load_step(waits, waited), LoadStep::CapReached);
    }
}
