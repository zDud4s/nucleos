//! The half of the conflict resolver that starts one: it finds an escalated merge, stages the
//! conflict in a worktree of its own, and hands that to an agent.
//!
//! **It lives outside the queue's executor, and that is a finding rather than a filing decision.**
//! The executor holds a pool, not an `AppState`, so it cannot start runs at all — which turns out to
//! be exactly right: a resolution takes as long as an agent takes, and the queue's contract is that
//! a repository is held for the length of a git command. Escalating releases the repository; this
//! loop picks the conflict up afterwards, and every other project's merges carry on in between.
//!
//! The other half is `git_exec::verify_resolution`, which refuses a resolution that threw work away.
//! It landed first, deliberately: building the launcher before the check would have meant a window
//! in which an agent can publish a resolution nothing verified.

use crate::state::AppState;

/// How often the daemon looks for a conflict nobody has attempted.
///
/// Much slower than the queue's own 500ms, and the difference is what each tick costs. The queue's
/// poll is one index-covered query on an empty index, and a person is watching for the merge they
/// asked for. A tick here can mint an agent, and nobody is waiting on it in that way — the asker has
/// already been told, in the feed and on the row, that their merge did not happen. A minute's
/// latency on a conflict costs nothing next to a launch nobody wanted.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Looks for conflicts to resolve, forever. Spawned once by `main.rs`.
///
/// Two passes on one tick, and they are separate because they are about different rows at different
/// moments: one starts a resolution, the other says what an already-published one cost. Sharing a
/// tick is all they share — the second runs even when the first has been stopped, which is deliberate
/// and argued at `record_discards`.
pub async fn run_resolution_loop(state: AppState) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    loop {
        interval.tick().await;
        launch_once(&state).await;
        record_discards(&state.pool).await;
    }
}

/// One escalated merge, as the launcher reads it.
#[derive(sqlx::FromRow)]
struct Candidate {
    id: i64,
    op: String,
    args: String,
    project_id: String,
    project_root: String,
    /// Git's own account of the conflict, which goes into the prompt. `None` once retention has
    /// cleared it (`vcs::prune`), and the resolution is still worth starting without it — the
    /// conflict is in the branches, not in the text.
    output_tail: Option<String>,
}

