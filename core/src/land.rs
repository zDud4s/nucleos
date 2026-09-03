//! The owner of the main tree: which branch a project's landings merge into, and whether a given
//! landing may be admitted at all.
//!
//! Design: `.ai/specs/2026-08-27-dono-da-arvore-principal-design.md`, and — for a landing that
//! names its own destination — `.ai/specs/2026-09-03-alcada-por-projecto-design.md` §4.1.
//!
//! **The two documents number their decisions separately, and both have a decision #2.** An
//! unqualified "decision #N" anywhere in this file is the 2026-08-27 design's, which is the
//! numbering the module was written against; a citation of the alçada design says "alçada §4.1"
//! before its number. Getting this wrong costs a reader the wrong document, so it is spelled out
//! rather than left to be inferred.
//!
//! The dono asked for one thing in his own words — *"a responsabilidade do bom merge sem conflitos
//! ser de um módulo específico"* — after watching the queue land on a tree nobody chose roughly
//! ten times. Two defects composed to cause it: the target used to be read off the main checkout's
//! HEAD (`http.rs`, before this module existed), and a checkout parked on the wrong branch silently
//! redirected every landing there. This module exists so that answer has exactly one home.
//!
//! **What is this module's, and what stays where it already was.** `vcs.rs` still decides WHEN an
//! admitted request runs — exclusivity, ordering, retrying a merge whose target moved underneath
//! it (decision #4, `vcs::drain_once`). `git_exec.rs` still decides HOW a merge is computed and
//! published. This module decides WHAT a landing's `Op::Merge` names — which branch is the
//! project's integration branch, whether a caller may name a different destination for one landing,
//! and whether a source is worth submitting at all — and it is the only site in the core that
//! builds a landing's `Op::Merge`. `resolver.rs` keeps starting resolutions on its own schedule;
//! what this module adds is linking a resolution's landing back to the escalation it answers, at
//! the moment that landing is admitted.

use std::path::Path;
use std::time::Instant;

use crate::vcs::{Branch, Op, Origin, ResolvedRepo};

/// Decision #7's feed kind: a conflict resolution the agent could not produce, or that admission
/// otherwise refused. Named so it reads as this module's own line among `vcs_request_finished` and
/// the rest, rather than blending into them.
const RESOLUTION_FAILED_KIND: &str = "land_resolution_failed";

/// Why a landing could not be submitted.
///
/// Four arms rather than one string because `http.rs` answers them with different status codes: a
/// worktree already standing on the target is a conflict with itself, a source with nothing new to
/// bring is the same in spirit, a target that cannot be resolved is something the caller has to
/// change before anything lands, and an admission that failed after passing every check is this
/// daemon's own database. `land.rs` stays free of `axum` either way — the mapping to a status code
/// is `http.rs`'s to make, not this module's to import a web framework in order to state.
///
/// **`Refused` covers two kinds of thing, and the split does not separate them.** A project whose
/// integration branch is misconfigured and a caller who typed a target the project does not admit
/// both land here. The 422 `http.rs` answers with is right for both — one says "fix your project",
/// the other "fix your argument", and neither is this daemon's fault — but a fifth arm was not
/// added for the second, because nothing downstream would do anything different with it.
#[derive(Debug)]
pub enum LandRefusal {
    /// The worktree is already standing on the integration branch. Refused rather than admitted as
    /// a no-op: "land it" means "take my separate work", and there is none — a `Merge` naming one
    /// branch twice is a shape `git_exec` should never be handed.
    AlreadyOnTarget(String),
    /// Decision #3: the source is already an ancestor of the target, so there is nothing to bring
    /// in. Refused before a row is written, not during execution — a queued row is a promise to
    /// whoever is waiting on it, and "there was nothing to land" is not a promise worth making.
    NothingToLand(String),
    /// The landing has no target it may use. Three causes, and after the alçada design the last
    /// two are the common ones: the integration branch could not be resolved — decision #2's
    /// refusal (a declared branch that no longer exists, or nothing to derive one from), or a
    /// database read that failed on the way; the caller named a target this project does not
    /// admit (alçada §4.1, decision #2); or the named target is admitted and its branch is gone.
    Refused(String),
    /// Every check passed and the queue still would not admit the request.
    NotAdmitted(String),
}

