//! Checking a conflict resolution: what it discarded from either side, and which worktree holds a branch.

use super::*;

/// Whether a conflict resolver's branch may be merged at all, checked before it is.
///
/// **The gate protects against "it broke" and never against "it threw work away".** A resolution
/// that keeps one side and discards the other compiles, passes every test, and is indistinguishable
/// from a good one by any measure the suite has. So the two things that cannot be checked afterwards
/// are checked here, and both are structural rather than about content — neither asks what the right
/// answer was, only whether this could possibly be one.
///
/// **A two-parent tip.** The daemon left a merge half-finished in the resolver's worktree, so an
/// ordinary `git commit` on top of that produces a merge commit with both parents and the resolver
/// has to do nothing special to earn it. One parent therefore does not mean "resolved differently";
/// it means the merge was thrown away and something else was committed in its place — a flattened
/// branch, a reset, a cherry-pick. That is refused without looking at the content at all, because
/// the content of a flattened resolution can look perfect.
///
/// **No conflict markers left in the tree.** `git diff --check` reports them, and an agent that
/// stopped halfway commits them without noticing: the file has both sides in it, the tests may even
/// pass if the markers land in a comment or a string, and the merge would publish something no
/// human wrote.
///
/// Refusing here is `Escalated` rather than `Failed` for the same reason a conflict is: somebody has
/// to look, and the row is what tells them so.
pub(super) async fn verify_resolution(
    project_root: &Path,
    source: &str,
    deadline: std::time::Instant,
) -> Result<(), Outcome> {
    let parents = git(
        project_root,
        &["rev-list", "--parents", "-1", source],
        deadline,
    )
    .await?;
    if !parents.succeeded() {
        return Err(escalated(
            format!("could not read {source}'s tip to verify the resolution"),
            &parents,
        ));
    }
    // `rev-list --parents -1` prints "<commit> <parent>..." — so the parent count is the field count
    // minus the commit itself.
    if parents.stdout.split_whitespace().count() - 1 < 2 {
        return Err(Outcome::Escalated {
            reason: format!(
                "{source} was supposed to be a resolved merge and its tip has fewer than two \
                 parents, so the merge it was asked to resolve is not in it. Nothing was merged. A \
                 flattened resolution can look entirely correct and still be one side of the work \
                 thrown away, which is why this is refused without reading the content."
            ),
            exit_code: parents.exit_code,
            output_tail: parents.output_tail,
        });
    }

    let markers = git(
        project_root,
        &["diff", "--check", &format!("{source}^1"), source],
        deadline,
    )
    .await?;
    // Non-zero here is `--check` reporting, not git failing: it exits 2 when it finds markers or
    // whitespace errors. The tail is what says which, and it goes in the row.
    if !markers.succeeded() && markers.output_tail.contains("conflict marker") {
        return Err(Outcome::Escalated {
            reason: format!(
                "{source} still has conflict markers committed in it, so the resolution was left \
                 half-done. Nothing was merged."
            ),
            exit_code: markers.exit_code,
            output_tail: markers.output_tail,
        });
    }
    Ok(())
}

/// How many of the incoming branch's files are examined for lost work before the record says it
/// stopped looking. A resolution of a long-lived branch can touch hundreds, and each one costs a
/// `git show` inside the same budget the merge itself came out of.
pub(super) const DISCARD_FILE_CEILING: usize = 200;

/// How many of the missing lines are quoted back. The COUNTS are always complete; this bounds only
/// the excerpt, because the column is read by a person and not by a diff tool.
pub(super) const DISCARD_SAMPLE_LINES: usize = 8;

