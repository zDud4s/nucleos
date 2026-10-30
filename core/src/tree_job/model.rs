//! The durable tree manifest: the serde port of `tree_state.py`'s branch and manifest shapes.

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