impl LandRefusal {
    pub fn message(&self) -> &str {
        match self {
            Self::AlreadyOnTarget(message)
            | Self::NothingToLand(message)
            | Self::Refused(message)
            | Self::NotAdmitted(message) => message,
        }
    }
}

/// The branch a project's landings merge into — declared once, in `autopilot_state.integration_branch`,
/// and never read off any worktree's HEAD.
///
/// **This is the 2026-08-27 design's decision #2 in full** — the alçada design has a decision #2
/// of its own, about a target a caller may NAME, which is `resolve_target`'s and not this
/// function's — **and the sentence that matters is the negative one: nothing here
/// ever runs `current_branch` on the project's main checkout.** That read is the exact line the
/// design's defect 1 traces to (`http.rs`, before this module existed) — a checkout parked on a
/// feature branch used to redirect every landing in the project to it. Answering from a column
/// instead makes that redirection impossible to express, which is the whole of what the dono asked
/// for.
///
/// Answered in this order, and the order is the design:
///
/// 1. **A value is recorded.** Confirmed against the repository — `refs/heads/<branch>` has to
///    still exist — because a branch renamed or deleted out from under a stale column is a
///    misconfigured project, not a project with no integration branch, and the two get different
///    answers. Refused by name rather than failing three git commands deep inside a merge. One
///    exception, documented on `resolve_target` rather than here because it is that function's
///    choice: a landing that NAMES an admitted target which exists never reaches this function at
///    all, so "nothing lands" is really "nothing lands by default".
/// 2. **Nothing is recorded.** Derived once — `origin/HEAD`, else a local `master`, else a local
///    `main`, else refused — and the derivation is written back so every landing after the first
///    reads a column instead of asking git again. A project that is already healthy derives
///    `master` on its first landing and nothing about its behaviour changes.
pub async fn integration_branch(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    deadline: Instant,
) -> Result<Branch, String> {
    let recorded: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT integration_branch FROM autopilot_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("could not read {project_id}'s integration branch: {error}"))?
    .flatten();

    if let Some(branch) = recorded.filter(|value| !value.trim().is_empty()) {
        let exists = crate::git_exec::branch_exists(project_root, &branch, deadline)
            .await
            .map_err(|reason| {
                format!("could not confirm {project_id}'s integration branch {branch} still exists: {reason}")
            })?;
        if !exists {
            return Err(format!(
                "{project_id}'s integration branch is set to {branch}, and refs/heads/{branch} \
                 does not exist — nothing lands by default until it is corrected"
            ));
        }
        return Branch::new(&branch);
    }

    let derived = derive_integration_branch(project_root, deadline).await?;
    sqlx::query("UPDATE autopilot_state SET integration_branch = ? WHERE project_id = ?")
        .bind(&derived)
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(|error| {
            format!(
                "derived {derived} as {project_id}'s integration branch but could not record it: {error}"
            )
        })?;
    Branch::new(&derived)
}

/// PURE-ish (one repository, no database): the branch decision #2 derives when a project has never
/// declared one. `origin/HEAD` first, because it is the one answer somebody else already chose —
/// whoever ran `git remote add` or `git clone` set it. `master` and `main` after, in that order,
/// because they are the two names a repository with no remote configured is plausibly using; a
/// third name would have to be declared, not guessed.
async fn derive_integration_branch(
    project_root: &Path,
    deadline: Instant,
) -> Result<String, String> {
    if let Some(branch) = crate::git_exec::default_remote_branch(project_root, deadline).await? {
        return Ok(branch);
    }
    for candidate in ["master", "main"] {
        if crate::git_exec::branch_exists(project_root, candidate, deadline).await? {
            return Ok(candidate.to_owned());
        }
    }
    Err(format!(
        "{} has no origin/HEAD and no local master or main branch — there is nothing to derive an \
         integration branch from; set one explicitly",
        project_root.display()
    ))
}

