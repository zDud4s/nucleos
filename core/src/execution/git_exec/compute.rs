//! Computing a merge or a revert in the integration worktree, and the line-ending note that explains a conflict.

use super::*;

/// The prefix of the daemon's own worktree directory inside a project's worktree root.
///
/// Named, and `pub`, because `worktree.rs`'s orphan sweeper must be provably unable to see it: spec
/// §7.1 established that `owner_from_dir_name` only claims `run-<id>` and `job-<id>`, so this name is
/// invisible to the GC *by construction* — which is an accident of a parser until a test pins it.
/// Task 8 writes that test, against `integration_worktree` itself rather than against this string.
pub const INTEGRATION_PREFIX: &str = "integration-";

/// Where this project's integration worktree lives.
///
/// **Qualified by the project's directory name, and that is not decoration.**
/// `worktree::worktree_root` returns `NUCLEOS_WORKTREE_ROOT` *verbatim when it is set*, ignoring
/// `project_root` entirely (`worktree.rs:25-32`) — and the daemon's own error messages tell users to
/// set it (`worktree.rs:107-111`). One unqualified `integration` directory would then be shared by
/// every project on the machine — and sharing it is worse than refusing it, because the sharing is
/// silent: the second project finds a directory that exists and is a perfectly valid worktree, so
/// nothing declines. Run worktrees do not have this problem, because run and job ids are already
/// globally unique; a fixed name is not.
///
/// Two projects whose directory *leaf names* match still collide under that override, and the way
/// that fails is worth stating exactly rather than hand-waving: the second project finds the
/// directory already there, takes the reset path, and computes inside the **first** project's
/// worktree and object database. Nothing is published — `publish` reads the target ref from the
/// second project's root and finds a sha the first project's merge never saw — but the failure
/// arrives late, wearing a message about the branch having moved. Left unfixed here and carried
/// forward; the fix, when it is wanted, is a name derived from the whole project path rather than
/// its last component.
pub fn integration_worktree(project_root: &Path) -> std::path::PathBuf {
    let project = project_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_owned());
    crate::execution::worktree::worktree_root(project_root)
        .join(format!("{INTEGRATION_PREFIX}{project}"))
}

/// A merge that exists as an object and has not been published.
///
/// `Debug` because the tests `expect_err` on a `Result<Computed, Outcome>`, and `expect_err` is
/// bounded on the *success* type being printable.
#[derive(Debug)]
pub struct Computed {
    /// Where the target branch was when the merge was computed against it. The compare-and-swap
    /// value: publishing is only safe if the branch is still here.
    pub old: String,
    pub new: String,
    /// What the merge printed, for the row to record. `publish` moves it into the `Outcome`, which
    /// is the reader Task 3 wrote a `cfg_attr(test, expect(dead_code))` here to wait for — the
    /// `expect` was written to delete itself at exactly this task, and did.
    pub output_tail: String,
}

/// What `pending_map_blob` holds when the merge deletes the test map: there is no blob to name,
/// and deleting the map is still a change the owner approves (spec 2026-10-05 §3.4).
pub const DELETED_MAP: &str = "deleted";

/// Whether `computed` changes the test map, and if so the blob id the target would carry (or
/// `DELETED_MAP`). Diffs `old..new`, the merge commit against the target it would replace, so it
/// sees the map however it reached the source branch: an edit, a checkout from another ref, a
/// cherry-pick, a merge.
pub(super) async fn map_change(
    project_root: &Path,
    computed: &Computed,
    deadline: std::time::Instant,
) -> Result<Option<String>, Outcome> {
    let map = crate::tests_map::MAP_FILE;
    let changed = git(
        project_root,
        &[
            "diff",
            "--name-only",
            &computed.old,
            &computed.new,
            "--",
            map,
        ],
        deadline,
    )
    .await?;
    if changed.exit_code != Some(0) {
        return Err(Outcome::Failed {
            reason: format!("could not tell whether this merge changes {map}"),
            exit_code: changed.exit_code,
            output_tail: changed.output_tail,
        });
    }
    if changed.stdout.trim().is_empty() {
        return Ok(None);
    }
    let spec = format!("{}:{map}", computed.new);
    let blob = git(
        project_root,
        &["rev-parse", "--verify", "--quiet", &spec],
        deadline,
    )
    .await?;
    Ok(Some(if blob.exit_code == Some(0) {
        blob.stdout.trim().to_owned()
    } else {
        DELETED_MAP.to_owned()
    }))
}

