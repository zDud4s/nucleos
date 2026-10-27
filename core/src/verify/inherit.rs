//! Telling an inherited red from the session's own (task-claims spec 2026-10-22, section 3 Verify).
//!
//! The pure half (`decide`) is below; the half that reads git and the claims store (`apply`,
//! `ensure_fresh`, `refresh_master_red`) follows it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::task_claims::{ProbeStart, Status, TaskKey, TaskRow, store};
use crate::test_select;
use crate::tests_map::{self, MapState};
use crate::verify::exec::Executor;
use crate::verify::failures::Failures;
use crate::verify::plan::{Ticket, UnitReport};
use crate::verify::postgate_worker::GATE_UNIT;
use crate::verify::runs as verify_runs;
use crate::verify::store as verify_store;
use crate::verify::store::RequestRow;
use crate::verify::{probe, resolve};

/// The live `master-red` row, as the decision needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Incident {
    pub task_id: i64,
    pub status: Status,
    /// "claimed by session abc" / "open, nobody has claimed it".
    pub holder: String,
    pub red_tests: BTreeSet<String>,
    pub red_groups: Vec<String>,
    pub red_since_sha: Option<String>,
    /// `checked_sha` is today's master head (or the incident was just proved at it).
    pub fresh: bool,
}

impl Incident {
    pub fn from_row(row: &TaskRow, fresh: bool) -> Incident {
        Incident {
            task_id: row.id,
            status: row.status,
            holder: describe_holder(row),
            red_tests: detail_strings(&row.detail, "red_tests").into_iter().collect(),
            red_groups: detail_strings(&row.detail, "red_groups"),
            red_since_sha: row.detail.get("red_since_sha").and_then(Value::as_str).map(str::to_owned),
            fresh,
        }
    }
}

/// The strings of `detail[key]` (an array of strings), empty when absent or of another shape.
pub(super) fn detail_strings(detail: &Value, key: &str) -> Vec<String> {
    detail
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn describe_holder(row: &TaskRow) -> String {
    match row.holder() {
        Some(holder) => format!("claimed by {} {}", holder.kind.as_str(), holder.id),
        None => "open, nobody has claimed it".to_owned(),
    }
}

/// What is known about one failed unit. `group` is the gate name (`gate_command`) for a unit with none.
pub struct Facts<'a> {
    pub group: &'a str,
    /// `None`: the runner gave no ids (`failures::extract`). An empty set counts as none.
    pub failed: Option<&'a Failures>,
    /// The group's mapped paths do not intersect the session's changes (`select` did not pick it).
    pub suspect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Class {
    /// The session's own failure; `note` says why when that is not obvious.
    Task { note: Option<String> },
    Inherited { task_id: i64, note: String },
    /// Waiting for the (re-)proof of task `task_id` to settle.
    Pending { task_id: i64, reprobe: bool },
    /// No incident and the group is suspect: someone has to prove it on master.
    NeedsProbe,
}

fn short(sha: &str) -> String {
    sha.chars().take(10).collect()
}

/// `branch_has_red_since`: `Some(bool)` is the ancestry test ("the verified HEAD contains the
/// incident's `red_since_sha`"); `None` when it was not made (the incident has no such key) and
/// counts as satisfied.
pub fn decide(facts: &Facts<'_>, incident: Option<&Incident>, branch_has_red_since: Option<bool>) -> Class {
    let own = |note: Option<String>| Class::Task { note };
    let Some(incident) = incident else {
        return if facts.suspect { Class::NeedsProbe } else { own(None) };
    };
    if incident.status == Status::Probing {
        return if facts.suspect {
            Class::Pending { task_id: incident.task_id, reprobe: false }
        } else {
            own(None)
        };
    }
    // `open` or `claimed` from here.
    let ids = facts.failed.map(|failed| &failed.tests).filter(|tests| !tests.is_empty());
    let by_group = ids.is_none();
    let matches = match ids {
        Some(ids) => !incident.red_tests.is_empty() && ids.is_subset(&incident.red_tests),
        None => facts.suspect && incident.red_groups.iter().any(|group| group == facts.group),
    };
    if !matches {
        // The same group failing other tests is the session's own break and must not be masked.
        return own(None);
    }
    if by_group && !incident.fresh {
        return own(Some(format!(
            "incident #{} was not re-checked at the current master head, so it cannot vouch for a group \
             that reported no test ids",
            incident.task_id
        )));
    }
    if branch_has_red_since == Some(false) {
        let since = incident.red_since_sha.as_deref().map(short).unwrap_or_default();
        return own(Some(format!(
            "your branch predates the break (it does not contain {since}); rebase to check whether this \
             red is yours"
        )));
    }
    let id = incident.task_id;
    let mut note = format!(
        "incident #{id} ({}): do not fix it, keep working on your task, and re-verify when #{id} closes",
        incident.holder
    );
    if by_group {
        note.push_str(" [matched by group and untouched paths: the runner gave no test ids]");
    }
    Class::Inherited { task_id: id, note }
}

/// A done ticket whose failed units are all inherited has the verdict `inherited`.
pub fn only_inherited(classes: &[Class]) -> bool {
    !classes.is_empty() && classes.iter().all(|class| matches!(class, Class::Inherited { .. }))
}

// ---- the half that reads git and the claims store ------------------------------------------------

/// Every git read of one classification (master head, diff, ancestry) shares this budget.
const GIT_BUDGET: Duration = Duration::from_secs(20);

/// One failed unit and what it was decided to be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitClass {
    /// Index in `ticket.units`.
    pub index: usize,
    pub class: Class,
}