/// The branch THIS landing is for.
///
/// **Alçada §4.1, decision #2, and it is the guard that lets that design's decision #1 exist at
/// all.** This module was written because the target used to be *inferred* from a checkout's HEAD
/// and the queue landed on a branch nobody chose about ten times. An argument is not that defect —
/// inferred is not the same as said — but a typo that happens to name a real branch would be, so a
/// named target has to be admitted before it is anything else.
///
/// The integration branch is admissible whether or not the table names it. An empty table has to
/// mean "only the usual place"; reading it as "nowhere" would break every project that never opens
/// the page.
///
/// **The order below is load-bearing, and it is the table BEFORE `integration_branch`.** A named
/// target that the project admits is answered without ever asking what the default would have
/// been, because the default is not part of that question — and asking anyway made a project whose
/// default cannot be derived (no `origin/HEAD`, no local `master` or `main`) refuse `--land
/// release` for a reason with nothing to do with what was asked. It also spends one to three `git`
/// spawns per landing on an answer the success path throws away. `declared` is needed for exactly
/// three things, all of them off that path: the `None` default, the "you named the default"
/// shortcut, and naming the alternatives in a refusal.
///
/// **The trade that order buys, stated rather than left to be found.** A project whose *recorded*
/// integration branch has since been deleted can now still land on a declared target that exists,
/// where before this function every landing was refused until the column was corrected. That is
/// the right answer — you asked for `X`, the project admits `X`, and `X` exists — but it does
/// weaken `integration_branch`'s "nothing lands until it is corrected" to "nothing lands *by
/// default* until it is corrected".
///
/// **What the refusal can and cannot claim.** `land_targets` swallows a read failure into an empty
/// `Vec` and a `tracing::warn!` — failing toward refusing, which is the direction this house wants
/// and which `project_policy` chose deliberately. The cost is that a database hiccup is
/// indistinguishable here from a project that declared nothing, so the message says what was
/// *recorded* rather than what the project *admits*: on that one bad day the branch really is
/// admitted and this function cannot know it, and a refusal that overstates its own certainty is
/// worse than one that names its evidence.
async fn resolve_target(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    requested: Option<&str>,
    deadline: Instant,
) -> Result<Branch, String> {
    let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return integration_branch(pool, project_id, project_root, deadline).await;
    };

    let recorded = crate::project_policy::land_targets(pool, project_id).await;
    if recorded.iter().any(|branch| branch == requested) {
        // Validated BEFORE it reaches git, because `declare_land_target` trims a branch name but
        // does not check it: a name `Branch::new` rejects would otherwise be told it does not
        // exist, which sends the caller looking for a missing branch when the problem is the name
        // — and spawns a git process to reach that wrong conclusion.
        let requested = Branch::new(requested)?;

        // Confirmed against the repository for `integration_branch`'s own reason: a branch declared
        // and then deleted is a misconfigured project, not a project with no target, and refusing
        // it by name beats failing three git commands deep inside a merge. Wrapped like its sibling
        // above, because a bare deadline error names neither the project nor the branch nor which
        // of this module's two `branch_exists` calls produced it.
        let exists = crate::git_exec::branch_exists(project_root, requested.as_str(), deadline)
            .await
            .map_err(|reason| {
                format!(
                    "could not confirm {project_id}'s landing target {} still exists: {reason}",
                    requested.as_str()
                )
            })?;
        if !exists {
            return Err(format!(
                "{project_id} admits {branch} as a landing target, but refs/heads/{branch} does \
                 not exist — create it or withdraw the target",
                branch = requested.as_str()
            ));
        }
        return Ok(requested);
    }

    let declared = integration_branch(pool, project_id, project_root, deadline).await?;
    if requested == declared.as_str() {
        return Ok(declared);
    }

    let mut alternatives = recorded;
    alternatives.push(declared.as_str().to_owned());
    alternatives.sort();
    alternatives.dedup();
    Err(format!(
        "{requested} is not among the landing targets recorded for {project_id} — those are: {}",
        alternatives.join(", ")
    ))
}