/// Whether this merge publishes into the project's integration branch, the target the test map
/// governs. Unknown (the branch could not be resolved) counts as yes: the safe side of a guard
/// is the side that asks.
pub(super) fn lands_on_the_integration_branch(
    request: &crate::execution::vcs::ClaimedRequest,
    target: &crate::execution::vcs::Branch,
) -> bool {
    request
        .integration_branch
        .as_ref()
        .is_none_or(|branch| branch.as_str() == target.as_str())
}

/// Computes the revert of `merge_sha` on top of `target`, on a detached HEAD in the integration
/// worktree (spec 2026-10-05 §6.2, D4).
///
/// `-m 1`: the queue's merges are always `--no-ff`, so the first parent is the line the merge
/// landed on. A commit that is not a merge makes git refuse, and that refusal is the outcome. The
/// result is a new commit whose first parent is the old tip of `target`, so `publish` treats it
/// exactly like a merge. A commit that is not on `target` is refused before anything is touched,
/// and a conflicting revert is aborted and reported as a failure — never escalated, because the
/// resolver only knows how to resolve merges.
pub async fn compute_revert(
    project_root: &Path,
    merge_sha: &str,
    target: &str,
    deadline: std::time::Instant,
) -> Result<Computed, Outcome> {
    let integration = prepare_integration_worktree(project_root, deadline).await?;

    let checkout = git(&integration, &["checkout", "--detach", target], deadline).await?;
    if !checkout.succeeded() {
        return Err(failed(
            format!("could not check out {target} to revert on"),
            &checkout,
        ));
    }

    let old = revision(&integration, "HEAD", deadline).await?;

    let on_target = git(
        &integration,
        &["merge-base", "--is-ancestor", merge_sha, "HEAD"],
        deadline,
    )
    .await?;
    if !on_target.succeeded() {
        return Err(failed(
            format!("{merge_sha} is not on {target}, nothing was reverted"),
            &on_target,
        ));
    }

    let revert = git(
        &integration,
        &[
            "revert",
            "-m",
            "1",
            "--no-edit",
            "--end-of-options",
            merge_sha,
        ],
        deadline,
    )
    .await?;
    if !revert.succeeded() {
        // Best-effort: the next operation resets this worktree anyway, and the abort must not
        // replace git's own refusal, which is what the row records.
        let _ = git(&integration, &["revert", "--abort"], deadline).await;
        return Err(failed(
            format!("reverting {merge_sha} on {target} failed, nothing was reverted"),
            &revert,
        ));
    }

    let new = revision(&integration, "HEAD", deadline).await?;
    Ok(Computed {
        old,
        new,
        output_tail: revert.output_tail,
    })
}