/// Stored in `verify_requests.classification`. A draft (`done == false`) caches what git said so a
/// poll never reruns it; a final one (`done == true`) is read back as stored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub done: bool,
    /// Failed unit index -> its group is suspect. `None` until the diff was read.
    pub suspects: Option<BTreeMap<usize, bool>>,
    /// `red_since_sha` -> the verified worktree's HEAD contains it.
    pub contains: BTreeMap<String, bool>,
    /// Re-proofs this ticket started. It starts at most one: after a lapsed flight it gives up and
    /// reads the incident as it stands (`Freshness::GaveUp`).
    pub reprobes: u8,
    /// Final per-unit classes (empty until `done`; the draft carries the pending ones too).
    pub classes: Vec<UnitClass>,
}

impl Classification {
    /// Writes what was decided onto the ticket the reader is about to return.
    fn apply_to(&self, ticket: &mut Ticket) {
        let mut inherited = BTreeSet::new();
        let mut waiting: Vec<String> = Vec::new();
        for entry in &self.classes {
            let Some(unit) = ticket.units.get_mut(entry.index) else { continue };
            match &entry.class {
                Class::Inherited { task_id, note } => {
                    unit.inherited = Some(note.clone());
                    inherited.insert(*task_id);
                }
                Class::Pending { task_id, reprobe } => {
                    let what = if *reprobe { "reprobe" } else { "probe" };
                    unit.pending = Some(format!("{what} #{task_id}"));
                    waiting.push(format!("{what} #{task_id}"));
                }
                Class::Task { note } => unit.not_inherited = note.clone(),
                Class::NeedsProbe => {}
            }
        }
        ticket.inherited_task_ids = inherited.iter().copied().collect();
        let note = |ticket: &mut Ticket, text: String| {
            ticket.note = Some(match ticket.note.take() {
                Some(old) if !old.is_empty() => format!("{old}; {text}"),
                _ => text,
            });
        };
        if !waiting.is_empty() {
            // Not finished: `wait_ticket` keeps waiting and the asker keeps polling.
            ticket.done = false;
            ticket.verdict = None;
            waiting.dedup();
            note(ticket, format!("waiting for {}", waiting.join(", ")));
            return;
        }
        let classes: Vec<Class> = self.classes.iter().map(|entry| entry.class.clone()).collect();
        let clean = ticket.units.iter().all(|unit| !matches!(unit.status.as_str(), "errored" | "unknown"));
        if clean && only_inherited(&classes) {
            ticket.verdict = Some("inherited".to_owned());
            let ids: Vec<String> = inherited.iter().map(|id| format!("#{id}")).collect();
            note(
                ticket,
                format!(
                    "every failed unit is inherited from the master incident {}: do not fix it, keep \
                     working on your task, and re-verify when it closes",
                    ids.join(", ")
                ),
            );
        }
    }
}

/// Only tickets a person or an autonomous run is waiting on are classified.
fn classified(caller: &str) -> bool {
    caller == "owner" || caller.starts_with("run:")
}

/// Case 1, and the entry to cases 2 and 3. **Fail-open in everything**: any error logs a warning and
/// leaves the ticket exactly as `assemble` made it.
pub async fn apply(
    pool: &SqlitePool,
    executor: Option<&Arc<Executor>>,
    request: &RequestRow,
    ticket: &mut Ticket,
    now: DateTime<Utc>,
) {
    if let Err(error) = try_apply(pool, executor, request, ticket, now).await {
        tracing::warn!(%error, ticket = ticket.ticket, "verify: cannot classify the red units; they stay failed");
    }
}