/// The oldest conflict nobody has attempted, or `None`.
///
/// **Three conditions and each excludes a different thing.** `resolution_run_id IS NULL` is the one
/// attempt: a value there means an agent has already had this conflict, whether it succeeded, gave
/// up, or crashed, and a second one would be a second opinion nobody asked for. `from_resolution = 0`
/// is the loop brake: a landing that came out of a resolution and conflicted anyway escalates to a
/// person, because resolving it would produce another landing that can conflict, one agent per turn,
/// for ever. `op = 'merge'` is the vocabulary — every other operation escalates for reasons an agent
/// in a worktree cannot touch.
///
/// Separated from `launch_once` so the filter can be tested against a pool alone. It is the part
/// that decides which conflicts a person never has to look at, and it should not need an agent
/// runner to prove.
async fn next_conflict(pool: &sqlx::SqlitePool) -> sqlx::Result<Option<Candidate>> {
    sqlx::query_as(
        "SELECT id, op, args, project_id, project_root, output_tail
           FROM vcs_requests
          WHERE status = 'escalated'
            AND op = 'merge'
            AND resolution_run_id IS NULL
            AND from_resolution = 0
          ORDER BY id
          LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

/// Starts at most one resolution, and returns the run it started.
///
/// **One per tick, deliberately.** Each resolution occupies one of its project's concurrency slots,
/// so a burst of escalations launched together would have the first take the slot and the rest come
/// back `Busy` — and `Busy` is refused before the run row exists, so nothing is claimed and they
/// would simply try again next tick. Which is correct, and is also an argument for not asking: a
/// backlog drains one a minute, in id order, with each one's slot released before the next is asked
/// for.
async fn launch_once(state: &AppState) -> Option<i64> {
    // The emergency stop, and it has no exemptions — `runs::create_run` makes the same argument at
    // its own door. Fails closed: a switch that cannot be read stops runs rather than starting them.
    if crate::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return None;
    }
    // A resolution costs an agent, so it is spend, and spend is what the budget governs.
    //
    // **The WIP limit and the attention brake are deliberately NOT consulted here**, and the
    // difference is worth stating because they sit next to the budget in every other loop. Both pace
    // work the daemon is proposing to START — a scheduled rule, a repo trigger, an errand. A
    // resolution starts nothing: it finishes something already in flight, whose branch is written,
    // whose merge was asked for, and which is stuck until somebody clears the conflict. Deferring it
    // for a busy project would leave that work stranded precisely when the project is busy enough
    // for the conflict to matter.
    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::budget::budget_permits_new_run(&state.pool, chrono::Utc::now()).await
    {
        tracing::info!(%reason, "budget exhausted; no conflict resolution started this tick");
        return None;
    }

    let candidate = match next_conflict(&state.pool).await {
        Ok(candidate) => candidate?,
        // Best-effort like every other polling loop here: a failed poll is the next tick's problem.
        Err(error) => {
            tracing::warn!(%error, "resolver: could not look for conflicts to resolve");
            return None;
        }
    };

    // `escalated` is reached by more than a conflict — `verify_resolution` refuses onto it too — and
    // the ones it refuses are already resolutions. Those are excluded by `from_resolution = 0`
    // above; what is left here is a row whose operation will not parse, which the queue itself
    // treats as terminal and unexecutable rather than as work.
    let (source, target) = match crate::vcs::Op::from_stored(&candidate.op, &candidate.args) {
        Ok(crate::vcs::Op::Merge { source, target }) => (source, target),
        Ok(other) => {
            tracing::warn!(
                vcs_request_id = candidate.id,
                op = other.kind(),
                "resolver: a row selected as a merge parsed as something else"
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(
                vcs_request_id = candidate.id,
                %error,
                "resolver: an escalated merge's stored operation could not be parsed"
            );
            return None;
        }
    };

    let prompt = resolution_prompt(
        source.as_str(),
        target.as_str(),
        candidate.output_tail.as_deref(),
    );
    let started = crate::runs::create_resolution_run(
        state,
        prompt,
        candidate.project_id.clone(),
        candidate.project_root.clone(),
        crate::runs::Resolution {
            request_id: candidate.id,
            source: source.as_str().to_owned(),
            target: target.as_str().to_owned(),
        },
    )
    .await;

    match started {
        Ok(run_id) => {
            tracing::info!(
                vcs_request_id = candidate.id,
                run_id,
                source = source.as_str(),
                target = target.as_str(),
                "resolver: started a conflict resolution"
            );
            // The feed is the only surface this pillar has, and a daemon that mints an agent on its
            // own has to say so where the person who asked for the merge is already looking.
            let _ = crate::feed::append(
                &state.pool,
                Some(candidate.project_id.as_str()),
                "vcs_resolution_started",
                &format!(
                    "resolving the conflict between {} and {} (vcs request {})",
                    source.as_str(),
                    target.as_str(),
                    candidate.id
                ),
                Some(run_id),
            )
            .await;
            Some(run_id)
        }
        // Nothing was claimed: `Busy` is refused before the run row exists, so the conflict keeps its
        // attempt and the next tick asks again. Not a warning — a busy project deferring its own
        // resolution is the documented consequence of charging a resolution a slot.
        Err(crate::runs::CreateRunError::Busy) => {
            tracing::info!(
                vcs_request_id = candidate.id,
                project_id = %candidate.project_id,
                "resolver: the project is full; its conflict waits for a slot"
            );
            None
        }
        // Everything else HAS spent the attempt, because the claim is written before the conflict is
        // staged — see `create_run_with`. `fail_provisioning` has already put the reason in the feed;
        // this is the log line that names the request it was about.
        Err(error) => {
            tracing::warn!(
                vcs_request_id = candidate.id,
                %error,
                "resolver: the conflict could not be handed to an agent"
            );
            None
        }
    }
}

/// One published resolution whose cost has not been worked out yet.
#[derive(sqlx::FromRow)]
struct Published {
    id: i64,
    project_id: String,
    project_root: String,
    /// The merge commit the queue put on the target branch. Everything the computation needs hangs
    /// off it by sha — `^2` is the resolution, `^2^2` is the branch it was asked to bring in — which
    /// is what lets this run after the worktree GC has deleted the resolution's branch by name.
    result_sha: String,
}

/// Works out what ONE published resolution left behind, and writes it on the row.
///
/// **Separate from the merge that published it, and not for tidiness.** The queue's contract is that
/// a repository is held for the length of a git command; this reads a `git show` per changed file,
/// which for a long-lived branch is hundreds of them. Doing it inside the claim would hold every
/// other operation on that repository behind an accounting pass nobody is waiting for.
///
/// **It runs even when the kill switch is engaged**, and that is the point rather than an oversight.
/// The switch stops the daemon STARTING work; this starts nothing — it reads commits that are already
/// in the repository and writes one row. The moment somebody pulls the emergency stop is also the
/// moment they most want to know what the last resolution cost.
///
/// A failure to work it out is written into the column rather than left NULL to be retried for ever.
/// One attempt, like the resolution itself: a repository that has moved away does not come back by
/// being asked every minute, and a column that always holds an answer — even "could not" — is one a
/// reader can act on.
///
/// Takes the pool and not the `AppState`, which is not tidiness: this pass starts nothing, so the
/// runner, the run handles and the kill switch are all things it has no business reaching. The
/// narrower argument is also what lets it be tested without an agent runner.
async fn record_discards(pool: &sqlx::SqlitePool) {
    let candidate: Option<Published> = match sqlx::query_as(
        "SELECT id, project_id, project_root, result_sha
           FROM vcs_requests
          WHERE from_resolution = 1
            AND status = 'succeeded'
            AND result_sha IS NOT NULL
            AND discarded IS NULL
          ORDER BY id
          LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    {
        Ok(candidate) => candidate,
        Err(error) => {
            tracing::warn!(%error, "resolver: could not look for resolutions to account for");
            return;
        }
    };
    let Some(published) = candidate else {
        return;
    };

    let deadline = std::time::Instant::now() + crate::git_exec::OPERATION_TIMEOUT;
    let record = match crate::git_exec::discarded_by_resolution(
        std::path::Path::new(&published.project_root),
        &published.result_sha,
        deadline,
    )
    .await
    {
        Ok(record) => record,
        Err(reason) => {
            tracing::warn!(
                vcs_request_id = published.id,
                %reason,
                "resolver: what a published resolution discarded could not be worked out"
            );
            format!("could not be worked out: {reason}")
        }
    };

    if let Err(error) = sqlx::query("UPDATE vcs_requests SET discarded = ? WHERE id = ?")
        .bind(&record)
        .bind(published.id)
        .execute(pool)
        .await
    {
        tracing::warn!(
            vcs_request_id = published.id,
            %error,
            "resolver: what a published resolution discarded could not be recorded"
        );
        return;
    }

    // **Only when something was actually lost.** A feed row IS a notification, and one saying
    // "nothing was lost" after every resolution trains its reader to skip the line that one day
    // says otherwise.
    if record.starts_with("nothing") {
        return;
    }
    let headline = record.lines().next().unwrap_or(&record);
    let _ = crate::feed::append(
        pool,
        Some(published.project_id.as_str()),
        "vcs_resolution_discarded",
        &format!(
            "vcs request {} published a resolution: {headline}",
            published.id
        ),
        None,
    )
    .await;
}

/// PURE: what the resolving agent is told.
///
/// **It describes a worktree that is already conflicted**, because it is — the daemon stages the
/// merge before the agent exists. So the instructions are "resolve and commit", never "merge", and
/// that is not a wording preference: a `git merge` from inside a session goes to the queue, and the
/// queue is what refused this merge for conflicting. An agent told to merge would circle.
///
/// The prohibitions are the ones that produce a green result nobody read. `-X ours` and its family
/// resolve every conflict instantly and correctly-looking, and half the work is gone; the reason
/// they are named individually is that "resolve it properly" does not stop an agent that is out of
/// ideas from reaching for one.
fn resolution_prompt(source: &str, target: &str, output_tail: Option<&str>) -> String {
    let mut prompt = format!(
        "You are resolving a merge conflict that the version-control queue met and could not carry.\n\
         \n\
         The conflict is ALREADY STAGED in this worktree. The tree was created on {target} and \
         {source} was merged into it half-way, so the conflicting files have git's markers in them \
         right now and MERGE_HEAD is set. `git status` lists them.\n\
         \n\
         What to do:\n\
         \n\
         1. Resolve every conflicted file. Read both sides and keep what each of them was doing — a \
         resolution's whole job is that neither side's work is lost.\n\
         2. `git add` what you resolved and `git commit`. Committing on top of what is staged \
         produces the merge commit by itself; you do not have to do anything special to get it.\n\
         3. Run `nucleos-core --land` from this worktree. That asks the queue to publish the result. \
         The queue decides when.\n\
         \n\
         Rules that are not negotiable:\n\
         \n\
         - Never `-X ours`, `-X theirs`, `git checkout --ours`, `git checkout --theirs`, or anything \
         else that picks a side without you having read it. A green result that chose a side on its \
         own is the failure this run exists to prevent, not a way out of it.\n\
         - Do not rebase, squash, amend the merge away, or reset. The commit must keep BOTH parents. \
         A tip with one parent is refused before anything is published, without its content being \
         read at all — because a flattened resolution is exactly what looks perfect.\n\
         - Do not merge, push, pull, or delete branches. Those go to the queue, and the queue is what \
         asked you for this.\n\
         \n\
         If the two sides genuinely cannot be reconciled — contradictory intent, not merely awkward \
         — stop and say so instead of committing a guess. Leaving it for a person is a correct \
         ending for this run.\n"
    );
    if let Some(tail) = output_tail.map(str::trim).filter(|tail| !tail.is_empty()) {
        prompt.push_str(&format!(
            "\nGit's own account of the conflict, from when the queue met it:\n\n{tail}\n"
        ));
    }
    prompt
}

/// Whether the branch a worktree is asking to land was produced by a conflict resolver.
///
/// Asked of the database rather than of a branch-name convention, because a name is something an
/// agent can write and this decides whether the merge is verified.
///
/// **The join is through the run that owns the worktree**, which is sound at exactly the moment it is
/// asked: `--land` is run from inside the worktree, so the tree is on disk, so its row has not been
/// collected. It is not sound at claim time, hours later, which is why the answer is written onto the
/// request at submission instead of computed when the queue gets to it.
///
/// `removed_at` is deliberately not part of the condition. A resolution worktree whose row was marked
/// removed while the directory survived should still have its output verified — the question is what
/// PRODUCED this branch, and that does not stop being true when the tree is collected.
pub(crate) async fn landing_is_a_resolution(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    branch: &str,
) -> bool {
    let found: sqlx::Result<bool> = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM worktrees w
               JOIN vcs_requests r ON r.resolution_run_id = w.owner_id
              WHERE w.owner_kind = 'run'
                AND w.project_id = ?
                AND w.branch = ?
         )",
    )
    .bind(project_id)
    .bind(branch)
    .fetch_one(pool)
    .await;
    match found {
        Ok(found) => found,
        // Fails toward verifying. A landing wrongly treated as a resolution is refused if its tip has
        // one parent, which costs an ordinary session one escalation it can answer; a resolution
        // wrongly treated as ordinary is published without anything having checked that it kept both
        // sides, which is the loss this whole mechanism exists to prevent.
        Err(error) => {
            tracing::warn!(
                project_id,
                branch,
                %error,
                "could not tell whether a landing came out of a conflict resolution; verifying it"
            );
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// An escalated merge, admitted through the real INSERT so the stored operation is the one the
    /// launcher will actually have to parse back.
    async fn escalated_merge(pool: &sqlx::SqlitePool, from_resolution: bool) -> i64 {
        let repo = crate::vcs::ResolvedRepo::synthetic("proj", "C:/repo", "proj");
        let op = crate::vcs::Op::Merge {
            source: "feat/x".into(),
            target: "master".into(),
        };
        let id = if from_resolution {
            crate::vcs::submit_resolution(pool, &repo, &op, crate::vcs::Origin::Shell).await
        } else {
            crate::vcs::submit(pool, &repo, &op, crate::vcs::Origin::Shell).await
        }
        .expect("admit the merge");
        sqlx::query("UPDATE vcs_requests SET status = 'escalated' WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await
            .expect("escalate it");
        id
    }

    /// The three exclusions, each of which prevents a different runaway.
    #[tokio::test]
    async fn only_a_conflict_that_nobody_has_attempted_is_picked_up() {
        let pool = test_pool().await;

        let attempted = escalated_merge(&pool, false).await;
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 9 WHERE id = ?")
            .bind(attempted)
            .execute(&pool)
            .await
            .unwrap();
        // A landing that came OUT of a resolution and conflicted anyway. Resolving it would produce
        // another landing that can conflict, one agent per turn, for ever. It goes to a person.
        let looped = escalated_merge(&pool, true).await;
        // Escalated, but not a merge: nothing an agent in a worktree can do about a push.
        let other = crate::vcs::submit(
            &pool,
            &crate::vcs::ResolvedRepo::synthetic("proj", "C:/repo", "proj"),
            &crate::vcs::Op::Push {
                remote: "origin".into(),
                branch: "master".into(),
            },
            crate::vcs::Origin::Shell,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE vcs_requests SET status = 'escalated' WHERE id = ?")
            .bind(other)
            .execute(&pool)
            .await
            .unwrap();

        let fresh = escalated_merge(&pool, false).await;

        let picked = next_conflict(&pool)
            .await
            .expect("the query should run")
            .expect("there is one conflict left for somebody to look at");
        assert_eq!(
            picked.id, fresh,
            "picked {} — attempted={attempted}, from a resolution={looped}, not a merge={other}",
            picked.id
        );

        // And once it too has been claimed, there is nothing left to start.
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 10 WHERE id = ?")
            .bind(fresh)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            next_conflict(&pool).await.unwrap().is_none(),
            "every conflict has been attempted; a fresh agent per tick is the failure this prevents"
        );
    }

    /// The branch a resolution produced is recognised by the run that owns its worktree, not by what
    /// it is called — a name is something an agent can write, and this decides whether the merge is
    /// verified before it is published.
    #[tokio::test]
    async fn a_landing_is_recognised_as_a_resolution_by_the_run_that_produced_it() {
        let pool = test_pool().await;
        let request = escalated_merge(&pool, false).await;
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 7 WHERE id = ?")
            .bind(request)
            .execute(&pool)
            .await
            .unwrap();
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Run(7),
            "proj",
            "C:/repo",
            "C:/wt/run-7",
            "nucleos/run-7",
            None,
        )
        .await
        .unwrap();
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Run(8),
            "proj",
            "C:/repo",
            "C:/wt/run-8",
            "nucleos/run-8",
            None,
        )
        .await
        .unwrap();

        assert!(landing_is_a_resolution(&pool, "proj", "nucleos/run-7").await);
        assert!(
            !landing_is_a_resolution(&pool, "proj", "nucleos/run-8").await,
            "an ordinary run's landing must not be verified as a resolution — its tip has one \
             parent, like every branch, and it would be refused for it"
        );
        assert!(
            !landing_is_a_resolution(&pool, "other", "nucleos/run-7").await,
            "the answer is scoped to the project, or two projects' branch names decide each other's"
        );
    }

    /// The prompt is where the resolution's safety is spent or kept, so the words that carry it are
    /// pinned. `-X ours` and its family resolve every conflict instantly, produce a green tree, and
    /// throw half the work away — and "resolve it properly" does not stop an agent that has run out
    /// of ideas from reaching for one.
    #[test]
    fn the_prompt_names_the_ways_a_resolution_can_look_right_and_still_be_wrong() {
        let prompt = resolution_prompt("feat/x", "master", Some("CONFLICT (content): seed.txt"));

        for forbidden in ["-X ours", "-X theirs", "--ours", "--theirs"] {
            assert!(
                prompt.contains(forbidden),
                "a strategy that picks a side unread has to be named, not implied: {forbidden}"
            );
        }
        assert!(
            prompt.contains("BOTH parents"),
            "the agent should know why a flattened resolution is refused before it makes one"
        );
        assert!(
            prompt.contains("nucleos-core --land"),
            "a resolution nobody can publish is not one"
        );
        assert!(
            prompt.contains("ALREADY STAGED"),
            "an agent told to merge would go to the queue that refused this merge, and circle"
        );
        assert!(
            prompt.contains("CONFLICT (content): seed.txt"),
            "git's own account of the conflict is the one thing here nobody has to guess at"
        );
    }

    /// **A resolution that cannot be accounted for is still accounted for**, and the column is what
    /// stops the pass from asking again every minute for ever. A repository that has moved away does
    /// not come back by being polled, and a NULL left behind would have this loop reading the same
    /// row, running the same git, and failing the same way until somebody noticed the log.
    ///
    /// It is also the honest answer: "could not be worked out" is a different thing from "nothing was
    /// lost", and a reader who finds the second when the first is true has been told something
    /// nobody checked.
    #[tokio::test]
    async fn a_resolution_whose_repository_is_gone_records_that_rather_than_asking_for_ever() {
        let pool = test_pool().await;
        let request = escalated_merge(&pool, true).await;
        sqlx::query(
            "UPDATE vcs_requests
                SET status = 'succeeded', result_sha = 'deadbeef', project_root = 'C:/gone'
              WHERE id = ?",
        )
        .bind(request)
        .execute(&pool)
        .await
        .unwrap();

        record_discards(&pool).await;

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT discarded FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&pool)
                .await
                .unwrap();
        let recorded = recorded.expect("a row that could not be worked out still gets an answer");
        assert!(
            recorded.starts_with("could not be worked out"),
            "and the answer says which of the three states this is: {recorded}"
        );
        assert!(
            !recorded.starts_with("nothing"),
            "a repository nobody could read must never report a clean resolution"
        );

        // The pass moves on rather than finding the same row again.
        record_discards(&pool).await;
        let unchanged: Option<String> =
            sqlx::query_scalar("SELECT discarded FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unchanged.as_deref(), Some(recorded.as_str()));
    }

    /// Retention clears `output_tail` on old rows, and a conflict that outlived its text is still
    /// worth resolving — the conflict is in the branches, not in what git said about it.
    #[test]
    fn a_conflict_whose_output_has_aged_out_still_gets_a_prompt() {
        let prompt = resolution_prompt("feat/x", "master", None);
        assert!(prompt.contains("ALREADY STAGED"));
        assert!(!prompt.contains("Git's own account"));
    }
}
