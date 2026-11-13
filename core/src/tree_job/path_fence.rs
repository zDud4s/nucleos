//! The path fence of a leaf: before a green branch is merged into its parent, the files it changed
//! must sit inside the `paths` it declared, and a new migration must not reuse a number the parent
//! already has. Without it the reserved paths protect nothing and parallel leaves collide.

use serde_json::Value;
use sqlx::SqlitePool;

use super::docs_paths::owned_glob_matches;
use super::driver::{Park, git, persist};
use super::model::Branch;
use super::store::TreeJob;

/// The file every leaf may touch, whatever its `paths`.
const MODULE_MAP: &str = "core/MODULE_MAP.md";
/// Migrations are numbered centrally: a leaf may add one anywhere in this directory.
const MIGRATIONS_DIR: &str = "core/migrations/";

/// Branch `extra` keys of a fence refusal: its reason, the decision the owner was asked on (kept
/// after the answer so a repeated refusal reuses it), and the head the refusal was made on.
pub(super) const REFUSAL: &str = "fence_refusal";
pub(super) const DECISION: &str = "fence_decision_id";
pub(super) const HEAD: &str = "fence_head";

/// The numeric prefix of a migration file (`core/migrations/0302_x.sql` -> `0302`).
fn migration_number(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let digits = name.split('_').next()?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

fn is_migration(path: &str) -> bool {
    path.starts_with(MIGRATIONS_DIR) && migration_number(path).is_some()
}

/// Checks one leaf's diff. `paths` are its owned globs (empty = no restriction), `changed` every
/// file it changed against its parent, `parent_migrations` the migration files (repo-relative) at
/// the parent's tip. `Err` is the reason for the refusal, naming the offending files.
pub(super) fn check_leaf_diff(
    paths: &[String],
    changed: &[String],
    parent_migrations: &[String],
) -> Result<(), String> {
    let mut outside: Vec<&str> = Vec::new();
    let mut duplicates: Vec<String> = Vec::new();
    for file in changed {
        let new_migration = is_migration(file) && !parent_migrations.contains(file);
        if new_migration {
            if let Some(number) = migration_number(file)
                && parent_migrations
                    .iter()
                    .any(|existing| migration_number(existing) == Some(number))
            {
                duplicates.push(format!("{file} (number {number} is taken)"));
            }
            continue;
        }
        if paths.is_empty() || file == MODULE_MAP {
            continue;
        }
        if !paths.iter().any(|glob| owned_glob_matches(glob, file)) {
            outside.push(file);
        }
    }
    let mut reasons = Vec::new();
    if !outside.is_empty() {
        reasons.push(format!(
            "it changed files outside its declared paths [{}]: {}. Move that change to the branch that owns those paths, or have the parent widen this branch's paths",
            paths.join(", "),
            outside.join(", ")
        ));
    }
    if !duplicates.is_empty() {
        reasons.push(format!(
            "it adds a migration whose number the parent already has: {}. Renumber it to the next free number under core/migrations",
            duplicates.join(", ")
        ));
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons.join("; and "))
    }
}

/// Runs the fence for a green branch about to merge into `target`. `Ok(())` also when the branch has
/// no git branch yet. `Err` is a git failure (retried on the next pass), `Ok(Some(reason))` the
/// refusal.
pub(super) async fn check_branch(
    job: &TreeJob,
    branch: &Branch,
    target: &str,
) -> Result<Option<String>, String> {
    let Some(git_branch) = &branch.git_branch else {
        return Ok(None);
    };
    let root = &job.manifest.project_root;
    let range = format!("{target}...{git_branch}");
    let changed = lines(&git(root, &["diff", "--name-only", &range]).await?);
    if changed.is_empty() {
        return Ok(None);
    }
    let parent_migrations = lines(
        &git(
            root,
            &["ls-tree", "-r", "--name-only", target, "--", MIGRATIONS_DIR],
        )
        .await?,
    );
    Ok(check_leaf_diff(&branch.paths, &changed, &parent_migrations)
        .err()
        .map(|reason| {
            format!(
                "Branch {} cannot merge into {target}: {reason}.",
                branch.name
            )
        }))
}