async fn try_apply(
    pool: &SqlitePool,
    executor: Option<&Arc<Executor>>,
    request: &RequestRow,
    ticket: &mut Ticket,
    now: DateTime<Utc>,
) -> Result<(), String> {
    if !ticket.done || ticket.verdict.as_deref() != Some("failed") || !classified(&request.caller) {
        return Ok(());
    }
    let mut stored: Classification = request
        .classification
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    if stored.done {
        stored.apply_to(ticket);
        return Ok(());
    }

    let failed: Vec<usize> = ticket
        .units
        .iter()
        .enumerate()
        .filter(|(_, unit)| unit.status == verify_runs::STATUS_FAILED)
        .map(|(index, _)| index)
        .collect();
    let project_id = request.project_id.as_str();
    let project_root = crate::inspect::project_root(pool, project_id)
        .await
        .map_err(|error| error.to_string())?
        .map(PathBuf::from)
        .ok_or_else(|| format!("project {project_id} has no root recorded"))?;
    let worktree = Path::new(&request.worktree);
    let deadline = Instant::now() + GIT_BUDGET;

    let head = probe::master_head(pool, project_id, &project_root, deadline).await.ok();
    let incident = store::live_by_key(pool, project_id, &TaskKey::master_red(), now)
        .await
        .map_err(|error| error.to_string())?;

    // Case 3 first: an incident not re-checked at today's master head is re-proved before anything
    // is decided against it, and the units wait while that runs.
    let mut freshness = Freshness::Fresh;
    if let Some(row) = incident.as_ref().filter(|row| matches!(row.status, Status::Open | Status::Claimed)) {
        freshness = match head.as_deref() {
            Some(head) => {
                ensure_fresh(pool, executor, project_id, &project_root, row, head, stored.reprobes > 0, now).await?
            }
            None => Freshness::GaveUp,
        };
    }
    if let (Some(row), Freshness::InFlight { started }) = (incident.as_ref(), freshness) {
        if started {
            stored.reprobes += 1;
        }
        let class = Class::Pending { task_id: row.id, reprobe: true };
        stored.classes = failed.iter().map(|&index| UnitClass { index, class: class.clone() }).collect();
        persist(pool, request.id, &stored).await;
        stored.apply_to(ticket);
        return Ok(());
    }
    let fresh = freshness == Freshness::Fresh;

    let suspects = match stored.suspects.clone() {
        Some(known) => known,
        None => {
            let found = suspects_of(pool, request, &project_root, ticket, &failed, deadline).await;
            stored.suspects = Some(found.clone());
            found
        }
    };

    let mut classes =
        decide_units(&mut stored, ticket, &failed, &suspects, incident.as_ref(), fresh, worktree, deadline).await;

    // Units that need a master proof share ONE proof of all their groups.
    let needing: Vec<usize> = (0..classes.len()).filter(|&i| classes[i] == Class::NeedsProbe).collect();
    if !needing.is_empty() {
        let mut groups: Vec<String> = Vec::new();
        for &i in &needing {
            if let Some(group) = ticket.units[failed[i]].group.clone()
                && !groups.contains(&group)
            {
                groups.push(group);
            }
        }
        match probe_step(pool, executor, request, &project_root, &groups, head.as_deref(), now).await? {
            ProbeStep::Task(note) => {
                for &i in &needing {
                    classes[i] = Class::Task { note: Some(note.clone()) };
                }
            }
            ProbeStep::Pending(task_id) => {
                for &i in &needing {
                    classes[i] = Class::Pending { task_id, reprobe: false };
                }
            }
            ProbeStep::Incident(row) => {
                // A proof (ours or someone's) opened the incident meanwhile: decide against it. It was
                // proved at the master head by definition, so it is fresh when `checked_sha` says so.
                let proved_fresh = head.as_deref().is_some_and(|h| row.checked_sha.as_deref() == Some(h));
                let subset: Vec<usize> = needing.iter().map(|&i| failed[i]).collect();
                let redone =
                    decide_units(&mut stored, ticket, &subset, &suspects, Some(&row), proved_fresh, worktree, deadline)
                        .await;
                for (&i, class) in needing.iter().zip(redone) {
                    classes[i] = class;
                }
            }
        }
    }

    stored.done = !classes.iter().any(|class| matches!(class, Class::Pending { .. }));
    stored.classes = failed.iter().copied().zip(classes).map(|(index, class)| UnitClass { index, class }).collect();
    persist(pool, request.id, &stored).await;
    stored.apply_to(ticket);
    Ok(())
}

