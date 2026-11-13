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
            Ok(target) => check_branch(job, held, &target)
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(job = %job.id, branch = name, %error, "the path fence could not read the diff");
                    None
                }),
            Err(_) => None,
        },
        None => None,
    };
    let Some(mut reason) = fenced else {
        return Ok(false);
    };
    // Files that came in with the base are not the branch's own doing: the driver can settle that
    // itself (merge the base, retry). Widening the paths stays with the owner.
    if from_base(job, name).await {
        reason.push(' ');
        reason.push_str(super::retry::FROM_BASE_MARK);
    }
    tracing::warn!(job = %job.id, branch = name, %reason, "the path fence refused the merge");
    if let Some(held) = job.manifest.branches.get_mut(name) {
        held.extra
            .insert("fence_refusal".to_owned(), Value::from(reason.clone()));
    }
    persist(pool, job).await?;
    if let Some(node) = node {
        parks.push((node, reason));
    }
    Ok(true)
}

/// The files of `changed` the fence would call out of path (migrations and the module map aside).
fn outside_files(paths: &[String], changed: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    changed
        .iter()
        .filter(|file| {
            file.as_str() != MODULE_MAP
                && !is_migration(file)
                && !paths.iter().any(|glob| owned_glob_matches(glob, file))
        })
        .cloned()
        .collect()
}

/// Whether every out-of-path file arrived with the branch's base rather than from its own work:
/// none of them is in what the branch changed on top of that base.
fn all_from_base(outside: &[String], own: &[String]) -> bool {
    !outside.is_empty() && outside.iter().all(|file| !own.contains(file))
}

/// Whether the branch's fence refusal is entirely the base's doing. Any doubt (no base, no git
/// branch, a git failure) answers `false`: the owner keeps the question.
async fn from_base(job: &TreeJob, name: &str) -> bool {
    let Some(branch) = job.manifest.branches.get(name) else {
        return false;
    };
    let (Some(git_branch), Some(base)) = (&branch.git_branch, &branch.base) else {
        return false;
    };
    let Some(base_git) = job
        .manifest
        .branches
        .get(base)
        .and_then(|held| held.git_branch.clone())
    else {
        return false;
    };
    let Ok(target) = super::nesting::merge_target(&job.manifest, name) else {
        return false;
    };
    let root = &job.manifest.project_root;
    let Ok(changed) = git(
        root,
        &["diff", "--name-only", &format!("{target}...{git_branch}")],
    )
    .await
    else {
        return false;
    };
    let Ok(own) = git(
        root,
        &["diff", "--name-only", &format!("{base_git}...{git_branch}")],
    )
    .await
    else {
        return false;
    };
    all_from_base(
        &outside_files(&branch.paths, &lines(&changed)),
        &lines(&own),
    )
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
    fn out_of_path_files_are_from_the_base_only_when_the_branch_did_not_touch_them() {
        let outside = outside_files(
            &owned(),
            &s(&["core/src/execution/a.rs", "core/src/role_policy.rs"]),
        );
        assert_eq!(outside, s(&["core/src/role_policy.rs"]));
        assert!(all_from_base(&outside, &s(&["core/src/execution/a.rs"])));
        assert!(!all_from_base(&outside, &s(&["core/src/role_policy.rs"])));
        // Nothing outside the paths: nothing the base can be blamed for.
        assert!(!all_from_base(&[], &[]));
        // No declared paths restrict nothing.
        assert!(outside_files(&[], &s(&["x.rs"])).is_empty());
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
}
