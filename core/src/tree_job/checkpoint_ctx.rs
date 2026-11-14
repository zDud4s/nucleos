//! What a tree branch's checkpoint commit says about itself: the node that wrote the work, the
//! branch and the tree. The message itself is `execution::worktree::checkpoint_message`.

use crate::execution::worktree::CheckpointContext;

use super::store::TreeJob;

/// The context for a checkpoint of branch `name` of `job`.
///
/// The id is the branch's node when it has one, else the branch name. The title is the branch name
/// and the tree's goal (`checkpoint_message` flattens and cuts it). `run_id` stays `None`: a node's
/// run is a text id in the node table and the message carries an integer run. `gated` is `true` only
/// where the checkpoint is the very commit a gate is about to measure.
pub(super) fn for_branch(job: &TreeJob, name: &str, gated: bool) -> CheckpointContext {
    let node_id = job
        .manifest
        .branches
        .get(name)
        .and_then(|held| held.node_id.clone());
    let goal = job.manifest.goal.trim();
    let title = if goal.is_empty() {
        name.to_owned()
    } else {
        format!("{name}: {goal}")
    };
    CheckpointContext {
        id: node_id.unwrap_or_else(|| name.to_owned()),
        title,
        run_id: None,
        tree_id: Some(job.manifest.tree_id.clone()),
        gated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::worktree::{checkpoint_message, is_checkpoint_message};
    use crate::tree_job::model::{Branch, Manifest};

    fn job(goal: &str, node_id: Option<&str>) -> TreeJob {
        let mut manifest = Manifest::new("tree-9", "feat/x", goal, "C:/repo", &[]);
        let mut branch = Branch::new("api", &["core/src".to_owned()], "code", None);
        branch.node_id = node_id.map(str::to_owned);
        manifest.branches.insert("api".to_owned(), branch);
        TreeJob {
            id: "job-1".to_owned(),
            root_node_id: "root-1".to_owned(),
            project_id: None,
            feature_branch: Some("feat/x".to_owned()),
            goal: goal.to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn subject(ctx: &CheckpointContext) -> String {
        checkpoint_message(ctx).lines().next().unwrap().to_owned()
    }

    #[test]
    fn the_subject_names_the_branch_node_and_the_goal() {
        let ctx = for_branch(&job("fix the thing", Some("node-7")), "api", false);
        assert_eq!(subject(&ctx), "nucleos(node-7): api: fix the thing");
    }

    #[test]
    fn a_branch_without_a_node_is_named_by_its_branch() {
        let ctx = for_branch(&job("fix the thing", None), "api", false);
        assert_eq!(subject(&ctx), "nucleos(api): api: fix the thing");
    }

    #[test]
    fn an_empty_goal_leaves_the_branch_name_as_the_title() {
        let ctx = for_branch(&job("  ", Some("node-7")), "api", false);
        assert_eq!(subject(&ctx), "nucleos(node-7): api");
    }

    #[test]
    fn a_long_multiline_goal_stays_one_short_subject() {
        let goal = format!("first line\nsecond line {}", "x".repeat(200));
        let ctx = for_branch(&job(&goal, Some("node-7")), "api", false);
        let line = subject(&ctx);
        assert!(!line.contains('\n'));
        assert!(
            line.chars().count() <= "nucleos(node-7): ".len() + 60,
            "{line:?}"
        );
    }

    #[test]
    fn the_message_carries_the_tree_and_the_gated_trailer() {
        let ungated = checkpoint_message(&for_branch(&job("g", Some("n")), "api", false));
        let gated = checkpoint_message(&for_branch(&job("g", Some("n")), "api", true));
        assert!(ungated.contains("Tree: tree-9"), "{ungated}");
        assert!(
            ungated.ends_with("Nucleos-Checkpoint: ungated"),
            "{ungated}"
        );
        assert!(gated.ends_with("Nucleos-Checkpoint: gated"), "{gated}");
        assert!(is_checkpoint_message(&ungated) && is_checkpoint_message(&gated));
    }

    #[test]
    fn an_unknown_branch_still_gets_a_context() {
        let ctx = for_branch(&job("g", Some("n")), "ghost", false);
        assert_eq!(ctx.id, "ghost");
    }
}