/// Admits a landing: `source`, into `requested_target` when the caller named one the project
/// admits, and otherwise into whatever `integration_branch` answers for `repo`.
///
/// **The only site in the core that builds a landing's `Op::Merge`.** `http.rs`'s route hands this
/// a `cwd` already resolved to a branch and a repository; this decides whether that branch is
/// admissible and, if so, submits it — through `vcs::submit_resolution` when the branch is a
/// conflict resolver's output (`resolver::landing_is_a_resolution`) and `vcs::submit` otherwise.
/// Neither constructs the `Op` itself; both take the one this function built.
///
/// **Linking a resolution to the escalation it answers happens here, at admission, not later.**
/// Decision #5: the original escalated request is still terminal the moment this runs — nothing
/// resumes it — so the only way `wait_for` can follow a session's ticket past it is a column set
/// the instant the successor exists. Best-effort: a resolution that cannot be linked still lands,
/// and the session waiting on the original ticket reads a stale `escalated` rather than the answer
/// — worse for that one wait, never wrong, which is the trade `escalated_request_id`'s own doc
/// comment makes explicit.
pub async fn submit(
    pool: &sqlx::SqlitePool,
    repo: &ResolvedRepo,
    project_root: &Path,
    source: &str,
    requested_target: Option<&str>,
    deadline: Instant,
) -> Result<i64, LandRefusal> {
    let source = Branch::new(source).map_err(LandRefusal::Refused)?;
    let target = resolve_target(
        pool,
        repo.project_id(),
        project_root,
        requested_target,
        deadline,
    )
    .await
    .map_err(LandRefusal::Refused)?;

    if source.as_str() == target.as_str() {
        return Err(LandRefusal::AlreadyOnTarget(format!(
            "this worktree is already on {}, which is where work lands",
            target.as_str()
        )));
    }

    let already_landed =
        crate::git_exec::is_ancestor(project_root, source.as_str(), target.as_str(), deadline)
            .await
            .map_err(LandRefusal::Refused)?;
    if already_landed {
        return Err(LandRefusal::NothingToLand(format!(
            "{} is already part of {} — there is nothing to land",
            source.as_str(),
            target.as_str()
        )));
    }

    let op = Op::Merge {
        source: source.clone(),
        target: target.clone(),
    };

    let from_resolution =
        crate::resolver::landing_is_a_resolution(pool, repo.project_id(), source.as_str()).await;

    let submission = if from_resolution {
        crate::vcs::submit_resolution(pool, repo, &op, Origin::Shell).await
    } else {
        crate::vcs::submit(pool, repo, &op, Origin::Shell).await
    };

    let id = match submission {
        Ok(id) => id,
        Err(error) => {
            // Decision #7: a resolver agent's own session is not a person watching for the answer
            // the way an ordinary landing's caller is, so a resolution that the agent could not get
            // admitted has to say so on its own — through the feed's waiting room, since an
            // unresolved landing is stuck work, not governance, and does not earn an immediate
            // ping. An ordinary landing's admission failure is unchanged: its caller is a live
            // session that already sees the refusal in its own response.
            if from_resolution {
                let summary = format!(
                    "{}'s conflict resolution on {} could not be admitted: {error}",
                    repo.project_id(),
                    source.as_str()
                );
                if let Err(notify_error) =
                    crate::notify::deliver_or_defer(pool, RESOLUTION_FAILED_KIND, &summary).await
                {
                    tracing::warn!(%notify_error, "land: could not notify about a failed resolution");
                }
            }
            return Err(LandRefusal::NotAdmitted(format!(
                "the request could not be admitted: {error}"
            )));
        }
    };

    if from_resolution
        && let Some(escalated_id) =
            crate::resolver::escalated_request_id(pool, repo.project_id(), source.as_str()).await
        && let Err(error) = sqlx::query("UPDATE vcs_requests SET resolved_by = ? WHERE id = ?")
            .bind(id)
            .bind(escalated_id)
            .execute(pool)
            .await
    {
        tracing::warn!(
            escalated_id,
            id,
            %error,
            "land: admitted a resolution but could not link it back to the escalation it answers"
        );
    }

    Ok(id)
}

