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

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::vcs::{Branch, Op, Origin, ResolvedRepo};

/// Decision #7's feed kind: a conflict resolution the agent could not produce, or that admission
/// otherwise refused. Named so it reads as this module's own line among `vcs_request_finished` and
/// the rest, rather than blending into them.
pub const RESOLUTION_FAILED_KIND: &str = "land_resolution_failed";

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
    // `origin/HEAD` names a branch on the REMOTE; the landing targets the local one. A clone that
    // never checked that branch out locally has no `refs/heads/<name>`, and persisting the name
    // anyway would make every later landing refuse on a branch that was never there — so it counts
    // only when the local branch exists, and otherwise the local fallbacks below answer.
    if let Some(branch) = crate::git_exec::default_remote_branch(project_root, deadline).await?
        && crate::git_exec::branch_exists(project_root, &branch, deadline).await?
    {
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

/// Where a landing with no argument would go, for somebody reading rather than landing.
///
/// **Four arms because a page that showed a branch name and called it admissible would be wrong in
/// three of them**, and the wrongness is invisible: a name on screen looks equally true whether the
/// column declared it, whether the ref still exists, and whether anything recorded it at all.
///
/// This is the answer `inspect::Branches::integration` is NOT. That field is
/// `current_branch(project_root)` — whatever the main checkout happens to be parked on — and its own
/// doc says so, naming this module's existence as the reason it may not be used for this. A project
/// whose clone is sitting on a feature branch would otherwise have the page name that branch, caption
/// it "always admissible", and be contradicted by `resolve_target` refusing it by name. That is
/// decision #2's defect wearing a different coat, and it is the whole reason this function exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IntegrationBranch {
    /// The column holds a branch and `refs/heads/<branch>` is there. Admissible, full stop.
    Declared { branch: String },
    /// The column holds a branch and the ref is gone.
    ///
    /// Its own arm rather than folded into [`IntegrationBranch::Unknown`], because
    /// [`integration_branch`] treats it as a misconfigured project and not as a project without one
    /// — *"nothing lands by default until it is corrected"* — and a reader can only correct what they
    /// can see the name of. Nothing lands here by default and the branch is still worth showing.
    Stale { branch: String },
    /// Nothing is recorded, and this is what the first landing would derive and write down.
    ///
    /// A project that has never landed is the ordinary case, not a broken one: `--land` with no
    /// argument works today and lands exactly here. Reporting `null` for it would be a fresh
    /// falsehood in the other direction — this is where the work goes, it simply has not been
    /// written down yet, and this arm says both halves.
    Derived { branch: String },
    /// There is no answer, and this is the daemon's own sentence for why.
    ///
    /// Both ways of having none: a project with no folder to ask git about, and a repository with no
    /// `origin/HEAD` and no local `master` or `main` to derive from. They are one arm because the
    /// page's response to each is the same — there is nothing to name and nothing to caption — while
    /// the sentence that differs travels in `why`.
    Unknown { why: String },
}

/// [`integration_branch`], read rather than decided — **and it does not write.**
///
/// **The one difference from [`integration_branch`], and it is the reason there are two.** That one
/// records a derived default as a side effect, so that every landing after the first reads a column
/// instead of asking git. It is right for a landing to do that and wrong for a GET: a project that
/// has never landed follows `origin/HEAD`, and a route that pinned the column would freeze it to
/// whatever `origin/HEAD` said at the moment somebody opened a page. That is decision #2's defect
/// once more — a landing target settled by something other than a declaration — with "a page was
/// opened" standing in for "a checkout was parked". A display must not decide what it is displaying.
///
/// **Both functions derive through [`derive_integration_branch`] and neither has its own copy**, so
/// the branch this reports and the branch a landing takes cannot come apart. What a reader is told
/// would happen is what happens.
///
/// `Err` is the database failing and nothing else; every answer the filesystem or git can give is an
/// arm of [`IntegrationBranch`].
pub async fn integration_branch_reading(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    deadline: Instant,
) -> Result<IntegrationBranch, String> {
    let recorded: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT integration_branch FROM autopilot_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("could not read {project_id}'s integration branch: {error}"))?
    .flatten();
    let recorded = recorded.filter(|value| !value.trim().is_empty());

    let root = crate::inspect::project_root(pool, project_id)
        .await
        .map_err(|error| format!("could not read {project_id}'s root: {error}"))?;
    let Some(root) = root.filter(|root| Path::new(root).is_dir()) else {
        // A project switched off has had its root cleared, and one whose folder moved has a root
        // pointing nowhere. Neither can be asked about a ref — but a column that still holds a name
        // is worth reporting, since it is what a landing would use once the folder is back.
        return Ok(match recorded {
            Some(branch) => IntegrationBranch::Stale { branch },
            None => IntegrationBranch::Unknown {
                why: format!(
                    "{project_id} has no folder on this machine, so there is nothing to read a \
                     branch from"
                ),
            },
        });
    };
    let root = Path::new(&root);

    if let Some(branch) = recorded {
        return Ok(
            match crate::git_exec::branch_exists(root, &branch, deadline).await {
                Ok(true) => IntegrationBranch::Declared { branch },
                // A ref git says is absent and a git that would not answer are both "this name will
                // not do", which is what `integration_branch` refuses on. The name is kept either
                // way, because it is the thing somebody has to go and correct.
                Ok(false) | Err(_) => IntegrationBranch::Stale { branch },
            },
        );
    }

    Ok(match derive_integration_branch(root, deadline).await {
        Ok(branch) => IntegrationBranch::Derived { branch },
        Err(why) => IntegrationBranch::Unknown { why },
    })
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