/// Final classifications also store the incident ids the close notice joins on.
async fn persist(pool: &SqlitePool, id: i64, stored: &Classification) {
    let Ok(json) = serde_json::to_string(stored) else { return };
    let written = if stored.done {
        let ids: BTreeSet<i64> = stored
            .classes
            .iter()
            .filter_map(|entry| match entry.class {
                Class::Inherited { task_id, .. } => Some(task_id),
                _ => None,
            })
            .collect();
        let ids: Vec<i64> = ids.into_iter().collect();
        verify_store::set_classification(pool, id, &json, &ids).await
    } else {
        verify_store::set_classification_draft(pool, id, &json).await
    };
    if let Err(error) = written {
        tracing::warn!(%error, ticket = id, "verify: cannot store the classification");
    }
}

/// `decide` for each unit in `indexes` (indexes into `ticket.units`) against one incident. The
/// ancestry test is made once per `red_since_sha` and remembered in `stored.contains`.
#[allow(clippy::too_many_arguments)]
async fn decide_units(
    stored: &mut Classification,
    ticket: &Ticket,
    indexes: &[usize],
    suspects: &BTreeMap<usize, bool>,
    row: Option<&TaskRow>,
    fresh: bool,
    worktree: &Path,
    deadline: Instant,
) -> Vec<Class> {
    let incident = row.map(|row| Incident::from_row(row, fresh));
    let branch_has = match incident.as_ref().and_then(|inc| inc.red_since_sha.as_deref()) {
        Some(since) if incident.as_ref().is_some_and(|inc| inc.status != Status::Probing) => {
            Some(contains(stored, worktree, since, deadline).await)
        }
        _ => None,
    };
    indexes
        .iter()
        .map(|&index| {
            let unit: &UnitReport = &ticket.units[index];
            // The runner's ids live on the unit, not in `Failures`: rebuild them for `decide`.
            let failures = unit
                .failed_tests
                .as_ref()
                .map(|ids| Failures { tests: ids.iter().cloned().collect(), files: BTreeSet::new() });
            let facts = Facts {
                group: unit.group.as_deref().unwrap_or(GATE_UNIT),
                failed: failures.as_ref(),
                suspect: suspects.get(&index).copied().unwrap_or(false),
            };
            decide(&facts, incident.as_ref(), branch_has)
        })
        .collect()
}

/// Does the verified worktree's HEAD contain `sha`? Memoised; any doubt (a sha that is not hex, a
/// git error) is "no", which is the side that never masks a failure.
async fn contains(stored: &mut Classification, worktree: &Path, sha: &str, deadline: Instant) -> bool {
    if let Some(known) = stored.contains.get(sha) {
        return *known;
    }
    let hex = (7..=40).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit());
    let found = hex && resolve::is_ancestor(worktree, sha, "HEAD", deadline).await.unwrap_or(false);
    stored.contains.insert(sha.to_owned(), found);
    found
}

/// Which failed units are in a group the session's diff did not select (case 2's "suspect").
/// Anything that cannot be read -- no map, no base, a git error -- makes nobody suspect.
async fn suspects_of(
    pool: &SqlitePool,
    request: &RequestRow,
    project_root: &Path,
    ticket: &Ticket,
    failed: &[usize],
    deadline: Instant,
) -> BTreeMap<usize, bool> {
    let none = || failed.iter().map(|&index| (index, false)).collect::<BTreeMap<_, _>>();
    let worktree = PathBuf::from(&request.worktree);
    let map = {
        let worktree = worktree.clone();
        match tokio::task::spawn_blocking(move || tests_map::load(&worktree)).await {
            Ok(MapState::Valid(map)) => map,
            _ => return none(),
        }
    };
    let base = match ticket.base.clone() {
        Some(base) => base,
        None => match resolve::resolve_base(pool, &request.project_id, project_root, &worktree, None, deadline).await {
            Ok(base) => base,
            Err(_) => return none(),
        },
    };
    let Ok(changed) = resolve::changed_paths(&worktree, &base, deadline).await else { return none() };
    let fillable = {
        let (worktree, changed) = (worktree.clone(), changed.clone());
        tokio::task::spawn_blocking(move || test_select::vet(&worktree, &changed)).await.unwrap_or_default()
    };
    // An empty diff selects nothing, so every group is "untouched by the session": suspect.
    let picked: BTreeSet<String> =
        test_select::select(&map, &changed, &fillable, false).units.into_iter().map(|unit| unit.group).collect();
    failed
        .iter()
        .map(|&index| {
            let suspect = ticket.units[index].group.as_deref().is_some_and(|group| !picked.contains(group));
            (index, suspect)
        })
        .collect()
}

