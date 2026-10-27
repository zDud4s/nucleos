//! The `master-red` incident of the post-merge gate (spec 2026-10-22 task-claims §3 Postgate).
//!
//! The gate owns the red; this module only mirrors it in `task_claims` so that sessions, runs and
//! the land gate can see it. Every write here is fail-open: a claims error is logged and the gate
//! carries on exactly as it did before claims existed.

use chrono::Utc;
use serde_json::{Value, json};

use super::*;
use crate::task_claims::{
    self, ClaimOutcome, Closer, Holder, HolderKind, MASTER_RED_KEY, NewTask, Status, TaskKind, TaskRow,
    keys,
};

/// The postgate as a claimant: its id is the project, its worktree the one the gate checks out.
pub(super) fn gate_holder(project_id: &str, project_root: &Path) -> Holder {
    Holder {
        kind: HolderKind::Postgate,
        id: project_id.to_owned(),
        worktree: verify_postgate::postgate_worktree(project_root)
            .to_string_lossy()
            .into_owned(),
    }
}

/// `"session abc"`: what the feed and the refusal messages call a holder.
pub(super) fn describe(holder: &Holder) -> String {
    format!("{} {}", format!("{:?}", holder.kind).to_lowercase(), holder.id)
}

/// The holder a row records, if it records a whole one.
pub(super) fn holder_of(row: &TaskRow) -> Option<Holder> {
    Some(Holder {
        kind: row.holder_kind?,
        id: row.holder_id.clone()?,
        worktree: row.holder_worktree.clone()?,
    })
}

/// The failing tests and files the gate's ticket named (chunk 4, Tasks 1-2: `UnitReport.failed_tests`
/// and `failed_files`). Empty when the runner printed no ids, which is a real and common case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RedFailures {
    pub tests: Vec<String>,
    pub files: Vec<String>,
}

/// Folds `(failed_tests, failed_files)` of each failed unit into sorted, deduplicated lists. A unit
/// whose runner gave no ids (`None`) adds no test; the units that did give ids still count.
pub(super) fn fold_failures<'a>(
    units: impl IntoIterator<Item = (Option<&'a [String]>, &'a [String])>,
) -> RedFailures {
    let mut tests = std::collections::BTreeSet::new();
    let mut files = std::collections::BTreeSet::new();
    for (ids, paths) in units {
        if let Some(ids) = ids {
            tests.extend(ids.iter().cloned());
        }
        files.extend(paths.iter().cloned());
    }
    RedFailures {
        tests: tests.into_iter().collect(),
        files: files.into_iter().collect(),
    }
}

/// What the gate knows about the red. `None` values are left out, so merging never writes a null
/// over something an earlier writer (a verify probe) knew; `red_tests` is left out when the runner
/// named none, for the same reason.
pub(super) fn red_patch(
    target: &str,
    red_since_sha: Option<&str>,
    red_sha: Option<&str>,
    red_base_sha: Option<&str>,
    red_groups: &[String],
    failures: &RedFailures,
) -> Value {
    let mut patch = serde_json::Map::new();
    patch.insert("source".to_owned(), json!("postgate"));
    patch.insert("target".to_owned(), json!(target));
    patch.insert("red_groups".to_owned(), json!(red_groups));
    if !failures.tests.is_empty() {
        patch.insert("red_tests".to_owned(), json!(failures.tests));
    }
    for (name, value) in [
        ("red_since_sha", red_since_sha),
        ("red_sha", red_sha),
        ("red_base_sha", red_base_sha),
    ] {
        if let Some(value) = value {
            patch.insert(name.to_owned(), json!(value));
        }
    }
    Value::Object(patch)
}

/// The `detail` of an incident the gate creates: the patch, with `red_tests` an empty list when the
/// runner named none (chunk 4 reads an empty `red_tests` as "re-prove by group").
pub(super) fn new_incident_detail(
    target: &str,
    red_since_sha: Option<&str>,
    red_sha: Option<&str>,
    red_base_sha: Option<&str>,
    red_groups: &[String],
    failures: &RedFailures,
) -> Value {
    let mut detail = red_patch(target, red_since_sha, red_sha, red_base_sha, red_groups, failures);
    if detail.get("red_tests").is_none() {
        detail["red_tests"] = json!([]);
    }
    detail
}