/// Computes `source` into `target` on a detached HEAD in the daemon's integration worktree.
///
/// `Err` is the outcome to record, not an error to propagate: a conflict is the answer to the
/// question that was asked, and every failure of a compute *is* how that request ended. The single
/// call site would map any other type onto an `Outcome` immediately.
///
/// `--no-ff` always, including when a fast-forward would do. The merge commit's first parent is then
/// always the old target, which is exactly what makes every publish a fast-forward (spec §6.3) —
/// including the case that looks backwards, merging `master` into an agent's own branch, where the
/// branch's own tip is the first parent and the agent's worktree can therefore be fast-forwarded to
/// it.
pub async fn compute_merge(
    project_root: &Path,
    source: &str,
    target: &str,
    deadline: std::time::Instant,
) -> Result<Computed, Outcome> {
    let integration = prepare_integration_worktree(project_root, deadline).await?;

    // Detach rather than checkout: `--detach` is permitted even when `target` is checked out in
    // another worktree, which is the ordinary case — the user is standing on it.
    let checkout = git(&integration, &["checkout", "--detach", target], deadline).await?;
    if !checkout.succeeded() {
        return Err(failed(
            format!("could not check out {target} to merge into"),
            &checkout,
        ));
    }

    let old = revision(&integration, "HEAD", deadline).await?;

    let merge = git(
        &integration,
        &[
            "merge",
            "--no-ff",
            "-m",
            &format!("merge {source} into {target}"),
            source,
        ],
        deadline,
    )
    .await?;
    if !merge.succeeded() {
        // **The conflicted index is read HERE, before the abort**, because the abort is what
        // destroys it and both of the questions below are asked of it: whether anything conflicted
        // at all, and whether the two sides of what did conflict even agree about line endings. One
        // line per stage, `<mode> SP <sha> SP <stage> TAB <path>`.
        let unmerged = match git(&integration, &["ls-files", "--unmerged"], deadline).await {
            Ok(listed) if listed.succeeded() => Some(listed.stdout),
            // Could not look. Neither of the two things it decides can be decided, and both fall
            // back to what this arm did before either of them existed: escalate, and say only what
            // git said.
            _ => None,
        };
        let note = match unmerged.as_deref() {
            Some(listing) => endings_note(&integration, listing, source, target, deadline).await,
            None => String::new(),
        };
        // Best-effort: the next operation resets this worktree anyway, and a failure to abort must
        // not replace the conflict — the conflict is what the caller needs to read.
        let _ = git(&integration, &["merge", "--abort"], deadline).await;

        // **A merge that never started is not a conflict, and confusing the two costs an agent.**
        // Every non-zero exit used to arrive as `Escalated`, and escalating is a promise that a
        // resolution can be minted for the row — so "not something we can merge" got one. Measured:
        // request 91 on this repository asked to merge a branch named `feat`, which does not exist
        // here. Git printed `merge: feat - not something we can merge`, the row escalated,
        // `resolver.rs` minted run 900436 for it inside the minute, and that run failed three
        // seconds later with nothing staged to resolve. What the row asks now is for a person to
        // look at a conflict that never existed, and it cannot even be retried: its
        // `resolution_run_id` is spent.
        //
        // Two signals, because either alone has a failure mode. The empty unmerged index is the
        // authority; the absence of the word git prints for a conflict guards the shape nobody has
        // met yet — a conflict that somehow left no stage entries would otherwise be demoted to a
        // failure and lose the resolver it deserves.
        if unmerged
            .as_deref()
            .is_some_and(|listing| listing.trim().is_empty())
            && !merge.output_tail.contains("CONFLICT")
        {
            return Err(Outcome::Failed {
                reason: format!(
                    "merging {source} into {target} did not happen at all: git refused the merge \
                     before starting it, and nothing anywhere is left conflicted. It is the \
                     request that is wrong rather than the branches — most often a name no branch \
                     has — and what git printed says which."
                ),
                exit_code: merge.exit_code,
                output_tail: merge.output_tail.clone(),
            });
        }

        // **The reason names the owner, because the asker's instinct is to fix it.** An agent that
        // has just finished work and is told "merging failed" will reach for the conflict, and it
        // is the one thing here that is not its to reach for: the merge happened in an integration
        // worktree it does not have, was aborted, and left nothing conflicted anywhere. There is no
        // conflicted state in its copy to resolve — only the temptation to manufacture one.
        //
        // **What it must NOT do is send them at the other direction, which is what it used to.** It
        // told the asker to bring the target into its own branch, resolve there, and ask again —
        // and that is unactionable twice over. If A into B conflicts then B into A is the same
        // conflict, so the reverse merge meets the same wall; and every `git merge` from a session
        // is taken over by the queue and computed HERE, in the same integration worktree, and
        // aborted the same way. So the advice named a remedy that could not be performed, and the
        // asker who tries it gets a second escalation naming the first one's inverse. Measured: it
        // cost one session two hours and three escalated rows before it built the two-parent commit
        // by hand — the very thing `verify_resolution` says a resolver arrives already holding.
        //
        // What it says instead is what actually happens, because a resolution IS started for them:
        // `resolver.rs` picks this row up within a minute and hands the conflict to an agent in a
        // worktree of its own, staged, with MERGE_HEAD set. Nothing here is the asker's to do.
        return Err(Outcome::Escalated {
            reason: format!(
                "merging {source} into {target} conflicts, so nothing was published and no copy \
                 was left conflicted. This is the queue's to carry and not yours to fix from \
                 here — the merge ran in an integration worktree you do not have, and there is no \
                 conflicted state in your copy to resolve. What happens next is not yours to start \
                 either: a resolution is minted for this row within a minute, in a worktree of its \
                 own with the conflict already staged, and if one cannot be started then this row \
                 is what asks a person to look. Do not re-run the merge, and do not merge the \
                 other direction to get at the conflict — it is the same conflict, and it is \
                 computed here too."
            ),
            exit_code: merge.exit_code,
            // The note rides with git's own output rather than with the reason, because the tail is
            // the half that reaches the reader who can act on it: `resolver.rs` hands it to the
            // resolution agent as "git's own account of the conflict", and the row shows it to
            // whoever asked. Empty unless there is something to say.
            output_tail: format!("{}{note}", merge.output_tail),
        });
    }

    let new = revision(&integration, "HEAD", deadline).await?;
    Ok(Computed {
        old,
        new,
        output_tail: merge.output_tail,
    })
}

