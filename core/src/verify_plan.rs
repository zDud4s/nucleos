//! The pure half of `verify`: which units a request needs, and how the ticket reads back.
//!
//! Nothing here touches the disk, the database or a process. [`plan`] turns a map, the changed
//! paths and the caller's `kind`/`scope` into the list of units to run; [`assemble`] turns a
//! stored request plus the live state of its `verify_runs` rows into the ticket a caller sees.
//! Keeping both pure is what lets the whole decision table be tested without a daemon.

// Wired in by `verify.rs`; until every caller exists the build sees these as unused.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::test_select::{Reason, select};
use crate::tests_map::MapState;
use crate::verify_runs::{
    STATUS_ERRORED, STATUS_FAILED, STATUS_PASSED, STATUS_QUEUED, STATUS_RUNNING,
    STATUS_SKIPPED_CACHED, State,
};
use crate::verify_store::{PlannedUnit, RequestRow};

/// What a unit does: the group's cheap `check` command, or its `command` (the tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Check,
    Test,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Check => "check",
            Kind::Test => "test",
        }
    }
}

/// How wide the request reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeArg {
    /// What the caller changed, selected through the map.
    Own,
    /// What the whole branch changed since its base, selected through the map.
    Scope,
    /// The project's `gate_command`, whatever changed.
    Full,
}

impl ScopeArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ScopeArg::Own => "own",
            ScopeArg::Scope => "scope",
            ScopeArg::Full => "full",
        }
    }
}

pub(crate) const NO_MAP_OWN_NOTE: &str =
    "no selection: this project has no nucleos.tests.yaml; run your targeted tests";
pub(crate) const NOTHING_CHANGED_NOTE: &str =
    "nothing changed since the base; no group is selected";

/// Paths named in one `why`; a hundred-file change must not make a hundred-file sentence.
const WHY_PATHS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub units: Vec<PlannedUnit>,
    pub unclaimed: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PlanError {
    InvalidMap(Vec<String>),
    NoGateCommand,
    BadGateCommand(String),
    FullRefused(String),
}

/// The unit that runs the project's `gate_command`. It never goes to the cache (`cacheable`
/// false): the command is opaque, so nothing says which files it reads.
fn gate_unit(gate_command: Option<&str>) -> Result<PlannedUnit, PlanError> {
    let command = gate_command
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .ok_or(PlanError::NoGateCommand)?;
    let argv = crate::gate::split_command(command).map_err(PlanError::BadGateCommand)?;
    if argv.is_empty() {
        return Err(PlanError::BadGateCommand(
            "the gate_command is empty".into(),
        ));
    }
    Ok(PlannedUnit {
        group: None,
        argv,
        why: "gate_command".into(),
        fingerprint: None,
        cacheable: false,
        run_id: None,
        cached_from: None,
        skipped: None,
    })
}

fn why(reason: &Reason) -> String {
    match reason {
        Reason::FullSweep { path } => format!("full sweep: {path}"),
        Reason::Unclaimed => "unclaimed paths".into(),
        Reason::Paths { paths } => {
            let shown = paths
                .iter()
                .take(WHY_PATHS)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            match paths.len().saturating_sub(WHY_PATHS) {
                0 => format!("paths: {shown}"),
                more => format!("paths: {shown} and {more} more"),
            }
        }
    }
}

