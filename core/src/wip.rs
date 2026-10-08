//! §spec agenticos-foundation-and-autopilot
//!
//! The work-in-progress brake (spec §8.4/§8.7).
//!
//! The budget bounds what autonomy costs; this bounds what it costs *you*. An unbounded approval
//! queue turns autonomy into a second job — the system generates faster than the human reviews, and
//! the backlog becomes the very bottleneck the Autopilot existed to remove. So a project stops
//! starting new autonomous work once it already has enough unreviewed proposals waiting.
//!
//! Unlike the budget, this brake is self-clearing: it releases the moment the human reviews
//! something, because it throttles on the thing that is actually saturated.

use sqlx::SqlitePool;

/// Whether a project may start another autonomous run, judged by how much already waits for review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WipDecision {
    Allow,
    Defer { reason: String },
}

/// The effective open-proposal ceiling for one project: its own override when set, otherwise the
/// global default. `None` means the brake is switched off (only expressible globally, by design).
pub async fn wip_limit(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<i64>> {
    let project_override: Option<Option<i64>> =
        sqlx::query_scalar("SELECT wip_limit FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    if let Some(Some(limit)) = project_override {
        return Ok(Some(limit));
    }
    global_wip_limit(pool).await
}

/// The default ceiling every project inherits when it has no override of its own.
pub async fn global_wip_limit(pool: &SqlitePool) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT wip_limit FROM autopilot_global LIMIT 1")
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
}

/// The two terms, each as a scalar subquery, parameterized by the SQL expression naming the
/// project. `?1` for a bound read; `state.project_id` for a correlated one inside the roster.
///
/// **Macros and not `&str` constants, because the roster cannot bind a parameter.**
/// [`crate::autopilot::project_roster`] reads every project in one pass, so its copy of this
/// arithmetic has to correlate against the outer row rather than bind `?1` — and that single
/// difference is the whole reason two hand-written copies existed at all. They drifted twice.
/// The first time, the shadow term lost its `runs.mode` scope in one copy; the second, the
/// proposals term kept every `kind` in one copy while the other had already excluded two, and the
/// roster showed 19 for a queue the daemon refused work at 13. Both were found in production, by
/// somebody reading two numbers that disagreed. A macro takes the one difference as an argument
/// and leaves nothing else to keep in sync: there is now one text, expanded twice.
///
/// **Excluding `skipped-item` is load-bearing and not tidying.** It and `action-approval` arrive
/// through the same function in `hooks.rs` and say opposite things about the scarce resource this
/// limit protects:
///
/// - *Work done, waiting for you to look at it.* That is what §8.4 says the limit is for — the
///   system generating faster than a person reviews — and it is what an action approval, a contact
///   merge and an unreviewed shadow decision all are.
/// - *Work NOT done, waiting for you to decide whether it should be.* A skipped item cost nobody
///   any attention and produced nothing to review. It is a note, not a queue.
///
/// Counting the second as the first would close autonomy at the third skipped item of a night with
/// the default limit of 3 — the job that skipped them would be the reason the next job is refused,
/// for work that was never done.
///
/// Spelled as an exclusion rather than as `kind = 'action-approval'`, deliberately, and the reason
/// is about the kinds that do not exist yet. `contact-merge` and `calendar-event` never reach this
/// count today for an unrelated reason — they are written with a NULL `project_id`, and the filter
/// above is per project — so either spelling would agree about them. Where the two forms differ is
/// on the NEXT kind somebody adds: an exclusion counts it by default and an inclusion drops it
/// silently, and for a brake, being counted is the direction to fail in.
///
/// **`fleet-exclusion` is the next kind, and it is the exception that paragraph was written to make
/// visible rather than to forbid.** It is the first kind that is inherently about one project's
/// jobs, so unlike the two above it does carry a `project_id` and does reach this count — the
/// default landed on it, exactly as designed, and the default is wrong here.
///
/// The direction is what settles it. Every other kind counted here is *the system produced
/// something and now needs you*; an exclusion is *you asked the system to do less*. Counting the
/// second as the first inverts the brake: at the default limit of 3, drawing three "these two must
/// not run together" edges would close the project's autonomy completely, and the person who asked
/// for restraint would be throttled by their own request — with a reason naming a review backlog
/// they do not have.
///
/// **`refused-action` is the third exclusion, and it is the accidental one the paragraph above
/// warns about, found the day the accident stopped holding.** A refused action is a record of
/// something the system was NOT allowed to do: the classifier or the injection barrier said no, the
/// turn carried on or ended, and nothing was produced for anyone to review. Nor can it be approved
/// into happening — `approve` answers 409 for it, which is why `get_refused_actions` in `http.rs`
/// gives it a door of its own. That makes it a note, the same species as `skipped-item`, and not
/// work waiting on a person.
///
/// For as long as it has existed it stayed out of this count only because `create_refused_action`
/// wrote it with a NULL `project_id` — the unrelated reason above, holding a kind out by accident.
/// On 2026-09-14 the row learned its run's project, so that a refusal could be attributed at all,
/// and without this line the skipped item's arithmetic comes back: job 26 on 2026-09-13 recorded six
/// refusals, and at the default limit of 3 one such night would close the project's autonomy for
/// work the system was stopped from doing.
macro_rules! open_proposals_term {
    ($project:literal) => {
        concat!(
            "(SELECT COUNT(*) FROM proposals
     WHERE project_id = ",
            $project,
            " AND status = 'pending'
       AND kind <> 'skipped-item' AND kind <> 'fleet-exclusion'
       AND kind <> 'refused-action')"
        )
    };
}