/// What a conflict's two sides do about line endings, when they do not do the same thing.
///
/// **This is the one thing about a conflict that reading the conflict cannot tell you.** Sides that
/// disagree about endings have no aligned line anywhere, so a three-way merge produces one conflict
/// running from the first line to the last whatever the real change was, and the markers then carry
/// no information at all. Measured, at length: `shell/src/preview/daemon.ts` conflicted end to end
/// on a merge whose actual change was three lines. One session spent most of a day on it and two
/// resolution agents were defeated by it before the cause was found, and the cause was one byte — a
/// NUL in the file, which makes git classify it as binary, which means the `core.autocrlf` filter
/// never ran on it, which left its blob CRLF while every sibling in the checkout was LF.
///
/// The queue holds both blobs at the moment it gives up, and nobody downstream of it does. So it
/// says what they are, in the tail the row keeps and the resolution prompt quotes.
///
/// Silent unless the sides actually differ, because a note that appears every time means nothing.
/// Silent too when a side is binary in earnest and the endings are merely not identical: newline
/// counts in a PNG are an accident of its bytes, and reporting them would be noise inside an
/// escalation. The escape from that guard is two PURE and opposite answers — all CRLF against all
/// LF — which is what a text file with a NUL in it looks like and what random bytes essentially
/// never do.
pub(super) async fn endings_note(
    repo: &Path,
    unmerged: &str,
    source: &str,
    target: &str,
    deadline: std::time::Instant,
) -> String {
    let mut said = Vec::new();
    for (path, ours, theirs) in sides(unmerged).into_iter().take(INSPECTED_PATHS) {
        // A path with only one side is a delete/modify conflict: there is no second set of endings
        // to disagree with.
        let (Some(ours), Some(theirs)) = (ours, theirs) else {
            continue;
        };
        let (Some(ours), Some(theirs)) = (
            blob(repo, &ours, deadline).await,
            blob(repo, &theirs, deadline).await,
        ) else {
            continue;
        };
        let (mine, yours) = (Endings::of(&ours), Endings::of(&theirs));
        if mine == yours || mine == Endings::Absent || yours == Endings::Absent {
            continue;
        }
        let opposites = matches!(
            (mine, yours),
            (Endings::Crlf, Endings::Lf) | (Endings::Lf, Endings::Crlf)
        );
        let nul = match (ours.contains('\0'), theirs.contains('\0')) {
            (true, true) => Some("both sides contain".to_owned()),
            (true, false) => Some(format!("{target}'s side contains")),
            (false, true) => Some(format!("{source}'s side contains")),
            (false, false) => None,
        };
        if nul.is_some() && !opposites {
            continue;
        }
        let mut line = format!(
            "  {path}: {target} has {}, {source} has {}",
            mine.spelled(),
            yours.spelled()
        );
        if let Some(who) = nul {
            // Named because it is the cause and it is invisible. A file with a NUL in it is binary
            // to git: no end-of-line filter has ever run on it, and that is how one file in a
            // checkout comes to keep endings that none of its siblings have.
            line.push_str(&format!(
                " — and {who} a NUL byte, which is usually why: git classifies a file with a NUL \
                 in it as binary, so no end-of-line filter has ever run on it"
            ));
        }
        said.push(line);
    }
    if said.is_empty() {
        return String::new();
    }
    format!(
        "\nthe queue's note, computed from the two sides while the conflict still existed:\n{}\n\
         a file whose sides disagree about line endings has no line that aligns, so the merge \
         conflicts from its first line to its last however small the real change is. Normalise one \
         side before reading the markers — until then they say nothing about the change.\n",
        said.join("\n")
    )
}