/// The units a request needs.
///
/// `full_allowed` is false while the project gates after landing, where a full run is the gate's
/// job and not a session's.
pub(crate) fn plan(
    map: &MapState,
    kind: Kind,
    scope: ScopeArg,
    changed: &[String],
    fillable: &BTreeSet<String>,
    gate_command: Option<&str>,
    full_allowed: bool,
) -> Result<Plan, PlanError> {
    if scope == ScopeArg::Full {
        if !full_allowed {
            return Err(PlanError::FullRefused(
                "scope full is refused while the project gates after landing".into(),
            ));
        }
        return Ok(Plan {
            units: vec![gate_unit(gate_command)?],
            unclaimed: Vec::new(),
            note: None,
        });
    }

    let map = match map {
        MapState::Invalid(errors) => return Err(PlanError::InvalidMap(errors.clone())),
        MapState::Absent => {
            // Without a map `own` has nothing to select from, but `scope` can still be answered
            // by the one command the project does declare.
            return if scope == ScopeArg::Own {
                Ok(Plan {
                    units: Vec::new(),
                    unclaimed: Vec::new(),
                    note: Some(NO_MAP_OWN_NOTE.into()),
                })
            } else {
                Ok(Plan {
                    units: vec![gate_unit(gate_command)?],
                    unclaimed: Vec::new(),
                    note: Some(
                        "no test map: the scope falls back to the project's gate_command".into(),
                    ),
                })
            };
        }
        MapState::Valid(map) => map,
    };

    if changed.is_empty() {
        return Ok(Plan {
            units: Vec::new(),
            unclaimed: Vec::new(),
            note: Some(NOTHING_CHANGED_NOTE.into()),
        });
    }

    let selection = select(map, changed, fillable);
    let mut note = None;
    if selection.full {
        // Every unit carries the same reason when the selection is full, so the first names it.
        let cause = match selection.units.first().map(|unit| &unit.reason) {
            Some(Reason::FullSweep { path }) => format!("{path} is a full-sweep path"),
            _ => "paths no group claims".into(),
        };
        note = Some(format!("every group runs: {cause}"));
    }

    let units = selection
        .units
        .iter()
        .map(|unit| {
            let group = map.tests.groups.get(&unit.group);
            let (argv, cacheable, skipped) = match kind {
                Kind::Test => (
                    unit.argv.clone(),
                    group.is_none_or(|group| group.cache),
                    None,
                ),
                Kind::Check => match group.and_then(|group| group.check.as_deref()) {
                    Some(check) => (
                        crate::gate::split_command(check).unwrap_or_default(),
                        group.is_none_or(|group| group.cache),
                        None,
                    ),
                    None => (
                        Vec::new(),
                        false,
                        Some("this group has no `check` command".to_string()),
                    ),
                },
            };
            PlannedUnit {
                group: Some(unit.group.clone()),
                argv,
                why: why(&unit.reason),
                fingerprint: None,
                cacheable,
                run_id: None,
                cached_from: None,
                skipped,
            }
        })
        .collect();

    Ok(Plan {
        units,
        unclaimed: selection.unclaimed,
        note,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Progress {
    pub total: usize,
    pub finished: usize,
    pub queued: usize,
    pub running: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnitReport {
    pub group: Option<String>,
    pub argv: Vec<String>,
    pub why: String,
    /// queued|running|passed|failed|errored|skipped_cached|skipped|unknown
    pub status: String,
    pub duration_ms: Option<i64>,
    pub exit_code: Option<i64>,
    pub output_tail: Option<String>,
    pub skipped_reason: Option<String>,
    pub run_id: Option<i64>,
    pub cached_from: Option<i64>,
    /// Set on a failed unit whose group is already red on the target branch; it does not change
    /// the unit's status or the ticket's verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub already_failing: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ticket {
    pub ticket: i64,
    pub done: bool,
    /// Some exactly when `done`: "passed" | "failed" | "errored" | "nothing_ran".
    pub verdict: Option<String>,
    pub project_id: String,
    pub worktree: String,
    pub kind: String,
    pub scope: String,
    pub base: Option<String>,
    pub note: Option<String>,
    pub unclaimed: Vec<String>,
    pub progress: Progress,
    pub units: Vec<UnitReport>,
}

fn report(unit: &PlannedUnit, live: &HashMap<i64, State>) -> UnitReport {
    let mut report = UnitReport {
        group: unit.group.clone(),
        argv: unit.argv.clone(),
        why: unit.why.clone(),
        status: "unknown".into(),
        duration_ms: None,
        exit_code: None,
        output_tail: None,
        skipped_reason: None,
        run_id: unit.run_id,
        cached_from: unit.cached_from,
        already_failing: None,
    };
    if let Some(reason) = &unit.skipped {
        report.status = "skipped".into();
        report.skipped_reason = Some(reason.clone());
    } else if let Some(cached) = unit.cached_from {
        report.status = STATUS_SKIPPED_CACHED.into();
        report.skipped_reason = Some(format!(
            "a green run #{cached} over the same fingerprint is reused"
        ));
    } else if let Some(state) = unit.run_id.and_then(|id| live.get(&id)) {
        report.status = state.status.clone();
        report.exit_code = state.exit_code;
        report.duration_ms = state.duration_ms;
        // Only a run that went wrong has a tail worth reading; a green one is noise.
        if state.status == STATUS_FAILED || state.status == STATUS_ERRORED {
            report.output_tail = state.output_tail.clone();
        }
    }
    report
}

/// The ticket for `request`, given the live state of its `verify_runs` rows by id.
///
/// The request's state is derived here from its units and never stored, so it cannot disagree
/// with the rows it summarises.
pub(crate) fn assemble(request: &RequestRow, live: &HashMap<i64, State>) -> Ticket {
    let units: Vec<UnitReport> = request.plan.iter().map(|unit| report(unit, live)).collect();

    let total = units.len();
    let is_queued = |unit: &UnitReport| unit.status == STATUS_QUEUED;
    let is_running = |unit: &UnitReport| unit.status == STATUS_RUNNING;
    let queued = units.iter().filter(|unit| is_queued(unit)).count();
    let finished = units
        .iter()
        .filter(|unit| !is_queued(unit) && !is_running(unit))
        .count();
    let running = units
        .iter()
        .filter(|unit| is_running(unit))
        .map(|unit| unit.group.clone().unwrap_or_else(|| unit.argv.join(" ")))
        .collect();

    let done = finished == total;
    let verdict = done.then(|| {
        let any = |status: &str| units.iter().any(|unit| unit.status == status);
        if any(STATUS_FAILED) {
            "failed"
        } else if any(STATUS_ERRORED) || any("unknown") {
            "errored"
        } else if any(STATUS_PASSED) || any(STATUS_SKIPPED_CACHED) {
            "passed"
        } else {
            "nothing_ran"
        }
        .to_string()
    });

    Ticket {
        ticket: request.id,
        done,
        verdict,
        project_id: request.project_id.clone(),
        worktree: request.worktree.clone(),
        kind: request.kind.clone(),
        scope: request.scope.clone(),
        base: request.base.clone(),
        note: request.note.clone(),
        unclaimed: request.unclaimed.clone(),
        progress: Progress {
            total,
            finished,
            queued,
            running,
        },
        units,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify_runs::{STATUS_ERRORED, STATUS_FAILED};

    const MAP: &str = "version: 1
tests:
  groups:
    core:
      paths: [core/]
      check: cargo check -p nucleos-core
      command: cargo test -p nucleos-core
    py:
      paths: ['**/*.py']
      command: pytest
      select: pytest -q -- {files}
      cache: false
";

    fn valid() -> MapState {
        MapState::Valid(crate::tests_map::parse(MAP).unwrap())
    }

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| p.to_string()).collect()
    }

    fn fillable(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|p| p.to_string()).collect()
    }

    fn plan_with(
        map: &MapState,
        kind: Kind,
        scope: ScopeArg,
        changed: &[&str],
        gate: Option<&str>,
        full_allowed: bool,
    ) -> Result<Plan, PlanError> {
        plan(
            map,
            kind,
            scope,
            &paths(changed),
            &fillable(changed),
            gate,
            full_allowed,
        )
    }

    #[test]
    fn full_runs_the_gate_command_as_one_uncacheable_unit() {
        let plan = plan_with(
            &valid(),
            Kind::Test,
            ScopeArg::Full,
            &["core/a.rs"],
            Some("bash scripts/gates.sh all"),
            true,
        )
        .unwrap();
        assert_eq!(plan.units.len(), 1);
        let unit = &plan.units[0];
        assert_eq!(unit.group, None);
        assert_eq!(unit.argv, ["bash", "scripts/gates.sh", "all"]);
        assert_eq!(unit.why, "gate_command");
        assert!(!unit.cacheable);
        assert!(plan.unclaimed.is_empty());
        assert_eq!(plan.note, None);
    }

    #[test]
    fn full_without_a_gate_command_is_refused() {
        for gate in [None, Some("   ")] {
            let result = plan_with(&valid(), Kind::Test, ScopeArg::Full, &[], gate, true);
            assert_eq!(result, Err(PlanError::NoGateCommand));
        }
    }

    #[test]
    fn full_is_refused_when_not_allowed() {
        let result = plan_with(
            &valid(),
            Kind::Test,
            ScopeArg::Full,
            &[],
            Some("make test"),
            false,
        );
        assert!(matches!(result, Err(PlanError::FullRefused(_))));
    }

    #[test]
    fn own_without_a_map_runs_nothing_and_says_so() {
        let plan = plan_with(
            &MapState::Absent,
            Kind::Test,
            ScopeArg::Own,
            &["a.rs"],
            Some("make test"),
            true,
        )
        .unwrap();
        assert!(plan.units.is_empty());
        assert!(plan.unclaimed.is_empty());
        assert_eq!(plan.note.as_deref(), Some(NO_MAP_OWN_NOTE));
    }

    #[test]
    fn scope_without_a_map_falls_back_to_the_gate_command() {
        let plan = plan_with(
            &MapState::Absent,
            Kind::Test,
            ScopeArg::Scope,
            &["a.rs"],
            Some("make test"),
            true,
        )
        .unwrap();
        assert_eq!(plan.units.len(), 1);
        assert_eq!(plan.units[0].argv, ["make", "test"]);
        assert_eq!(plan.units[0].group, None);
        assert!(!plan.units[0].cacheable);
        assert!(plan.note.unwrap().contains("gate_command"));
    }

    #[test]
    fn scope_without_a_map_or_gate_command_is_refused() {
        let result = plan_with(
            &MapState::Absent,
            Kind::Test,
            ScopeArg::Scope,
            &["a.rs"],
            None,
            true,
        );
        assert_eq!(result, Err(PlanError::NoGateCommand));
    }

    #[test]
    fn an_invalid_map_is_refused_with_its_errors() {
        let map = MapState::Invalid(vec!["bad one".into(), "bad two".into()]);
        let result = plan_with(&map, Kind::Test, ScopeArg::Own, &["a.rs"], None, true);
        assert_eq!(
            result,
            Err(PlanError::InvalidMap(vec![
                "bad one".into(),
                "bad two".into()
            ]))
        );
    }

    #[test]
    fn test_kind_uses_the_filled_select_argv() {
        let plan = plan_with(
            &valid(),
            Kind::Test,
            ScopeArg::Own,
            &["tools/a.py"],
            None,
            true,
        )
        .unwrap();
        assert_eq!(plan.units.len(), 1);
        assert_eq!(plan.units[0].group.as_deref(), Some("py"));
        assert_eq!(plan.units[0].argv, ["pytest", "-q", "--", "./tools/a.py"]);
        assert_eq!(plan.units[0].why, "paths: tools/a.py");
        assert_eq!(plan.units[0].skipped, None);
    }

    #[test]
    fn check_kind_uses_the_groups_check_command() {
        let plan = plan_with(
            &valid(),
            Kind::Check,
            ScopeArg::Own,
            &["core/src/a.rs"],
            None,
            true,
        )
        .unwrap();
        assert_eq!(plan.units.len(), 1);
        assert_eq!(plan.units[0].group.as_deref(), Some("core"));
        assert_eq!(plan.units[0].argv, ["cargo", "check", "-p", "nucleos-core"]);
        assert!(plan.units[0].cacheable);
        assert_eq!(plan.units[0].skipped, None);
    }

    #[test]
    fn check_kind_skips_a_group_without_check() {
        let plan = plan_with(
            &valid(),
            Kind::Check,
            ScopeArg::Own,
            &["tools/a.py"],
            None,
            true,
        )
        .unwrap();
        let unit = &plan.units[0];
        assert!(unit.argv.is_empty());
        assert!(!unit.cacheable);
        assert_eq!(
            unit.skipped.as_deref(),
            Some("this group has no `check` command")
        );
    }

    #[test]
    fn a_group_with_cache_false_is_not_cacheable() {
        let plan = plan_with(
            &valid(),
            Kind::Test,
            ScopeArg::Own,
            &["tools/a.py", "core/src/a.rs"],
            None,
            true,
        )
        .unwrap();
        let cacheable = |name: &str| {
            plan.units
                .iter()
                .find(|unit| unit.group.as_deref() == Some(name))
                .unwrap()
                .cacheable
        };
        assert!(!cacheable("py"));
        assert!(cacheable("core"));
    }

    #[test]
    fn an_unclaimed_path_is_reported_and_runs_every_group() {
        let plan = plan_with(
            &valid(),
            Kind::Test,
            ScopeArg::Own,
            &["README.md"],
            None,
            true,
        )
        .unwrap();
        assert_eq!(plan.unclaimed, ["README.md"]);
        assert_eq!(plan.units.len(), 2);
        assert!(plan.units.iter().all(|unit| unit.why == "unclaimed paths"));
        assert!(plan.note.unwrap().starts_with("every group runs"));
    }

    #[test]
    fn nothing_changed_plans_nothing() {
        let plan = plan_with(&valid(), Kind::Test, ScopeArg::Scope, &[], None, true).unwrap();
        assert!(plan.units.is_empty());
        assert_eq!(plan.note.as_deref(), Some(NOTHING_CHANGED_NOTE));
    }

    #[test]
    fn kind_and_scope_read_lowercase_json() {
        assert_eq!(
            serde_json::from_str::<Kind>("\"check\"").unwrap(),
            Kind::Check
        );
        assert_eq!(
            serde_json::from_str::<Kind>("\"test\"").unwrap(),
            Kind::Test
        );
        assert_eq!(
            serde_json::from_str::<ScopeArg>("\"own\"").unwrap(),
            ScopeArg::Own
        );
        assert_eq!(
            serde_json::from_str::<ScopeArg>("\"scope\"").unwrap(),
            ScopeArg::Scope
        );
        assert_eq!(
            serde_json::from_str::<ScopeArg>("\"full\"").unwrap(),
            ScopeArg::Full
        );
        assert!(serde_json::from_str::<Kind>("\"Check\"").is_err());
        assert_eq!(Kind::Check.as_str(), "check");
        assert_eq!(ScopeArg::Full.as_str(), "full");
    }

    fn unit(group: &str, run_id: Option<i64>) -> PlannedUnit {
        PlannedUnit {
            group: Some(group.to_string()),
            argv: vec!["run".into(), group.to_string()],
            why: "paths: a".into(),
            fingerprint: None,
            cacheable: true,
            run_id,
            cached_from: None,
            skipped: None,
        }
    }

    fn request(plan: Vec<PlannedUnit>) -> RequestRow {
        RequestRow {
            id: 7,
            project_id: "p".into(),
            worktree: "/w".into(),
            kind: "test".into(),
            scope: "own".into(),
            base: Some("main".into()),
            priority: 0,
            caller: "owner".into(),
            note: Some("a note".into()),
            unclaimed: vec!["x".into()],
            plan,
            created_at: "now".into(),
        }
    }

    fn state(id: i64, status: &str) -> State {
        State {
            id,
            status: status.to_string(),
            exit_code: None,
            duration_ms: Some(5),
            output_tail: Some("tail".into()),
            interruptions: 0,
        }
    }

    fn live(states: Vec<State>) -> HashMap<i64, State> {
        states.into_iter().map(|s| (s.id, s)).collect()
    }

    #[test]
    fn a_ticket_with_a_running_unit_is_not_done_and_names_it() {
        let request = request(vec![unit("core", Some(1)), unit("py", Some(2))]);
        let ticket = assemble(
            &request,
            &live(vec![state(1, STATUS_PASSED), state(2, STATUS_RUNNING)]),
        );
        assert!(!ticket.done);
        assert_eq!(ticket.verdict, None);
        assert_eq!(ticket.progress.total, 2);
        assert_eq!(ticket.progress.finished, 1);
        assert_eq!(ticket.progress.queued, 0);
        assert_eq!(ticket.progress.running, ["py"]);
        assert_eq!(ticket.ticket, 7);
        assert_eq!(ticket.project_id, "p");
        assert_eq!(ticket.base.as_deref(), Some("main"));
        assert_eq!(ticket.note.as_deref(), Some("a note"));
        assert_eq!(ticket.unclaimed, ["x"]);
    }

    #[test]
    fn every_unit_passed_is_done_and_passed() {
        let request = request(vec![unit("core", Some(1)), unit("py", Some(2))]);
        let ticket = assemble(
            &request,
            &live(vec![state(1, STATUS_PASSED), state(2, STATUS_PASSED)]),
        );
        assert!(ticket.done);
        assert_eq!(ticket.verdict.as_deref(), Some("passed"));
        assert_eq!(ticket.progress.finished, 2);
    }

    #[test]
    fn one_failed_unit_fails_the_verdict_and_keeps_its_tail() {
        let request = request(vec![unit("core", Some(1)), unit("py", Some(2))]);
        let mut failed = state(2, STATUS_FAILED);
        failed.exit_code = Some(1);
        let ticket = assemble(&request, &live(vec![state(1, STATUS_PASSED), failed]));
        assert_eq!(ticket.verdict.as_deref(), Some("failed"));
        assert_eq!(ticket.units[1].exit_code, Some(1));
        assert_eq!(ticket.units[1].output_tail.as_deref(), Some("tail"));
        assert_eq!(ticket.units[1].duration_ms, Some(5));
    }

    #[test]
    fn an_errored_unit_without_failures_errors_the_verdict() {
        let request = request(vec![unit("core", Some(1)), unit("py", Some(2))]);
        let ticket = assemble(
            &request,
            &live(vec![state(1, STATUS_PASSED), state(2, STATUS_ERRORED)]),
        );
        assert_eq!(ticket.verdict.as_deref(), Some("errored"));
        assert_eq!(ticket.units[1].output_tail.as_deref(), Some("tail"));
    }

    #[test]
    fn a_cached_unit_counts_as_finished_and_passed() {
        let mut cached = unit("core", Some(9));
        cached.cached_from = Some(3);
        let ticket = assemble(&request(vec![cached]), &HashMap::new());
        assert!(ticket.done);
        assert_eq!(ticket.verdict.as_deref(), Some("passed"));
        let report = &ticket.units[0];
        assert_eq!(report.status, STATUS_SKIPPED_CACHED);
        assert_eq!(report.duration_ms, None);
        assert_eq!(report.cached_from, Some(3));
        assert_eq!(
            report.skipped_reason.as_deref(),
            Some("a green run #3 over the same fingerprint is reused")
        );
    }

    #[test]
    fn a_skipped_unit_carries_its_reason() {
        let mut skipped = unit("py", None);
        skipped.skipped = Some("no check".into());
        let ticket = assemble(&request(vec![skipped]), &HashMap::new());
        assert!(ticket.done);
        assert_eq!(ticket.units[0].status, "skipped");
        assert_eq!(ticket.units[0].skipped_reason.as_deref(), Some("no check"));
        assert_eq!(ticket.verdict.as_deref(), Some("nothing_ran"));
    }

    #[test]
    fn an_empty_plan_is_done_with_nothing_ran() {
        let ticket = assemble(&request(Vec::new()), &HashMap::new());
        assert!(ticket.done);
        assert_eq!(ticket.verdict.as_deref(), Some("nothing_ran"));
        assert_eq!(ticket.progress.total, 0);
    }

    #[test]
    fn a_missing_row_is_unknown_and_errors_the_verdict() {
        let request = request(vec![unit("core", Some(1)), unit("py", Some(2))]);
        let ticket = assemble(&request, &live(vec![state(1, STATUS_PASSED)]));
        assert_eq!(ticket.units[1].status, "unknown");
        assert!(ticket.done);
        assert_eq!(ticket.verdict.as_deref(), Some("errored"));
    }

    #[test]
    fn passed_units_carry_no_output_tail() {
        let request = request(vec![unit("core", Some(1))]);
        let ticket = assemble(&request, &live(vec![state(1, STATUS_PASSED)]));
        assert_eq!(ticket.units[0].output_tail, None);
    }
}
