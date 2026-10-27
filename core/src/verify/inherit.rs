//! Telling an inherited red from the session's own (task-claims spec 2026-10-22, section 3 Verify).
//!
//! The pure half (`decide`) is below; the half that reads git and the claims store (`apply`,
//! `ensure_fresh`, `refresh_master_red`) follows it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task_claims::{Status, TaskRow};
use crate::verify::failures::Failures;

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