/// Two `cat-file` calls per conflicted path and no more than this many paths: the note is a hint
/// offered on the way past, not an audit, and a merge that conflicts in fifty files must not spend a
/// hundred processes proving it. The shape this catches — a whole checkout's endings against a
/// branch's — is visible in the first file of any conflict it caused.
pub(super) const INSPECTED_PATHS: usize = 8;

/// One conflicted path and the blobs the two sides brought to it. Stage 2 is the target's, because
/// the integration worktree is detached on the target and the source is what was merged in; stage 3
/// is the source's. Stage 1 is the base, and the base is not a side.
///
/// Order is first-seen, so the note reads in the order git listed the conflict.
pub(super) fn sides(unmerged: &str) -> Vec<(String, Option<String>, Option<String>)> {
    let mut paths: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for line in unmerged.lines() {
        let Some((entry, path)) = line.split_once('\t') else {
            continue;
        };
        let mut fields = entry.split_whitespace();
        let (Some(_mode), Some(sha), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if paths.iter().all(|(known, _, _)| known != path) {
            paths.push((path.to_owned(), None, None));
        }
        let slot = paths
            .iter_mut()
            .find(|(known, _, _)| known == path)
            .expect("the path was just ensured to be present");
        match stage {
            "2" => slot.1 = Some(sha.to_owned()),
            "3" => slot.2 = Some(sha.to_owned()),
            _ => {}
        }
    }
    paths
}

/// A blob's bytes, as far as they can be carried in a `String`.
///
/// Lossy decoding is what `run_git` does with every command's output, and it is harmless for the one
/// question asked here: an invalid sequence becomes a replacement character without adding or
/// removing a `\n`, `\r` or NUL, so the counting below is unaffected. `None` means the answer could
/// not be had, which the caller treats as nothing to say rather than as an error — this whole path
/// is a courtesy on top of an escalation that is happening either way.
pub(super) async fn blob(repo: &Path, sha: &str, deadline: std::time::Instant) -> Option<String> {
    match git(repo, &["cat-file", "blob", sha], deadline).await {
        Ok(shown) if shown.succeeded() => Some(shown.stdout),
        _ => None,
    }
}

/// How a blob ends its lines, as far as the blob alone can say.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum Endings {
    /// No line ending at all: one line, or an empty file. Nothing to disagree about.
    Absent,
    Lf,
    Crlf,
    Mixed,
}

impl Endings {
    pub(super) fn of(blob: &str) -> Self {
        let breaks = blob.matches('\n').count();
        if breaks == 0 {
            return Self::Absent;
        }
        // Every CRLF contains an LF, so this can never exceed `breaks` — the equality below is a
        // real "all of them" and not an accident of counting two different things.
        match blob.matches("\r\n").count() {
            0 => Self::Lf,
            carriages if carriages == breaks => Self::Crlf,
            _ => Self::Mixed,
        }
    }

    pub(super) fn spelled(self) -> &'static str {
        match self {
            Self::Absent => "no line endings",
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
            Self::Mixed => "a mixture of CRLF and LF",
        }
    }
}