/// The per-branch cargo target directories a landed branch leaves behind, as bare directory names.
///
/// **Pure, and the whole of the safety argument for what may be deleted.** A branch built in its
/// own worktree gets `C:/Projects/.cargo-target-<branch>`, and once it has landed that directory
/// is dead weight of several GB. The names derived here are the only ones
/// `remove_landed_target_dirs` will ever touch: `.cargo-target-` plus the branch with `/`
/// replaced by `-`, and plus its last `/`-segment, deduplicated.
///
/// A suffix is dropped, never repaired, when it is empty, `.` or `..`, when it holds any character
/// outside `[A-Za-z0-9._-]`, or when it is one of the shared directories (`test`, `gates`, `gate`)
/// that belong to nobody's branch. A branch literally named `test` therefore derives nothing.
pub fn target_dir_names(branch: &str) -> Vec<String> {
    const PREFIX: &str = ".cargo-target-";
    const SHARED: [&str; 3] = ["test", "gates", "gate"];
    let last = branch.rsplit('/').next().unwrap_or(branch);
    let mut names: Vec<String> = Vec::new();
    for suffix in [branch.replace('/', "-"), last.to_string()] {
        let safe = !suffix.is_empty()
            && suffix != "."
            && suffix != ".."
            && suffix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !SHARED.contains(&suffix.as_str());
        if !safe {
            continue;
        }
        let name = format!("{PREFIX}{suffix}");
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `target_dir_names(branch)`, less every name some OTHER local branch also derives.
///
/// The last-segment form is shared by construction — `feat/x` and `fix/x` both derive
/// `.cargo-target-x` — and so is the full form between `feat/x` and a branch literally named
/// `feat-x`. A directory two live branches could own is neither one's to delete on landing, so it
/// is left for whoever owns it. `others` is every local branch except the landed one.
pub fn landed_target_dir_names(branch: &str, others: &[String]) -> Vec<String> {
    let claimed: std::collections::HashSet<String> = others
        .iter()
        .filter(|other| other.as_str() != branch)
        .flat_map(|other| target_dir_names(other))
        .collect();
    target_dir_names(branch)
        .into_iter()
        .filter(|name| !claimed.contains(name))
        .collect()
}

/// What happened to one candidate directory in `remove_landed_target_dirs`.
#[derive(Debug, PartialEq, Eq)]
pub enum TargetDirRemoval {
    Removed(PathBuf),
    /// Nothing there: the common case, and not worth a log line.
    Absent(PathBuf),
    /// Refused on purpose: a link, a non-directory, or a path that is not a direct child of the
    /// parent once resolved. The string is the reason.
    Skipped(PathBuf, String),
    Failed(PathBuf, String),
}

/// Remove the target directories `landed_target_dir_names(branch, others)` derives, directly under
/// `parent`.
///
/// **Blocking, and it never panics.** Every candidate is checked before anything is deleted: it
/// must exist, must not be a symlink or (on Windows) a reparse point such as a junction — so a link
/// planted at that name is never followed into somebody else's tree — must be a real directory, and
/// once both it and `parent` are canonicalised it must be a direct child of `parent` carrying
/// exactly the derived name. Anything else is `Skipped` with its reason, and a failed delete is
/// `Failed`, never an error that propagates.
pub fn remove_landed_target_dirs(
    parent: &Path,
    branch: &str,
    others: &[String],
) -> Vec<TargetDirRemoval> {
    landed_target_dir_names(branch, others)
        .into_iter()
        .map(|name| remove_one_target_dir(parent, &name))
        .collect()
}

fn remove_one_target_dir(parent: &Path, name: &str) -> TargetDirRemoval {
    let path = parent.join(name);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return TargetDirRemoval::Absent(path);
        }
        Err(error) => return TargetDirRemoval::Failed(path, format!("could not stat it: {error}")),
    };
    if meta.file_type().is_symlink() {
        return TargetDirRemoval::Skipped(path, "it is a symbolic link".to_string());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return TargetDirRemoval::Skipped(path, "it is a reparse point".to_string());
        }
    }
    if !meta.is_dir() {
        return TargetDirRemoval::Skipped(path, "it is not a directory".to_string());
    }
    let (canon_parent, canon_path) = match (parent.canonicalize(), path.canonicalize()) {
        (Ok(parent), Ok(path)) => (parent, path),
        (Err(error), _) | (_, Err(error)) => {
            return TargetDirRemoval::Failed(path, format!("could not canonicalise: {error}"));
        }
    };
    if canon_path.parent() != Some(canon_parent.as_path())
        || canon_path.file_name() != Some(std::ffi::OsStr::new(name))
    {
        return TargetDirRemoval::Skipped(
            path,
            "it does not resolve to a direct child of the parent".to_string(),
        );
    }
    match std::fs::remove_dir_all(&path) {
        Ok(()) => TargetDirRemoval::Removed(path),
        Err(error) => TargetDirRemoval::Failed(path, error.to_string()),
    }
}

