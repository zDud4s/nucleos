//! The durable tree manifest: the serde port of `tree_state.py`'s branch and manifest shapes.
//!
//! A manifest is written by more than one hand and grows keys after it is created, so every key
//! this port does not name is kept in an `extra` map and written back unchanged. The nested shapes
//! (`gates`, `reviews`, `records`, ...) are not pinned yet, so they stay plain JSON.

use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The manifest version this port reads and writes (`MANIFEST_VERSION`).
pub const MANIFEST_VERSION: u32 = 1;

pub const BRANCH_STATES: [&str; 11] = [
    "queued",
    "open",
    "building",
    "gating",
    "green",
    "red",
    "reviewing",
    "fixing",
    "merged",
    "parked",
    "dropped",
];

/// The states in which a BRANCH has stopped moving. A parked branch counts here, and that does not
/// carry over to a job: see [`JOB_TERMINAL_STATE`].
pub const TERMINAL_STATES: [&str; 3] = ["merged", "parked", "dropped"];

/// The only state in which a JOB is over. A parked job is still live, so that boot can resume it.
pub const JOB_TERMINAL_STATE: &str = "finished";

/// One branch of a tree, as `new_branch` creates it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub paths: Vec<String>,
    pub layer: String,
    pub base: Option<String>,
    pub state: String,
    pub git_branch: Option<String>,
    pub worktree: Option<String>,
    pub target_dir: Option<String>,
    pub node_id: Option<String>,
    pub gates: Vec<Value>,
    pub reviews: Vec<Value>,
    pub park: Option<Value>,
    /// Keys `tree.py` adds later, such as `merged_at`, `merge_sha`, `drop_reason`, `dropped_at`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Branch {
    pub fn new(name: &str, paths: &[String], layer: &str, base: Option<&str>) -> Branch {
        Branch {
            name: name.to_owned(),
            paths: paths.to_vec(),
            layer: layer.to_owned(),
            base: base.map(str::to_owned),
            state: "queued".to_owned(),
            git_branch: None,
            worktree: None,
            target_dir: None,
            node_id: None,
            gates: Vec::new(),
            reviews: Vec::new(),
            park: None,
            extra: BTreeMap::new(),
        }
    }
}