/// `git`, for a reader that is not executing a queued request and so has no `Outcome` to return.
///
/// It goes through `remaining` rather than reading the clock itself, and that is the whole point:
/// the gate that refuses a spent budget BEFORE a child is spawned lives in one place, and a second
/// entry that re-derived it would be exactly the kind of guarantee that quietly stops holding. Only
/// the error SHAPE differs, and it differs at the edge — `remaining` reports an `Outcome` because
/// every other caller is answering for a row, and this one answers for a background pass that has
/// no row to fail.
pub(super) async fn git_read(
    repo: &Path,
    args: &[&str],
    deadline: std::time::Instant,
) -> Result<CommandResult, String> {
    let budget = match remaining(deadline, &args.join(" ")) {
        Ok(budget) => budget,
        // `remaining` builds this variant and no other; the second arm is the compiler's price for
        // that being a fact about the function rather than about the type.
        Err(Outcome::Failed { reason, .. }) => return Err(reason),
        Err(other) => return Err(format!("the budget could not be read: {other:?}")),
    };
    let arguments: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    run_git(repo, &arguments, budget).await
}

/// What the incoming branch added and the published resolution does not have, worked out from the
/// commits.
///
/// **This is the half of the guarantee `verify_resolution` cannot give.** That one refuses the two
/// shapes a resolution CANNOT be right in — a flattened merge, committed conflict markers — and both
/// are structural, so both can be refused without reading a line. What neither it nor the gate can
/// see is the resolution that kept one side and dropped the other: it compiles, it passes, and by
/// every measure the suite has it is indistinguishable from a good one. So it is published, and then
/// this says what it cost.
///
/// **Computed, never reported.** An agent that loses work and says it did not is exactly the case
/// this exists to catch, so nothing the agent wrote is an input.
///
/// It works from the published merge commit alone and needs no branch name, which matters more than
/// it looks: by the time this runs the resolution's branch is merged, so the worktree GC's
/// `git branch -d` is entitled to delete it. Everything needed is reachable from what was published
/// — `published^2` is the resolution, `published^2^2` is the branch the resolver was asked to bring
/// in — and shas outlive names.
///
/// **What "missing" means here, stated because the record has to be read literally:** a line the
/// incoming branch ADDED, relative to where the two sides parted, that appears nowhere in the
/// published file. A resolution that rewrote a line to carry both intents is reported by this, and
/// that is a false positive worth having — the alternative is a judgement about meaning, and a
/// mechanism that guesses at meaning is one nobody can act on. The wording says "not present
/// verbatim" for that reason.
pub async fn discarded_by_resolution(
    project_root: &Path,
    published: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let resolution = second_parent(project_root, published, deadline).await?;
    let incoming = second_parent(project_root, &resolution, deadline).await?;
    let first = format!("{resolution}^1");

    let base = git_read(project_root, &["merge-base", &first, &incoming], deadline).await?;
    if !base.succeeded() {
        return Err(format!(
            "could not find where {incoming} and the target parted: {}",
            base.output_tail
        ));
    }
    let base = base.stdout.trim().to_owned();

    let listed = git_read(
        project_root,
        &["diff", "--name-only", &base, &incoming],
        deadline,
    )
    .await?;
    if !listed.succeeded() {
        return Err(format!(
            "could not list what {incoming} changed: {}",
            listed.output_tail
        ));
    }
    let files: Vec<&str> = listed
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    let examined = files.len().min(DISCARD_FILE_CEILING);

    let mut per_file: Vec<(String, usize)> = Vec::new();
    let mut samples: Vec<String> = Vec::new();
    let mut total = 0usize;
    for file in &files[..examined] {
        let added = added_lines(project_root, &base, &incoming, file, deadline).await?;
        if added.is_empty() {
            continue;
        }
        // A file the resolution removed outright answers non-zero here, and every line it added is
        // then missing — which is the right reading, and the reason this is not an error.
        let published_file = git_read(
            project_root,
            &["show", &format!("{published}:{file}")],
            deadline,
        )
        .await?;
        let kept: std::collections::HashSet<&str> = if published_file.succeeded() {
            published_file.stdout.lines().map(str::trim).collect()
        } else {
            std::collections::HashSet::new()
        };
        let missing: Vec<&String> = added
            .iter()
            .filter(|line| !kept.contains(line.as_str()))
            .collect();
        if missing.is_empty() {
            continue;
        }
        total += missing.len();
        per_file.push(((*file).to_owned(), missing.len()));
        for line in missing {
            if samples.len() < DISCARD_SAMPLE_LINES {
                samples.push(format!(
                    "{file}: {}",
                    line.chars().take(120).collect::<String>()
                ));
            }
        }
    }

    // Named rather than swallowed: a ceiling nobody is told about reads as "we looked everywhere and
    // found nothing", which is the one thing this record must never imply.
    let truncated = files.len() > examined;
    if total == 0 {
        return Ok(if truncated {
            format!(
                "nothing, in the first {examined} of {} files — the rest were not examined",
                files.len()
            )
        } else {
            "nothing".to_owned()
        });
    }

    // **The count AND the files go on the first line**, which is not formatting: the feed carries
    // one line of this and the feed is the only surface a person passes without going looking. A
    // headline that says "12 lines went missing" and makes them query the database to find out
    // where is a headline that gets skipped.
    let where_ = per_file
        .iter()
        .map(|(file, count)| format!("{file} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut record = format!(
        "{total} line(s) that {incoming} added are not present verbatim in what was published — \
         {where_}"
    );
    if truncated {
        record.push_str(&format!(
            " (the first {examined} of {} files were examined)",
            files.len()
        ));
    }
    record.push_str("\n\nfor example:\n");
    for sample in &samples {
        record.push_str(&format!("  {sample}\n"));
    }
    Ok(record)
}

/// The second parent of `revision`, which for a merge commit is the side that was brought in.
pub(super) async fn second_parent(
    project_root: &Path,
    revision: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let parents = git_read(
        project_root,
        &["rev-list", "--parents", "-1", revision],
        deadline,
    )
    .await?;
    if !parents.succeeded() {
        return Err(format!(
            "could not read {revision}'s parents: {}",
            parents.output_tail
        ));
    }
    parents
        .stdout
        .split_whitespace()
        .nth(2)
        .map(str::to_owned)
        .ok_or_else(|| format!("{revision} is not a merge commit, so it brought nothing in"))
}

/// The lines `incoming` added to `file` since the two sides parted, trimmed and without the blanks.
///
/// `-U0` so nothing but the changed lines comes back. Blank lines are dropped because a blank
/// "missing" from the published file is noise in a record meant to be read by a person.
pub(super) async fn added_lines(
    project_root: &Path,
    base: &str,
    incoming: &str,
    file: &str,
    deadline: std::time::Instant,
) -> Result<Vec<String>, String> {
    let diff = git_read(
        project_root,
        &["diff", "-U0", base, incoming, "--", file],
        deadline,
    )
    .await?;
    if !diff.succeeded() {
        return Err(format!("could not read what changed in {file}"));
    }
    Ok(diff
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix('+'))
        .filter(|line| !line.starts_with("++"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The worktree that has `target` checked out, if any.
///
/// Read out of `git worktree list --porcelain`, whose blocks are `worktree <path>` / `HEAD <sha>` /
/// then either `branch refs/heads/<name>` or `detached`. The main checkout is in that list too,
/// which is the point — "the user has master open" is the ordinary answer.
///
/// The daemon's own integration worktree can never be the answer, and not because it is filtered
/// out: it is always detached, so it has no `branch` line to match. Task 3's `--detach` is what makes
/// that true, and `an_integration_worktree_never_holds_the_branch_it_merges_into` — written later in
/// this chunk, and deliberately named here before it exists — is what pins it.
///
/// An error here is `Outcome::Failed` and never `Ok(None)`: "we could not find out" must not read as
/// "nobody has it", which would send the publish down the compare-and-swap route and move a ref out
/// from under somebody. `worktree.rs:321-327` makes the identical argument about the identical
/// command.
pub(super) async fn worktree_holding(
    project_root: &Path,
    target: &str,
    deadline: std::time::Instant,
) -> Result<Option<std::path::PathBuf>, Outcome> {
    let listed = git(project_root, &["worktree", "list", "--porcelain"], deadline).await?;
    if !listed.succeeded() {
        return Err(failed(
            "could not find out which worktree has the target branch open".to_owned(),
            &listed,
        ));
    }

    let wanted = format!("branch refs/heads/{target}");
    let mut current: Option<std::path::PathBuf> = None;
    for line in listed.stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(std::path::PathBuf::from(path));
        } else if line == wanted {
            return Ok(current);
        }
    }
    Ok(None)
}