#[cfg(test)]
mod tests {
    // A process-wide guard held across awaits on purpose: it serialises mutation of the shared
    // NUCLEOS_WORKTREE_ROOT override, and there is no multi-thread runtime here to starve. Same
    // guard and same reasoning as `git_exec::tests`, which these tests borrow their repositories
    // from.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// One row on the roster, the shape every test here needs: a project whose root is a real
    /// repository, and an integration branch either left to be derived (`None`) or declared
    /// (`Some`) so a test can isolate submission from derivation.
    async fn seed_project(
        pool: &sqlx::SqlitePool,
        project_id: &str,
        root: &Path,
        branch: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES (?, 'active', ?, ?)",
        )
        .bind(project_id)
        .bind(root.to_string_lossy().into_owned())
        .bind(branch)
        .execute(pool)
        .await
        .expect("seed the project's roster row");
    }

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn sha_of(repo: &Path, revision: &str) -> String {
        crate::git_exec::tests::sha_of(repo, revision)
    }

    fn deadline() -> Instant {
        Instant::now() + crate::git_exec::OPERATION_TIMEOUT
    }

    /// `master`, a `feat/x` one commit ahead of it, and a third branch — `parked` — the main
    /// checkout is left standing on. Every test below that does not care about the conflict shape
    /// starts here.
    fn repo_parked_off_target(prefix: &str, parked: &str) -> (tempfile::TempDir, PathBuf) {
        let container = crate::git_exec::tests::space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", parked]));
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/x"]));
        std::fs::write(repo.join("feature.txt"), "from the branch\n").expect("write");
        assert!(git_in(&repo, &["add", "-A"]));
        assert!(git_in(&repo, &["commit", "-m", "feature"]));
        assert!(git_in(&repo, &["checkout", "-q", parked]));
        (container, repo)
    }

    /// `master` and `feat/x`, each touching `seed.txt` differently, so merging one into the other
    /// conflicts. The main checkout is left on `master`.
    fn repo_with_a_conflict(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let container = crate::git_exec::tests::space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/x"]));
        std::fs::write(repo.join("seed.txt"), "theirs\n").expect("write");
        assert!(git_in(&repo, &["commit", "-am", "theirs"]));
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        std::fs::write(repo.join("seed.txt"), "ours\n").expect("write");
        assert!(git_in(&repo, &["commit", "-am", "ours"]));
        (container, repo)
    }

    /// **The regression the whole design exists to fix, and the spec's own first test.** The main
    /// checkout stands on `parked` — neither `master` nor `feat/x` — for the entire test. Before
    /// this module existed, `http.rs` read the target off exactly this checkout's HEAD, so a
    /// landing would have gone to `parked`. It has to land on `master` instead.
    #[tokio::test]
    async fn a_landing_targets_the_declared_branch_even_though_the_main_checkout_stands_elsewhere()
    {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-regression-", "chore/other");
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        let feat_sha = sha_of(&repo, "feat/x");

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let id = submit(&pool, &repo_id, &repo, "feat/x", None, deadline())
            .await
            .expect("a checkout parked elsewhere must not stop feat/x landing on master");

        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );

        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "succeeded"
        );
        assert_eq!(
            sha_of(&repo, "master^2"),
            feat_sha,
            "master's merge commit has to carry feat/x, not whatever `parked` pointed at"
        );
        let checked_out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("git should start");
        assert_eq!(
            String::from_utf8_lossy(&checked_out.stdout).trim(),
            "chore/other",
            "the main checkout itself is untouched — landing does not check anything out"
        );
    }

    /// Decision #3: a source already inside the target is refused before a row is written at all —
    /// a queued row is a promise, and "there was nothing to land" is not one worth making.
    #[tokio::test]
    async fn a_source_already_landed_is_refused_with_no_row_in_the_queue() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-ancestor-", "chore/other");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        assert!(git_in(
            &repo,
            &["merge", "--no-ff", "-m", "already landed", "feat/x"]
        ));
        seed_project(&pool, "alpha", &repo, Some("master")).await;

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let refusal = submit(&pool, &repo_id, &repo, "feat/x", None, deadline())
            .await
            .expect_err("feat/x is already part of master — there is nothing left to land");

        assert!(
            matches!(refusal, LandRefusal::NothingToLand(_)),
            "got {refusal:?}"
        );
        assert!(
            refusal.message().contains("nothing to land"),
            "{}",
            refusal.message()
        );

        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            queued, 0,
            "a refusal this early must never occupy a slot in the queue"
        );
    }

    /// Decision #2's other refusal: a project whose declared branch has since been renamed or
    /// deleted is told so by name, rather than failing three git commands deep inside a merge.
    #[tokio::test]
    async fn a_declared_branch_that_no_longer_exists_is_refused_by_name() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let container = crate::git_exec::tests::space_free_tempdir("nucleos-land-dangling-");
        let repo = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        seed_project(&pool, "alpha", &repo, Some("ramo-fantasma")).await;

        let refusal = integration_branch(&pool, "alpha", &repo, deadline())
            .await
            .expect_err("a declared branch that does not exist must not be silently re-derived");

        assert!(refusal.contains("ramo-fantasma"), "{refusal}");
        assert!(refusal.contains("alpha"), "{refusal}");
    }

    /// Alçada §4.1, decision #2. A named target has to be admitted before it is anything else --
    /// and the refusal NAMES what would have been admitted, because a caller who has to open the
    /// app to find out has lost the reason this command exists.
    #[tokio::test]
    async fn a_target_outside_the_admitted_list_is_refused_by_name() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-target-", "chore/other");
        seed_project(&pool, "alpha", &repo, Some("master")).await;
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        // No `release` branch is created, and none is needed: admission is answered before
        // `branch_exists` is ever reached. A test that created one would be asserting about a
        // branch the code never looks at.
        let refusal = submit(
            &pool,
            &repo_id,
            &repo,
            "feat/x",
            Some("release"),
            deadline(),
        )
        .await
        .unwrap_err();

        assert!(matches!(refusal, LandRefusal::Refused(_)));
        assert!(
            refusal.message().contains("release"),
            "{}",
            refusal.message()
        );
        assert!(
            refusal.message().contains("master"),
            "{}",
            refusal.message()
        );
    }

    /// Alçada §4.1: the integration branch is admissible without being in the table. An empty
    /// table means "only the usual place", never "nowhere".
    #[tokio::test]
    async fn the_integration_branch_is_admitted_without_being_declared() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-declared-", "chore/other");
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, Some("master")).await;
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        submit(&pool, &repo_id, &repo, "feat/x", Some("master"), deadline())
            .await
            .expect("naming the integration branch explicitly must be admitted");
    }

    /// **Why `resolve_target` reads the table before it asks what the default would have been.**
    /// This project's branch is `trunk`: no `origin/HEAD`, no local `master`, no local `main`, so
    /// `derive_integration_branch` has nothing to derive from and refuses. That refusal has nothing
    /// to do with the question asked — `release` is declared, `refs/heads/release` exists — and a
    /// caller who names an admitted target should never be handed it. Consulting
    /// `integration_branch` first made this landing fail with "there is nothing to derive an
    /// integration branch from"; the order is what fixes it, so the order needs a test.
    #[tokio::test]
    async fn an_admitted_target_lands_in_a_project_with_no_derivable_default() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let container = crate::git_exec::tests::space_free_tempdir("nucleos-land-trunk-");
        let repo = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "trunk"]));
        assert!(git_in(&repo, &["branch", "release", "trunk"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/x"]));
        std::fs::write(repo.join("feature.txt"), "from the branch\n").expect("write");
        assert!(git_in(&repo, &["add", "-A"]));
        assert!(git_in(&repo, &["commit", "-m", "feature"]));
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        crate::project_policy::declare_land_target(&pool, "alpha", "release")
            .await
            .unwrap();
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        let id = submit(
            &pool,
            &repo_id,
            &repo,
            "feat/x",
            Some("release"),
            deadline(),
        )
        .await
        .expect("an admitted target that exists must not need a derivable default to land");

        let args: String = sqlx::query_scalar("SELECT args FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(args.contains(r#""target":"release""#), "{args}");
    }

    /// Alçada §4.1: a target the project admits but git no longer has is refused HERE, not three
    /// git commands into a merge -- the same rule `integration_branch` already applies to a stale
    /// column.
    #[tokio::test]
    async fn an_admitted_target_whose_branch_is_gone_is_refused() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-gone-", "chore/other");
        seed_project(&pool, "alpha", &repo, Some("master")).await;
        crate::project_policy::declare_land_target(&pool, "alpha", "release")
            .await
            .unwrap();
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        let refusal = submit(
            &pool,
            &repo_id,
            &repo,
            "feat/x",
            Some("release"),
            deadline(),
        )
        .await
        .unwrap_err();

        assert!(matches!(refusal, LandRefusal::Refused(_)));
        assert!(
            refusal.message().contains("refs/heads/release"),
            "{}",
            refusal.message()
        );
    }

    /// **The test that pins the feature itself, rather than one of its refusals.** Every other test
    /// here asserts about a message, or about a column that is supposed not to move; a
    /// `resolve_target` that validated the argument and then returned the integration branch anyway
    /// — the exact defect this module exists to prevent, arriving by the new door — would pass all
    /// of them. This one reads the queued row back and asks where the merge actually goes.
    #[tokio::test]
    async fn an_admitted_target_is_what_the_queued_row_merges_into() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-pinned-", "chore/other");
        assert!(git_in(&repo, &["branch", "release", "master"]));
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, Some("master")).await;
        crate::project_policy::declare_land_target(&pool, "alpha", "release")
            .await
            .unwrap();
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        let id = submit(
            &pool,
            &repo_id,
            &repo,
            "feat/x",
            Some("release"),
            deadline(),
        )
        .await
        .expect("an admitted target that exists must be submitted");

        let args: String = sqlx::query_scalar("SELECT args FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let op: serde_json::Value = serde_json::from_str(&args).expect("the queue stores JSON");
        assert_eq!(
            op["target"], "release",
            "the row has to merge into the branch the caller named, not the project's default: \
             {args}"
        );
        assert_eq!(op["source"], "feat/x", "{args}");
    }

    /// Alçada §4.1's last line — decision #1's negative half, and the one that keeps the old defect
    /// from returning by another door: naming a target for one landing must not declare the
    /// project's default.
    ///
    /// **Seeded with no integration branch on purpose.** With a column already set, this test would
    /// be a tautology — `integration_branch` would only read back what `seed_project` wrote, and no
    /// code path could have failed it. `NULL` is the case where a write genuinely exists: the
    /// derive-and-record path in `integration_branch` is the one thing in this module that ever
    /// puts a value in that column, so a still-`NULL` column afterwards is proof the named target
    /// never went near it.
    #[tokio::test]
    async fn an_explicit_target_does_not_rewrite_the_integration_branch() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-nowrite-", "chore/other");
        assert!(git_in(&repo, &["branch", "release", "master"]));
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        crate::project_policy::declare_land_target(&pool, "alpha", "release")
            .await
            .unwrap();
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        submit(
            &pool,
            &repo_id,
            &repo,
            "feat/x",
            Some("release"),
            deadline(),
        )
        .await
        .expect("an admitted target that exists must be submitted");

        let after: Option<String> = sqlx::query_scalar(
            "SELECT integration_branch FROM autopilot_state WHERE project_id = ?",
        )
        .bind("alpha")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            after, None,
            "naming a target for one landing must not declare the project's default — this project \
             still has none"
        );
    }

    /// Decision #6: a merge the project's gate refuses is `Failed`, never `Escalated` — nothing
    /// here is a conflict, and a red gate must not mint a resolver agent to "fix" someone else's
    /// worktree. Pinned structurally: an escalated row is the only kind `resolver.rs` ever picks
    /// up, so a gate refusal that stayed `failed` is a refusal no agent will ever be started for.
    #[tokio::test]
    async fn a_red_gate_fails_the_landing_and_starts_no_agent() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-gate-", "chore/other");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        std::fs::create_dir_all(
            repo.join(crate::config::AUTOPILOT_RULES_PATH)
                .parent()
                .unwrap(),
        )
        .unwrap();
        std::fs::write(
            repo.join(crate::config::AUTOPILOT_RULES_PATH),
            "gate_before_publish: true\ngate_command: git rev-parse --verify nao-existe\n",
        )
        .unwrap();
        seed_project(&pool, "alpha", &repo, Some("master")).await;

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let id = submit(&pool, &repo_id, &repo, "feat/x", None, deadline())
            .await
            .expect("a red gate is discovered at execution, not at submission");

        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );

        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "failed"
        );
        let escalated: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests WHERE status = 'escalated'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            escalated, 0,
            "resolver.rs only ever picks up `escalated` rows — a red gate must never produce one"
        );
    }

    /// Decision #5, both halves: a conflict escalates, its resolution is admitted linked to it by
    /// `resolved_by`, and a session still waiting on the ORIGINAL ticket follows the link to the
    /// answer instead of reading a stale `escalated` and giving up.
    #[tokio::test]
    async fn a_resolution_is_linked_to_its_escalation_and_wait_follows_it() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_with_a_conflict("nucleos-land-resolution-");
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, Some("master")).await;
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        // The conflict, exactly as an ordinary landing meets it.
        let original = submit(&pool, &repo_id, &repo, "feat/x", None, deadline())
            .await
            .expect("an ordinary landing is admitted; the conflict is discovered at execution");
        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM vcs_requests WHERE id = ?")
                .bind(original)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "escalated"
        );

        // The resolver having started an agent for it — `resolver::launch_once`'s own write,
        // reproduced by hand so this test does not need a runner.
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 42 WHERE id = ?")
            .bind(original)
            .execute(&pool)
            .await
            .unwrap();

        // The agent's resolution: a real merge commit, both sides kept, no markers left.
        assert!(git_in(
            &repo,
            &["checkout", "-q", "-b", "nucleos/run-42", "master"]
        ));
        assert!(
            !git_in(&repo, &["merge", "--no-ff", "feat/x"]),
            "the merge conflicts, by design"
        );
        std::fs::write(repo.join("seed.txt"), "ours\ntheirs\n").expect("write the resolution");
        assert!(git_in(&repo, &["add", "-A"]));
        assert!(git_in(&repo, &["commit", "--no-edit"]));
        assert!(git_in(&repo, &["checkout", "-q", "master"]));

        let resolution = submit(&pool, &repo_id, &repo, "nucleos/run-42", None, deadline())
            .await
            .expect("a verified resolution is admitted");
        assert_ne!(
            resolution, original,
            "the resolution is a NEW request, not the old one reborn"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT from_resolution FROM vcs_requests WHERE id = ?")
                .bind(resolution)
                .fetch_one(&pool)
                .await
                .unwrap(),
            1,
            "a resolution's branch has to be VERIFIED before it is merged"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<i64>>(
                "SELECT resolved_by FROM vcs_requests WHERE id = ?"
            )
            .bind(original)
            .fetch_one(&pool)
            .await
            .unwrap(),
            Some(resolution),
            "the original escalation has to name its answer the moment the answer is admitted, \
             not once the answer finishes"
        );

        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM vcs_requests WHERE id = ?")
                .bind(resolution)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "succeeded"
        );

        let ticket = crate::vcs::wait_for(&pool, original, std::time::Duration::ZERO)
            .await
            .expect("the original id is still a live ticket");
        assert_eq!(
            ticket.id, original,
            "the ticket the session holds keeps its own id"
        );
        assert_eq!(
            ticket.status, "succeeded",
            "the wait follows resolved_by past the escalation to the resolution that answered it"
        );
        assert!(ticket.result_sha.is_some());
    }

    /// Decision #7: a resolver agent's own session is nobody watching for the answer, so a
    /// resolution that cannot be admitted has to say so through the feed itself. `vcs_requests` is
    /// dropped on purpose — the cheapest way to force `submit_resolution` to fail deterministically
    /// without standing up a real conflict — and it also makes `landing_is_a_resolution`'s own read
    /// error, which that function documents as failing TOWARD treating the branch as a resolution;
    /// this is what puts this landing on the path decision #7 covers rather than an ordinary one.
    #[tokio::test]
    async fn a_resolution_that_cannot_be_admitted_notifies_through_the_feed() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-notify-", "chore/other");
        assert!(git_in(
            &repo,
            &["checkout", "-q", "-b", "nucleos/run-77", "feat/x"]
        ));
        assert!(git_in(&repo, &["checkout", "-q", "chore/other"]));
        seed_project(&pool, "alpha", &repo, Some("master")).await;
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");

        sqlx::query("DROP TABLE vcs_requests")
            .execute(&pool)
            .await
            .expect("break admission so the resolution cannot be admitted");

        let refusal = submit(&pool, &repo_id, &repo, "nucleos/run-77", None, deadline())
            .await
            .expect_err("admission is broken on purpose — the resolution cannot land");

        assert!(
            matches!(refusal, LandRefusal::NotAdmitted(_)),
            "got {refusal:?}"
        );

        let summary: String = sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ?")
            .bind(RESOLUTION_FAILED_KIND)
            .fetch_one(&pool)
            .await
            .expect("a resolution that could not be admitted has to say so on the feed");
        assert!(summary.contains("nucleos/run-77"), "{summary}");
    }
}