/// `base` with every non-null key of `patch` written over it. Done here, by read-modify-write, so
/// the result does not depend on whether `update_detail` replaces or merges.
pub(super) fn merge_detail(base: &Value, patch: &Value) -> Value {
    let mut merged = base.as_object().cloned().unwrap_or_default();
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            if !value.is_null() {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(merged)
}

/// The live `master-red` row of `project_id`, if any. Found by listing rather than by key, so the
/// callers that have no `project_root` at hand (a failed revert) need none.
pub(super) async fn live_master_red(pool: &sqlx::SqlitePool, project_id: &str) -> Option<TaskRow> {
    match task_claims::store::list_live(pool, project_id, Utc::now()).await {
        Ok(rows) => rows.into_iter().find(|row| row.key.as_str() == MASTER_RED_KEY),
        Err(error) => {
            tracing::warn!(project = project_id, %error, "postgate: cannot list the live incidents");
            None
        }
    }
}

/// Writes `patch` into the row's `detail` (and the paths, which `update_detail` REPLACES when `Some`:
/// pass the full list). The row is updated in place only when the write succeeded.
async fn store_detail(
    pool: &sqlx::SqlitePool,
    row: &mut TaskRow,
    patch: &Value,
    guards: Option<Vec<String>>,
    hints: Option<Vec<String>>,
) {
    let merged = merge_detail(&row.detail, patch);
    if merged == row.detail && guards.is_none() && hints.is_none() {
        return;
    }
    match task_claims::store::update_detail(
        pool,
        row.id,
        merged.clone(),
        guards.clone(),
        hints.clone(),
        Utc::now(),
    )
    .await
    {
        Ok(()) => {
            row.detail = merged;
            if let Some(guards) = guards {
                row.guard_paths = guards;
            }
            if let Some(hints) = hints {
                row.hint_paths = hints;
            }
        }
        Err(error) => tracing::warn!(incident = row.id, %error, "postgate: cannot update the incident"),
    }
}

/// What the failed units of the gate's ticket name. Read from the ticket's `failed_tests` and
/// `failed_files` (chunk 4, Tasks 1-2), never from `output_tail`.
pub(super) fn failures_of_ticket(ticket: &crate::verify::plan::Ticket) -> RedFailures {
    fold_failures(
        ticket
            .units
            .iter()
            .filter(|unit| unit.status == "failed")
            .map(|unit| (unit.failed_tests.as_deref(), unit.failed_files.as_slice())),
    )
}

/// Creates the `master-red` incident for the confirmed red in `state`, or adopts the live one (a
/// verify probe may have opened it first) and brings its `detail` up to date. Idempotent: it runs
/// on the flake-check ticket that confirms the red (with `failures`) and again on every bisection
/// tick that has no probe in flight (with `None`), so a crash between the confirmation and the first
/// write still ends with the incident before any probe is submitted. `failures` are the failing
/// tests and files of the gate's ticket; `None` or empty (the runner printed no ids) leaves whatever
/// the row already holds.
pub(super) async fn ensure_incident(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    state: &State,
    failures: Option<&RedFailures>,
) -> Result<TaskRow, String> {
    let key = keys::normalize(MASTER_RED_KEY, project_root).map_err(|error| format!("{error:?}"))?;
    let none = RedFailures::default();
    let failures = failures.unwrap_or(&none);
    // The patch, not `new_incident_detail`: `create_or_adopt_open` merges `detail` over an adopted
    // row with the new keys winning, so an empty `red_tests` here would wipe the tests an earlier
    // pass (or a verify probe) named. The empty list a fresh row needs is added below instead.
    let patch = red_patch(
        &state.target,
        state.red_since_sha.as_deref(),
        state.red_sha.as_deref(),
        state.red_base_sha.as_deref(),
        &state.red_groups,
        failures,
    );
    let new = NewTask {
        key,
        kind: TaskKind::Incident,
        title: format!("{} is red: {}", state.target, state.red_groups.join(", ")),
        detail: patch.clone(),
        guard_paths: failures.files.clone(),
        hint_paths: Vec::new(),
    };
    let mut row = task_claims::store::create_or_adopt_open(pool, project_id, new, Utc::now())
        .await
        .map_err(|error| error.to_string())?;
    // Adoption keeps what the row already knew; the postgate's word wins on the shas, groups and
    // (when the runner named any) the tests. A row that has no `red_tests` at all (just created, or
    // adopted from a writer that set none) gets the empty list `new_incident_detail` gives.
    let patch = if row.detail.get("red_tests").is_none() {
        new_incident_detail(
            &state.target,
            state.red_since_sha.as_deref(),
            state.red_sha.as_deref(),
            state.red_base_sha.as_deref(),
            &state.red_groups,
            failures,
        )
    } else {
        patch
    };
    // `create_or_adopt_open` unions the paths of an open row; a claimed one may not, so the union is
    // made here and written when it adds anything (`update_detail` replaces, so the whole list).
    let mut guards = row.guard_paths.clone();
    for file in &failures.files {
        if !guards.contains(file) {
            guards.push(file.clone());
        }
    }
    let widened = guards.len() != row.guard_paths.len();
    store_detail(pool, &mut row, &patch, widened.then_some(guards), None).await;
    Ok(row)
}

/// Writes the bisection's verdict into the incident: the culprit (and so `hint_paths`, the files
/// of the culprit commit) or the candidates. `hint_paths` never block anything (spec §3 Land).
pub(super) async fn record_verdict(
    pool: &sqlx::SqlitePool,
    mut row: TaskRow,
    project_root: &Path,
    verdict: &Verdict,
    deadline: Instant,
) -> TaskRow {
    let (patch, culprit) = match verdict {
        Verdict::Culprit { sha, also_suspect } => (
            json!({"culprit_sha": sha, "also_suspect": also_suspect}),
            Some(sha.as_str()),
        ),
        Verdict::Inconclusive { candidates, also_suspect } => (
            json!({"candidates": candidates, "also_suspect": also_suspect}),
            None,
        ),
        Verdict::NoCandidates => (json!({}), None),
    };
    let hints = match culprit {
        Some(sha) => git_exec::changed_by_commit(project_root, sha, deadline).await.ok(),
        None => None,
    };
    store_detail(pool, &mut row, &patch, None, hints).await;
    row
}

/// The answer to "may the daemon revert now?".
pub(super) enum RevertRight {
    /// The incident is the gate's (just claimed, or already its own).
    Mine,
    /// Another live holder has it; nothing is reverted while that holds.
    HeldBy { incident: i64, who: String },
    /// The claims store failed (or the row could not be had even after one retry). **The only path
    /// on which the daemon reverts without holding the incident**, on purpose: claims are fail-open
    /// (spec §3), a database glitch must not stop the revert the owner switched on. The land gate
    /// then sees an unclaimed incident, so the gate's own revert still passes (it is an `Op::Revert`,
    /// never gated), and a later pass reclaims.
    Unknown,
}

/// One claim attempt, with the store's error kept so the caller can tell a vanished row apart.
async fn try_claim(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    row: &TaskRow,
) -> sqlx::Result<RevertRight> {
    let me = gate_holder(project_id, project_root);
    Ok(match task_claims::store::claim_by_id(pool, row.id, &me, Utc::now()).await? {
        ClaimOutcome::Won { .. } => RevertRight::Mine,
        ClaimOutcome::Lost { holder, .. } => RevertRight::HeldBy {
            incident: row.id,
            who: describe(&holder),
        },
    })
}

/// Claims `row` for the gate. `claim_by_id` is idempotent for the same holder, so repeating this
/// (a crash after the claim, a second pass) renews the claim and never loses to itself. If the row
/// vanished between `ensure_incident` and here (`RowNotFound`), the incident is opened again ONCE
/// and that fresh row claimed; any other failure, or a second one, is `Unknown`.
pub(super) async fn claim_for_revert(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    state: &State,
    row: &TaskRow,
) -> RevertRight {
    match try_claim(pool, project_id, project_root, row).await {
        Ok(right) => right,
        Err(sqlx::Error::RowNotFound) => {
            let fresh = match ensure_incident(pool, project_id, project_root, state, None).await {
                Ok(fresh) => fresh,
                Err(reason) => {
                    tracing::warn!(project = project_id, %reason, "postgate: cannot reopen the incident");
                    return RevertRight::Unknown;
                }
            };
            match try_claim(pool, project_id, project_root, &fresh).await {
                Ok(right) => right,
                Err(error) => {
                    tracing::warn!(project = project_id, incident = fresh.id, %error, "postgate: cannot claim the reopened incident");
                    RevertRight::Unknown
                }
            }
        }
        Err(error) => {
            tracing::warn!(project = project_id, incident = row.id, %error, "postgate: cannot claim the incident");
            RevertRight::Unknown
        }
    }
}

/// One poll of "red, reported, unreverted, and the incident is free or already the gate's": claim
/// it and start the revert. `None` means nothing to do, and the tick carries on.
///
/// `tip` is the target's current tip. The step acts only when it is the sha the red was attempted
/// on (`state.last_attempted_sha`): a target that moved on since is judged again by the gate, and a
/// stale red must never start a revert. The caller (the `Decision::Idle` arm) already implies this;
/// the check keeps the function safe on its own.
pub(super) async fn takeover_step(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    state: &State,
    tip: &str,
    rules: &crate::config::AutopilotRules,
) -> Option<Step> {
    let culprit = state.culprit_sha.as_deref()?;
    if !rules.revert_on_red
        || state.red_groups.is_empty()
        || state.red_phase.is_some()
        || state.revert_merge_sha.is_some()
        || state.reported_sha.is_none()
        || state.last_attempted_sha.as_deref() != Some(tip)
    {
        return None;
    }
    let pool = &executor.pool;
    let row = live_master_red(pool, project_id).await?;
    // A revert of this culprit already failed once: the gate does not retry it for ever.
    if row.detail.get("revert_failed_for").and_then(Value::as_str) == Some(culprit) {
        return None;
    }
    let now = Utc::now();
    let mine = row.holder_kind == Some(HolderKind::Postgate) && row.holder_id.as_deref() == Some(project_id);
    if row.status == Status::Claimed
        && !mine
        && task_claims::store::is_live(pool, &row, now).await.unwrap_or(true)
    {
        return None;
    }
    // The gate's own claim without a revert is the crash gap between claim and report: go on.
    match claim_for_revert(pool, project_id, project_root, state, &row).await {
        RevertRight::Mine => {}
        RevertRight::HeldBy { .. } | RevertRight::Unknown => return None,
    }
    let summary = format!(
        "post-merge gate on {target}: incident #{id} has no live owner, so the daemon claimed it \
         and is reverting {culprit} on {target} through the queue, then a correction run follows \
         on a fix/ branch.",
        target = state.target,
        id = row.id,
        culprit = short(culprit),
    );
    match verify_postgate::restart_revert(pool, project_id, culprit, &summary).await {
        Ok(true) => Some(Step::IncidentTaken { incident: row.id, merge_sha: culprit.to_owned() }),
        Ok(false) => None,
        Err(error) => Some(Step::Failed(format!("cannot start the revert: {error}"))),
    }
}

/// Why the revert of a culprit ended without a fix on the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RevertEnd {
    /// The queue or the correction branch really failed (publication, conflict). Marked on the
    /// incident so the gate does not retry the same culprit for ever.
    Failed,
    /// `revert_on_red` was switched off before the queue received the revert: the owner changed their
    /// mind, nothing failed. Not marked, so switching it back on resumes the revert.
    Dropped,
}

