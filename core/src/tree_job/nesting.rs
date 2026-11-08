//! Nested branches: an orchestrator branch is the parent of the branches its node starts. Pure
//! reads over the manifest; nothing here touches the database or git.

use super::model::{Branch, Manifest};
use super::root::ROOT_BRANCH;

/// The branch bound to `caller_node`, or `None` when that is the stem branch (`root`) or no branch
/// is bound, so a stem-level start stays unparented.
pub fn parent_branch_of(manifest: &Manifest, caller_node: &str) -> Option<String> {
    manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(caller_node))
        .map(|branch| branch.name.clone())
        .filter(|name| name != ROOT_BRANCH)
}

/// The parent, the grandparent and so on, cut where a name repeats or names no branch.
pub fn ancestors(manifest: &Manifest, branch: &str) -> Vec<String> {
    let mut seen = vec![branch.to_owned()];
    let mut out = Vec::new();
    let mut next = manifest
        .branches
        .get(branch)
        .and_then(|b| b.parent.as_deref());
    while let Some(name) = next {
        if seen.iter().any(|s| s == name) {
            break;
        }
        let Some(found) = manifest.branches.get(name) else {
            break;
        };
        seen.push(name.to_owned());
        out.push(name.to_owned());
        next = found.parent.as_deref();
    }
    out
}

/// The branches whose `parent` is `parent`, by name.
pub fn children<'a>(manifest: &'a Manifest, parent: &str) -> Vec<&'a Branch> {
    manifest
        .branches
        .values()
        .filter(|branch| branch.parent.as_deref() == Some(parent))
        .collect()
}

/// True when every child is `merged` or `dropped`. A `parked` child is not settled: the owner is
/// asked about it.
pub fn children_settled(manifest: &Manifest, parent: &str) -> bool {
    children(manifest, parent)
        .iter()
        .all(|child| matches!(child.state.as_str(), "merged" | "dropped"))
}

/// True for an orchestrator branch whose children all ended `dropped`: it has no diff of its own.
pub fn all_children_dropped(manifest: &Manifest, parent: &str) -> bool {
    let kids = children(manifest, parent);
    !kids.is_empty() && kids.iter().all(|child| child.state == "dropped")
}

/// The git ref a branch's worktree starts from: its parent's git branch when `branch.parent` is set,
/// else `feature` (the feature branch as `open_branch` computes it). `Err` names a parent that is
/// not live or has no git branch yet.
pub fn start_ref(manifest: &Manifest, branch: &Branch, feature: &str) -> Result<String, String> {
    match branch.parent.as_deref() {
        None => Ok(feature.to_owned()),
        Some(parent) => live_parent_ref(manifest, parent),
    }
}

/// The git branch `branch` merges into: its parent's for a child, the feature branch otherwise. `Err`
/// names a parent that is `dropped`, `parked` or has no git branch.
pub fn merge_target(manifest: &Manifest, branch: &str) -> Result<String, String> {
    match manifest
        .branches
        .get(branch)
        .and_then(|held| held.parent.as_deref())
    {
        None => Ok(manifest.feature_branch.clone()),
        Some(parent) => live_parent_ref(manifest, parent),
    }
}

fn live_parent_ref(manifest: &Manifest, parent: &str) -> Result<String, String> {
    let Some(found) = manifest.branches.get(parent) else {
        return Err(format!("parent branch {parent} is not in this tree"));
    };
    if matches!(found.state.as_str(), "dropped" | "parked") {
        return Err(format!(
            "parent branch {parent} is {}: its children merge nowhere",
            found.state
        ));
    }
    found
        .git_branch
        .clone()
        .ok_or_else(|| format!("parent branch {parent} has no git branch yet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(branches: &[(&str, &str, Option<&str>, Option<&str>)]) -> Manifest {
        let mut manifest = Manifest::new("t", "feat/t", "goal", "C:/p", &[]);
        for (name, state, parent, node) in branches {
            let mut branch = Branch::new(name, &[], "backend", None);
            branch.state = (*state).to_owned();
            branch.parent = parent.map(str::to_owned);
            branch.node_id = node.map(str::to_owned);
            manifest.branches.insert((*name).to_owned(), branch);
        }
        manifest
    }

    #[test]
    fn the_stem_branch_has_no_parent() {
        let m = manifest(&[("root", "building", None, Some("n-root"))]);
        assert_eq!(parent_branch_of(&m, "n-root"), None);
        assert_eq!(parent_branch_of(&m, "unbound"), None);
    }

    #[test]
    fn a_child_of_an_orchestrator_branch_finds_that_branch() {
        let m = manifest(&[
            ("root", "building", None, Some("n-root")),
            ("api", "building", None, Some("n-api")),
        ]);
        assert_eq!(parent_branch_of(&m, "n-api"), Some("api".to_owned()));
    }

    #[test]
    fn ancestors_walk_up_and_stop_on_a_cycle() {
        let m = manifest(&[
            ("a", "building", None, None),
            ("b", "building", Some("a"), None),
            ("c", "building", Some("b"), None),
        ]);
        assert_eq!(ancestors(&m, "c"), ["b", "a"]);
        let cyclic = manifest(&[
            ("a", "building", Some("b"), None),
            ("b", "building", Some("a"), None),
        ]);
        assert_eq!(ancestors(&cyclic, "a"), ["b"]);
    }

    #[test]
    fn a_child_starts_from_and_merges_into_a_live_parent_only() {
        let mut m = manifest(&[
            ("p", "building", None, None),
            ("c", "green", Some("p"), None),
            ("s", "green", None, None),
        ]);
        m.branches.get_mut("p").unwrap().git_branch = Some("tree/t/p".to_owned());
        let child = m.branches["c"].clone();
        assert_eq!(start_ref(&m, &child, "feat/t").unwrap(), "tree/t/p");
        assert_eq!(merge_target(&m, "c").unwrap(), "tree/t/p");
        assert_eq!(merge_target(&m, "s").unwrap(), "feat/t");
        for state in ["dropped", "parked"] {
            m.branches.get_mut("p").unwrap().state = state.to_owned();
            let error = merge_target(&m, "c").unwrap_err();
            assert!(error.contains("parent branch p"), "{error}");
            assert!(start_ref(&m, &child, "feat/t").is_err());
        }
    }

    #[test]
    fn all_children_dropped_needs_children_and_no_survivor() {
        let m = manifest(&[
            ("p", "building", None, None),
            ("a", "dropped", Some("p"), None),
            ("b", "merged", Some("p"), None),
        ]);
        assert!(!all_children_dropped(&m, "p"));
        assert!(!all_children_dropped(&m, "a"));
        let all = manifest(&[
            ("p", "building", None, None),
            ("a", "dropped", Some("p"), None),
        ]);
        assert!(all_children_dropped(&all, "p"));
    }

    #[test]
    fn children_settled_only_when_every_child_is_merged_or_dropped() {
        let done = manifest(&[
            ("p", "building", None, None),
            ("m", "merged", Some("p"), None),
            ("d", "dropped", Some("p"), None),
        ]);
        assert!(children_settled(&done, "p"));
        assert_eq!(children(&done, "p").len(), 2);
        for state in ["parked", "building", "green"] {
            let m = manifest(&[
                ("p", "building", None, None),
                ("m", "merged", Some("p"), None),
                ("x", state, Some("p"), None),
            ]);
            assert!(!children_settled(&m, "p"), "{state}");
        }
    }
}