/// PURE: whether a merge of `source` may clean up target directories at all, and if so every OTHER
/// local branch, for `landed_target_dir_names`. `refs` is `git_exec::branch_refs`' answer.
///
/// `None` unless `source` is a local branch (`refs/heads/<source>`) that is not also a
/// remote-tracking name: merging `origin/master` into `master` lands nobody's work-in-progress, and
/// its last segment `master` would otherwise derive `.cargo-target-master`.
pub fn cleanup_others(source: &str, refs: &[String]) -> Option<Vec<String>> {
    let local: Vec<&str> = refs
        .iter()
        .filter_map(|reference| reference.strip_prefix("refs/heads/"))
        .collect();
    let remote_tracking = refs
        .iter()
        .filter_map(|reference| reference.strip_prefix("refs/remotes/"))
        .any(|name| name == source);
    if !local.contains(&source) || remote_tracking {
        return None;
    }
    Some(
        local
            .into_iter()
            .filter(|branch| *branch != source)
            .map(str::to_owned)
            .collect(),
    )
}

/// After a landing succeeded, drop the landed branch's per-branch cargo target directory.
///
/// **Best-effort and fully detached.** This returns at once: the whole body, including the
/// lookup of the integration branch, runs on a spawned task. The lookup runs git, so nothing of
/// it may be awaited by the drain. Only a merge of a branch into the project's integration branch
/// counts; a merge into any other branch, or of the integration branch itself, removes nothing.
/// The directories live next to the project root (`<root>/..`), and the delete runs on a blocking
/// thread, so several GB of files never hold the repository's drain. Every failure is a log line
/// and nothing else: the landing already happened and is already recorded.
pub fn clean_after_landing(pool: &sqlx::SqlitePool, claimed: &crate::vcs::ClaimedRequest) {
    let pool = pool.clone();
    let claimed = claimed.clone();
    tokio::spawn(async move {
        let Op::Merge { source, target } = &claimed.op else {
            return;
        };
        let project_root = Path::new(&claimed.project_root);
        let integration = match integration_branch(
            &pool,
            &claimed.project_id,
            project_root,
            Instant::now() + crate::git_exec::OPERATION_TIMEOUT,
        )
        .await
        {
            Ok(branch) => branch,
            Err(reason) => {
                tracing::warn!(%reason, "land: could not resolve the integration branch; no target directory cleanup");
                return;
            }
        };
        if target.as_str() != integration.as_str() || source.as_str() == integration.as_str() {
            return;
        }
        let refs = match crate::git_exec::branch_refs(
            project_root,
            Instant::now() + crate::git_exec::OPERATION_TIMEOUT,
        )
        .await
        {
            Ok(refs) => refs,
            Err(reason) => {
                tracing::warn!(%reason, "land: could not list the branches; no target directory cleanup");
                return;
            }
        };
        let Some(others) = cleanup_others(source.as_str(), &refs) else {
            return;
        };
        let Some(parent) = project_root.parent() else {
            return;
        };
        let parent = parent.to_path_buf();
        let branch = source.as_str().to_string();
        drop(tokio::task::spawn_blocking(move || {
            for result in remove_landed_target_dirs(&parent, &branch, &others) {
                match result {
                    TargetDirRemoval::Removed(path) => {
                        tracing::info!(path = %path.display(), "land: removed the landed branch's target directory");
                    }
                    TargetDirRemoval::Absent(_) => {}
                    TargetDirRemoval::Skipped(path, reason) => {
                        tracing::warn!(path = %path.display(), %reason, "land: left a target directory alone");
                    }
                    TargetDirRemoval::Failed(path, reason) => {
                        tracing::warn!(path = %path.display(), %reason, "land: could not remove a target directory");
                    }
                }
            }
        }));
    });
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
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
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
        let container = crate::git_exec::testkit::space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&repo);
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
        let container = crate::git_exec::testkit::space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&repo);
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
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
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

    /// `master` (with a test map when `map_on_master` is set) and `feat/x`, which changes the map
    /// to `branch_map`, or deletes it when that is `None`. The main checkout is left on `master`.
    fn repo_changing_the_map(
        prefix: &str,
        map_on_master: Option<&str>,
        branch_map: Option<&str>,
    ) -> (tempfile::TempDir, PathBuf) {
        let container = crate::git_exec::testkit::space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        let map = repo.join(crate::tests_map::MAP_FILE);
        if let Some(text) = map_on_master {
            std::fs::write(&map, text).expect("write");
            assert!(git_in(&repo, &["add", "-A"]));
            assert!(git_in(&repo, &["commit", "-m", "map"]));
        }
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/x"]));
        match branch_map {
            Some(text) => std::fs::write(&map, text).expect("write"),
            None => std::fs::remove_file(&map).expect("remove"),
        }
        std::fs::write(repo.join("feature.txt"), "from the branch\n").expect("write");
        assert!(git_in(&repo, &["add", "-A"]));
        assert!(git_in(&repo, &["commit", "-m", "feature"]));
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        (container, repo)
    }

    async fn status_and_blobs(
        pool: &sqlx::SqlitePool,
        id: i64,
    ) -> (String, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT status, pending_map_blob, finished_at FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// Submits `feat/x` for landing and drains once: the first half of every map-approval test.
    async fn land_feat_x_once(pool: &sqlx::SqlitePool, repo: &Path) -> i64 {
        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let id = submit(pool, &repo_id, repo, "feat/x", None, deadline())
            .await
            .expect("submit the landing");
        assert!(
            crate::vcs::drain_once(pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );
        id
    }

    #[tokio::test]
    async fn a_landing_that_changes_the_test_map_waits_for_the_owner() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            repo_changing_the_map("nucleos-map-wait-", None, Some("version: 1\n"));
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-map-wait-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        let master_before = sha_of(&repo, "master");

        let id = land_feat_x_once(&pool, &repo).await;

        let (status, blob, finished_at) = status_and_blobs(&pool, id).await;
        assert_eq!(status, "awaiting_owner");
        assert_eq!(
            blob.as_deref(),
            Some(sha_of(&repo, "feat/x:nucleos.tests.yaml").as_str())
        );
        assert!(finished_at.is_none(), "a pause is not terminal");
        assert_eq!(
            sha_of(&repo, "master"),
            master_before,
            "master did not move"
        );
        assert!(
            !crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await,
            "a paused request is not claimable"
        );
    }

    #[tokio::test]
    async fn an_approved_map_change_lands() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            repo_changing_the_map("nucleos-map-approve-", None, Some("version: 1\n"));
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-map-approve-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        let id = land_feat_x_once(&pool, &repo).await;
        assert_eq!(status_and_blobs(&pool, id).await.0, "awaiting_owner");

        crate::vcs::approve_for_owner(&pool, id)
            .await
            .expect("approve");
        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );

        assert_eq!(status_and_blobs(&pool, id).await.0, "succeeded");
        assert_eq!(sha_of(&repo, "master^2"), sha_of(&repo, "feat/x"));
    }

    #[tokio::test]
    async fn a_refused_map_change_never_lands() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            repo_changing_the_map("nucleos-map-refuse-", None, Some("version: 1\n"));
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-map-refuse-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        let master_before = sha_of(&repo, "master");
        let id = land_feat_x_once(&pool, &repo).await;

        crate::vcs::refuse_for_owner(&pool, id)
            .await
            .expect("refuse");

        assert_eq!(status_and_blobs(&pool, id).await.0, "rejected");
        assert!(
            !crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );
        assert_eq!(sha_of(&repo, "master"), master_before);
    }

    #[tokio::test]
    async fn a_map_edited_after_approval_waits_again() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            repo_changing_the_map("nucleos-map-edited-", None, Some("version: 1\n"));
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-map-edited-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        let master_before = sha_of(&repo, "master");
        let id = land_feat_x_once(&pool, &repo).await;
        let first_blob = status_and_blobs(&pool, id).await.1.expect("a pending blob");
        crate::vcs::approve_for_owner(&pool, id)
            .await
            .expect("approve");

        assert!(git_in(&repo, &["checkout", "-q", "feat/x"]));
        std::fs::write(
            repo.join(crate::tests_map::MAP_FILE),
            "version: 1\n# edited\n",
        )
        .expect("write");
        assert!(git_in(&repo, &["commit", "-am", "edit the map"]));
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );

        let (status, blob, _) = status_and_blobs(&pool, id).await;
        assert_eq!(status, "awaiting_owner");
        let blob = blob.expect("the new pending blob");
        assert_ne!(blob, first_blob, "the pause names the NEW content");
        assert_eq!(blob, sha_of(&repo, "feat/x:nucleos.tests.yaml"));
        assert_eq!(sha_of(&repo, "master"), master_before);
    }

    #[tokio::test]
    async fn deleting_the_test_map_waits_for_the_owner_too() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            repo_changing_the_map("nucleos-map-delete-", Some("version: 1\n"), None);
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-map-delete-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;

        let id = land_feat_x_once(&pool, &repo).await;

        let (status, blob, _) = status_and_blobs(&pool, id).await;
        assert_eq!(status, "awaiting_owner");
        assert_eq!(blob.as_deref(), Some(crate::git_exec::DELETED_MAP));
    }

    #[tokio::test]
    async fn a_landing_that_leaves_the_map_alone_does_not_wait() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_changing_the_map(
            "nucleos-map-same-",
            Some("version: 1\n"),
            Some("version: 1\n"),
        );
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-map-same-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;

        let id = land_feat_x_once(&pool, &repo).await;

        assert_eq!(status_and_blobs(&pool, id).await.0, "succeeded");
    }

    /// The guard asks only when the merge publishes into the integration branch. Master merged into
    /// an agent's branch carries a map the owner already approved.
    #[tokio::test]
    async fn merging_master_into_an_agent_branch_does_not_wait() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let container = crate::git_exec::testkit::space_free_tempdir("nucleos-map-agent-");
        let repo = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/x"]));
        std::fs::write(repo.join("feature.txt"), "from the branch\n").expect("write");
        assert!(git_in(&repo, &["add", "-A"]));
        assert!(git_in(&repo, &["commit", "-m", "feature"]));
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        std::fs::write(repo.join(crate::tests_map::MAP_FILE), "version: 1\n").expect("write");
        assert!(git_in(&repo, &["add", "-A"]));
        assert!(git_in(&repo, &["commit", "-m", "map on master"]));
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-map-agent-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, Some("master")).await;

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let op = Op::Merge {
            source: "master".into(),
            target: "feat/x".into(),
        };
        let id = crate::vcs::submit(&pool, &repo_id, &op, Origin::Human)
            .await
            .unwrap();
        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );

        assert_eq!(status_and_blobs(&pool, id).await.0, "succeeded");
    }

    /// **The same defect, reached by a delete.** The queue landed `feat/x` on `master` while the main
    /// checkout stood on `chore/other`, and then refused to delete it: `git branch --delete` asked
    /// the parked HEAD, which never saw the landing. Through `drain_once`, because what is being
    /// proved is that the claim carries the integration branch to the executor.
    #[tokio::test]
    async fn a_landed_branch_is_deleted_though_the_main_checkout_stands_elsewhere() {
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-delete-", "chore/other");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        assert!(git_in(
            &repo,
            &["merge", "--no-ff", "-m", "land feat/x", "feat/x"]
        ));
        assert!(git_in(&repo, &["checkout", "-q", "chore/other"]));
        seed_project(&pool, "alpha", &repo, Some("master")).await;

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let op = Op::BranchDelete {
            branch: "feat/x".into(),
        };
        let id = crate::vcs::submit(&pool, &repo_id, &op, Origin::Human)
            .await
            .unwrap();
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
        assert!(
            !git_in(&repo, &["rev-parse", "--verify", "-q", "refs/heads/feat/x"]),
            "feat/x is gone"
        );
    }

    /// **The same regression as the test above, read instead of landed.**
    ///
    /// The panel at `/projects/{id}/github` reported this branch from
    /// `inspect::Branches::integration` — `current_branch(project_root)` — and captioned it *"always
    /// admissible"*. With the main checkout parked on `chore/other` that named a branch
    /// `resolve_target` refuses by name, while the branch that IS admissible appeared nowhere. A
    /// landing being right is not enough if the page that reports on landings is wrong: the owner
    /// reads the page.
    ///
    /// Both halves are asserted, because the first alone would pass on a reading that named both.
    #[tokio::test]
    async fn a_reading_names_the_declared_branch_and_never_the_parked_checkout() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-reading-", "chore/other");
        seed_project(&pool, "alpha", &repo, Some("master")).await;

        // What the old source of this answer would have said, measured rather than assumed.
        assert_eq!(
            crate::git_exec::current_branch(&repo, deadline())
                .await
                .ok(),
            Some("chore/other".to_owned()),
            "the fixture has to have the checkout parked, or this test proves nothing"
        );

        assert_eq!(
            integration_branch_reading(&pool, "alpha", deadline())
                .await
                .expect("the roster row is there"),
            IntegrationBranch::Declared {
                branch: "master".to_owned()
            }
        );
    }

    /// **A read does not write, and this is the difference between the two functions.**
    ///
    /// [`integration_branch`] records a derived default so that every landing after the first reads
    /// a column. That is right for a landing and wrong for a GET: `GET /projects/{id}/land-targets`
    /// is opened by looking at a page, and a project that has never landed follows `origin/HEAD`
    /// until something pins it. A page that pinned it would settle a landing target by a means that
    /// is not a declaration — decision #2's defect with "a page was opened" in place of "a checkout
    /// was parked".
    ///
    /// So: reading derives the same answer and leaves the column alone; landing writes it. Both
    /// derive through `derive_integration_branch`, so the branch reported and the branch taken
    /// cannot come apart — which is the other half of why this is safe.
    #[tokio::test]
    async fn reading_the_integration_branch_derives_without_recording_it() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-noswrite-", "chore/other");
        seed_project(&pool, "alpha", &repo, None).await;

        let recorded = || {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT integration_branch FROM autopilot_state WHERE project_id = 'alpha'",
            )
            .fetch_one(&pool)
        };

        assert_eq!(
            integration_branch_reading(&pool, "alpha", deadline())
                .await
                .expect("the roster row is there"),
            IntegrationBranch::Derived {
                branch: "master".to_owned()
            },
            "a project that has never landed still lands somewhere, and this is where"
        );
        assert_eq!(
            recorded().await.unwrap(),
            None,
            "reading must leave the column exactly as it found it"
        );

        // And the landing path does record it, which is what makes the pair a decision rather than
        // an inconsistency.
        integration_branch(&pool, "alpha", &repo, deadline())
            .await
            .expect("master is derivable here");
        assert_eq!(recorded().await.unwrap(), Some("master".to_owned()));
    }

    /// A column naming a branch git cannot find is a misconfigured project, not one without a
    /// default — and the name is what somebody has to go and correct, so it is kept.
    ///
    /// [`integration_branch`] refuses this case in words: *"nothing lands by default until it is
    /// corrected"*. The reading says the same thing in a shape a page can caption, which is the
    /// point of the arm existing at all: a branch name on screen is a claim, and this arm is where
    /// the claim must not be "always admissible".
    #[tokio::test]
    async fn a_declared_branch_whose_ref_is_gone_is_named_and_not_called_admissible() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) = repo_parked_off_target("nucleos-land-stale-", "chore/other");
        seed_project(&pool, "alpha", &repo, Some("release/gone")).await;

        assert_eq!(
            integration_branch_reading(&pool, "alpha", deadline())
                .await
                .expect("the roster row is there"),
            IntegrationBranch::Stale {
                branch: "release/gone".to_owned()
            }
        );

        // The landing path refuses the same project, which is the behaviour the arm is describing.
        assert!(
            integration_branch(&pool, "alpha", &repo, deadline())
                .await
                .is_err(),
            "a page must not caption a branch admissible that a landing would refuse"
        );
    }

    /// A project with no folder has nothing to be asked, and says so in the daemon's own words.
    #[tokio::test]
    async fn a_project_with_no_folder_has_no_integration_branch_to_name() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('off', 'off', NULL)",
        )
        .execute(&pool)
        .await
        .expect("seed a project switched off");

        let read = integration_branch_reading(&pool, "off", deadline())
            .await
            .expect("the roster row is there");
        let IntegrationBranch::Unknown { why } = read else {
            panic!("a project with no folder cannot name a branch, got {read:?}");
        };
        assert!(why.contains("no folder"), "{why}");
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
        let container = crate::git_exec::testkit::space_free_tempdir("nucleos-land-dangling-");
        let repo = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&repo);
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
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
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
        let container = crate::git_exec::testkit::space_free_tempdir("nucleos-land-trunk-");
        let repo = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "trunk"]));
        assert!(git_in(&repo, &["branch", "release", "trunk"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/x"]));
        std::fs::write(repo.join("feature.txt"), "from the branch\n").expect("write");
        assert!(git_in(&repo, &["add", "-A"]));
        assert!(git_in(&repo, &["commit", "-m", "feature"]));
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
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
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
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
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
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
        let (container, repo) = repo_parked_off_target("nucleos-land-gate-", "chore/other");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        // Where the queue reads `alpha`'s rules: a temporary stand-in for `~/.nucleos`.
        let machine_root = container.path().join("nucleos-home");
        let rules = crate::project_state::file(
            Some(&machine_root),
            "alpha",
            crate::project_state::AUTOPILOT_FILE,
        )
        .unwrap();
        std::fs::create_dir_all(rules.parent().unwrap()).unwrap();
        std::fs::write(
            &rules,
            "gate_before_publish: true\ngate_command: git rev-parse --verify nao-existe\n",
        )
        .unwrap();
        seed_project(&pool, "alpha", &repo, Some("master")).await;

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let id = submit(&pool, &repo_id, &repo, "feat/x", None, deadline())
            .await
            .expect("a red gate is discovered at execution, not at submission");

        let executor = crate::git_exec::GitExecutor {
            machine_root: Some(machine_root),
            ..crate::git_exec::GitExecutor::default()
        };
        assert!(crate::vcs::drain_once(&pool, "alpha", &executor).await);

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
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
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

    /// `feat/x` and `fix/x` both derive `.cargo-target-x`; landing one must not take the other's.
    #[test]
    fn a_target_dir_name_another_branch_derives_is_not_the_landed_branchs_to_delete() {
        assert_eq!(
            landed_target_dir_names("feat/x", &[]),
            vec![".cargo-target-feat-x", ".cargo-target-x"]
        );
        assert_eq!(
            landed_target_dir_names("feat/x", &["fix/x".to_owned(), "master".to_owned()]),
            vec![".cargo-target-feat-x"]
        );
        // The full form collides too, with a branch literally spelled with a dash.
        assert_eq!(
            landed_target_dir_names("feat/x", &["feat-x".to_owned()]),
            vec![".cargo-target-x"]
        );

        let parent = tempfile::tempdir().expect("tempdir");
        for name in [".cargo-target-feat-x", ".cargo-target-x"] {
            std::fs::create_dir(parent.path().join(name)).expect("create target dir");
        }
        remove_landed_target_dirs(parent.path(), "feat/x", &["fix/x".to_owned()]);
        assert!(!parent.path().join(".cargo-target-feat-x").exists());
        assert!(
            parent.path().join(".cargo-target-x").exists(),
            "landing feat/x deleted the directory fix/x also derives"
        );
    }

    /// Only a local branch that is not a remote-tracking name may trigger a cleanup: merging
    /// `origin/master` would otherwise derive `.cargo-target-master`.
    #[test]
    fn a_merge_of_a_remote_tracking_branch_cleans_up_nothing() {
        let refs: Vec<String> = [
            "refs/heads/master",
            "refs/heads/feat/x",
            "refs/heads/fix/x",
            "refs/remotes/origin/master",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(cleanup_others("origin/master", &refs), None);
        assert_eq!(cleanup_others("gone/branch", &refs), None);
        assert_eq!(
            cleanup_others("feat/x", &refs),
            Some(vec!["master".to_owned(), "fix/x".to_owned()])
        );
    }

    /// `origin/HEAD` names a remote branch; one with no local counterpart must not be persisted as
    /// the integration branch, or every landing refuses on a branch that never existed here.
    #[tokio::test]
    async fn an_origin_head_without_a_local_branch_falls_back_to_master() {
        let container = crate::git_exec::testkit::space_free_tempdir("nucleos-land-originhead-");
        let repo = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        assert!(git_in(
            &repo,
            &["update-ref", "refs/remotes/origin/trunk", "HEAD"]
        ));
        assert!(git_in(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk"
            ]
        ));

        assert_eq!(
            derive_integration_branch(&repo, deadline()).await,
            Ok("master".to_owned())
        );

        // The control: once the local branch exists, origin/HEAD wins as before.
        assert!(git_in(&repo, &["branch", "trunk"]));
        assert_eq!(
            derive_integration_branch(&repo, deadline()).await,
            Ok("trunk".to_owned())
        );
    }

    #[test]
    fn target_dir_names_follow_the_branch() {
        assert_eq!(
            target_dir_names("feat/x"),
            vec![".cargo-target-feat-x", ".cargo-target-x"]
        );
        assert_eq!(
            target_dir_names("land-fix"),
            vec![".cargo-target-land-fix"],
            "a branch with no slash derives one name, not two copies of it"
        );
        assert_eq!(
            target_dir_names("a/b/c.d_e"),
            vec![".cargo-target-a-b-c.d_e", ".cargo-target-c.d_e"]
        );
    }

    #[test]
    fn reserved_and_unsafe_branch_names_derive_no_target_dir() {
        for branch in ["test", "gates", "gate", "", ".", "..", "feat/.."] {
            let names = target_dir_names(branch);
            for reserved in ["test", "gates", "gate", "", ".", ".."] {
                assert!(
                    !names.contains(&format!(".cargo-target-{reserved}")),
                    "{branch:?} derived {names:?}"
                );
            }
        }
        assert!(target_dir_names("test").is_empty());
        // The slash-joined form is fine, only the reserved last segment is dropped.
        assert_eq!(
            target_dir_names("feat/test"),
            vec![".cargo-target-feat-test"]
        );
        assert!(target_dir_names("feat/a b").len() <= 1);
        for branch in ["feat/a b", "feat/é", "feat/a:b", "a\\b", "x$y"] {
            for name in target_dir_names(branch) {
                assert!(
                    name.trim_start_matches(".cargo-target-")
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                    "{branch:?} derived the unsafe {name:?}"
                );
            }
        }
        assert!(target_dir_names("x$y").is_empty());
    }

    #[test]
    fn only_the_derived_target_dir_is_removed() {
        let parent = crate::git_exec::testkit::space_free_tempdir("nucleos-target-rm-");
        let derived = parent.path().join(".cargo-target-feat-x");
        let other = parent.path().join(".cargo-target-other");
        let shared = parent.path().join(".cargo-target-test");
        let plain = parent.path().join("feat-x");
        for dir in [&derived, &other, &shared, &plain] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("sentinel"), "x").unwrap();
        }

        let results = remove_landed_target_dirs(parent.path(), "feat/x", &[]);

        assert!(
            results
                .iter()
                .any(|r| matches!(r, TargetDirRemoval::Removed(p) if p == &derived)),
            "{results:?}"
        );
        assert!(!derived.exists());
        for kept in [&other, &shared, &plain] {
            assert!(kept.join("sentinel").exists(), "{kept:?} must survive");
        }
    }

    #[test]
    fn an_absent_target_dir_is_a_quiet_no_op() {
        let parent = crate::git_exec::testkit::space_free_tempdir("nucleos-target-absent-");
        let results = remove_landed_target_dirs(parent.path(), "feat/x", &[]);
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|r| matches!(r, TargetDirRemoval::Absent(_))),
            "{results:?}"
        );
        // A plain file at the derived name is refused, not deleted.
        let file = parent.path().join(".cargo-target-feat-x");
        std::fs::write(&file, "not a directory").unwrap();
        let results = remove_landed_target_dirs(parent.path(), "feat/x", &[]);
        assert!(
            results
                .iter()
                .any(|r| matches!(r, TargetDirRemoval::Skipped(p, _) if p == &file)),
            "{results:?}"
        );
        assert!(file.exists());
    }

    #[test]
    fn a_linked_target_dir_is_never_followed() {
        let parent = crate::git_exec::testkit::space_free_tempdir("nucleos-target-link-");
        let elsewhere = parent.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("sentinel"), "precious").unwrap();
        let link = parent.path().join(".cargo-target-feat-x");

        #[cfg(windows)]
        {
            let status = Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&elsewhere)
                .output()
                .expect("cmd should start");
            assert!(status.status.success(), "mklink /J failed: {status:?}");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, &link).expect("symlink");

        let results = remove_landed_target_dirs(parent.path(), "feat/x", &[]);

        assert!(
            results
                .iter()
                .any(|r| matches!(r, TargetDirRemoval::Skipped(p, _) if p == &link)),
            "{results:?}"
        );
        assert!(
            elsewhere.join("sentinel").exists(),
            "the link's target must survive untouched"
        );
    }

    async fn wait_until_gone(path: &Path) -> bool {
        for _ in 0..300 {
            if !path.exists() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        !path.exists()
    }

    // The cleanup is detached (the lookup and the delete both run on a spawned task), so this
    // polls for the removal instead of expecting it when the call returns.
    #[tokio::test]
    async fn a_successful_landing_removes_its_branch_target_dir() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (container, repo) = repo_parked_off_target("nucleos-land-cleanup-", "chore/other");
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        let target_dir = container.path().join(".cargo-target-x");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("sentinel"), "x").unwrap();

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let id = submit(&pool, &repo_id, &repo, "feat/x", None, deadline())
            .await
            .expect("the landing is admitted");
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
        assert!(
            wait_until_gone(&target_dir).await,
            "the landed branch's target directory should have been removed"
        );
    }

    #[tokio::test]
    async fn a_merge_into_a_worktree_branch_removes_no_target_dir() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (container, repo) = repo_parked_off_target("nucleos-land-nocleanup-", "chore/other");
        let roots = crate::git_exec::testkit::space_free_tempdir("nucleos-land-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());
        seed_project(&pool, "alpha", &repo, None).await;
        let target_dir = container.path().join(".cargo-target-master");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("sentinel"), "x").unwrap();

        let repo_id = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let op = Op::Merge {
            source: Branch::new("master").unwrap(),
            target: Branch::new("feat/x").unwrap(),
        };
        crate::vcs::submit(&pool, &repo_id, &op, Origin::Shell)
            .await
            .expect("the merge is admitted");
        assert!(
            crate::vcs::drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await
        );

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(
            target_dir.join("sentinel").exists(),
            "merging INTO a feature branch must never clean the source's target directory"
        );
    }
}