/// **The `shadow_decisions` term is scoped to `runs.mode = 'shadow'`, and that scope is not
/// incidental — it is the fix for a real defect, not a tidy-up.** A `worktree`-mode decision was
/// already ENFORCED: the command ran, and there is no verdict left for a human to give it. If the
/// classifier had withheld it instead, it became a `pending_approval` proposal, which the FIRST
/// term above already counts — so an unfiltered second term either double-counts a proposal or
/// counts an enforced action as if it were still waiting on someone, and a project that has only
/// ever run in `worktree` mode can accumulate thousands of such rows with nobody ever able to clear
/// them. That breaks the promise at the top of this module: this brake is NOT self-clearing without
/// the filter, because nothing will ever present a `worktree`-mode row for review, so it can never
/// be reviewed, so it never releases. Restricting the term to `shadow`-mode runs is what makes
/// "releases the moment the human reviews something" true again — shadow decisions are the only
/// ones a human can still render a verdict on.
macro_rules! open_shadow_decisions_term {
    ($project:literal) => {
        concat!(
            "(SELECT COUNT(*) FROM shadow_decisions
     JOIN runs ON shadow_decisions.run_id = runs.id
     WHERE runs.project_id = ",
            $project,
            " AND shadow_decisions.human_verdict IS NULL
       AND runs.mode = 'shadow')"
        )
    };
}

pub(crate) use {open_proposals_term, open_shadow_decisions_term};

/// How much a project has waiting on the human right now.
///
/// Both kinds of waiting, not just proposals. A shadow run never mints a proposal — it records
/// `shadow_decisions` for review instead — so counting proposals alone meant the brake was inert in
/// the one mode whose entire purpose is to accumulate reviewable evidence. A busy watched branch
/// could pile up an unbounded backlog while this kept answering Allow, which is precisely the
/// failure §8.4 describes: the system generating faster than the human reviews.
///
/// Kept as one query so the roster's `queue_full` flag and this gate stay the same arithmetic --
/// and, since the macros above, literally the same text as the roster's own.
///
/// **Carried as its two parts rather than as the total, and that is a real defect fixed rather
/// than a nicety.**
/// The brake says "13 items already waiting for review" and the person opens the proposals list,
/// which is the only screen the word suggests -- and finds it empty. Measured on `nucleos`
/// 2026-09-08: of the 13, **zero** were proposals and **all 13** were unreviewed shadow decisions,
/// which live on a different screen entirely. Nothing was broken and nothing was drifting; the
/// number simply never said which of two queues it was counting, so the only way to find out was
/// to read this file. Split, it points at the screen that can clear it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenReviewItems {
    /// Pending proposals, excluding the kinds that are notes rather than queue --
    /// see [`open_proposals_term`], which is where that exclusion is argued.
    pub proposals: i64,
    /// Unreviewed decisions from `shadow`-mode runs -- what `crate::shadow::list_unreviewed`
    /// serves, scoped identically, which `autopilot.rs` holds with a cross-module test.
    pub shadow_decisions: i64,
}