/// Remembers on the incident that the revert of `merge_sha` really failed, so the gate does not
/// retry the same culprit for ever. It runs BEFORE `fail_revert` clears the state: a crash between
/// the two leaves a harmless marker (the revert is simply still recorded and settled on the next
/// tick), never a free state with no marker, which would let `takeover_step` start the revert again.
/// `Dropped` marks nothing. `false` means the marker could not be written; the caller must not clear
/// the state then, so the next tick tries again.
pub(super) async fn mark_revert_failed(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    merge_sha: &str,
    end: RevertEnd,
) -> bool {
    if end != RevertEnd::Failed {
        return true;
    }
    let Some(mut row) = live_master_red(pool, project_id).await else {
        return true;
    };
    store_detail(pool, &mut row, &json!({"revert_failed_for": merge_sha}), None, None).await;
    row.detail.get("revert_failed_for").and_then(Value::as_str) == Some(merge_sha)
}

/// A revert of `merge_sha` ended without a fix and `fail_revert` cleared it: let go of the incident
/// (the gate's own claim only), so a session or a person can take it. The incident stays `open` and
/// the feed line is `fail_revert`'s. The failure marker is `mark_revert_failed`'s, written earlier.
pub(super) async fn settle_after_revert(pool: &sqlx::SqlitePool, project_id: &str) {
    let Some(row) = live_master_red(pool, project_id).await else {
        return;
    };
    if let Some(holder) = holder_of(&row)
        && holder.kind == HolderKind::Postgate
        && let Err(error) =
            task_claims::store::release(pool, row.id, &holder, "the daemon's revert ended without a fix", Utc::now()).await
    {
        tracing::warn!(incident = row.id, %error, "postgate: cannot release the incident");
    }
}