/// The fence as the driver's merge step runs it: a leaf may only merge what its declared `paths`
/// own (and new migrations with a free number). A refusal is recorded on the branch as
/// `fence_refusal`, parks its node for the owner and answers `true`. A git failure reading the
/// diff fails open: the fence is a guard, and the queue's own merge reports a branch git cannot
/// read.
pub(super) async fn parked_by_fence(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
    node: Option<String>,
    parks: &mut Vec<Park>,
) -> Result<bool, String> {
    let fenced = match job.manifest.branches.get(name) {
        Some(held) => match super::nesting::merge_target(&job.manifest, name) {
            Ok(target) => check_branch(job, held, &target).await,
            Err(_) => Ok(None),
        },
        None => Ok(None),
    };
    let reason = match fenced {
        Ok(Some(reason)) => reason,
        Ok(None) => {
            // The diff is inside the paths now: whatever an earlier refusal left is over.
            let cleared = job.manifest.branches.get_mut(name).is_some_and(|held| {
                // Every key is removed: `any` would short-circuit, so count instead.
                [REFUSAL, DECISION, HEAD]
                    .iter()
                    .filter(|key| held.extra.remove(**key).is_some())
                    .count()
                    > 0
            });
            if cleared {
                persist(pool, job).await?;
            }
            return Ok(false);
        }
        Err(error) => {
            tracing::warn!(job = %job.id, branch = name, %error, "the path fence could not read the diff");
            return Ok(false);
        }
    };
    tracing::warn!(job = %job.id, branch = name, %reason, "the path fence refused the merge");
    let head = match job.manifest.branches.get(name) {
        Some(held) => head_of(job, held).await.ok(),
        None => None,
    };
    if let Some(held) = job.manifest.branches.get_mut(name) {
        held.extra
            .insert(REFUSAL.to_owned(), Value::from(reason.clone()));
        if let Some(head) = head {
            held.extra.insert(HEAD.to_owned(), Value::from(head));
        }
    }
    persist(pool, job).await?;
    if let Some(node) = node {
        parks.push((node, reason));
    }
    Ok(true)
}

/// The branch's head, as git names it.
async fn head_of(job: &TreeJob, branch: &Branch) -> Result<String, String> {
    let Some(git_branch) = &branch.git_branch else {
        return Err("the branch has no git branch".to_owned());
    };
    Ok(git(&job.manifest.project_root, &["rev-parse", git_branch])
        .await?
        .trim()
        .to_owned())
}

/// The files a fence refusal names as outside the declared paths, in the order it lists them. Empty
/// when the refusal is about a migration number only.
pub(super) fn files_named_in(reason: &str) -> Vec<String> {
    const MARK: &str = "declared paths [";
    let Some(start) = reason.find(MARK) else {
        return Vec::new();
    };
    let rest = &reason[start + MARK.len()..];
    let Some(close) = rest.find("]: ") else {
        return Vec::new();
    };
    let rest = &rest[close + 3..];
    let end = rest.find(". Move that change").unwrap_or(rest.len());
    rest[..end]
        .split(", ")
        .map(|file| file.trim().trim_end_matches('.').to_owned())
        .filter(|file| !file.is_empty())
        .collect()
}

/// Whether a branch released from a fence refusal changed head since: only then does it need a gate
/// again before it may merge.
pub(super) fn regate_needed(
    extra: &std::collections::BTreeMap<String, Value>,
    current_head: &str,
) -> bool {
    extra.contains_key(DECISION)
        && extra
            .get(HEAD)
            .and_then(Value::as_str)
            .is_some_and(|seen| seen != current_head)
}