impl OpenReviewItems {
    /// What the brake compares against the ceiling.
    pub fn total(self) -> i64 {
        self.proposals + self.shadow_decisions
    }

    /// Where to send the person, said in the order that puts the non-empty queue first. `None` when
    /// there is nothing waiting at all -- a caller with nothing to explain should say nothing.
    pub fn describe(self) -> Option<String> {
        match (self.proposals, self.shadow_decisions) {
            (0, 0) => None,
            (0, shadow) => Some(format!("{shadow} shadow {}", decisions(shadow))),
            (proposals, 0) => Some(format!("{proposals} {}", nouns(proposals))),
            (proposals, shadow) => Some(format!(
                "{proposals} {} and {shadow} shadow {}",
                nouns(proposals),
                decisions(shadow)
            )),
        }
    }
}

fn nouns(count: i64) -> &'static str {
    if count == 1 { "proposal" } else { "proposals" }
}

fn decisions(count: i64) -> &'static str {
    if count == 1 { "decision" } else { "decisions" }
}

/// Both terms in one round trip, because the caller that wants the split always wants the total too.
pub async fn open_review_items(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<OpenReviewItems> {
    let (proposals, shadow_decisions): (i64, i64) = sqlx::query_as(concat!(
        "SELECT ",
        open_proposals_term!("?1"),
        ", ",
        open_shadow_decisions_term!("?1")
    ))
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    Ok(OpenReviewItems {
        proposals,
        shadow_decisions,
    })
}

/// PURE: whether a queue at `open` is full against `limit`. Split out so the daemon's decision and
/// the roster flag the shell renders are the same comparison.
pub fn queue_full(open: i64, limit: Option<i64>) -> bool {
    limit.is_some_and(|limit| open >= limit)
}

/// Fails CLOSED — an unreadable queue defers rather than piling more onto a human who may already
/// be behind, the same posture the budget gate takes.
pub async fn wip_permits_new_run(pool: &SqlitePool, project_id: &str) -> WipDecision {
    let limit = match wip_limit(pool, project_id).await {
        Ok(None) => return WipDecision::Allow,
        Ok(Some(limit)) => limit,
        Err(error) => {
            return WipDecision::Defer {
                reason: format!("could not read the WIP limit: {error}"),
            };
        }
    };

    let items = match open_review_items(pool, project_id).await {
        Ok(items) => items,
        Err(error) => {
            return WipDecision::Defer {
                reason: format!("could not count open proposals: {error}"),
            };
        }
    };
    let open = items.total();

    if queue_full(open, Some(limit)) {
        // "items", not "proposals". `open_review_items` counts unreviewed `shadow_decisions`
        // too, and for a project that has spent time in shadow mode they are nearly all of it: this
        // said "85 proposals already waiting" for a project whose `/proposals` had exactly one, so
        // the one person who went to look concluded the brake was broken and went hunting. A brake's
        // reason is read precisely when something has stopped — naming the wrong queue sends the
        // reader to a page that disagrees with it.
        //
        // And the breakdown, because "items" alone still sends the reader to the wrong screen.
        // Measured on `nucleos` 2026-09-08, this project's 13 were 0 proposals and 13 shadow
        // decisions -- so the person who followed the word "review" to the proposals list found it
        // empty a second time, for a different reason than the first. Naming both queues is what
        // makes the sentence actionable: it says which page clears the brake.
        let where_they_are = items
            .describe()
            .map(|split| format!(" ({split})"))
            .unwrap_or_default();
        return WipDecision::Defer {
            reason: format!(
                "{open} items already waiting for review{where_they_are} (limit {limit})"
            ),
        };
    }
    WipDecision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn add_project(pool: &SqlitePool, project_id: &str) {
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'shadow')")
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Shadow work waiting for review: a run owned by the project, plus decisions recorded under it
    /// with no human verdict yet. This is what a shadow run actually produces — no proposal is ever
    /// minted — which is why the brake has to see it.
    async fn add_unreviewed_shadow_decisions(pool: &SqlitePool, project_id: &str, count: usize) {
        add_unreviewed_decisions_in_mode(pool, project_id, "shadow", count).await;
    }

    /// Same shape as `add_unreviewed_shadow_decisions`, but the owning run can be seeded under any
    /// mode — in particular `worktree`, to prove an already-enforced decision does not count here.
    async fn add_unreviewed_decisions_in_mode(
        pool: &SqlitePool,
        project_id: &str,
        mode: &str,
        count: usize,
    ) {
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'work', 'completed', ?, '2026-07-27T00:00:00Z')",
        )
        .bind(project_id)
        .bind(mode)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();

        for index in 0..count {
            sqlx::query(
                "INSERT INTO shadow_decisions
                 (run_id, tool_name, decision, action_class, classifier_version, created_at)
                 VALUES (?, 'Bash', 'allow', 'read-local', 2, ?)",
            )
            .bind(run_id)
            .bind(format!("2026-07-27T00:0{index}:00Z"))
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn add_pending_proposals(pool: &SqlitePool, project_id: &str, count: usize) {
        for index in 0..count {
            sqlx::query(
                "INSERT INTO proposals (kind, status, run_id, project_id, reasoning, created_at)
                 VALUES ('action-approval', 'pending', ?, ?, 'test', '2026-07-27T00:00:00Z')",
            )
            .bind(index as i64 + 1)
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn add_skipped_items(pool: &SqlitePool, project_id: &str, count: usize) {
        for index in 0..count {
            sqlx::query(
                "INSERT INTO proposals (kind, status, run_id, project_id, reasoning, created_at)
                 VALUES ('skipped-item', 'pending', ?, ?, 'test', '2026-08-07T00:00:00Z')",
            )
            .bind(index as i64 + 100)
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn add_pending_exclusions(pool: &SqlitePool, project_id: &str, count: usize) {
        for _ in 0..count {
            sqlx::query(
                "INSERT INTO proposals (kind, status, run_id, project_id, reasoning, created_at)
                 VALUES ('fleet-exclusion', 'pending', NULL, ?, 'test', '2026-08-15T00:00:00Z')",
            )
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// Refusals minted by the real constructor, each on a run of its own that belongs to the project
    /// — one open refusal per run is what `one_open_refused_action_per_run` allows, and job 26's six
    /// were six nodes. Through `create_refused_action` rather than a hand-written INSERT, so the
    /// `project_id` the brake sees is the one production writes and not one this fixture chose.
    async fn add_refused_actions(pool: &SqlitePool, project_id: &str, count: usize) {
        for _ in 0..count {
            let run_id = sqlx::query(
                "INSERT INTO runs (project_id, prompt, status, mode, created_at)
                 VALUES (?, 'node', 'completed', 'worktree', '2026-09-13T00:00:00Z')",
            )
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid();
            crate::proposals::create_refused_action(
                pool,
                run_id,
                None,
                "Bash",
                "the classifier did not recognise this command",
                Some(r#"{"command":"cargo fmt --all"}"#),
                None,
            )
            .await
            .unwrap();
        }
    }

    /// A request from the person is not a queue for the person.
    ///
    /// The pair is the test, and neither half is enough on its own: the first would pass with the
    /// count broken to zero, the second passes with today's SQL. Together they pin the behaviour
    /// between the two.
    ///
    /// The arithmetic this prevents, at the default limit of 3: somebody who draws three exclusions
    /// — asking for LESS to happen at once — closes their project's autonomy entirely, and the
    /// brake's reason would tell them they have work waiting for review that they do not have.
    #[tokio::test]
    async fn a_pending_exclusion_does_not_fill_the_review_queue() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_pending_exclusions(&pool, "project-a", 5).await;

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            0,
            "five requests from the person are no backlog at all"
        );
        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );

        // And the count is not simply broken: an action approval beside them still counts.
        add_pending_proposals(&pool, "project-a", 1).await;
        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            1
        );
    }

    #[test]
    fn queue_full_only_when_a_limit_is_set_and_reached() {
        assert!(!queue_full(9, None));
        assert!(!queue_full(2, Some(3)));
        assert!(queue_full(3, Some(3)));
        // Over the line (a limit lowered under an existing queue) still counts as full.
        assert!(queue_full(5, Some(3)));
    }

    /// The brake counted `proposals`, and a shadow run never mints one — it records
    /// `shadow_decisions` for the human to review instead. So the one mode whose entire purpose is
    /// to accumulate reviewable evidence was the one mode the review-backlog brake ignored, and a
    /// busy watched branch could pile up an unbounded queue while `wip_permits_new_run` kept
    /// answering Allow.
    #[tokio::test]
    async fn unreviewed_shadow_decisions_count_towards_the_queue() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_unreviewed_shadow_decisions(&pool, "project-a", 3).await;

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            3
        );
        assert!(matches!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Defer { .. }
        ));
    }

    /// A `worktree`-mode decision was already ENFORCED — the command ran — so there is no verdict
    /// left for a human to give it, and it must not spend the review-backlog limit.
    ///
    /// Measured on the real database this task fixes: 1226 unreviewed `worktree`-mode `allow` rows,
    /// against a `wip_limit` of 3, with an empty review queue. Without this filter every one of
    /// those projects refuses to start new work forever, because nothing will ever present those
    /// rows for a human to clear.
    #[tokio::test]
    async fn uma_decisao_aplicada_em_worktree_nao_e_fila_de_revisao() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_unreviewed_decisions_in_mode(&pool, "project-a", "worktree", 50).await;

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            0,
            "an enforced worktree decision is not a review backlog"
        );
        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );
    }

    /// The §8.4 case this module exists for must stay intact: a shadow run mints no proposal, so
    /// its unreviewed decisions are the only signal the brake has that a busy watched branch is
    /// piling up review work faster than a human clears it. Restricting the term to `shadow` mode
    /// must not simply switch the brake off — it must keep braking on the mode it was built for.
    #[tokio::test]
    async fn uma_decisao_em_modo_shadow_continua_a_travar() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_unreviewed_decisions_in_mode(&pool, "project-a", "shadow", 5).await;

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            5
        );
        let WipDecision::Defer { reason } = wip_permits_new_run(&pool, "project-a").await else {
            panic!("unreviewed shadow decisions must still brake new work");
        };
        assert!(reason.contains('5'), "got: {reason}");
    }

    /// The FIRST term is untouched by this change: pending proposals still count in full, and the
    /// `skipped-item` / `fleet-exclusion` exclusions argued at length above still hold. Only the
    /// SECOND term (`shadow_decisions`) gained a mode filter.
    #[tokio::test]
    async fn as_propostas_continuam_a_contar_e_as_excecoes_continuam_fora() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_pending_proposals(&pool, "project-a", 2).await;
        add_skipped_items(&pool, "project-a", 5).await;
        add_pending_exclusions(&pool, "project-a", 5).await;
        add_unreviewed_decisions_in_mode(&pool, "project-a", "shadow", 1).await;
        add_unreviewed_decisions_in_mode(&pool, "project-a", "worktree", 50).await;

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            3,
            "2 pending proposals + 1 shadow decision; skipped items, exclusions and the \
             worktree decisions must not count"
        );
    }

    #[tokio::test]
    async fn a_reviewed_shadow_decision_releases_the_brake() {
        // Self-clearing is the property that makes this brake tolerable: it has to throttle on what
        // is actually saturated, and release the moment the human clears it.
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_unreviewed_shadow_decisions(&pool, "project-a", 3).await;
        sqlx::query("UPDATE shadow_decisions SET human_verdict = 'approve' WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            2
        );
        assert!(matches!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        ));
    }

    #[tokio::test]
    async fn proposals_and_shadow_decisions_add_up() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_pending_proposals(&pool, "project-a", 2).await;
        add_unreviewed_shadow_decisions(&pool, "project-a", 1).await;

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            3
        );
    }

    /// A skipped item is not a review backlog, and counting it as one would close the autonomy this
    /// brake is supposed to pace.
    ///
    /// The two kinds share a table and arrive through the same function in `hooks.rs`, which is
    /// exactly why the distinction has to be pinned rather than trusted to a reader: one is work
    /// DONE waiting to be looked at — what §8.4 says the scarce resource is — and the other is work
    /// NOT done, waiting on a decision, which has consumed no attention and produced nothing to
    /// review.
    ///
    /// The arithmetic below is the failure this prevents, at the default limit of 3: a night that
    /// skips three items would refuse the next job, for work nobody did.
    #[tokio::test]
    async fn a_skipped_item_is_not_a_review_backlog() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_pending_proposals(&pool, "project-a", 1).await;
        add_skipped_items(&pool, "project-a", 5).await;

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            1,
            "only the action approval is a review item"
        );
        assert!(
            matches!(
                wip_permits_new_run(&pool, "project-a").await,
                WipDecision::Allow
            ),
            "five skipped items must not spend a limit of three"
        );
    }

    /// A refusal is a record of what the system was stopped from doing, not work waiting on a
    /// person, and since it carries its run's project it is in reach of this count.
    ///
    /// Six because job 26 on 2026-09-13 recorded six. At the default limit of 3, counting them would
    /// make the refusals of one night the reason the next job is refused. The first assertion is
    /// what keeps the rest honest: were the refusals written with no project, they would miss the
    /// per-project filter and pass for a reason that has nothing to do with the exclusion.
    #[tokio::test]
    async fn a_refused_action_is_not_a_review_backlog() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_refused_actions(&pool, "project-a", 6).await;

        let attributed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proposals
             WHERE kind = 'refused-action' AND status = 'pending' AND project_id = 'project-a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(attributed, 6, "the refusals must be the project's own");

        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            0,
            "six refusals are no backlog at all"
        );
        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow,
            "six refusals must not spend a limit of three"
        );

        // And the count is not simply broken: an action approval beside them still counts.
        add_pending_proposals(&pool, "project-a", 1).await;
        assert_eq!(
            open_review_items(&pool, "project-a")
                .await
                .unwrap()
                .proposals,
            1
        );
    }

    #[tokio::test]
    async fn the_global_default_is_on_at_three() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;

        // Migration 0017 ships the brake engaged, so a fresh install is protected by default.
        assert_eq!(global_wip_limit(&pool).await.unwrap(), Some(3));
        assert_eq!(wip_limit(&pool, "project-a").await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn a_project_override_wins_over_the_global_default() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_project(&pool, "project-b").await;
        sqlx::query("UPDATE autopilot_state SET wip_limit = 1 WHERE project_id = 'project-a'")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(wip_limit(&pool, "project-a").await.unwrap(), Some(1));
        // project-b left its override NULL, so it still inherits.
        assert_eq!(wip_limit(&pool, "project-b").await.unwrap(), Some(3));
    }

    #[tokio::test]
    async fn a_null_global_limit_switches_the_brake_off() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        sqlx::query("UPDATE autopilot_global SET wip_limit = NULL")
            .execute(&pool)
            .await
            .unwrap();
        add_pending_proposals(&pool, "project-a", 50).await;

        assert_eq!(wip_limit(&pool, "project-a").await.unwrap(), None);
        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );
    }

    #[tokio::test]
    async fn a_full_queue_defers_and_an_empty_one_allows() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;

        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );

        add_pending_proposals(&pool, "project-a", 3).await;

        let WipDecision::Defer { reason } = wip_permits_new_run(&pool, "project-a").await else {
            panic!("a full queue must defer");
        };
        assert!(reason.contains('3'), "got: {reason}");
    }

    #[tokio::test]
    async fn decided_proposals_release_the_brake() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_pending_proposals(&pool, "project-a", 3).await;
        sqlx::query("UPDATE proposals SET status = 'approved' WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();

        // Self-clearing: reviewing ONE item is enough to let work start again.
        assert_eq!(
            open_review_items(&pool, "project-a").await.unwrap().total(),
            2
        );
        assert_eq!(
            wip_permits_new_run(&pool, "project-a").await,
            WipDecision::Allow
        );
    }

    #[tokio::test]
    async fn the_queue_is_counted_per_project() {
        let pool = test_pool().await;
        add_project(&pool, "project-a").await;
        add_project(&pool, "project-b").await;
        add_pending_proposals(&pool, "project-a", 3).await;

        assert_eq!(
            open_review_items(&pool, "project-b").await.unwrap().total(),
            0
        );
        assert_eq!(
            wip_permits_new_run(&pool, "project-b").await,
            WipDecision::Allow
        );
    }
}