/// Closes the live `master-red` incident when the gate is green on a descendant of the sha the red
/// began at (spec §3 Postgate, "Fecho"). It reads the gate's colour, not the event that turned it:
/// a gate pass, a covered tip and a flake clear all end here, and so does a restart in between.
/// `Closer::Green` records the previous holder, which is how a session owner is told (`Notice`).
pub(super) async fn close_if_green(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    state: &State,
) {
    if !state.red_groups.is_empty() {
        return;
    }
    let Some(green) = state.last_green_sha.as_deref() else {
        return;
    };
    let Some(row) = live_master_red(pool, project_id).await else {
        return;
    };
    let red_since = row
        .detail
        .get("red_since_sha")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| row.checked_sha.clone());
    if let Some(red_since) = red_since {
        let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;
        match git_exec::is_ancestor(project_root, &red_since, green, deadline).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(reason) => {
                tracing::warn!(project = project_id, %reason, "postgate: cannot tell if the green follows the red");
                return;
            }
        }
    }
    let note = format!("green at {}", short(green));
    match task_claims::store::done(pool, row.id, Closer::Green { sha: green.to_owned() }, &note, Utc::now()).await {
        Ok(_) => {}
        Err(error) => tracing::warn!(incident = row.id, %error, "postgate: cannot close the incident"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_patch_never_overwrites_what_the_row_already_knows_with_nothing() {
        // An incident adopted from a verify probe already has `red_tests`; the postgate's patch
        // carries none, and a null it does carry must not wipe a value.
        let probed = json!({"red_tests": ["core::a::t"], "red_since_sha": "old"});
        let none = RedFailures::default();
        let patch = red_patch("main", Some("s1"), Some("r1"), None, &["core".to_owned()], &none);
        let merged = merge_detail(&probed, &patch);
        assert_eq!(merged["red_tests"], json!(["core::a::t"]), "a runner with no ids wipes nothing");
        assert_eq!(merged["red_since_sha"], json!("s1"), "a known sha is the postgate's word");
        assert_eq!(merged["red_groups"], json!(["core"]));
        assert!(merged.get("red_base_sha").is_none(), "None is left out, not written as null");

        // The ids the gate's own ticket gave do win over the probe's.
        let named = RedFailures { tests: vec!["core::a::boom".to_owned()], files: vec![] };
        let patch = red_patch("main", Some("s1"), None, None, &["core".to_owned()], &named);
        assert_eq!(merge_detail(&probed, &patch)["red_tests"], json!(["core::a::boom"]));
    }

    #[test]
    fn a_new_incident_starts_with_the_ids_the_gate_named_and_none_when_it_named_none() {
        let groups = ["core".to_owned()];
        let bare = new_incident_detail("main", Some("s1"), Some("r1"), Some("b0"), &groups, &RedFailures::default());
        assert_eq!(bare["red_tests"], json!([]), "a runner that prints no ids leaves the list empty");
        assert_eq!(bare["source"], json!("postgate"));
        assert_eq!(bare["red_since_sha"], json!("s1"));
        let named = RedFailures { tests: vec!["core::a::boom".to_owned()], files: vec!["core/src/a.rs".to_owned()] };
        let detail = new_incident_detail("main", Some("s1"), None, None, &groups, &named);
        assert_eq!(detail["red_tests"], json!(["core::a::boom"]));
    }

    #[test]
    fn failing_units_fold_into_sorted_deduplicated_ids_and_a_unit_without_ids_adds_none() {
        let first_tests = vec!["b::t2".to_owned(), "a::t1".to_owned()];
        let second_tests = vec!["a::t1".to_owned()];
        let first_files = vec!["core/src/a.rs".to_owned()];
        let second_files = vec!["core/src/a.rs".to_owned(), "core/src/b.rs".to_owned()];
        let no_files: Vec<String> = Vec::new();
        let folded = fold_failures([
            (Some(first_tests.as_slice()), first_files.as_slice()),
            (Some(second_tests.as_slice()), second_files.as_slice()),
            (None, no_files.as_slice()),
        ]);
        assert_eq!(folded.tests, vec!["a::t1".to_owned(), "b::t2".to_owned()]);
        assert_eq!(folded.files, vec!["core/src/a.rs".to_owned(), "core/src/b.rs".to_owned()]);
        assert_eq!(
            fold_failures([(None, no_files.as_slice())]),
            RedFailures::default(),
            "no ids from the runner, nothing to guard"
        );
    }

    #[test]
    fn a_holder_is_described_by_kind_and_id() {
        let holder = Holder {
            kind: HolderKind::Session,
            id: "abc".to_owned(),
            worktree: "C:/wt".to_owned(),
        };
        assert_eq!(describe(&holder), "session abc");
    }
}