/// Whether an open/claimed incident was checked against today's master head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Freshness {
    Fresh,
    /// A re-proof is running (`started`: this call launched it).
    InFlight { started: bool },
    /// It is stale, no flight is running, and this ticket already tried once (or nobody can run
    /// one): read the incident as it stands.
    GaveUp,
}

enum ProbeStep {
    Task(String),
    Pending(i64),
    Incident(TaskRow),
}

/// Task 4 body; Task 6 replaces it with the real re-proof.
#[allow(clippy::too_many_arguments)]
pub(super) async fn ensure_fresh(
    _pool: &SqlitePool,
    _executor: Option<&Arc<Executor>>,
    _project_id: &str,
    _project_root: &Path,
    _incident: &TaskRow,
    _head: &str,
    _already_tried: bool,
    _now: DateTime<Utc>,
) -> Result<Freshness, String> {
    Ok(Freshness::Fresh)
}

/// Case 2: the suspect groups need a proof on the master head. One proof per project serves every
/// ticket waiting on it.
async fn probe_step(
    pool: &SqlitePool,
    executor: Option<&Arc<Executor>>,
    request: &RequestRow,
    project_root: &Path,
    groups: &[String],
    head: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ProbeStep, String> {
    // This ticket already has its answer: never repeat a proof for it.
    if let Some(outcome) = probe::ProbeOutcome::of(request) {
        return Ok(ProbeStep::Task(outcome.note(head)));
    }
    // Somebody proved these groups green on this very head a moment ago.
    if let Some(head) = head
        && probe::passed_recently(&request.project_id, head, groups, Instant::now())
    {
        return Ok(ProbeStep::Task(format!("passes on master at {}: the failure is yours", short(head))));
    }
    let Some(executor) = executor else {
        return Ok(ProbeStep::Task("no verify executor to prove it on master; treated as your failure".to_owned()));
    };
    let started = store::begin_probe(pool, &request.project_id, &request.id.to_string(), &request.worktree, now)
        .await
        .map_err(|error| error.to_string())?;
    match started {
        ProbeStart::Started(row) => {
            probe::spawn_proof(
                executor.clone(),
                probe::ProofJob::Probe {
                    project_id: request.project_id.clone(),
                    project_root: project_root.to_path_buf(),
                    task_id: row.id,
                    ticket_id: request.id,
                    groups: groups.to_vec(),
                },
            );
            Ok(ProbeStep::Pending(row.id))
        }
        ProbeStart::InFlight(row) => Ok(ProbeStep::Pending(row.id)),
        ProbeStart::Exists(row) => Ok(ProbeStep::Incident(row)),
    }
}

#[cfg(test)]
mod pure_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn failed(ids: &[&str]) -> Option<Failures> {
        Some(Failures { tests: ids.iter().map(|s| (*s).to_owned()).collect(), files: BTreeSet::new() })
    }

    fn incident(status: Status, red: &[&str], groups: &[&str], since: Option<&str>) -> Incident {
        Incident {
            task_id: 7,
            status,
            holder: "claimed by session abc".to_owned(),
            red_tests: red.iter().map(|s| (*s).to_owned()).collect(),
            red_groups: groups.iter().map(|s| (*s).to_owned()).collect(),
            red_since_sha: since.map(str::to_owned),
            fresh: true,
        }
    }

    fn facts<'a>(group: &'a str, ids: &'a Option<Failures>, suspect: bool) -> Facts<'a> {
        Facts { group, failed: ids.as_ref(), suspect }
    }

    fn task_note(class: Class) -> String {
        match class {
            Class::Task { note: Some(note) } => note,
            other => panic!("expected a Task with a note, got {other:?}"),
        }
    }

    #[test]
    fn ids_contained_in_red_tests_on_a_branch_with_the_break_are_inherited() {
        let ids = failed(&["a::boom"]);
        let inc = incident(Status::Claimed, &["a::boom", "a::bang"], &["gate_command"], Some("s0"));
        match decide(&facts("core", &ids, false), Some(&inc), Some(true)) {
            Class::Inherited { task_id, note } => {
                assert_eq!(task_id, 7);
                assert!(note.contains("#7") && note.contains("session abc"), "{note}");
                assert!(note.contains("do not fix"), "{note}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_same_group_failing_other_tests_is_the_tasks_failure() {
        let ids = failed(&["a::boom", "a::mine"]);
        let inc = incident(Status::Claimed, &["a::boom"], &["core"], Some("s0"));
        assert_eq!(decide(&facts("core", &ids, true), Some(&inc), Some(true)), Class::Task { note: None });
    }

    #[test]
    fn known_ids_against_an_incident_with_no_red_tests_are_the_tasks_failure() {
        let ids = failed(&["a::boom"]);
        let inc = incident(Status::Open, &[], &["core"], Some("s0"));
        assert_eq!(decide(&facts("core", &ids, true), Some(&inc), Some(true)), Class::Task { note: None });
    }

    #[test]
    fn a_branch_that_predates_the_break_is_not_inherited_even_with_the_same_ids() {
        let ids = failed(&["a::boom"]);
        let inc = incident(Status::Open, &["a::boom"], &["core"], Some("s0"));
        let note = task_note(decide(&facts("core", &ids, true), Some(&inc), Some(false)));
        assert!(note.contains("rebase") && note.contains("s0"), "{note}");
    }

    #[test]
    fn an_incident_with_no_red_since_sha_key_skips_the_ancestry_test() {
        let ids = failed(&["a::boom"]);
        let inc = incident(Status::Open, &["a::boom"], &["core"], None);
        assert!(matches!(decide(&facts("core", &ids, true), Some(&inc), None), Class::Inherited { .. }));
    }

    #[test]
    fn a_group_without_ids_needs_suspect_listed_fresh_and_a_branch_with_the_break() {
        let none = None;
        let inc = incident(Status::Open, &[], &["core"], Some("s0"));
        match decide(&facts("core", &none, true), Some(&inc), Some(true)) {
            Class::Inherited { note, .. } => assert!(note.contains("no test ids"), "{note}"),
            other => panic!("{other:?}"),
        }
        // Not suspect: the session touched the group.
        assert_eq!(decide(&facts("core", &none, false), Some(&inc), Some(true)), Class::Task { note: None });
        // Not one of the incident's red groups.
        assert_eq!(decide(&facts("py", &none, true), Some(&inc), Some(true)), Class::Task { note: None });
        // The incident was last checked at an older master head.
        let mut old = inc.clone();
        old.fresh = false;
        let note = task_note(decide(&facts("core", &none, true), Some(&old), Some(true)));
        assert!(note.contains("re-checked"), "{note}");
        // The branch predates the break.
        let note = task_note(decide(&facts("core", &none, true), Some(&inc), Some(false)));
        assert!(note.contains("rebase"), "{note}");
    }

    #[test]
    fn an_empty_id_set_counts_as_no_ids() {
        let empty = failed(&[]);
        let inc = incident(Status::Open, &["a::boom"], &["core"], Some("s0"));
        assert!(matches!(decide(&facts("core", &empty, true), Some(&inc), Some(true)), Class::Inherited { .. }));
    }

    #[test]
    fn without_an_incident_only_a_suspect_group_is_probed() {
        let ids = failed(&["a::boom"]);
        assert_eq!(decide(&facts("core", &ids, true), None, None), Class::NeedsProbe);
        assert_eq!(decide(&facts("core", &ids, false), None, None), Class::Task { note: None });
    }

    #[test]
    fn a_probe_in_flight_makes_a_suspect_group_pending_and_leaves_the_rest() {
        let ids = failed(&["a::boom"]);
        let probing = incident(Status::Probing, &[], &[], None);
        assert_eq!(
            decide(&facts("core", &ids, true), Some(&probing), None),
            Class::Pending { task_id: 7, reprobe: false }
        );
        assert_eq!(decide(&facts("core", &ids, false), Some(&probing), None), Class::Task { note: None });
    }

    #[test]
    fn a_ticket_is_inherited_only_when_every_failed_unit_is() {
        let inh = Class::Inherited { task_id: 7, note: String::new() };
        let task = Class::Task { note: None };
        assert!(only_inherited(&[inh.clone(), inh.clone()]));
        assert!(!only_inherited(&[inh, task]));
        assert!(!only_inherited(&[]));
    }
}