/// A branch the owner answered after a fence refusal merges on its current head only when that head
/// is the one that was gated. When it moved, the branch goes back to `reviewing` so it is gated
/// again first. `true` means it moved and the merge step must stop here.
pub(super) async fn regate_if_moved(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
) -> Result<bool, String> {
    let Some(held) = job.manifest.branches.get(name) else {
        return Ok(false);
    };
    if !held.extra.contains_key(DECISION) {
        return Ok(false);
    }
    let Ok(head) = head_of(job, held).await else {
        return Ok(false);
    };
    if !regate_needed(&held.extra, &head) {
        return Ok(false);
    }
    let Some(held) = job.manifest.branches.get_mut(name) else {
        return Ok(false);
    };
    super::engine::apply_transition(held, "reviewing").map_err(|error| error.to_string())?;
    held.extra.remove(HEAD);
    persist(pool, job).await?;
    Ok(true)
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim().replace('\\', "/"))
        .filter(|line| !line.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn owned() -> Vec<String> {
        s(&["core/src/execution/**", "core/tests/**", "core/AGENTS.md"])
    }

    #[test]
    fn a_diff_inside_the_paths_passes() {
        let changed = s(&[
            "core/src/execution/a/b.rs",
            "core/tests/x.rs",
            "core/AGENTS.md",
        ]);
        assert_eq!(check_leaf_diff(&owned(), &changed, &[]), Ok(()));
    }

    #[test]
    fn a_file_outside_the_paths_is_refused_and_named() {
        let changed = s(&["core/src/execution/a.rs", "core/src/role_policy.rs"]);
        let reason = check_leaf_diff(&owned(), &changed, &[]).unwrap_err();
        assert!(reason.contains("core/src/role_policy.rs"), "{reason}");
        assert!(!reason.contains("execution/a.rs"), "{reason}");
    }

    #[test]
    fn empty_paths_restrict_nothing() {
        let changed = s(&["core/src/role_policy.rs", "shell/x.ts"]);
        assert_eq!(check_leaf_diff(&[], &changed, &[]), Ok(()));
    }

    #[test]
    fn the_module_map_and_a_new_migration_always_pass() {
        let changed = s(&["core/MODULE_MAP.md", "core/migrations/0303_new.sql"]);
        let parent = s(&[
            "core/migrations/0301_old.sql",
            "core/migrations/0302_old.sql",
        ]);
        assert_eq!(check_leaf_diff(&owned(), &changed, &parent), Ok(()));
    }

    #[test]
    fn a_duplicate_migration_number_is_refused() {
        let changed = s(&["core/migrations/0302_mine.sql"]);
        let parent = s(&["core/migrations/0302_theirs.sql"]);
        let reason = check_leaf_diff(&owned(), &changed, &parent).unwrap_err();
        assert!(reason.contains("0302_mine.sql"), "{reason}");
        assert!(reason.contains("next free number"), "{reason}");
    }

    #[test]
    fn editing_an_existing_migration_is_fenced_like_any_file() {
        let changed = s(&["core/migrations/0302_theirs.sql"]);
        let parent = s(&["core/migrations/0302_theirs.sql"]);
        let reason = check_leaf_diff(&owned(), &changed, &parent).unwrap_err();
        assert!(reason.contains("0302_theirs.sql"), "{reason}");
    }

    #[test]
    fn the_files_a_refusal_names_are_read_back_exactly() {
        let changed = s(&["core/src/execution/a.rs", "core/src/x.rs", "core/src/y.rs"]);
        let reason = check_leaf_diff(&owned(), &changed, &[]).unwrap_err();
        assert_eq!(
            files_named_in(&reason),
            s(&["core/src/x.rs", "core/src/y.rs"])
        );
        assert!(
            files_named_in("it adds a migration whose number the parent already has: a").is_empty()
        );
    }

    #[test]
    fn a_branch_is_regated_only_when_its_head_moved_since_the_refusal() {
        let mut extra = std::collections::BTreeMap::new();
        assert!(
            !regate_needed(&extra, "bbb"),
            "no refusal, nothing to compare"
        );
        extra.insert(DECISION.to_owned(), Value::from("d-1"));
        extra.insert(HEAD.to_owned(), Value::from("aaa"));
        assert!(
            !regate_needed(&extra, "aaa"),
            "an unchanged head merges as it is"
        );
        assert!(regate_needed(&extra, "bbb"), "a changed head needs a gate");
    }
}