/// A whole tree, as `new_manifest` creates it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub tree_id: String,
    pub feature_branch: String,
    pub goal: String,
    /// RFC 3339 text.
    pub created_at: String,
    pub state: String,
    /// Always with forward slashes.
    pub project_root: String,
    pub dirty_owner_files: Vec<String>,
    pub branches: BTreeMap<String, Branch>,
    pub records: Vec<Value>,
    pub attempts: BTreeMap<String, Value>,
    /// `Value` rather than `f64`, so that a whole number written by `tree.py` stays one.
    pub costs_gb: BTreeMap<String, Value>,
    pub disk_samples: Vec<Value>,
    pub park: Option<Value>,
    /// Keys `tree.py` adds later, such as `finish`, `root_closed`, `integration_reviews`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Manifest {
    pub fn new(
        tree_id: &str,
        feature_branch: &str,
        goal: &str,
        project_root: &str,
        dirty: &[String],
    ) -> Manifest {
        Manifest {
            version: MANIFEST_VERSION,
            tree_id: tree_id.to_owned(),
            feature_branch: feature_branch.to_owned(),
            goal: goal.to_owned(),
            // Whole seconds and `+00:00`, the form `now_iso` writes.
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false),
            state: "open".to_owned(),
            project_root: project_root.replace('\\', "/"),
            dirty_owner_files: dirty.to_vec(),
            branches: BTreeMap::new(),
            records: Vec::new(),
            attempts: BTreeMap::new(),
            costs_gb: BTreeMap::new(),
            disk_samples: Vec::new(),
            park: None,
            extra: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn branch_states_match_the_python_list() {
        assert_eq!(
            BRANCH_STATES.to_vec(),
            vec![
                "queued",
                "open",
                "building",
                "gating",
                "green",
                "red",
                "reviewing",
                "fixing",
                "merged",
                "parked",
                "dropped",
            ]
        );
    }

    #[test]
    fn terminal_states_match_the_python_list_and_are_branch_states() {
        assert_eq!(
            TERMINAL_STATES.to_vec(),
            vec!["merged", "parked", "dropped"]
        );
        for state in TERMINAL_STATES.iter() {
            assert!(
                BRANCH_STATES.contains(state),
                "{state} is terminal but not a branch state"
            );
        }
    }

    #[test]
    fn a_new_branch_has_the_new_branch_defaults() {
        let paths = vec!["core/src/**".to_owned()];
        let branch = Branch::new("api", &paths, "backend", Some("db"));
        assert_eq!(
            serde_json::to_value(&branch).unwrap(),
            json!({
                "name": "api",
                "paths": ["core/src/**"],
                "layer": "backend",
                "base": "db",
                "state": "queued",
                "git_branch": null,
                "worktree": null,
                "target_dir": null,
                "node_id": null,
                "gates": [],
                "reviews": [],
                "park": null,
            })
        );

        let rootless = Branch::new("db", &[], "data", None);
        assert_eq!(rootless.base, None);
        assert!(rootless.paths.is_empty());
    }

    #[test]
    fn a_new_manifest_has_the_new_manifest_defaults() {
        let dirty = vec!["shell/package.json".to_owned()];
        let manifest = Manifest::new(
            "tree-1",
            "feat/tree",
            "Ship it",
            "C:\\Projects\\nucleos",
            &dirty,
        );
        let mut value = serde_json::to_value(&manifest).unwrap();

        // The stamp is the clock's, so it is checked for form and taken out of the comparison.
        let created_at = value.as_object_mut().unwrap().remove("created_at").unwrap();
        chrono::DateTime::parse_from_rfc3339(created_at.as_str().unwrap())
            .expect("created_at is RFC 3339");

        assert_eq!(
            value,
            json!({
                "version": 1,
                "tree_id": "tree-1",
                "feature_branch": "feat/tree",
                "goal": "Ship it",
                "state": "open",
                "project_root": "C:/Projects/nucleos",
                "dirty_owner_files": ["shell/package.json"],
                "branches": {},
                "records": [],
                "attempts": {},
                "costs_gb": {},
                "disk_samples": [],
                "park": null,
            })
        );
    }

    /// `tree.py` adds `merged_at`, `merge_sha`, `drop_reason` and `dropped_at` to a branch after
    /// `new_branch`, so a round trip that drops them would lose state.
    #[test]
    fn a_populated_branch_round_trips_through_json_keeping_keys_added_later() {
        let original = json!({
            "name": "api",
            "paths": ["core/src/**", "core/tests/**"],
            "layer": "backend",
            "base": "db",
            "state": "merged",
            "git_branch": "tree/1/api",
            "worktree": "C:/worktrees/api",
            "target_dir": "C:/targets/api",
            "node_id": "node-1",
            "gates": [{"status": "green", "at": "2026-10-30T10:00:00+00:00"}],
            "reviews": [{"round": 1, "findings": [{"id": "f1", "state": "fixed"}]}],
            "park": {"needs": "a decision"},
            "merged_at": "2026-10-30T11:00:00+00:00",
            "merge_sha": "abc123",
        });

        let branch: Branch = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(branch.state, "merged");
        assert_eq!(branch.git_branch.as_deref(), Some("tree/1/api"));
        assert_eq!(serde_json::to_value(&branch).unwrap(), original);

        let reread: Branch =
            serde_json::from_str(&serde_json::to_string(&branch).unwrap()).unwrap();
        assert_eq!(reread, branch);
    }

    /// `tree.py` adds `finish`, `root_closed` and `integration_reviews` to a manifest after
    /// `new_manifest`, so a round trip that drops them would lose state.
    #[test]
    fn a_populated_manifest_round_trips_through_json_keeping_keys_added_later() {
        let original = json!({
            "version": 1,
            "tree_id": "tree-1",
            "feature_branch": "feat/tree",
            "goal": "Ship it",
            "created_at": "2026-10-30T10:00:00+00:00",
            "state": "finished",
            "project_root": "C:/Projects/nucleos",
            "dirty_owner_files": ["shell/package.json"],
            "branches": {
                "api": {
                    "name": "api",
                    "paths": ["core/src/**"],
                    "layer": "backend",
                    "base": null,
                    "state": "building",
                    "git_branch": null,
                    "worktree": null,
                    "target_dir": null,
                    "node_id": "node-1",
                    "gates": [],
                    "reviews": [],
                    "park": null,
                },
            },
            "records": [{"node_id": "node-1", "status": "done"}],
            "attempts": {"api": 2},
            "costs_gb": {"api": 1.5},
            "disk_samples": [{"free_gb": 120.5}],
            "park": null,
            "finish": {"status": "done", "summary": "landed"},
            "root_closed": true,
            "integration_reviews": [{"round": 1}],
        });

        let manifest: Manifest = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(manifest.tree_id, "tree-1");
        assert_eq!(manifest.branches["api"].state, "building");
        assert_eq!(serde_json::to_value(&manifest).unwrap(), original);

        let reread: Manifest =
            serde_json::from_str(&serde_json::to_string(&manifest).unwrap()).unwrap();
        assert_eq!(reread, manifest);
    }
}
