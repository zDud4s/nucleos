//! §spec autopilot-job-graph
//!
//! The job state machine: a sequence of runs over one shared worktree.
//!
//! A run is one `claude -p` subprocess and therefore one context window, which caps how large a
//! piece of autonomous work can be. A job lifts that cap by running several nodes in sequence —
//! `plan → implement×N → gate → review` — each with a fresh window, sharing state through the
//! worktree on disk rather than through a transcript.
//!
//! The decisions and the I/O are kept on opposite sides of a line, the same way `classifier.rs` is
//! split from `hooks.rs`: `next_step` is pure, so every interesting question about a job ("does a
//! red gate stop the chain?", "where does a resume pick up?") is a table test with no database, no
//! worktree and no subprocess. Everything below it is allowed to be dumb.
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::state::AppState;

/// [`LIVE_STATUSES`] as a SQL list, for the compare-and-set every job status write makes.
///
/// A macro rather than a `const` because `concat!` only takes literals, and sqlx refuses SQL built
/// at run time; `concat!` keeps every statement a `&'static str` while the list lives in one place.
/// `the_status_guard_names_every_live_status` holds it against the constant. Defined above its
/// first use because `macro_rules!` is scoped by position in the file.
macro_rules! live_statuses_sql {
    () => {
        "('planning','implementing','gating','reviewing','awaiting_approval','waiting')"
    };
}

mod brakes;
mod decide;
mod drive;
mod gate;
mod ingest;
mod plan;
mod prompts;
mod spawn;
mod start;
mod store;
mod views;

use self::brakes::*;
pub use self::decide::*;
pub use self::drive::*;
pub use self::gate::*;
pub use self::ingest::*;
pub use self::plan::*;
pub use self::prompts::*;
use self::spawn::*;
pub use self::start::*;
pub use self::store::*;
pub use self::views::*;

#[cfg(test)]
mod tests {
    // The walk tests hold `test_env_lock()` across their awaits on purpose: it serialises mutation
    // of the process-wide `NUCLEOS_WORKTREE_ROOT` override, which is the whole reason it exists. It
    // is a `std::sync::Mutex` because sync `#[test]`s share it, these are `current_thread` tests, and
    // there is no multi-thread runtime here to starve — the same false positive `worktree.rs` and
    // `runs.rs` already carry this allow for.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// Spec A D14: cancelling a job is not the owner saying "not this action", so the parked
    /// node's question expires rather than being written down as a rejection.
    #[tokio::test]
    async fn cancelling_a_parked_node_expires_its_question() {
        let pool = test_pool().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('p', 'x', 'awaiting_approval', 'worktree', '2026-09-27T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let proposal_id = crate::proposals::create_action_approval(
            &pool,
            run_id,
            None,
            Some("p"),
            "Bash",
            "why",
            Some("git push"),
        )
        .await
        .unwrap();

        cancel_parked_node(&pool, run_id).await.unwrap();

        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "expired");
        let run: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(run, "cancelled");
    }

    async fn test_state(pool: sqlx::SqlitePool) -> AppState {
        test_state_with_runner(pool).await.0
    }

    async fn quota_state(pool: sqlx::SqlitePool, resets_at: Option<&str>) -> AppState {
        let now = Utc::now();
        let address = crate::quota::test_support::stub_sidecar(
            crate::quota::test_support::live_answer("claude", "5h", 1.0, resets_at, now),
        )
        .await;
        crate::quota::test_support::arm(&pool, true, 85, 90).await;
        let mut state = test_state(pool).await;
        state.quota = std::sync::Arc::new(crate::quota::QuotaRuntime::new(
            crate::quota::client::QuotaClient::new(&address, "bearer".into()),
            "claude".into(),
        ));
        state
    }

    async fn test_state_with_runner(
        pool: sqlx::SqlitePool,
    ) -> (
        AppState,
        std::sync::Arc<crate::execution::runner::FakeCommandRunner>,
    ) {
        let runner = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        let state = AppState {
            token: crate::auth::Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: runner.clone(),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            run_tails: Default::default(),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };
        (state, runner)
    }

    /// A job with a directory it can hand work through, and no git anywhere near it.
    ///
    /// The reconciler reads `plan.json` off the disk and looks the path up in `worktrees`; neither
    /// step cares whether git ever made the directory, so the tests that exercise the state machine
    /// do not have to build a repository to do it.
    async fn seed_worktree(pool: &sqlx::SqlitePool, job_id: i64, path: &std::path::Path) {
        crate::execution::worktree::record(
            pool,
            crate::execution::worktree::Owner::Job(job_id),
            "project-a",
            &path.to_string_lossy(),
            &path.to_string_lossy(),
            "nucleos/job",
            None,
        )
        .await
        .expect("record the job's worktree");
        std::fs::create_dir_all(path.join(crate::execution::worktree::ARTIFACTS_DIR))
            .expect("create the handoff directory");
    }

    async fn write_plan(worktree: &std::path::Path, contents: &str) {
        std::fs::write(
            worktree
                .join(crate::execution::worktree::ARTIFACTS_DIR)
                .join(PLAN_FILE),
            contents,
        )
        .expect("write a plan");
    }

    /// Adds a node run for a job, in whatever status the test needs it to have landed in.
    async fn seed_node(pool: &sqlx::SqlitePool, job_id: i64, stage: &str, status: &str) -> i64 {
        // The job's own project, not a literal: two `awaiting_approval` worktree runs sharing one
        // project id used to collide on `one_open_worktree_run_per_project`; that index is gone
        // (0053), and the shared id stays because these nodes do belong to one job's project.
        let project_id: String = sqlx::query_scalar("SELECT project_id FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .expect("the job exists");
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at, job_id, stage)
             VALUES (?, 'a node', ?, 'worktree', ?, ?, ?)",
        )
        .bind(project_id)
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(job_id)
        .bind(stage)
        .execute(pool)
        .await
        .expect("insert a node run")
        .last_insert_rowid()
    }

    async fn job_status(pool: &sqlx::SqlitePool, job_id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn item_statuses(pool: &sqlx::SqlitePool, job_id: i64) -> Vec<String> {
        sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
            .bind(job_id)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn feed_kinds(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT kind FROM feed ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// What the feed actually said, for the cases where the wording carries a fact a reader acts
    /// on — a ceiling's number, say — and not only that something happened.
    async fn feed_texts(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// A job of one round, which is what every test written before rounds existed is about.
    fn view(planned: bool, items: &[ItemState], review: ReviewState) -> JobView {
        JobView {
            // Already specced, so that every test written before the spec node
            // existed keeps asking the question it was written to ask.
            specced: true,
            speccing: false,
            planned,
            planning: false,
            items: items.iter().copied().map(ItemView::plain).collect(),
            review,
            rounds: RoundState::default(),
            has_team: false,
            // One, which is what a job without a team has and what makes the batch below come out
            // as a queue of one.
            max_parallel: 1,
        }
    }

    /// The same, for a job that was asked for more than one round.
    fn view_in_round(items: &[ItemState], review: ReviewState, rounds: RoundState) -> JobView {
        JobView {
            // Already specced, so that every test written before the spec node
            // existed keeps asking the question it was written to ask.
            specced: true,
            speccing: false,
            planned: true,
            planning: false,
            items: items.iter().copied().map(ItemView::plain).collect(),
            review,
            rounds,
            has_team: false,
            max_parallel: 1,
        }
    }

    /// A job a team directs.
    ///
    /// Separate from `view` rather than a fourth parameter on it, and that is deliberate: every
    /// test written before this slice describes the job of today, and it should keep saying so
    /// without an edit. The pair of helpers is what makes "did this change the job of today?" a
    /// question the diff answers.
    fn view_with_team(items: &[ItemState], review: ReviewState) -> JobView {
        JobView {
            // Already specced, so that every test written before the spec node
            // existed keeps asking the question it was written to ask.
            specced: true,
            speccing: false,
            has_team: true,
            ..view(true, items, review)
        }
    }

    /// A team's job that may work on more than one item at once.
    ///
    /// Separate again, and for the sibling reason: a team's job with a ceiling of one behaves like
    /// the queue of today apart from the endings, and most of the tests written for the earlier
    /// slices mean exactly that. Raising the number is the thing this slice does, so it says so.
    fn view_with_parallel(items: &[ItemView], max_parallel: usize) -> JobView {
        JobView {
            // Already specced, so that every test written before the spec node
            // existed keeps asking the question it was written to ask.
            specced: true,
            speccing: false,
            planned: true,
            planning: false,
            items: items.to_vec(),
            review: ReviewState::NotWanted,
            rounds: RoundState::default(),
            has_team: true,
            max_parallel,
        }
    }

    /// One item of a team's plan: a state, what it waits for, and what it says it will touch.
    fn item(state: ItemState, depends_on: &[i64], files: &[&str]) -> ItemView {
        ItemView {
            state,
            depends_on: depends_on.to_vec(),
            files: files.iter().map(|file| (*file).to_owned()).collect(),
        }
    }

    /// Two items that declare the same file are never started together.
    ///
    /// The one rule that makes parallel work worth having rather than merely faster to start. Two
    /// nodes writing one file do not conflict while they are apart — each tree builds, each gate
    /// could pass — and they collide at the merge, where the loser pays a resolution run and a
    /// second gate. That is the most expensive step in this design, and this is what avoids it.
    #[test]
    fn two_items_declaring_one_file_do_not_come_out_in_the_same_batch() {
        use ItemState::*;
        let shared = view_with_parallel(
            &[
                item(Pending, &[], &["core/src/job.rs", "core/src/runs.rs"]),
                item(Pending, &[], &["core/src/job.rs"]),
                item(Pending, &[], &["shell/src/App.tsx"]),
            ],
            3,
        );

        assert_eq!(
            next_step(&shared),
            Next::SpawnImplement(vec![0, 2]),
            "item 1 shares job.rs with item 0 and waits; item 2 shares nothing and goes"
        );
    }

    /// And greedy by ordinal, which can lose — said out loud rather than left to be discovered.
    ///
    /// Item 0 declaring both files takes the batch and excludes 1 and 2, which declare one each and
    /// could have run together. A batch of one where a different choice made two. The alternative is
    /// a combinatorial search inside a function that runs every thirty seconds for every live job,
    /// to buy one item's parallelism in a case the director can avoid by splitting differently.
    #[test]
    fn the_fold_is_greedy_by_ordinal_and_can_take_the_worse_pair() {
        use ItemState::*;
        let job = view_with_parallel(
            &[
                item(Pending, &[], &["a.rs", "b.rs"]),
                item(Pending, &[], &["a.rs"]),
                item(Pending, &[], &["b.rs"]),
            ],
            3,
        );

        assert_eq!(next_step(&job), Next::SpawnImplement(vec![0]));
    }

    /// A `Conflicted` item is work, and it does not block itself.
    ///
    /// It is in both lists — the states that hold paths, and the states that owe a run — so a
    /// candidate checked against the whole blocking set including itself would be permanently
    /// ineligible, blocked by its own declaration, with nothing anywhere to say why. The exclusion
    /// of self is what makes a conflict a step rather than a grave.
    #[test]
    fn a_conflicted_item_is_startable_and_does_not_block_itself() {
        use ItemState::*;
        let job = view_with_parallel(&[item(Conflicted, &[], &["a.rs"])], 2);

        assert_eq!(next_step(&job), Next::SpawnImplement(vec![0]));
    }

    /// But it does block a SIBLING that declares the same file, because its tree holds that work.
    #[test]
    fn a_conflicted_item_still_holds_the_paths_it_declared_against_everybody_else() {
        use ItemState::*;
        let job = view_with_parallel(
            &[
                item(Conflicted, &[], &["a.rs"]),
                item(Pending, &[], &["a.rs"]),
                item(Pending, &[], &["b.rs"]),
            ],
            3,
        );

        assert_eq!(next_step(&job), Next::SpawnImplement(vec![0, 2]));
    }

    /// An item still running holds its paths, and does not stop the queue.
    ///
    /// **Continuous start, in one assertion.** The old line was "anything in flight, wait", and it
    /// is what made the queue sequential before any rule did: the first item to start stopped the
    /// search, so a second could not begin until the first had finished. Here item 0 is running and
    /// item 2 starts beside it, while item 1 waits for the file item 0 is holding.
    #[test]
    fn an_item_already_running_holds_its_files_and_does_not_stop_the_queue() {
        use ItemState::*;
        let job = view_with_parallel(
            &[
                item(Running, &[], &["a.rs"]),
                item(Pending, &[], &["a.rs"]),
                item(Pending, &[], &["b.rs"]),
            ],
            3,
        );

        assert_eq!(next_step(&job), Next::SpawnImplement(vec![2]));

        // And without a team the same queue waits, exactly as it always did.
        let sequential = JobView {
            has_team: false,
            ..job
        };
        assert_eq!(next_step(&sequential), Next::Wait);
    }

    /// The ceiling counts what is running as well as what is being started.
    ///
    /// Continuous start is what makes that necessary: this function runs again while earlier items
    /// are still going, so a batch measured against its own length alone would add `max_parallel`
    /// more agents every time it was asked.
    #[test]
    fn the_ceiling_counts_the_batch_and_what_is_already_running() {
        use ItemState::*;
        let running_one = view_with_parallel(
            &[
                item(Running, &[], &["a.rs"]),
                item(Pending, &[], &["b.rs"]),
                item(Pending, &[], &["c.rs"]),
            ],
            2,
        );
        assert_eq!(
            next_step(&running_one),
            Next::SpawnImplement(vec![1]),
            "one running plus one started is the ceiling of two"
        );

        let idle = view_with_parallel(
            &[
                item(Pending, &[], &["a.rs"]),
                item(Pending, &[], &["b.rs"]),
                item(Pending, &[], &["c.rs"]),
            ],
            2,
        );
        assert_eq!(next_step(&idle), Next::SpawnImplement(vec![0, 1]));
    }

    /// A dependency that has not PASSED holds its dependent back, whatever else it has done.
    #[test]
    fn an_item_waits_until_everything_it_depends_on_has_passed() {
        use ItemState::*;
        for held in [Pending, Running, Merging, Conflicted, GateRetriable] {
            let job = view_with_parallel(
                &[item(held, &[], &["a.rs"]), item(Pending, &[0], &["b.rs"])],
                3,
            );
            assert!(
                !batch_of(&job).contains(&1),
                "an item started while its dependency was {held:?}"
            );
        }

        let landed = view_with_parallel(
            &[item(Passed, &[], &["a.rs"]), item(Pending, &[0], &["b.rs"])],
            3,
        );
        assert_eq!(batch_of(&landed), vec![1]);
    }

    /// An exclusive step takes precedence over the batch, and the reason is arithmetic rather than
    /// taste: an item's divergence from the job's branch grows with every minute it waits, and it is
    /// the retry — the most expensive step in this design — that has to absorb it. Draining what
    /// is already in flight before starting more is what keeps that divergence small.
    #[test]
    fn a_merge_or_a_gate_outranks_starting_new_work() {
        use ItemState::*;
        let to_merge = view_with_parallel(
            &[
                item(Implemented, &[], &["a.rs"]),
                item(Pending, &[], &["b.rs"]),
            ],
            3,
        );
        assert_eq!(next_step(&to_merge), Next::MergeItem { ordinal: 0 });

        let to_gate = view_with_parallel(
            &[item(Merging, &[], &["a.rs"]), item(Pending, &[], &["b.rs"])],
            3,
        );
        assert_eq!(next_step(&to_gate), Next::RunGate { ordinal: 0 });
    }

    /// `Implemented` and `Merging` hold their paths too, and this is asked of the fold directly.
    ///
    /// It has to be: the exclusive steps above return before the batch is ever built whenever an
    /// item is in either state, so `next_step` cannot reach a case that would show it. They are in
    /// the blocking set anyway, because it is a statement about which states hold paths — and a
    /// set that is only right because of the order of the checks in front of it is a set that breaks
    /// in silence the day that order moves.
    #[test]
    fn work_waiting_to_merge_holds_its_paths_against_the_batch() {
        use ItemState::*;
        for holder in [Implemented, Merging] {
            let job = view_with_parallel(
                &[
                    item(holder, &[], &["a.rs"]),
                    item(Pending, &[], &["a.rs"]),
                    item(Pending, &[], &["b.rs"]),
                ],
                3,
            );
            assert_eq!(
                batch_of(&job),
                vec![2],
                "an item in {holder:?} did not hold the file it is about to merge"
            );
        }
    }

    /// A queue that owes a run and cannot start one says `stopped`, and never `completed`.
    ///
    /// The shape is narrow and real: item 0 is skipped — a decision somebody was asked for — and
    /// item 1 depends on it and has already run, so the orphan step walks past it (its work is in a
    /// checkout of its own, and whether that work is still worth anything is not a sibling's
    /// verdict). It can never be selected, because its dependency will never pass. Nothing is in
    /// flight. Falling through would close the round and report `completed` over an item nobody
    /// started, which is the one reading that cannot be caught afterwards.
    #[test]
    fn a_queue_that_owes_a_run_and_cannot_start_one_is_stopped_and_not_completed() {
        use ItemState::*;
        let stuck = view_with_parallel(
            &[
                item(Skipped, &[], &["a.rs"]),
                item(Conflicted, &[0], &["b.rs"]),
            ],
            3,
        );

        assert!(batch_of(&stuck).is_empty(), "the fold cannot select it");
        assert_eq!(next_step(&stuck), Next::Finish(Outcome::Stopped));
    }

    /// And the same shape with something in flight WAITS, because the thing that lands next is what
    /// frees it. Told apart from the case above by one item's state, and they are opposite answers.
    #[test]
    fn a_batch_that_comes_out_empty_with_work_in_flight_waits() {
        use ItemState::*;
        let job = view_with_parallel(
            &[
                item(Running, &[], &["a.rs"]),
                item(Pending, &[0], &["b.rs"]),
            ],
            3,
        );

        assert!(batch_of(&job).is_empty());
        assert_eq!(next_step(&job), Next::Wait);
    }

    /// Seeds a job in a given status.
    ///
    /// It always starts `planning` and is moved afterwards, because that is the only status
    /// `insert_job` writes — a caller that could pick one could start a job mid-queue with no queue.
    async fn seed_job(
        pool: &sqlx::SqlitePool,
        project_id: &str,
        status: &str,
    ) -> sqlx::Result<i64> {
        seed_job_in(pool, project_id, status, "/project/a", None).await
    }

    async fn seed_job_in(
        pool: &sqlx::SqlitePool,
        project_id: &str,
        status: &str,
        project_root: &str,
        head_sha: Option<&str>,
    ) -> sqlx::Result<i64> {
        let job_id = insert_job(
            pool,
            &NewJob {
                project_id,
                project_root,
                rule_name: Some("nightly-backlog"),
                prompt: "pull from the todo list and advance what you can",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await?;
        if status != "planning" {
            sqlx::query("UPDATE jobs SET status = ? WHERE id = ?")
                .bind(status)
                .bind(job_id)
                .execute(pool)
                .await?;
        }
        Ok(job_id)
    }

    /// A job with nothing done at all: no brief, no queue, no node in flight.
    ///
    /// `view` answers `specced: true` on purpose — it is the shape every test written before this
    /// node existed is about — so the one shape those tests cannot express is written here.
    fn unspecced_view() -> JobView {
        JobView {
            specced: false,
            speccing: false,
            planned: false,
            planning: false,
            items: Vec::new(),
            review: ReviewState::Pending,
            rounds: RoundState::default(),
            has_team: false,
            max_parallel: 1,
        }
    }

    /// The brief comes before the queue, and while it is being written nothing else starts.
    ///
    /// One walk rather than three assertions, for the reason the walk below is one: what matters is
    /// the ORDER, and each of these three is right in isolation while being reachable at the wrong
    /// moment.
    #[test]
    fn a_job_writes_a_brief_before_it_plans() {
        let fresh = unspecced_view();
        assert_eq!(next_step(&fresh), Next::SpawnSpec);

        let writing = JobView {
            speccing: true,
            ..unspecced_view()
        };
        assert_eq!(
            next_step(&writing),
            Next::Wait,
            "a second spec node must not start on top of the first"
        );

        let written = JobView {
            specced: true,
            ..unspecced_view()
        };
        assert_eq!(next_step(&written), Next::SpawnPlan);
    }

    /// The property that makes this node safe to add to a machine with jobs already running.
    ///
    /// Every job that existed before the spec node did has a queue and has never had a spec run, so
    /// a `specced` computed from the run alone would answer `SpawnSpec` on the first tick after the
    /// upgrade — writing a brief for work whose items are half done, and re-answering it forever
    /// because the node's own output is not what this reads.
    #[test]
    fn um_job_a_meio_nao_recua_para_escrever_uma_spec() {
        let midway = JobView {
            specced: true,
            speccing: false,
            ..view(true, &[ItemState::Pending], ReviewState::Pending)
        };
        assert_eq!(
            next_step(&midway),
            Next::SpawnImplement(vec![0]),
            "a job with a queue is past the moment a brief would have helped"
        );
    }

    /// The node is told what to write and, as firmly, what not to do: a spec node that starts
    /// planning produces a queue nothing reads, and one that starts working leaves edits in a tree
    /// no gate has measured.
    #[test]
    fn a_spec_diz_onde_escrever_e_o_que_nao_fazer() {
        let prompt = spec_prompt("make the roster useful", "/wt/.nucleos");

        assert!(
            prompt.contains("/wt/.nucleos/spec.md"),
            "the node has to be told the exact file, or it writes somewhere nobody reads: {prompt}"
        );
        assert!(
            prompt.contains("make the roster useful"),
            "the request itself has to reach the node: {prompt}"
        );
        assert!(
            prompt.contains("Not this"),
            "the section that stops the work growing is the one this node is for: {prompt}"
        );
        assert!(
            prompt.contains("do not write a plan"),
            "a spec node that plans produces a queue the plan node then overwrites: {prompt}"
        );
        assert!(
            prompt.contains("Do not begin any of the work"),
            "a spec node that works leaves edits no gate has measured: {prompt}"
        );
    }

    /// The planner is sent to the brief only when there is one to read. A sentence naming a missing
    /// file costs the node a turn to find out, which is worse than not mentioning it.
    #[test]
    fn o_planeador_so_e_mandado_ler_a_spec_quando_ela_existe() {
        let with = plan_prompt("t", 5, "/wt/.nucleos", None, true);
        let without = plan_prompt("t", 5, "/wt/.nucleos", None, false);

        assert!(
            with.contains("/wt/.nucleos/spec.md"),
            "the planner was not sent to the brief that exists: {with}"
        );
        assert!(
            !without.contains("spec.md"),
            "the planner was sent to a brief that was never written: {without}"
        );
        assert!(
            with.contains("Decided for you"),
            "the planner has to be told which part of the brief is somebody's decision and which \
             part the spec node took on their behalf: {with}"
        );
    }

    #[test]
    fn a_job_walks_plan_then_each_item_then_review() {
        // One walk rather than five isolated assertions: the sequence is the behaviour, and a
        // transition that is right in isolation can still be reached in the wrong order.
        let mut items = vec![ItemState::Pending, ItemState::Pending];

        assert_eq!(
            next_step(&view(false, &[], ReviewState::Pending)),
            Next::SpawnPlan
        );
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement(vec![0])
        );

        items[0] = ItemState::Implemented;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::RunGate { ordinal: 0 }
        );

        items[0] = ItemState::Passed;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement(vec![1])
        );

        items[1] = ItemState::Passed;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnReview
        );
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Done)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn an_empty_queue_completes_without_implementing_anything() {
        // A planner that found no work had a successful night. Only an absent plan.json is a
        // failure, and that distinction is made before this function ever sees the job.
        assert_eq!(
            next_step(&view(true, &[], ReviewState::Pending)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_failed_item_stops_the_chain_instead_of_moving_on() {
        let items = [ItemState::Failed, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::Finish(Outcome::Failed)
        );
    }

    /// The distinction gate.rs guards and §7 of the spec insists must survive the trip up to the
    /// job: a non-zero exit says the code is broken, a binary that would not start says the
    /// measurement never happened. One of those is a verdict; the other is silence.
    ///
    /// It now shows up as a difference in WHEN the job ends rather than only in what it is called.
    /// A red gate lets the queue carry on — the item's work has been reverted, and the items after
    /// it were never the ones that broke. Silence stops everything, because building on work
    /// nothing has measured is what the gate exists to prevent.
    #[test]
    fn a_failed_gate_and_an_errored_gate_end_the_job_differently() {
        assert_eq!(
            next_step(&view(true, &[ItemState::GateErrored], ReviewState::Pending)),
            Next::Finish(Outcome::GateErrored),
            "a gate that would not run has to stop the chain where it is"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateErrored, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::Finish(Outcome::GateErrored),
            "and it stops it even with work still queued behind it"
        );

        assert_eq!(
            next_step(&view(true, &[ItemState::GateFailed], ReviewState::Pending)),
            Next::SpawnReview,
            "a red gate is no longer where the job stops"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement(vec![1]),
            "the item after a red one is still work worth doing"
        );
    }

    /// What a red gate DOES still decide: the ending.
    ///
    /// The point of letting the queue carry on was never to soften the verdict. A branch carrying an
    /// item the gate rejected is not one to tell somebody is complete, however many items passed
    /// after it — including the case where the red one is not the last.
    #[test]
    fn one_red_gate_makes_the_whole_job_gate_failed_however_it_ends() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Passed, ItemState::GateFailed, ItemState::Passed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::GateFailed)
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Passed],
                ReviewState::NotWanted
            )),
            Next::Finish(Outcome::GateFailed),
            "a job that wanted no review reaches the same verdict by the other door"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Passed, ItemState::Passed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::Completed),
            "and a queue nothing rejected still completes"
        );
    }

    /// Where an implemented item goes next, and it is the team that decides.
    ///
    /// Without a team the work is already on the job's branch — the items write into the one shared
    /// checkout — so there is nothing to bring anywhere and the gate follows immediately. With one,
    /// the work is on a branch of its own and the gate would measure a tree that does not have it.
    #[test]
    fn an_implemented_item_is_merged_first_only_when_a_team_wrote_it_elsewhere() {
        use ItemState::*;
        assert_eq!(
            next_step(&view(true, &[Implemented], ReviewState::NotWanted)),
            Next::RunGate { ordinal: 0 }
        );
        assert_eq!(
            next_step(&view_with_team(&[Implemented], ReviewState::NotWanted)),
            Next::MergeItem { ordinal: 0 }
        );
        // And once it has landed, the gate measures the branch it landed on.
        assert_eq!(
            next_step(&view_with_team(&[Merging], ReviewState::NotWanted)),
            Next::RunGate { ordinal: 0 }
        );
    }

    /// An item waiting on work that will never land is put down, and the job says so.
    ///
    /// Both halves matter. Left `pending` the item is work nothing will start, holding a slot and a
    /// checkout to the end of the job while a person reading the queue is told it is still coming;
    /// and reported `completed`, the job claims to have done work it never attempted.
    #[test]
    fn an_item_whose_dependency_will_not_land_is_put_down_and_counted() {
        use ItemState::*;
        let waiting_on = |state, depends_on: Vec<i64>| ItemView {
            state,
            depends_on,
            files: Vec::new(),
        };

        for blocker in [Failed, GateFailed, GateErrored, Skipped, Orphaned] {
            let job = JobView {
                has_team: true,
                items: vec![ItemView::plain(blocker), waiting_on(Pending, vec![0])],
                ..view_with_team(&[], ReviewState::NotWanted)
            };
            assert_eq!(
                next_step(&job),
                Next::Orphan { ordinal: 1 },
                "a dependency in {blocker:?} still lets its dependent be started"
            );
        }

        // And once it is down, the job does not report itself complete.
        let done = JobView {
            has_team: true,
            items: vec![ItemView::plain(Skipped), ItemView::plain(Orphaned)],
            ..view_with_team(&[], ReviewState::NotWanted)
        };
        assert_eq!(next_step(&done), Next::Finish(Outcome::Failed));
    }

    /// A dependency that merely has not finished yet is not a reason to put anything down.
    #[test]
    fn an_item_waiting_on_work_still_in_flight_is_left_alone() {
        use ItemState::*;
        for blocker in [Pending, Implemented, Passed, Merging, GateRetriable] {
            let job = JobView {
                has_team: true,
                items: vec![
                    ItemView::plain(blocker),
                    ItemView {
                        state: Pending,
                        depends_on: vec![0],
                        files: Vec::new(),
                    },
                ],
                ..view_with_team(&[], ReviewState::NotWanted)
            };
            assert_ne!(
                next_step(&job),
                Next::Orphan { ordinal: 1 },
                "a dependency in {blocker:?} orphaned its dependent"
            );
        }
    }

    /// A skipped item ALONE is still not a failure, which is the reading `ending()` protects.
    ///
    /// What changed is only the case where something was waiting for it: there the job did not do
    /// what was asked, because a person has to decide about the prerequisite first.
    #[test]
    fn a_skipped_item_nothing_waited_for_still_completes_the_job() {
        use ItemState::*;
        assert_eq!(
            next_step(&view(true, &[Passed, Skipped], ReviewState::Done)),
            Next::Finish(Outcome::Completed)
        );
    }

    /// Without a team nothing depends on anything, so no item is ever put down this way.
    #[test]
    fn without_a_team_a_failure_still_stops_the_job_rather_than_orphaning_anything() {
        use ItemState::*;
        assert_eq!(
            next_step(&view(true, &[Failed, Pending], ReviewState::NotWanted)),
            Next::Finish(Outcome::Failed)
        );
    }

    /// A conflicted item is work, not a verdict.
    ///
    /// It is found by the same positional search that finds a pending one, because it owes the same
    /// thing: a run, in the checkout it already has. What that run does differs — it resolves a
    /// staged merge instead of writing a feature — and that is a difference in the prompt. Leaving
    /// `Conflicted` out of the search would put the item down for good over two pieces of work
    /// disagreeing, which is not a judgement about either of them.
    #[test]
    fn a_conflicted_item_is_started_again_rather_than_given_up_on() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(&[Conflicted], ReviewState::NotWanted)),
            Next::SpawnImplement(vec![0])
        );
        // And it does not jump the queue: the same positional search means an earlier item still
        // goes first.
        assert_eq!(
            next_step(&view_with_team(
                &[Pending, Conflicted],
                ReviewState::NotWanted
            )),
            Next::SpawnImplement(vec![0])
        );
    }

    /// A merge that has landed is measured before another item is brought in.
    ///
    /// The order is the design and not an accident of which `if` came first: an item's divergence
    /// from the job's branch grows with every minute it waits, and the retry that has to absorb it
    /// is the most expensive step here. Draining what is in flight keeps that divergence small.
    #[test]
    fn a_landed_merge_is_gated_before_another_item_is_brought_in() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(
                &[Implemented, Merging],
                ReviewState::NotWanted
            )),
            Next::RunGate { ordinal: 1 },
            "the merge already on the branch is measured first"
        );
    }

    /// The precedence between unhappy endings, as a table — because it is a decision, and not a
    /// side effect of the order somebody happened to write the `if`s in.
    #[test]
    fn the_precedence_between_unhappy_endings() {
        use ItemState::*;
        for (items, expected) in [
            (vec![Passed, Passed], None),
            (vec![Passed, GateFailed], Some(Outcome::GateFailed)),
            (vec![Failed, Passed], Some(Outcome::Failed)),
            (vec![GateErrored, Passed], Some(Outcome::GateErrored)),
            // Silence beats a verdict, both ways round: a gate that would not start measured
            // nothing, so every other reading in this queue is in doubt.
            (vec![GateFailed, GateErrored], Some(Outcome::GateErrored)),
            (vec![Failed, GateErrored], Some(Outcome::GateErrored)),
            (vec![Failed, GateFailed], Some(Outcome::Failed)),
            // An orphan alone is impossible — it comes from a dependency that ended badly, and a
            // chain of orphans has a real failure at its root. The arm exists so the table is
            // total; if it ever decides, that invariant broke.
            (vec![Orphaned], Some(Outcome::Failed)),
            (vec![Orphaned, GateFailed], Some(Outcome::GateFailed)),
        ] {
            assert_eq!(
                failed_ending(&view_with_team(&items, ReviewState::NotWanted)),
                expected,
                "{items:?}"
            );
        }
    }

    /// **The regression that matters most in this slice: a job without a team is the job of
    /// today.**
    ///
    /// One row per item state, frozen. If one of these values changes, the change is a change of
    /// behaviour for every job that exists, and it has to be somebody's decision rather than a
    /// side effect of an edit to `failed_ending` or to the short-circuit above it.
    #[test]
    fn without_a_team_every_state_decides_exactly_as_it_did() {
        use ItemState::*;
        for (state, expected) in [
            (Pending, Next::SpawnImplement(vec![0])),
            (Running, Next::Wait),
            (Implemented, Next::RunGate { ordinal: 0 }),
            (Passed, Next::SpawnReview),
            (Failed, Next::Finish(Outcome::Failed)),
            (Cancelled, Next::Finish(Outcome::Cancelled)),
            (GateFailed, Next::SpawnReview),
            (GateRetriable, Next::SpawnImplement(vec![0])),
            (GateErrored, Next::Finish(Outcome::GateErrored)),
            (Skipped, Next::SpawnReview),
        ] {
            assert_eq!(
                next_step(&view(true, &[state], ReviewState::Pending)),
                expected,
                "{state:?} without a team"
            );
        }
    }

    /// With a team, a broken item is an item and not the end of the queue — and a cancelled one
    /// still is the end.
    ///
    /// The asymmetry is the point. `Failed` and `GateErrored` stopped the job because the shared
    /// tree held unmeasured edits the next item would have built on; a team's items each have a
    /// tree, so that reason is gone. `Cancelled` was never about trees.
    #[test]
    fn with_a_team_a_broken_item_does_not_stop_the_queue_but_a_cancelled_one_does() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(&[Failed, Pending], ReviewState::NotWanted)),
            Next::SpawnImplement(vec![1])
        );
        assert_eq!(
            next_step(&view_with_team(
                &[GateErrored, Pending],
                ReviewState::NotWanted
            )),
            Next::SpawnImplement(vec![1])
        );
        assert_eq!(
            next_step(&view_with_team(
                &[Cancelled, Pending],
                ReviewState::NotWanted
            )),
            Next::Finish(Outcome::Cancelled)
        );
    }

    /// And what carrying on must not cost: the job still ends badly.
    ///
    /// Without this, "a broken item does not stop the queue" quietly becomes "a broken item is not
    /// reported", which is the one way this slice could be worse than not doing it at all. Both
    /// doors are checked, because there are two — `ending` for a job of one round, and
    /// `close_the_round` for a job that had rounds left and must not open another over a failure.
    #[test]
    fn with_a_team_the_broken_item_still_decides_the_ending() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(&[Failed, Passed], ReviewState::NotWanted)),
            Next::Finish(Outcome::Failed),
            "through ending()"
        );

        let with_rounds = JobView {
            has_team: true,
            ..view_in_round(
                &[Failed, Passed],
                ReviewState::NotWanted,
                RoundState {
                    max_rounds: 5,
                    ..RoundState::default()
                },
            )
        };
        assert_eq!(
            next_step(&with_rounds),
            Next::Finish(Outcome::Failed),
            "and through close_the_round(), which must not open another round over it"
        );
    }

    /// `Reverted` is settled before anything else moves.
    ///
    /// The state is written between putting the branch back and recording the verdict, so a job
    /// found holding it was cut off in that window (a restart, a crash). Waiting on it would stall
    /// the job for ever, since nothing else will ever move that item; walking past it would report
    /// an item finished with its verdict never written. The settle is the first check of all, ahead
    /// of the short-circuit on a cancelled item, so it also happens for a job that is already
    /// being stopped.
    #[test]
    fn a_reverted_item_is_settled_before_anything_else_moves() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(
                &[Reverted, Pending],
                ReviewState::NotWanted
            )),
            Next::SettleRevert { ordinal: 0 },
            "Reverted must be settled, not waited for and not walked past"
        );
        assert_eq!(
            next_step(&view(true, &[Reverted, Cancelled], ReviewState::NotWanted)),
            Next::SettleRevert { ordinal: 0 },
            "the settle comes before the short-circuit on a cancelled item"
        );
    }

    /// An item whose branch could not be put back ends the job, team or not.
    ///
    /// The tree still holds work a gate called broken, so no later item may start on it. The ending
    /// is `stopped`, not `failed`: nothing is known about the item beyond the fact that the damage
    /// is still there, and a person has to look.
    #[test]
    fn a_revert_failed_item_ends_the_job_with_or_without_a_team() {
        use ItemState::*;
        assert_eq!(
            next_step(&view(
                true,
                &[RevertFailed, Pending],
                ReviewState::NotWanted
            )),
            Next::Finish(Outcome::Stopped),
            "a job with no team keeps going onto a tree that was not put back"
        );
        assert_eq!(
            next_step(&view_with_team(
                &[RevertFailed, Pending],
                ReviewState::NotWanted
            )),
            Next::Finish(Outcome::Stopped),
            "a team's job keeps going onto a tree that was not put back"
        );
    }

    /// `revert_failed` is a status `item_state_from` knows, and the item it names is over.
    ///
    /// The fallback in `item_state_from` is `Pending`, so a status it did not know would come back
    /// as work to START on a tree that was never put back. Terminal means it is not claimable, and
    /// "never lands" means a dependent is orphaned rather than left waiting.
    #[test]
    fn revert_failed_is_read_back_and_never_lands() {
        assert_eq!(
            item_state_from("revert_failed", 0, 0),
            ItemState::RevertFailed
        );
        assert_eq!(
            item_state_from("revert_failed", 3, 1),
            ItemState::RevertFailed,
            "the attempt counts decide nothing for this status"
        );
        assert!(never_lands(ItemState::RevertFailed));
        assert_eq!(ItemState::RevertFailed.claimable_as(), None);
        assert!(
            !holds_paths(ItemState::RevertFailed),
            "a finished item holds no paths that could collide"
        );
    }

    /// A red gate that still has a retry left has not finished with its item.
    ///
    /// The three numbers are read together or not at all. `gate_attempts` counts how many times this
    /// item's gate has gone red; `gate_retries` is what the job allows; neither says anything alone,
    /// and the pair is the only thing that can tell "try again" from "give up". Without this, the
    /// first red gate is terminal for the item — which is today's behaviour, and the behaviour the
    /// retry exists to replace for the work that a missing import or an unupdated test made red.
    #[test]
    fn a_first_red_gate_with_a_retry_left_is_retriable() {
        assert_eq!(
            item_state_from("gate_failed", 1, 1),
            ItemState::GateRetriable
        );
    }

    /// The budget is spent, not renewed.
    ///
    /// The second red gate against a budget of one is where the retry stops being a retry. An item
    /// that kept re-implementing on every red gate would spend the whole night's runs on the one
    /// piece of work that cannot be made to pass, and the items behind it would never be reached.
    #[test]
    fn a_second_red_gate_with_one_retry_is_terminal() {
        assert_eq!(item_state_from("gate_failed", 2, 1), ItemState::GateFailed);
    }

    /// Zero retries has to stay reachable, because it is what every existing job IS.
    ///
    /// `jobs.gate_retries` defaults to 0 in migration 0068 precisely so that no job already
    /// scheduled quietly acquires a retry it never asked for. This is the assertion that says the
    /// unretried path is still the old path: one red gate, one dead item.
    #[test]
    fn zero_retries_is_todays_behaviour() {
        assert_eq!(item_state_from("gate_failed", 1, 0), ItemState::GateFailed);
    }

    /// `EVERY_ITEM_STATE` covers the enum.
    ///
    /// Two halves, and both are needed. The exhaustive `match` makes the compiler speak when a
    /// variant is born; the length makes the LIST speak when somebody adds the variant and leaves
    /// the array alone — without which every test that walks the array quietly tests less than it
    /// says it does.
    #[test]
    fn every_item_state_covers_the_enum() {
        for state in EVERY_ITEM_STATE {
            let _: () = match state {
                ItemState::Pending
                | ItemState::Running
                | ItemState::Implemented
                | ItemState::Passed
                | ItemState::Failed
                | ItemState::Cancelled
                | ItemState::GateFailed
                | ItemState::GateRetriable
                | ItemState::GateErrored
                | ItemState::Skipped
                | ItemState::Merging
                | ItemState::Conflicted
                | ItemState::Reverted
                | ItemState::RevertFailed
                | ItemState::Orphaned
                | ItemState::Superseded => (),
            };
        }
    }

    /// `LIVE_ITEM_STATUSES` is exactly the set of stored statuses that mean an item may still run.
    ///
    /// The slot sweep reads that list, so a state missing from it is a checkout and a slot freed out
    /// from under work still in flight, and a state wrongly in it is a slot held for ever. The
    /// classification below is an exhaustive `match`, which is what forces a new variant to be
    /// judged rather than defaulted.
    ///
    /// `gate_failed` is stored by two states — `GateRetriable`, which may still run, and
    /// `GateFailed`, which is over — so it belongs in the list on the strength of the first. That is
    /// the one place the list is deliberately generous, and the doc on the constant says why.
    #[test]
    fn every_unfinished_state_is_a_status_the_sweep_spares() {
        fn may_still_run(state: ItemState) -> bool {
            match state {
                ItemState::Pending
                | ItemState::Running
                | ItemState::Implemented
                | ItemState::GateRetriable
                | ItemState::Merging
                | ItemState::Conflicted
                | ItemState::Reverted => true,
                ItemState::Passed
                | ItemState::Failed
                | ItemState::Cancelled
                | ItemState::GateFailed
                | ItemState::GateErrored
                | ItemState::Skipped
                | ItemState::RevertFailed
                | ItemState::Orphaned
                | ItemState::Superseded => false,
            }
        }

        // Read `(0, 1)` — an unspent retry — so that `gate_failed` shows up as the state that may
        // still run, which is why it is on the list.
        let mut spared: Vec<&str> = LIVE_ITEM_STATUSES
            .into_iter()
            .filter(|status| may_still_run(item_state_from(status, 0, 1)))
            .collect();
        spared.sort_unstable();
        let mut listed: Vec<&str> = LIVE_ITEM_STATUSES.into_iter().collect();
        listed.sort_unstable();
        assert_eq!(
            spared, listed,
            "the sweep spares a status that means the item is over"
        );

        // And nothing that may still run is left off it. Both halves are needed: the check above
        // catches a status that should not be spared, this one catches a state nothing spares.
        for state in EVERY_ITEM_STATE {
            if !may_still_run(state) {
                continue;
            }
            let stored = match state {
                ItemState::Pending => "pending",
                ItemState::Running => "running",
                ItemState::Implemented => "implemented",
                ItemState::GateRetriable => "gate_failed",
                ItemState::Merging => "merging",
                ItemState::Conflicted => "conflicted",
                ItemState::Reverted => "reverted",
                other => unreachable!("{other:?} does not still run"),
            };
            assert!(
                LIVE_ITEM_STATUSES.contains(&stored),
                "{state:?} is stored as `{stored}`, which the sweep would collect"
            );
        }
    }

    /// The round trip between `claimable_as` and `item_state_from`, over every state that names a
    /// status at all.
    ///
    /// This is the pair `claimable_as` documents itself as being kept against, made into something
    /// that fails. `(0, 1)` — no attempt spent, one retry allowed — is the reading under which
    /// `gate_failed` means `GateRetriable`, which is the state that actually names that string.
    #[test]
    fn every_claimable_state_reads_back_as_itself() {
        for state in EVERY_ITEM_STATE {
            let Some(status) = state.claimable_as() else {
                continue;
            };
            assert_eq!(
                item_state_from(status, 0, 1),
                state,
                "{state:?} claims itself as `{status}`, which reads back as something else"
            );
        }
    }

    /// The other half, and the expensive one: a state whose stored status nobody taught
    /// `item_state_from` falls into the `_ =>` and is handed back to the queue as `Pending`.
    ///
    /// The item then restarts, in a tree that already holds its work, with nothing anywhere saying
    /// why. That is the failure this repository chose when it made the fallback "not finished", and
    /// it is only the right choice while the list of statuses is complete.
    #[test]
    fn no_parallel_state_is_read_as_pending() {
        for (status, expected) in [
            ("merging", ItemState::Merging),
            ("conflicted", ItemState::Conflicted),
            ("reverted", ItemState::Reverted),
            ("revert_failed", ItemState::RevertFailed),
            ("orphaned", ItemState::Orphaned),
        ] {
            assert_eq!(
                item_state_from(status, 0, 0),
                expected,
                "`{status}` fell through to the `_ =>` arm"
            );
        }
    }

    /// A retriable item is work to do, and the work is the SAME item.
    ///
    /// The number in `SpawnImplement` is what the caller looks the item up by, so an off-by-one here
    /// would re-implement the wrong item — or, at the end of a queue, implement one that does not
    /// exist. Item 1 went red, so item 1 is what runs again; the passed item before it is finished
    /// with and must not be touched.
    #[test]
    fn a_retriable_item_reruns_the_same_ordinal() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Passed, ItemState::GateRetriable],
                ReviewState::Pending
            )),
            Next::SpawnImplement(vec![1])
        );
    }

    /// Position decides which item runs next, not how the item got to be work.
    ///
    /// A retriable item is picked by the same positional search that picks a pending one, so an
    /// earlier retriable item outranks a later pending one. Running the pending item first would
    /// build it on a tree that still stands where the red gate left it, and the next gate could no
    /// longer say which of the two items broke it — which is the whole reason gating happens between
    /// items rather than at the end.
    #[test]
    fn a_retriable_item_is_taken_before_a_later_pending_one() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateRetriable, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement(vec![0])
        );
    }

    /// An item that has spent its retries is walked past, exactly as it is today.
    ///
    /// Said again beside the retry tests rather than left to
    /// `a_failed_gate_and_an_errored_gate_end_the_job_differently`, because this is the assertion a
    /// new state most easily breaks: `GateFailed` arriving at the positional search must still be
    /// nothing to the queue, or a job whose first item ran out of retries would sit re-implementing
    /// it and never reach the four items behind it that were never the ones that broke.
    #[test]
    fn an_item_out_of_retries_lets_the_queue_carry_on() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement(vec![1])
        );
    }

    /// The retry moves WHEN the verdict is decided, never what it is.
    ///
    /// A branch carrying an item the gate rejected is not one to tell somebody is complete, and
    /// spending a retry on it does not change that — it only means the rejection is final rather
    /// than first. A retry that softened the ending would be worse than no retry at all: the night
    /// would report `completed` over work no gate ever agreed with.
    #[test]
    fn a_job_ending_on_a_red_gate_still_reports_gate_failed() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Passed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::GateFailed)
        );
    }

    /// Silence is not retriable, whatever the budget says.
    ///
    /// The asymmetry the whole two-state gate design rests on. A non-zero exit is a verdict about
    /// the code and is worth another attempt; a gate that would not run measured nothing, so there
    /// is nothing to attempt again — a second implement node would be told "the gate said" and given
    /// the silence. Buying past it with a retry budget would mean building on work nothing has
    /// looked at, which is the one thing the gate exists to prevent.
    #[test]
    fn a_gate_that_could_not_run_still_ends_the_job() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateErrored, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::Finish(Outcome::GateErrored)
        );
        assert_eq!(
            item_state_from("gate_errored", 1, 1),
            ItemState::GateErrored,
            "a retry left over cannot turn silence into something to retry"
        );
        assert_eq!(
            item_state_from("gate_errored", 1, 3),
            ItemState::GateErrored,
            "and neither can the largest budget the ceiling allows"
        );
    }

    /// A skipped item is walked past exactly like a passed one, and colours nothing.
    ///
    /// Both halves matter. If it blocked, one unrecognised command would park the night — which is
    /// the state this chunk exists to leave. If it made the job `gate_failed`, then a job that did
    /// everything it was ALLOWED to do would be reported as broken, and the word would stop meaning
    /// anything to whoever reads it in the morning.
    #[test]
    fn a_skipped_item_neither_blocks_the_queue_nor_colours_the_ending() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Skipped, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement(vec![1]),
            "the queue has to move past it"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Skipped, ItemState::Skipped],
                ReviewState::Done
            )),
            Next::Finish(Outcome::Completed),
            "a job whose every item was skipped still did what it was allowed to do"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Skipped, ItemState::GateFailed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::GateFailed),
            "and it does not hide a red gate beside it"
        );
    }

    #[test]
    fn work_resumes_at_the_first_unfinished_item_not_the_first_item() {
        // What `stage_cursor` exists for. Resuming at zero would redo work the gate already passed,
        // on a worktree that still holds it.
        let items = [ItemState::Passed, ItemState::Passed, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement(vec![2])
        );
    }

    #[test]
    fn a_running_node_is_waited_for_rather_than_raced() {
        let items = [ItemState::Running, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::Wait
        );
    }

    #[test]
    fn a_job_without_review_completes_at_the_last_item() {
        let items = [ItemState::Passed];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::NotWanted)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn an_absent_plan_is_a_planning_failure_not_an_empty_queue() {
        // The pair that matters most in this module. "The planner produced nothing" is a failure;
        // "the planner found no work" is a success. Collapsing them would make every crashed plan
        // node look like a quiet, successful night, and teach the reader to ignore the feed.
        assert!(matches!(parse_plan(None, 5, None), Err(PlanError::Absent)));
    }

    /// The replan node's other answer, read out of the same file by the same parser.
    ///
    /// One shape for both nodes rather than two, because the failure mode of two is a plan node's
    /// file becoming unreadable to the replan parser the day somebody adds a key to one of them.
    #[test]
    fn a_replan_can_say_the_work_is_over() {
        let done = br#"{"done": true, "why": "the task is met and the suite is green"}"#;
        let plan =
            parse_plan(Some(done), 5, None).expect("a done verdict is a result, not an error");
        assert_eq!(
            plan.done.as_deref(),
            Some("the task is met and the suite is green")
        );
        assert!(plan.items.is_empty());

        // Silence about the reason is not a parse failure: the verdict is the load-bearing half, and
        // failing a job over a missing sentence would throw away the answer to keep the explanation.
        let terse = br#"{"done": true}"#;
        assert_eq!(
            parse_plan(Some(terse), 5, None).unwrap().done.as_deref(),
            Some("no reason given")
        );

        // `done` wins over items alongside it. Honouring the items would queue work the node just
        // said was unnecessary; calling the pair malformed would fail a job over redundancy.
        let both = br#"{"done": true, "items": [{"description": "one more thing"}]}"#;
        assert!(parse_plan(Some(both), 5, None).unwrap().done.is_some());
    }

    /// The distinction ending #1 and ending #3 are built on. An empty queue is a round that produced
    /// nothing and might not be the last; `done` is a claim the node is making. One ends the job now,
    /// the other feeds a counter that ends it after two.
    #[test]
    fn an_empty_queue_is_not_a_claim_that_the_work_is_over() {
        let plan = parse_plan(Some(br#"{"items": []}"#), 5, None).unwrap();
        assert!(plan.items.is_empty());
        assert_eq!(plan.done, None);

        // A plan node's file never carries the key at all, and must not read as a failure.
        let ordinary = br#"{"items": [{"description": "a"}]}"#;
        assert_eq!(parse_plan(Some(ordinary), 5, None).unwrap().done, None);
    }

    /// The replan node cannot do its job without the archives, and the prompt has to say so either
    /// way. Without them it reproposes round 1, no round ever comes back empty, and the "until it
    /// dries up" brake never fires — leaving `max_rounds` as the only thing between the job and its
    /// budget.
    #[test]
    fn the_replan_prompt_carries_what_the_earlier_rounds_tried() {
        let with = replan_prompt(
            "add shout and whisper",
            2,
            &["plan-0.json".to_owned(), "plan-1.json".to_owned()],
            "/wt/.nucleos",
            None,
            &[],
        );
        assert!(with.contains("plan-0.json, plan-1.json"));
        // Nothing broke in this round, so the instruction keeps the flat form it always had.
        assert!(with.contains("Do not repropose any of it."));
        assert!(with.contains("/wt/.nucleos/plan.json"));
        assert!(with.contains("add shout and whisper"));
        // Ending #1 is one node; ending #4 costs a whole round to reach the same place, so the node
        // is told which one to prefer.
        assert!(with.contains("\"done\": true"));

        // No archives is a reachable state — the copy can fail — and the honest thing is to say so.
        // Naming a file that is not there sends the node looking, and what it finds is nothing.
        let without = replan_prompt("t", 1, &[], "/wt/.nucleos", None, &[]);
        assert!(without.contains("could not be recovered"));
        assert!(!without.contains("Do not repropose"));
    }

    /// Every node that could reach for git is told the history is already handled.
    ///
    /// Job 12 on 2026-08-08 is why. A task that said "I want to review each one on its own" — about
    /// how to split the work — became "commit X and Y as two separate commits" in all four items,
    /// every implement node reached for `git commit`, and under the zero-approval policy the night
    /// runs on, all four items were skipped with nothing built. The command was judged correctly;
    /// what was wrong is that the item asked for it.
    #[test]
    fn every_node_that_could_reach_for_git_is_told_the_job_commits() {
        let plan = plan_prompt("add eight modules", 5, "/wt/.nucleos", None, false);
        let replan = replan_prompt("add eight modules", 1, &[], "/wt/.nucleos", None, &[]);
        // No hint, because what this pins is the paragraph every node gets regardless of one.
        let implement = implement_prompt("write shout.py", 0, 4, "/wt/.nucleos", &[], None, None);

        for prompt in [&plan, &replan] {
            assert!(
                prompt.contains("The job commits the tree itself"),
                "a planning node was not told who owns the history"
            );
        }
        assert!(implement.contains("the job commits for you"));
        // Said as what happens, not only as a prohibition: a node told only "do not commit" invents
        // a way around it.
        assert!(plan.contains("already done for you"));
    }

    /// The nodes that only LOOK are told how to look, because the way they reached for by default
    /// was the one thing that could stop them.
    ///
    /// Across four jobs on 2026-08-08 every inspecting node reached for a shell loop or a pipeline —
    /// `for f in …; do cat "$f"; done` three times, `git reflog`, `ls -la && … && find … | sort`.
    /// The classifier reads a line and not a program, so none was recognised, and each one ended its
    /// node. `Read`, `Grep` and `Glob` need nobody and were there the whole time.
    #[test]
    fn the_nodes_that_only_look_are_told_what_to_look_with() {
        let replan = replan_prompt("t", 1, &[], "/wt/.nucleos", None, &[]);
        let review = review_prompt("t", Some("abc123"), "/wt/.nucleos", false, false);

        for prompt in [&replan, &review] {
            assert!(prompt.contains("Read, Grep and Glob"));
            // The consequence, not just the rule: this node is what the job spends to get an
            // answer, and a node told only "do not use shell" has no reason to care.
            assert!(prompt.contains("running unattended"));
        }
    }

    #[test]
    fn an_unparseable_plan_is_a_planning_failure() {
        let garbage = br#"{"items": [ truncated"#;
        assert!(matches!(
            parse_plan(Some(garbage), 5, None),
            Err(PlanError::Unreadable(_))
        ));
    }

    #[test]
    fn an_empty_item_list_is_a_legitimate_result() {
        let empty = br#"{"items": []}"#;
        let plan =
            parse_plan(Some(empty), 5, None).expect("an empty queue is a result, not an error");
        assert!(plan.items.is_empty());
        assert_eq!(plan.dropped, 0);
    }

    #[test]
    fn a_plan_over_max_items_is_truncated_and_says_how_much() {
        let seven = br#"{"items":[{"description":"a"},{"description":"b"},{"description":"c"},
                                  {"description":"d"},{"description":"e"},{"description":"f"},
                                  {"description":"g"}]}"#;
        let plan =
            parse_plan(Some(seven), 5, None).expect("an oversized plan is truncated, not rejected");

        assert_eq!(plan.items.len(), 5);
        // The queue is items, not strings: a description with no file hint is still a whole item,
        // and truncation cuts items rather than descriptions.
        assert_eq!(
            plan.items[0],
            PlannedItem {
                description: "a".to_owned(),
                files: Vec::new(),
                depends_on: Vec::new(),
                agent_id: None,
            }
        );
        // Reported, never silent: a queue quietly cut from seven to five reads downstream as "the
        // planner found five things", which is a different and wrong statement about the work.
        assert_eq!(plan.dropped, 2);
    }

    /// A team's rules, with a roster of two. Every graph test is held to these.
    fn directed(first_ordinal: usize) -> Graph {
        Graph {
            first_ordinal,
            roster: vec![
                TeamMate {
                    id: "ana".to_owned(),
                    speciality: "migrations".to_owned(),
                },
                TeamMate {
                    id: "bo".to_owned(),
                    speciality: "the shell".to_owned(),
                },
            ],
        }
    }

    /// Every way a team's plan is refused, each named and each a different mistake.
    ///
    /// The whole plan is refused for one bad row rather than the row dropped, and that is the
    /// decision to argue with. A dropped row leaves a queue that looks complete and is missing work
    /// somebody asked for; a refusal costs a round and says which item and why. A director that
    /// cannot answer the three questions should stop being run in parallel, not be run in parallel
    /// badly.
    ///
    /// **Every case names all three keys except the one it is about**, which is what stops this
    /// being a list of plans that merely happen to be refused. A row missing two keys is refused by
    /// whichever check runs first, and the case for the second key would pass with that check
    /// deleted.
    #[test]
    fn a_teams_plan_is_refused_for_a_graph_that_does_not_hold_up() {
        let graph = directed(0);
        for (why, json) in [
            (
                "no files",
                br#"{"items":[{"description":"x","depends_on":[],"agent_id":"ana"}]}"#.as_slice(),
            ),
            (
                "empty files",
                br#"{"items":[{"description":"x","files":[],"depends_on":[],
                              "agent_id":"ana"}]}"#
                    .as_slice(),
            ),
            (
                "no depends_on",
                br#"{"items":[{"description":"x","files":["a.rs"],"agent_id":"ana"}]}"#.as_slice(),
            ),
            (
                "an ordinal that is not an earlier item",
                br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[7],
                              "agent_id":"ana"}]}"#
                    .as_slice(),
            ),
            (
                "a dependency on itself",
                br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[0],
                              "agent_id":"ana"}]}"#
                    .as_slice(),
            ),
            (
                "a cycle, which pointing forwards already is",
                br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[1],
                              "agent_id":"ana"},
                              {"description":"y","files":["b.rs"],"depends_on":[0],
                              "agent_id":"bo"}]}"#
                    .as_slice(),
            ),
            (
                "no agent_id",
                br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[]}]}"#.as_slice(),
            ),
            (
                "an agent who is not on this team",
                br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[],
                              "agent_id":"someone-else"}]}"#
                    .as_slice(),
            ),
        ] {
            assert!(
                matches!(
                    parse_plan(Some(json), 5, Some(&graph)),
                    Err(PlanError::Rejected(_))
                ),
                "a plan with {why} was accepted"
            );
        }

        // And the shape that holds up: a backwards edge, an explicit "nothing", and two members of
        // the roster each given something.
        let good = br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[],
                                  "agent_id":"ana"},
                                 {"description":"y","files":["b.rs"],"depends_on":[0],
                                  "agent_id":"bo"}]}"#;
        let plan = parse_plan(Some(good), 5, Some(&graph)).expect("a plan that holds up");
        assert_eq!(plan.items[1].depends_on, vec![0]);
        assert_eq!(plan.items[0].depends_on, Vec::<i64>::new());
        assert_eq!(plan.items[0].agent_id.as_deref(), Some("ana"));
        assert_eq!(plan.items[1].agent_id.as_deref(), Some("bo"));
    }

    /// A refusal names who there IS, because "no such member" without the roster costs a whole
    /// round to act on: the director's next answer would be another guess at a name.
    ///
    /// The empty roster gets its own sentence, and that is not tidiness. It is a different problem
    /// with a different owner — the director answered as well as it could and there was nobody to
    /// answer with — and told as "`ana` is not on this team" it reads as the director's fault.
    #[test]
    fn a_refusal_over_an_agent_says_who_is_on_the_team() {
        let json = br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[],
                                  "agent_id":"nobody"}]}"#;
        let Err(PlanError::Rejected(said)) = parse_plan(Some(json), 5, Some(&directed(0))) else {
            panic!("a plan naming a stranger was accepted");
        };
        assert!(said.contains("ana"), "{said}");
        assert!(
            said.contains("migrations"),
            "the speciality is what to choose on: {said}"
        );
        assert!(said.contains("bo"), "{said}");

        let deserted = Graph {
            first_ordinal: 0,
            roster: Vec::new(),
        };
        let Err(PlanError::Rejected(said)) = parse_plan(Some(json), 5, Some(&deserted)) else {
            panic!("a plan under an empty roster was accepted");
        };
        assert!(
            said.contains("no members"),
            "an empty roster is the owner's problem, not the director's: {said}"
        );
    }

    /// Ordinals continue across rounds, so a later round's `depends_on` is checked against where
    /// its own items start — not against zero.
    ///
    /// This is the failure that would otherwise be invisible. A director on round 2 writing
    /// `"depends_on": [0]` to mean "the first item I just listed" names a FINISHED item of round 1.
    /// Nothing about that is ill-formed; ordinal 0 exists. Checking it against the round's own first
    /// ordinal is what turns a plan that reads fine into one that is refused with a reason.
    #[test]
    fn a_later_rounds_dependencies_are_checked_against_that_rounds_own_ordinals() {
        let round_two = directed(4);
        // Item 4 depending on item 3: an item of round 1, which is legitimate and useful.
        let across = br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[3],
                                    "agent_id":"ana"}]}"#;
        assert!(parse_plan(Some(across), 5, Some(&round_two)).is_ok());

        // The same plan on round 1 would be nonsense, and is refused there.
        assert!(matches!(
            parse_plan(Some(across), 5, Some(&directed(0))),
            Err(PlanError::Rejected(_))
        ));

        // And an item of round 2 cannot depend on its own successor.
        let forwards = br#"{"items":[{"description":"x","files":["a.rs"],"depends_on":[5],
                                      "agent_id":"ana"},
                                     {"description":"y","files":["b.rs"],"depends_on":[],
                                      "agent_id":"bo"}]}"#;
        assert!(matches!(
            parse_plan(Some(forwards), 5, Some(&round_two)),
            Err(PlanError::Rejected(_))
        ));
    }

    /// A job without a team reads exactly what it always read.
    ///
    /// The strictness is affordable only because it is scoped. Every `plan.json` ever written omits
    /// all three keys, and reading one has to stay a plan rather than become a refusal.
    #[test]
    fn without_a_team_a_plan_missing_both_keys_is_still_a_plan() {
        let old = br#"{"items":[{"description":"x"},{"description":"y","files":["b.rs"]}]}"#;
        let plan = parse_plan(Some(old), 5, None).expect("a plan written before the graph existed");
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items[0].files, Vec::<String>::new());
        assert_eq!(plan.items[1].depends_on, Vec::<i64>::new());
        assert_eq!(
            plan.items[0].agent_id, None,
            "there is nobody to name, and inventing one would change every job in the repository"
        );
    }

    /// The plan node is told the number its `depends_on` will be checked against.
    ///
    /// Without it the rule is unfollowable: the director has no way to know that the items it is
    /// about to list are numbered from 4, so any dependency it writes is a guess.
    #[test]
    fn a_teams_plan_prompt_names_the_ordinal_this_round_starts_at() {
        let prompt = plan_prompt("t", 5, "/wt/.nucleos", Some(&directed(4)), false);
        assert!(prompt.contains("NUMBERED FROM 4"), "{prompt}");
        assert!(prompt.contains("depends_on"));
        assert!(prompt.contains("EARLIER ordinal"));

        let replan = replan_prompt("t", 2, &[], "/wt/.nucleos", Some(&directed(9)), &[]);
        assert!(replan.contains("numbered from 9"), "{replan}");
        assert!(replan.contains("depends_on"));

        // And a job without a team is told none of it, because none of it applies.
        let plain = plan_prompt("t", 5, "/wt/.nucleos", None, false);
        assert!(!plain.contains("depends_on"));
        assert!(!replan_prompt("t", 2, &[], "/wt/.nucleos", None, &[]).contains("depends_on"));
    }

    /// The hint is the planner's, and it is optional at both ends: a planner that names files is
    /// believed, a planner that names none is not treated as having named an empty set of them.
    /// The absent case is not hypothetical — every plan.json written before this column existed
    /// looks exactly like it, and reading one has to stay a plan rather than a parse failure.
    #[test]
    fn parse_plan_reads_optional_file_hints_and_tolerates_their_absence() {
        let mixed = br#"{"items":[{"description":"x","files":["a.rs","b.rs"]},
                                  {"description":"y"}]}"#;
        let plan = parse_plan(Some(mixed), 5, None).expect("a plan without hints is still a plan");

        assert_eq!(
            plan.items,
            vec![
                PlannedItem {
                    description: "x".to_owned(),
                    files: vec!["a.rs".to_owned(), "b.rs".to_owned()],
                    depends_on: Vec::new(),
                    agent_id: None,
                },
                PlannedItem {
                    description: "y".to_owned(),
                    files: Vec::new(),
                    depends_on: Vec::new(),
                    agent_id: None,
                },
            ]
        );
        assert_eq!(plan.dropped, 0);
    }

    /// **A directed job says so, everywhere a job is read.**
    ///
    /// Three routes and one join, which is the point of the test rather than an economy of it: the
    /// summary columns were four copies of one list until `SUMMARY_SQL`, and `query_as` matches a
    /// row to a struct by NAME at run time — a copy that missed a column compiles and then fails on
    /// whichever route was forgotten, the first time somebody calls it.
    ///
    /// The name and the ceiling travel beside the id because neither is derivable from it. `teams.id`
    /// is slugged from the name and then outlives it, so a shell holding only the id would render a
    /// slug or fetch the whole catalogue; and the ceiling belongs to the team, so it has to be read
    /// when the job is looked at rather than copied onto `jobs` when it started.
    #[tokio::test]
    async fn a_directed_job_says_which_team_directs_it_and_how_wide_it_may_go() {
        let pool = test_pool().await;
        seed_agent(&pool, "dir", "directing", "lead").await;
        seed_crew(&pool, "crew", "dir", &[]).await;
        let alone = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let directed = seed_job(&pool, "project-b", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(directed)
            .execute(&pool)
            .await
            .unwrap();

        let view = detail(&pool, directed).await.unwrap().expect("it exists");
        assert_eq!(view.job.team_id.as_deref(), Some("crew"));
        assert_eq!(view.job.team_name.as_deref(), Some("Team crew"));
        assert_eq!(view.job.team_max_parallel, Some(2));

        // The LEFT of the join, and the case that is nearly every job: an undirected one still
        // comes back. An inner join here would have dropped it from every listing in the product.
        let plain = detail(&pool, alone).await.unwrap().expect("it exists");
        assert_eq!(plain.job.team_id, None);
        assert_eq!(plain.job.team_name, None);
        assert_eq!(plain.job.team_max_parallel, None);

        let listed = list(&pool, Some("project-b"), 20).await.unwrap();
        assert_eq!(listed[0].team_name.as_deref(), Some("Team crew"));
        let live = list_live(&pool, None, 20).await.unwrap();
        assert_eq!(live.len(), 2, "both jobs are live");
        let names: Vec<Option<&str>> = live.iter().map(|job| job.team_name.as_deref()).collect();
        assert!(names.contains(&Some("Team crew")) && names.contains(&None));
    }

    /// **An item of a directed job says who has it, what it waits for, and what it will touch.**
    ///
    /// The three facts that only exist once a job has a team, and the three that until now lived
    /// nowhere but inside the fold. A person watching two of five items move could see WHICH two and
    /// never why those two — the queue was a list of statuses with the reasoning removed.
    ///
    /// `agent_name` and not just the id, for the reason `agents.name` exists at all. And an
    /// undirected job's items come back with all three empty, which is what `0103` and `0102` say
    /// their NULL means: nobody was asked.
    #[tokio::test]
    async fn an_item_of_a_directed_job_says_who_has_it_and_why_it_may_start() {
        let pool = test_pool().await;
        seed_agent(&pool, "dir", "directing", "lead").await;
        seed_agent(&pool, "ana", "the database layer", "you are careful").await;
        seed_crew(&pool, "crew", "dir", &["ana"]).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["passed", "running"]).await;
        sqlx::query(
            "UPDATE job_items SET agent_id = 'ana', depends_on = '[]', files = '[\"a.rs\"]'
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE job_items SET agent_id = 'dir', depends_on = '[0]', files = '[\"b.rs\",\"c.rs\"]'
             WHERE job_id = ? AND ordinal = 1",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();

        let view = detail(&pool, job_id).await.unwrap().expect("it exists");

        assert_eq!(view.items[0].agent_id.as_deref(), Some("ana"));
        assert_eq!(view.items[0].agent_name.as_deref(), Some("Agent ana"));
        // An empty `depends_on` on a DIRECTED job is a real answer — this item waits for nobody —
        // and `team_id` beside it is what separates that from the NULL of a job nobody directs.
        assert_eq!(view.items[0].depends_on, Vec::<i64>::new());
        assert_eq!(view.items[0].files, vec!["a.rs".to_owned()]);
        assert_eq!(view.items[1].agent_name.as_deref(), Some("Agent dir"));
        assert_eq!(view.items[1].depends_on, vec![0]);
        assert_eq!(
            view.items[1].files,
            vec!["b.rs".to_owned(), "c.rs".to_owned()]
        );

        // The LEFT of the other join. Every item of every job without a team has a NULL `agent_id`,
        // and an inner join would have returned an empty queue for all of them.
        let plain_job = seed_job(&pool, "project-b", "implementing").await.unwrap();
        seed_items(&pool, plain_job, &["pending"]).await;
        let plain = detail(&pool, plain_job).await.unwrap().expect("it exists");
        assert_eq!(plain.items.len(), 1);
        assert_eq!(plain.items[0].agent_id, None);
        assert_eq!(plain.items[0].agent_name, None);
        assert!(plain.items[0].depends_on.is_empty());
        assert!(plain.items[0].files.is_empty());
    }

    async fn seed_items(pool: &sqlx::SqlitePool, job_id: i64, statuses: &[&str]) {
        for (ordinal, status) in statuses.iter().enumerate() {
            sqlx::query(
                "INSERT INTO job_items (job_id, ordinal, description, status)
                 VALUES (?, ?, 'an item', ?)",
            )
            .bind(job_id)
            .bind(ordinal as i64)
            .bind(status)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn seed_active_knowledge(pool: &sqlx::SqlitePool, title: &str) -> i64 {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'project', 'project-a', 'owner', 'memory', ?, 'body', 'active', ?)",
        )
        .bind(title)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_shown_item_trace(
        pool: &sqlx::SqlitePool,
        job_id: i64,
        ordinal: i64,
        run_id: i64,
    ) -> i64 {
        let knowledge_id = seed_active_knowledge(pool, "the cursor helper lives in ui").await;
        let item_id: i64 =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = ?")
                .bind(job_id)
                .bind(ordinal)
                .fetch_one(pool)
                .await
                .unwrap();
        crate::brief::record(
            pool,
            run_id,
            Some(item_id),
            &[crate::knowledge::Scored {
                knowledge_id,
                shown: true,
                s_fts: 0.0,
                s_sim: 0.0,
                s_scope: 0.0,
                s_structure: 0.0,
                s_recency: 0.0,
                s_use: 0.0,
                score: 0.0,
            }],
        )
        .await
        .unwrap();
        knowledge_id
    }

    async fn knowledge_counts(pool: &sqlx::SqlitePool, knowledge_id: i64) -> (i64, i64, i64) {
        sqlx::query_as("SELECT shown_count, outcome_count, green_count FROM knowledge WHERE id = ?")
            .bind(knowledge_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn trace_credited_at(
        pool: &sqlx::SqlitePool,
        run_id: i64,
        knowledge_id: i64,
    ) -> Option<String> {
        sqlx::query_scalar(
            "SELECT credited_at FROM run_knowledge WHERE run_id = ? AND knowledge_id = ?",
        )
        .bind(run_id)
        .bind(knowledge_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// Undoing a skipped node happens in the tree that node wrote in, and nowhere else.
    ///
    /// Three cases, and the third is the one worth the test. A node with an item of its own is put
    /// back to where ITS tree began; a node without one keeps the answer it always had; and a node
    /// whose item tree has no recorded base gets `None` — **never** the job's tree as a fallback.
    /// That last arm is the whole reason these two questions were joined into one: reverting a
    /// checkout to a footing taken from a different checkout is worse than reverting nothing, and
    /// it is exactly what two separately-resolved answers produced.
    #[tokio::test]
    async fn a_skipped_node_is_undone_in_the_tree_it_wrote_in() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running", "running"]).await;
        let items: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();

        // The job's own tree and a checkpoint to fall back to, which is what a node without an item
        // of its own is answered with.
        crate::execution::worktree::record(
            &pool,
            crate::execution::worktree::Owner::Job(job_id),
            "project-a",
            "/repo",
            "/trees/job",
            "nucleos/job",
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET head_sha = 'head0' WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        // Item 0 has a tree with a base; item 1 has a tree that was never measured.
        crate::execution::worktree::record(
            &pool,
            crate::execution::worktree::Owner::Item(items[0]),
            "project-a",
            "/repo",
            "/trees/item-a",
            "nucleos/item-a",
            Some("base0"),
        )
        .await
        .unwrap();
        crate::execution::worktree::record(
            &pool,
            crate::execution::worktree::Owner::Item(items[1]),
            "project-a",
            "/repo",
            "/trees/item-b",
            "nucleos/item-b",
            None,
        )
        .await
        .unwrap();

        async fn run(pool: &sqlx::SqlitePool, job_id: i64, item: Option<i64>) -> i64 {
            sqlx::query(
                "INSERT INTO runs (project_id, prompt, status, mode, created_at, job_id, item_id)
                 VALUES ('project-a', 'x', 'running', 'worktree', '2026-01-01T00:00:00Z', ?, ?)",
            )
            .bind(job_id)
            .bind(item)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
        }

        let with_base = run(&pool, job_id, Some(items[0])).await;
        assert_eq!(
            revert_target(&pool, job_id, with_base).await,
            Some((PathBuf::from("/trees/item-a"), "base0".to_owned())),
            "an item is put back to where its own tree began"
        );

        // Today's link between a node and its item is `job_items.run_id`, and it is what
        // `footing_for_run` walks. The run below carries no `item_id` precisely because that is
        // every node that exists before this slice.
        let no_item = run(&pool, job_id, None).await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE id = ?")
            .bind(no_item)
            .bind(items[0])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            revert_target(&pool, job_id, no_item).await,
            Some((PathBuf::from("/trees/job"), "head0".to_owned())),
            "a node without an item keeps the answer it always had"
        );

        let unmeasured = run(&pool, job_id, Some(items[1])).await;
        assert_eq!(
            revert_target(&pool, job_id, unmeasured).await,
            None,
            "an unmeasured item tree answers nothing — reverting the job's would be worse"
        );
    }

    #[tokio::test]
    async fn a_job_still_planning_has_no_queue_yet() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(!view.planned);
        // The queue is what this test is about, and it still does not exist. What comes FIRST
        // changed: a job with nothing at all writes its brief before it plans, so the queue is now
        // two nodes away rather than one.
        assert_eq!(next_step(&view), Next::SpawnSpec);
    }

    #[tokio::test]
    async fn a_planner_that_found_nothing_is_not_a_job_still_planning() {
        let pool = test_pool().await;
        // Past the plan node with an empty queue: the honest "there was no work" night. Reading
        // this as still-planning would spawn a second planner every tick, forever.
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(view.planned);
        assert_eq!(next_step(&view), Next::Finish(Outcome::Completed));
    }

    /// The column reaches `next_step`, or everything decided about teams is a decision no real job
    /// ever takes.
    ///
    /// The seeding is not ceremony: `storage.rs` runs with `foreign_keys` on, so a job cannot name
    /// a team out of thin air, and a team cannot name a director out of thin air either. The
    /// `UPDATE` is how a job acquires one for now — the director that writes it at creation is a
    /// later slice, and this slice deliberately leaves every production path writing NULL.
    #[tokio::test]
    async fn a_job_carries_whether_a_team_directs_it() {
        let pool = test_pool().await;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                 created_at, updated_at)
             VALUES ('dir', 'Dir', 'directing', 'lead', 'claude', 'inherit', ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES ('crew', 'Crew', 'ship it', 'dir', 3, 2, ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

        let alone = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let directed = seed_job(&pool, "project-b", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(directed)
            .execute(&pool)
            .await
            .unwrap();

        assert!(!load_view(&pool, alone).await.unwrap().has_team);
        assert!(load_view(&pool, directed).await.unwrap().has_team);
    }

    /// One agent in the catalogue, ready to be put on a team.
    async fn seed_agent(pool: &sqlx::SqlitePool, id: &str, speciality: &str, prompt: &str) {
        let now = "2026-08-20T00:00:00Z";
        sqlx::query(
            "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                 created_at, updated_at)
             VALUES (?, ?, ?, ?, 'claude', 'inherit', ?, ?)",
        )
        .bind(id)
        .bind(format!("Agent {id}"))
        .bind(speciality)
        .bind(prompt)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A team, its director, and however many members. `foreign_keys` is on, so the order is not
    /// optional: agents, then the team that names one of them, then the memberships.
    async fn seed_crew(pool: &sqlx::SqlitePool, team_id: &str, director: &str, members: &[&str]) {
        let now = "2026-08-20T00:00:00Z";
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES (?, ?, 'ship it', ?, 3, 2, ?, ?)",
        )
        .bind(team_id)
        .bind(format!("Team {team_id}"))
        .bind(director)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
        for member in members {
            sqlx::query("INSERT INTO team_members (team_id, agent_id) VALUES (?, ?)")
                .bind(team_id)
                .bind(member)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    /// The roster a director chooses from is its members AND itself.
    ///
    /// **A team of one would otherwise be dead on arrival**, and silently: a director with no
    /// members has nobody to name, every plan it writes is refused for naming a stranger, and the
    /// refusal blames the director for the one answer available to it. A director and no members is
    /// an ordinary way to ask for parallel work under a single persona, so it has to be a team that
    /// can plan.
    ///
    /// `team.rs` keeps its departments' directors off their rosters, and that is not a
    /// disagreement: there a director hands work to specialists and does none itself. Here the
    /// roster answers "who may write code in this job", and the director qualifies.
    #[tokio::test]
    async fn a_director_may_give_work_to_itself_and_to_every_member() {
        let pool = test_pool().await;
        seed_agent(&pool, "dir", "directing", "lead").await;
        seed_agent(&pool, "ana", "migrations", "you are careful with schemas").await;
        seed_crew(&pool, "crew", "dir", &["ana"]).await;
        seed_agent(&pool, "solo", "everything", "you do it all").await;
        seed_crew(&pool, "alone", "solo", &[]).await;

        let named = |roster: Vec<TeamMate>| {
            roster
                .into_iter()
                .map(|mate| mate.id)
                .collect::<Vec<String>>()
        };
        assert_eq!(
            named(roster_of(&pool, "crew").await),
            vec!["ana".to_owned(), "dir".to_owned()],
            "the director is on its own roster, and the order is stable"
        );
        assert_eq!(
            named(roster_of(&pool, "alone").await),
            vec!["solo".to_owned()],
            "a team of one can still be given work, or it can never plan at all"
        );
        assert_eq!(
            roster_of(&pool, "crew").await[0].speciality,
            "migrations",
            "the speciality travels, because it is the whole of what a director chooses on"
        );
    }

    /// The roster reaches the plan node, or the director is asked to name somebody it has never
    /// been told about.
    #[tokio::test]
    async fn a_teams_plan_prompt_introduces_the_people_it_may_name() {
        let pool = test_pool().await;
        seed_agent(&pool, "dir", "directing", "lead").await;
        seed_agent(&pool, "ana", "migrations and schema work", "careful").await;
        seed_crew(&pool, "crew", "dir", &["ana"]).await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let job = live_jobs(&pool).await.unwrap().pop().unwrap();

        let rules = graph_rules(&pool, &job).await.expect("a job with a team");
        let prompt = plan_prompt("t", 5, "/wt/.nucleos", Some(&rules), false);

        assert!(prompt.contains("agent_id"), "{prompt}");
        assert!(prompt.contains("ana"), "{prompt}");
        assert!(
            prompt.contains("migrations and schema work"),
            "the director chooses on the speciality, so it has to be in front of it: {prompt}"
        );
        assert!(prompt.contains("dir"), "{prompt}");
    }

    /// The item's agent is read when the node starts, not copied onto the row when the plan lands.
    ///
    /// Which is the behaviour a catalogue is for. An owner who corrects an agent's prompt between
    /// the plan and the run gets the correction; freezing it at plan time would leave them watching
    /// the old brief run and unable to say why their edit did nothing.
    #[tokio::test]
    async fn an_items_agent_is_read_when_the_node_starts_and_not_frozen_at_plan_time() {
        let pool = test_pool().await;
        seed_agent(&pool, "ana", "migrations", "the brief as it was written").await;

        let first = persona_for(&pool, Some("ana")).await.expect("an agent");
        assert!(first.contains("the brief as it was written"), "{first}");
        assert!(
            first.contains("migrations"),
            "the item is told WHY it was given to this one: {first}"
        );

        sqlx::query("UPDATE agents SET prompt = 'the brief after the correction' WHERE id = 'ana'")
            .execute(&pool)
            .await
            .unwrap();
        let second = persona_for(&pool, Some("ana")).await.expect("an agent");
        assert!(second.contains("after the correction"), "{second}");

        // Two ways there is nobody, and both leave the item running with the brief it would have
        // had before any of this existed rather than refusing to run it.
        assert_eq!(persona_for(&pool, None).await, None);
        assert_eq!(persona_for(&pool, Some("nobody")).await, None);
    }

    /// The persona goes FIRST, and everything after it is untouched.
    ///
    /// The second half is the half that protects every job without a team: `None` has to produce
    /// the brief this function has always produced, byte for byte, and the `ends_with` is what says
    /// the assigned one is that brief with something in front of it rather than a rewrite of it.
    #[test]
    fn a_teams_item_is_briefed_as_somebody_before_it_is_briefed_about_the_work() {
        let anybody = implement_prompt("write the thing", 0, 3, "/wt/.nucleos", &[], None, None);
        let ana = implement_prompt(
            "write the thing",
            0,
            3,
            "/wt/.nucleos",
            &[],
            None,
            Some("You are careful with schemas.\n\n"),
        );

        assert!(
            ana.starts_with("You are careful with schemas."),
            "who is reading comes before what they are reading about: {ana}"
        );
        assert!(
            ana.ends_with(&anybody),
            "the brief itself is unchanged, and the persona is in front of it: {ana}"
        );
        assert!(
            !anybody.contains("careful"),
            "a job with no team is told nothing about a team"
        );
    }

    /// And the two halves joined: a directed job with a failed item keeps going, an undirected one
    /// with the same queue stops. Same rows, same loader, one column apart.
    #[tokio::test]
    async fn the_team_column_is_what_decides_whether_a_failure_ends_the_queue() {
        let pool = test_pool().await;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                 created_at, updated_at)
             VALUES ('dir', 'Dir', 'directing', 'lead', 'claude', 'inherit', ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES ('crew', 'Crew', 'ship it', 'dir', 3, 2, ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

        let alone = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, alone, &["failed", "pending"]).await;
        let directed = seed_job(&pool, "project-b", "implementing").await.unwrap();
        seed_items(&pool, directed, &["failed", "pending"]).await;
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(directed)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            next_step(&load_view(&pool, alone).await.unwrap()),
            Next::Finish(Outcome::Failed)
        );
        assert_eq!(
            next_step(&load_view(&pool, directed).await.unwrap()),
            Next::SpawnImplement(vec![1])
        );
    }

    #[tokio::test]
    async fn a_loaded_view_resumes_at_the_first_unfinished_item() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "passed", "pending"]).await;

        let view = load_view(&pool, job_id).await.unwrap();

        assert_eq!(next_step(&view), Next::SpawnImplement(vec![2]));
    }

    // ---- rounds (Chunk 3) ------------------------------------------------------------------------

    /// The whole point of landing the schema and this logic together and inert: a job nobody asked
    /// for rounds takes the pre-rounds path, verdict for verdict.
    ///
    /// `max_rounds = 1` is what `load_view` resolves a NULL column to, so this is every job that
    /// existed before migration 0051 and every `graph:` rule scheduled today.
    #[test]
    fn a_one_round_job_ends_exactly_as_it_did_before_rounds_existed() {
        for (items, expected) in [
            (vec![ItemState::Passed], Outcome::Completed),
            (
                vec![ItemState::Passed, ItemState::GateFailed],
                Outcome::GateFailed,
            ),
            (vec![ItemState::Skipped], Outcome::Completed),
        ] {
            assert_eq!(
                next_step(&view(true, &items, ReviewState::Done)),
                Next::Finish(expected),
                "{items:?}"
            );
        }
    }

    /// The four endings of §5.2, one case per row, and the order between them is what the table is
    /// for: two say `completed` and two say `stopped`, which is the difference a person reads in the
    /// morning between "I finished" and "I was cut short and it is on the branch".
    #[test]
    fn a_round_closes_by_the_first_condition_that_holds() {
        let rounds = |round, dry, replanned| RoundState {
            round,
            max_rounds: 5,
            dry_rounds: dry,
            replanned,
            ..RoundState::default()
        };

        // (1) The replan said so. Ahead of everything: the node that just looked at the work is the
        // one saying the work is over.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(1, 0, Replan::Done)
            )),
            Next::Finish(Outcome::Completed)
        );

        // (3) It dried up. `completed`, because there was nothing left to do.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(1, DRY_ROUNDS_TO_STOP, Replan::NotYet)
            )),
            Next::Finish(Outcome::Completed)
        );

        // (4) Out of rounds. `stopped`, because there may well have been more.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(4, 0, Replan::NotYet)
            )),
            Next::Finish(Outcome::Stopped)
        );

        // None of them: ask again.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(1, 1, Replan::NotYet)
            )),
            Next::SpawnReplan
        );
    }

    /// A red gate ends a job NOBODY ASKED FOR, whatever the rounds say.
    ///
    /// `..RoundState::default()` leaves `a_broken_item_may_go_round_again` false, which is what a
    /// scheduled job gets and what every job got before that field existed — so this keeps asking
    /// the question it was written to ask, about the work it still holds for.
    #[test]
    fn a_red_gate_ends_a_job_that_had_rounds_left() {
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed, ItemState::GateFailed],
                ReviewState::Done,
                RoundState {
                    round: 0,
                    max_rounds: 5,
                    ..RoundState::default()
                }
            )),
            Next::Finish(Outcome::GateFailed)
        );
    }

    /// A replan is told what broke, and stops being told to leave it alone.
    ///
    /// Two faults in one paragraph, and the second is the one that would have survived a fix to the
    /// first. `implement_prompt` already carries the gate's output into a RETRY of an item, because
    /// §5.4 keeps no node from ever seeing another's session — so a node that cannot see what it
    /// broke spends a whole run rediscovering what the gate already printed. A replan is that same
    /// node one round later and was the one place the argument had not reached.
    ///
    /// And "Do not repropose any of it" is right about work that LANDED and exactly backwards aimed
    /// at an item the gate rejected: that item is the one thing the round did not achieve.
    ///
    /// The unmeasured arm is not decoration. An item whose node died before any gate looked at it
    /// has no verdict to hand on, and saying so plainly beats inventing one — a replan told "the
    /// gate said nothing" would reasonably read that as the gate having been content.
    #[test]
    fn a_replan_is_handed_what_broke_and_told_that_reproposing_it_is_the_point() {
        let archives = ["plan-0.json".to_owned()];
        let gated = Unfinished {
            ordinal: 2,
            description: "wire the handler".to_owned(),
            gate_output: Some("error[E0061]: this function takes 6 arguments".to_owned()),
        };
        let unmeasured = Unfinished {
            ordinal: 4,
            description: "the database-backed tests".to_owned(),
            gate_output: None,
        };

        let prompt = replan_prompt(
            "t",
            1,
            &archives,
            "/wt/.nucleos",
            None,
            &[gated, unmeasured],
        );

        // Ordinals are shown one-based, as every other line a person reads about items is.
        assert!(prompt.contains("item 3: wire the handler"), "{prompt}");
        assert!(prompt.contains("error[E0061]"), "{prompt}");
        assert!(
            prompt.contains("item 5: the database-backed tests"),
            "{prompt}"
        );
        assert!(prompt.contains("nothing measured this one"), "{prompt}");

        // The instruction is qualified rather than dropped: the parts that landed are still not to
        // be reproposed, and a round that lost that half would redo its own finished work.
        assert!(prompt.contains("the parts of it that LANDED"), "{prompt}");
        assert!(!prompt.contains("Do not repropose any of it."), "{prompt}");
    }

    /// The worst case `unfinished_items` can hand a replan, bounded — and the bound spent on the
    /// failures the round is actually answering.
    ///
    /// The numbers are the job's own ceilings rather than a scenario invented for the test:
    /// `MAX_ROUNDS_CEILING` rounds of `MAX_ITEMS_CEILING` items, every one of them red and none
    /// taken over, which is the 100 items `MAX_ROUNDS_CEILING`'s own comment names. At
    /// `GATE_OUTPUT_TAIL` each that is ~400 KiB of gate output in one prompt.
    ///
    /// Verified by mutation: with the budget removed (every item shown), this test fails on the
    /// size assertion at 409,600 bytes of gate output against a 20,480-byte bound.
    #[test]
    fn the_replan_prompt_bounds_the_gate_output_it_carries_and_spends_it_on_the_newest_failures() {
        let worst_case =
            crate::config::MAX_ROUNDS_CEILING as usize * crate::config::MAX_ITEMS_CEILING;
        let unfinished: Vec<Unfinished> = (0..worst_case)
            .map(|ordinal| Unfinished {
                ordinal,
                // Each item's own marker, so which ones kept their words is readable from the
                // prompt rather than inferred from its length.
                description: format!("item {ordinal}"),
                // Exactly `GATE_OUTPUT_TAIL` bytes, because that is what `record_gate` stores and
                // therefore what the worst case actually is. Padded rather than repeated to that
                // length: a marker per item, then filler, so the sum is the real 100 x 4 KiB.
                gate_output: Some(format!(
                    "{:x<width$}",
                    format!("gate-{ordinal} "),
                    width = GATE_OUTPUT_TAIL
                )),
            })
            .collect();

        let prompt = replan_prompt("t", 19, &[], "/wt/.nucleos", None, &unfinished);

        let carried: usize = unfinished
            .iter()
            .filter(|item| {
                prompt.contains(&format!(
                    "the gate said:\n{}",
                    item.gate_output.as_ref().unwrap()
                ))
            })
            .map(|item| item.gate_output.as_ref().unwrap().len())
            .sum();
        assert!(
            carried <= REPLAN_GATE_BUDGET,
            "carried {carried} bytes of gate output, over the {REPLAN_GATE_BUDGET}-byte budget"
        );

        // Every item is still LISTED. Spending the budget must never cost an item its identity:
        // a failure nothing takes over decides how the job ends, and a node that cannot see one
        // cannot write the `"replaces"` that would take it over.
        for ordinal in 0..worst_case {
            assert!(
                prompt.contains(&format!("- item {}: item {ordinal}\n", ordinal + 1)),
                "item {ordinal} is not listed"
            );
        }

        // The budget went to the END of the list, which is the most recent round. The first item
        // on a 100-item job is nineteen rounds old; the last is the one this replan is answering.
        assert!(
            prompt.contains(&format!(
                "the gate said:\n{}",
                unfinished[worst_case - 1].gate_output.as_ref().unwrap()
            )),
            "the newest failure lost its gate output"
        );
        assert!(
            !prompt.contains(&format!(
                "the gate said:\n{}",
                unfinished[0].gate_output.as_ref().unwrap()
            )),
            "the oldest failure kept its gate output"
        );

        // A squeezed item says what it is missing rather than reading as a gate that stayed quiet —
        // the same distinction the `None` arm above exists to preserve.
        assert!(
            prompt.contains("not shown: this prompt carries at most"),
            "{prompt}"
        );
        assert!(!prompt.contains("nothing measured this one"), "{prompt}");
    }

    /// A dead node stops ending the queue at the item it died on, and the items after it run.
    ///
    /// This is the one that would have saved job 23, and the arithmetic is the argument: its item 0
    /// was implemented, gated red, retried, and the retry was killed mid-flight — which marked the
    /// item `failed` and finished the job with items 1, 2 and 3 never attempted. Nothing about
    /// those three was broken, and nothing about them had been measured either way.
    ///
    /// The pending item is what the assertion is really about. `Finish` here means three items
    /// thrown away for one that broke; anything else means the queue carries on to them.
    #[test]
    fn a_dead_node_no_longer_takes_the_items_after_it_when_the_owner_asked_for_the_work() {
        let queue = [ItemState::Failed, ItemState::Pending];
        let rounds = |commissioned| RoundState {
            round: 0,
            max_rounds: 4,
            a_broken_item_may_go_round_again: commissioned,
            ..RoundState::default()
        };

        assert_eq!(
            next_step(&view_in_round(&queue, ReviewState::Done, rounds(false))),
            Next::Finish(Outcome::Failed),
            "a job nobody asked for ends exactly where it always did"
        );
        assert_ne!(
            next_step(&view_in_round(&queue, ReviewState::Done, rounds(true))),
            Next::Finish(Outcome::Failed),
            "and one the owner commissioned reaches the item that was never tried"
        );
    }

    /// A red gate on work the owner asked for goes round again instead of ending the night.
    ///
    /// The measurement behind it: job 23, 2026-08-30, was given four rounds and $25, went red on
    /// its first item in round 0, and under the arm above would have been reported `gate_failed`
    /// with three rounds and $14.63 never touched.
    ///
    /// Every row here is a way the change could be too broad, which is the risk in it — an
    /// exemption that swallowed the other endings would turn "keep trying" into "never stop".
    #[test]
    fn a_red_gate_on_commissioned_work_goes_round_again_but_nothing_else_does() {
        let commissioned = |round, dry, items: &[ItemState]| {
            view_in_round(
                items,
                ReviewState::Done,
                RoundState {
                    round,
                    max_rounds: 4,
                    dry_rounds: dry,
                    a_broken_item_may_go_round_again: true,
                    ..RoundState::default()
                },
            )
        };
        let red = [ItemState::Passed, ItemState::GateFailed];

        // The change itself: rounds left, so the job asks for another plan rather than giving up.
        assert_eq!(next_step(&commissioned(0, 0, &red)), Next::SpawnReplan);

        // Out of rounds. The ending is NOT softened — the failed item still decides what the job is
        // reported as, and `gate_failed` is what a person reads in the morning.
        assert_eq!(
            next_step(&commissioned(3, 0, &red)),
            Next::Finish(Outcome::GateFailed)
        );

        // Dried up: two rounds that added nothing stop it, exactly as they stop a happy job. Without
        // this arm a red item the replan cannot fix would spin against `max_rounds` spending money.
        assert_eq!(
            next_step(&commissioned(0, DRY_ROUNDS_TO_STOP, &red)),
            Next::Finish(Outcome::GateFailed)
        );

        // A dead node goes round again too, and for the same reason rather than by analogy:
        // `reconcile_nodes` puts its tree back to the item's footing before marking it, exactly as
        // `record_gate` does for a red gate, so the unmeasured edits are gone by the time this
        // reads the state.
        assert_eq!(
            next_step(&commissioned(0, 0, &[ItemState::Passed, ItemState::Failed])),
            Next::SpawnReplan
        );

        // `GateErrored` is the one that does NOT move, and it is why the set is written out rather
        // than as "any unhappy ending". A gate that would not RUN measured nothing and is
        // deliberately never reverted — its work might be perfectly good — so the build-on-top
        // argument is still exactly true for it.
        //
        // Asked of `close_the_round` and NOT of `next_step`, which would have been the comfortable
        // thing to write and would have proved nothing: `next_step` short-circuits on `GateErrored`
        // before a round is ever closed, so it answers `Finish` whatever this branch says, and a
        // widening of the set above would have sailed past a green assertion.
        assert_eq!(
            close_the_round(&commissioned(0, 0, &[ItemState::GateErrored])),
            Next::Finish(Outcome::GateErrored)
        );
        // Cancelled is a person having stopped this. No revert makes that stale, and carrying on
        // would be answering them.
        assert_eq!(
            next_step(&commissioned(0, 0, &[ItemState::Cancelled])),
            Next::Finish(Outcome::Cancelled)
        );

        // A replan already in flight is waited on, not spawned twice — the same trap the happy path
        // has its own guard for.
        assert_eq!(
            next_step(&view_in_round(
                &red,
                ReviewState::Done,
                RoundState {
                    round: 0,
                    max_rounds: 4,
                    replanning: true,
                    a_broken_item_may_go_round_again: true,
                    ..RoundState::default()
                }
            )),
            Next::Wait
        );
    }

    /// The sibling of `planning`, and it exists for the identical reason: between spawning the node
    /// and its items landing the queue is empty, so without it every tick would spawn another.
    #[test]
    fn a_replan_in_flight_is_waited_on_rather_than_spawned_again() {
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                RoundState {
                    round: 1,
                    max_rounds: 5,
                    replanning: true,
                    ..RoundState::default()
                }
            )),
            Next::Wait
        );
    }

    /// The one thing an empty queue can mean.
    #[test]
    fn an_empty_queue_is_a_planner_that_looked_and_found_nothing() {
        // Round 0: the planner looked and found nothing. Done, as it always was.
        assert_eq!(
            next_step(&view_in_round(
                &[],
                ReviewState::Pending,
                RoundState {
                    max_rounds: 5,
                    ..RoundState::default()
                }
            )),
            Next::Finish(Outcome::Completed)
        );

        // Only reachable on the first round, and that is a property of the queue not being filtered
        // by round: once anything has been queued at all, this is never empty again. A round that
        // adds nothing shows up as `dry_rounds`, never as an empty queue.
    }

    /// Why the queue is read UNFILTERED, stated as the two things filtering would have broken.
    ///
    /// Filtering by round is the obvious first design, and it is wrong twice. `ordinal` is half this
    /// table's primary key, so restarting it per round collides outright — which is how this was
    /// found. And the position in the loaded queue would stop being the ordinal in the table, which
    /// is the number `advance` binds into its lookups. Unfiltered costs nothing: a round closes only
    /// when every item in it is terminal, and those are states `next_step` already walks past.
    #[tokio::test]
    async fn a_new_rounds_item_is_found_past_the_finished_ones_and_keeps_its_real_ordinal() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "skipped"]).await;
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, round)
             VALUES (?, 2, 'what round 1 asked for', 'pending', 1)",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET round = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(
            view.items.iter().map(|item| item.state).collect::<Vec<_>>(),
            vec![ItemState::Passed, ItemState::Skipped, ItemState::Pending],
            "the earlier round's items stay in the queue, terminal and walked past"
        );
        assert_eq!(view.rounds.round, 1);
        // 2, not 0. This is the number `advance` binds into
        // `SELECT description FROM job_items WHERE job_id = ? AND ordinal = ?`.
        assert_eq!(next_step(&view), Next::SpawnImplement(vec![2]));
    }

    /// A NULL `max_rounds` is "nobody asked for rounds", not "use the daemon's ceiling". Resolving it
    /// the other way would switch rounds on for every `graph:` rule already scheduled, silently.
    #[tokio::test]
    async fn a_job_with_no_round_ceiling_is_a_job_of_one_round() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed"]).await;

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(view.rounds.max_rounds, 1);
        assert_eq!(view.rounds.round, 0);
        assert_eq!(view.rounds.replanned, Replan::NotYet);
    }

    #[tokio::test]
    async fn an_unrecognised_item_status_is_unfinished_rather_than_done() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "something-new"]).await;

        let view = load_view(&pool, job_id).await.unwrap();

        // Erring toward "not finished" costs a repeated item. Erring the other way skips work the
        // job exists to do and reports it complete, which is the failure nobody sees.
        assert_eq!(next_step(&view), Next::SpawnImplement(vec![1]));
    }

    #[tokio::test]
    async fn losing_the_slot_parks_the_job_instead_of_failing_it() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        wait(&pool, job_id, "slot").await.unwrap();

        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "waiting");
        // The reason is stored because `waiting` now covers a budget window that will reopen and a
        // slot another run holds, and those ask opposite things of whoever reads the feed.
        assert_eq!(reason.as_deref(), Some("slot"));
    }

    /// The note has to leave with the pause it explains.
    ///
    /// `retire` has cleared it since it was written; `resume` did not, so an unparked job carried
    /// the reason for its last pause through everything that came after. What a reader saw was
    /// `implementing / budget` — a job working normally, labelled with a brake that lifted hours
    /// ago, which is exactly the sort of thing somebody acts on.
    #[tokio::test]
    async fn resuming_a_job_takes_the_pause_note_with_it() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        wait(&pool, job_id, "budget").await.unwrap();
        resume(&pool, job_id).await.unwrap();

        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "implementing", "the stage it was parked at");
        assert_eq!(reason, None, "nothing is waiting, so nothing is the reason");
    }

    #[tokio::test]
    async fn a_finished_job_records_which_kind_of_ending_it_had() {
        let pool = test_pool().await;
        let broken = seed_job(&pool, "project-a", "gating").await.unwrap();
        finish(&pool, broken, Outcome::GateFailed).await.unwrap();
        let unmeasured = seed_job(&pool, "project-b", "gating").await.unwrap();
        finish(&pool, unmeasured, Outcome::GateErrored)
            .await
            .unwrap();

        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        // Distinct in the database, not just in the enum: a reader querying jobs must still be able
        // to tell broken code from a measurement that never happened.
        assert_eq!(statuses, vec!["gate_failed", "gate_errored"]);
    }

    /// A second live job for one project is now a row the database accepts.
    ///
    /// It used to be refused here, by `one_live_job_per_project`, and that index came out in 0053.
    /// The lock did not move — it is still an `INSERT`, now into `project_slots` — but it moved
    /// OFF this function, so `insert_job` no longer refuses anything and `start` is where a full
    /// project is turned away. Asserted rather than deleted, because a reader who remembers the old
    /// behaviour needs to find out here that it changed on purpose.
    #[tokio::test]
    async fn a_second_live_job_for_a_project_is_no_longer_refused_by_the_row() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "planning")
            .await
            .expect("the first job starts");

        let second = seed_job(&pool, "project-a", "planning").await;

        assert!(
            second.is_ok(),
            "the row no longer holds exclusivity; the slot ceiling does"
        );
    }

    /// The seam between the pure decision and the database, which is where a spec node would break
    /// silently: `spawn_node` writes `runs.stage`, `load_view` reads it back, and if the two ever
    /// disagreed on the word "spec" the job would spawn a spec node on every single tick — each one
    /// invisible to the read that is supposed to notice the last one.
    #[tokio::test]
    async fn um_no_de_spec_em_voo_e_visto_como_em_voo_e_depois_como_feito() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning")
            .await
            .expect("a job that has not planned yet");

        let fresh = load_view(&pool, job_id).await.unwrap();
        assert!(
            !fresh.specced && !fresh.speccing,
            "a job with no spec run has no brief and none in flight"
        );
        assert_eq!(next_step(&fresh), Next::SpawnSpec);

        let run_id = seed_node(&pool, job_id, "spec", "running").await;
        let running = load_view(&pool, job_id).await.unwrap();
        assert!(
            running.speccing && !running.specced,
            "a spec node in flight is speccing and not yet specced"
        );
        assert_eq!(next_step(&running), Next::Wait);

        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        let done = load_view(&pool, job_id).await.unwrap();
        assert!(
            done.specced && !done.speccing,
            "a spec node that finished leaves the job specced"
        );
        assert_eq!(
            next_step(&done),
            Next::SpawnPlan,
            "the queue is what comes after the brief"
        );
    }

    #[tokio::test]
    async fn a_finished_job_does_not_hold_the_project_slot() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "completed")
            .await
            .expect("a job that has finished");

        // The partial index covers live statuses only. Without that, a project would be wedged
        // forever by its own first job — a worse failure than the race the index exists to stop,
        // because nothing would ever clear it.
        seed_job(&pool, "project-a", "planning")
            .await
            .expect("a new job may start once the last one is done");
    }

    // ---- what a pass sees ----------------------------------------------------------------------

    /// The constant the tick iterates and the SQL a pass loads with have to name the same statuses,
    /// in both directions.
    ///
    /// It used to be checked against `one_live_job_per_project`, which is gone (migration 0053).
    /// The hazard survived the index, and only changed hands: a job in a status the tick does not
    /// drive still holds a concurrency slot, and `reconcile_orphaned_slots` still spares it, because
    /// the sweep's live-list is this same constant. So the project goes quiet with no error
    /// anywhere — with one slot fewer instead of none at all. `concurrency.rs` guards the sweep's
    /// half of that agreement; this guards the loader's.
    #[test]
    fn a_live_status_is_a_status_some_pass_would_load() {
        for status in LIVE_STATUSES {
            assert!(
                LIVE_JOBS_SQL.contains(&format!("'{status}'")),
                "`{status}` is live but no pass would ever load it"
            );
        }
        // And the other direction, which is the dangerous one: SQL that loads a status the tick has
        // no arm for would drive a job nothing knows how to move.
        let loaded = LIVE_JOBS_SQL
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            loaded,
            LIVE_STATUSES.len(),
            "a pass loads a status the tick does not drive: {LIVE_JOBS_SQL}"
        );
    }

    /// The same guard `LIVE_JOBS_SQL` already carries, for the same reason: sqlx refuses SQL
    /// assembled at runtime, so the list is spelled out by hand in several places and nothing but a
    /// test holds them to each other. A status missing from here would not break anything loudly —
    /// the canvas would simply stop drawing that kind of live job.
    #[test]
    fn the_live_listing_names_every_live_status() {
        for status in LIVE_STATUSES {
            assert!(
                LIVE_WHERE_SQL.contains(&format!("'{status}'")),
                "the live listing does not know `{status}`"
            );
        }
        let named = LIVE_WHERE_SQL
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            named,
            LIVE_STATUSES.len(),
            "the listing names a status nothing drives"
        );
    }

    /// A job that ends in a status the GC does not collect keeps its worktree forever, and nothing
    /// reports it: as far as the system is concerned the job is finished and the tree is nobody's.
    /// Written after adding `stopped` opened exactly that leak.
    #[test]
    fn every_ending_a_job_can_have_is_an_ending_the_gc_collects() {
        for status in TERMINAL_STATUSES {
            assert!(
                crate::execution::worktree::GC_CANDIDATES_SQL.contains(&format!("'{status}'")),
                "a job can end `{status}` and its worktree would never be collected"
            );
            assert!(
                !LIVE_STATUSES.contains(&status),
                "`{status}` both holds the project's slot and is collectable"
            );
        }
    }

    /// The job row has to say it is blocked on a person. Until it did, it read `implementing` while
    /// the whole chain waited, and the only sign anywhere was a proposal in a queue that never
    /// names the job it came from.
    #[tokio::test]
    async fn a_job_whose_node_stopped_to_ask_says_so_on_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "awaiting_approval").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ?")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        job_tick(&state, Utc::now()).await;
        assert_eq!(job_status(&pool, job_id).await, STATUS_AWAITING_APPROVAL);

        // And it goes back to what it was doing once the node moves on, rather than staying stuck
        // in a status nothing clears.
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        job_tick(&state, Utc::now()).await;
        assert_ne!(job_status(&pool, job_id).await, STATUS_AWAITING_APPROVAL);
        // Past `running`, wherever the same pass carried it — this project configures no gate, so
        // the item is waved through to `passed` in the step after the one being tested here.
        assert_ne!(item_statuses(&pool, job_id).await[0], "running");
    }

    /// The hazard of putting anything in front of a job's stage: `planning` is the one status that
    /// carries meaning the job cannot rebuild. A plan node that stops to ask permission must not
    /// come back as planned-with-an-empty-queue and report itself complete having planned nothing.
    #[tokio::test]
    async fn a_plan_node_stopping_to_ask_does_not_cost_the_job_its_queue() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        let run_id = seed_node(&pool, job_id, "plan", "awaiting_approval").await;

        job_tick(&state, Utc::now()).await;
        assert_eq!(job_status(&pool, job_id).await, STATUS_AWAITING_APPROVAL);
        assert!(!load_view(&pool, job_id).await.unwrap().planned);

        sqlx::query("UPDATE runs SET status = 'cancelled' WHERE id = ?")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        resume(&pool, job_id).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "planning");
    }

    /// Both levels of cancellation end the job, because a stopped node leaves the tree holding
    /// edits no gate measured and the next item must not build on them. What must not happen is
    /// either level reporting the user's own decision back to them as a failure.
    #[tokio::test]
    async fn cancelling_a_node_ends_its_job_cancelled_rather_than_failed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running", "pending"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "cancelled").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = 0")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["cancelled", "pending"]
        );
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Cancelled)
        );
    }

    /// The whole reason cancelling a job is a separate thing from cancelling a run: a job parked
    /// for the budget, or waiting for the slot, has no node to cancel. Without this it could only
    /// be stopped by waiting out the four-hour ceiling.
    #[tokio::test]
    async fn a_parked_job_can_be_cancelled_even_though_it_has_no_node_running() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "pending"]).await;
        wait(&pool, job_id, "budget").await.unwrap();

        assert_eq!(
            cancel(&state, job_id).await.unwrap(),
            CancelOutcome::Cancelled
        );
        assert_eq!(job_status(&pool, job_id).await, STATUS_CANCELLED);
        // The item that was never started is left alone. Marking it cancelled would claim the job
        // got to it and turned back, and the gated-green one keeps its verdict.
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["passed", "pending"]
        );
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_cancelled".to_owned())
        );
    }

    /// Cancelling a job also ends the node that was parked asking for permission — and closes the
    /// door that could still have said yes to it.
    ///
    /// `finalize_termination` cannot do this: it needs a live handle, and it guards its write with
    /// `status = 'running'`. A parked node has neither. So the run stayed `awaiting_approval` after
    /// the job that owned it was gone, holding the project's exclusivity — an index then, a
    /// concurrency slot since 0053, and a leak either way.
    ///
    /// Found in production on 2026-08-08: job 12 was cancelled with its review node parked, and the
    /// next job came up `waiting` with `wait_reason = slot` against a project whose only live job
    /// was itself — with nothing in the feed to say why, because from the project's side nothing
    /// had gone wrong.
    #[tokio::test]
    async fn cancelling_a_job_ends_the_node_parked_on_an_approval_and_shuts_its_door() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "reviewing").await.unwrap();
        seed_items(&pool, job_id, &["passed"]).await;
        let run_id = seed_node(&pool, job_id, "review", "awaiting_approval").await;
        let proposal_id = crate::proposals::create_action_approval(
            &pool,
            run_id,
            None,
            Some("project-a"),
            "Bash",
            "the review node wanted to look around",
            Some(r#"{"command":"git branch -a"}"#),
        )
        .await
        .expect("a parked node has a proposal");

        assert_eq!(
            cancel(&state, job_id).await.unwrap(),
            CancelOutcome::Cancelled
        );

        let run_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(
            run_status, "awaiting_approval",
            "the parked node outlived the job that owned it, and holds the project's slot"
        );

        let proposal_status: String =
            sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
                .bind(proposal_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(
            proposal_status, "pending",
            "approving this would resume a node whose job is over"
        );
    }

    /// A live job that holds no slot is put back on one by the tick.
    ///
    /// The window is between `start`'s INSERT and its claim, which cannot be closed by ordering —
    /// a slot is keyed on its owner's id, so the row must exist first. The sweep is no help either:
    /// it only takes slots away from owners that died, and there is nothing here to take. Left
    /// alone, the project runs one over its ceiling for as long as the job lasts.
    #[tokio::test]
    async fn the_tick_puts_a_live_job_back_on_a_slot_it_lost() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        assert_eq!(
            crate::execution::concurrency::slot_of(&pool, owner)
                .await
                .unwrap(),
            None
        );

        let job = load_job(&pool, job_id).await.unwrap();
        reclaim_the_slot(&state, &job).await;

        assert_eq!(
            crate::execution::concurrency::slot_of(&pool, owner)
                .await
                .unwrap(),
            Some(0)
        );

        // And again, because `claim` is idempotent per owner: a second pass must not take a second
        // number, or every tick would spend one until the ceiling refused the project's next start.
        reclaim_the_slot(&state, &job).await;
        assert_eq!(
            crate::execution::concurrency::slots_in_flight(&pool)
                .await
                .unwrap(),
            1
        );
    }

    /// Every ending gives the slot back, and `retire` is the one place that does it.
    ///
    /// Asserted here rather than at each ending because that is the design: `finish`, `cancel` and
    /// the startup reconciliation all funnel through this function, so an ending added later cannot
    /// forget. A slot held by a finished job is silent — it lowers the project's ceiling with no
    /// error anywhere — which is exactly the failure the sweep on the tick exists to bound.
    #[tokio::test]
    async fn retiring_a_job_gives_its_concurrency_slot_back() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        crate::execution::concurrency::claim(&pool, "project-a", owner)
            .await
            .unwrap();

        retire(&pool, job_id, STATUS_CANCELLED).await.unwrap();

        assert_eq!(
            crate::execution::concurrency::slot_of(&pool, owner)
                .await
                .unwrap(),
            None
        );
    }

    /// "I stopped it" and "it had already finished" are different answers to the question the user
    /// just asked, so the second one is not reported as success.
    #[tokio::test]
    async fn cancelling_a_job_that_is_already_over_says_so() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let done = seed_job(&pool, "project-a", "completed").await.unwrap();

        assert_eq!(cancel(&state, done).await.unwrap(), CancelOutcome::NotLive);
        assert_eq!(job_status(&pool, done).await, "completed");
        assert_eq!(
            cancel(&state, 9_999).await.unwrap(),
            CancelOutcome::NotFound
        );
    }

    /// A job's queue is what the detail view is for, and two of its columns answer different
    /// questions: `status` says where an item got to, `gate_status` says whether anything measured
    /// it. An item reading `passed` with no gate status was never measured.
    #[tokio::test]
    async fn a_jobs_detail_carries_its_queue_and_says_which_items_were_measured() {
        let pool = test_pool().await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["passed", "passed"]).await;
        sqlx::query("UPDATE job_items SET gate_status = 'passed' WHERE job_id = ? AND ordinal = 1")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let detail = detail(&pool, job_id)
            .await
            .unwrap()
            .expect("the job exists");

        assert_eq!(detail.job.status, "implementing");
        assert_eq!(detail.items.len(), 2);
        assert_eq!(detail.items[0].gate_status, None);
        assert_eq!(detail.items[1].gate_status.as_deref(), Some("passed"));
        assert_eq!(detail.branch.as_deref(), Some("nucleos/job"));
        assert!(detail.items[0].ordinal < detail.items[1].ordinal);
    }

    /// Which round an item came from has to travel, because nothing else in the view carries it.
    ///
    /// Ordinals continue across rounds rather than restart, so a queue of three passed items and two
    /// pending ones reads identically whether the plan node asked for five at once or asked for
    /// three and a replan added two. The round is the only thing that separates those, and until it
    /// is on the wire the answer is only in the database.
    #[tokio::test]
    async fn a_jobs_detail_says_which_round_each_item_came_from() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "passed", "pending"]).await;
        sqlx::query("UPDATE job_items SET round = 1 WHERE job_id = ? AND ordinal = 2")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE jobs SET round = 1, max_rounds = 4 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let detail = detail(&pool, job_id)
            .await
            .unwrap()
            .expect("the job exists");

        assert_eq!((detail.job.round, detail.job.max_rounds), (1, 4));
        let rounds: Vec<i64> = detail.items.iter().map(|item| item.round).collect();
        assert_eq!(rounds, vec![0, 0, 1]);
        // The point of the field, stated as what it is not: the ordinals do not say this.
        let ordinals: Vec<i64> = detail.items.iter().map(|item| item.ordinal).collect();
        assert_eq!(ordinals, vec![0, 1, 2]);
    }

    /// A job of today reads as one round of one, not as round zero of nothing.
    #[tokio::test]
    async fn a_job_without_rounds_lists_as_a_single_round() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "implementing").await.unwrap();

        let listed = list(&pool, Some("project-a"), 20).await.unwrap();

        assert_eq!((listed[0].round, listed[0].max_rounds), (0, 1));
    }

    /// Finished jobs stay in the listing. A job that stopped for the budget or ran out of clock is
    /// exactly the one worth looking at, and filtering to live ones would make it vanish at the
    /// moment it started mattering.
    #[tokio::test]
    async fn the_listing_keeps_the_jobs_that_already_ended() {
        let pool = test_pool().await;
        let old = seed_job(&pool, "project-a", "completed").await.unwrap();
        let live = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_job(&pool, "project-b", "implementing").await.unwrap();

        let mine = list(&pool, Some("project-a"), 20).await.unwrap();
        assert_eq!(
            mine.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![live, old],
            "newest first, and the finished one is still there"
        );
        assert_eq!(list(&pool, None, 20).await.unwrap().len(), 3);
    }

    /// The canvas draws a slot, and this is what ties one to the job holding it. `None` for a
    /// finished job is not a serialisation detail: a job that ended has already given the number
    /// back, and saying it still holds one would make the column count work that does not exist.
    #[tokio::test]
    async fn a_jobs_summary_carries_the_slot_it_holds_and_drops_it_when_it_ends() {
        let pool = test_pool().await;
        let live = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let done = seed_job(&pool, "project-a", "completed").await.unwrap();
        crate::execution::concurrency::claim(
            &pool,
            "project-a",
            crate::execution::worktree::Owner::Job(live),
        )
        .await
        .unwrap();

        let listed = list(&pool, Some("project-a"), 20).await.unwrap();

        let live_row = listed.iter().find(|row| row.id == live).unwrap();
        let done_row = listed.iter().find(|row| row.id == done).unwrap();
        assert_eq!(live_row.slot, Some(0));
        assert_eq!(done_row.slot, None);
    }

    /// Job ids and run ids come from different sequences and collide constantly. A join on the id
    /// alone would hand this job the slot belonging to the run that shares its number.
    #[tokio::test]
    async fn a_job_does_not_borrow_the_slot_of_the_run_with_its_number() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        crate::execution::concurrency::claim(
            &pool,
            "project-a",
            crate::execution::worktree::Owner::Run(job_id),
        )
        .await
        .unwrap();

        let listed = list(&pool, Some("project-a"), 20).await.unwrap();

        assert_eq!(
            listed[0].slot, None,
            "that slot is the run's, not this job's"
        );
    }

    /// The listing's ceiling of 20 is a window onto history. A live job outside it is precisely
    /// what the canvas needs in order to describe a slot, and precisely the case the window hides.
    #[tokio::test]
    async fn the_live_listing_reaches_a_live_job_the_recent_window_would_hide() {
        let pool = test_pool().await;
        let old_live = seed_job(&pool, "project-a", "implementing").await.unwrap();
        for _ in 0..25 {
            seed_job(&pool, "project-b", "completed").await.unwrap();
        }

        let recent = list(&pool, None, 20).await.unwrap();
        assert!(
            !recent.iter().any(|row| row.id == old_live),
            "the setup did not push the live job out of the window"
        );

        let live = list_live(&pool, None, crate::execution::concurrency::LIVE_LIST_LIMIT)
            .await
            .unwrap();
        assert!(live.iter().any(|row| row.id == old_live));
    }

    /// Only the live ones, and "live" is the same list the slot sweep spares.
    #[tokio::test]
    async fn the_live_listing_carries_nothing_terminal() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "completed").await.unwrap();
        seed_job(&pool, "project-a", "cancelled").await.unwrap();
        let alive = seed_job(&pool, "project-a", "waiting").await.unwrap();

        let live = list_live(&pool, None, crate::execution::concurrency::LIVE_LIST_LIMIT)
            .await
            .unwrap();

        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, alive);
    }

    // ---- resolving a request into a start ------------------------------------------------------

    fn summary(
        project_id: &str,
        mode: crate::governance::autopilot::Mode,
        root: Option<&str>,
    ) -> crate::governance::autopilot::ProjectSummary {
        crate::governance::autopilot::ProjectSummary {
            project_id: project_id.to_string(),
            mode,
            project_root: root.map(str::to_string),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_review_items: 0,
            open_proposals: 0,
            open_shadow_decisions: 0,
            wip_limit: None,
            queue_full: false,
            last_gate: None,
            last_gate_at: None,
        }
    }

    /// The whole refusal table, with no database in sight.
    ///
    /// The `Shadow` row is the one that matters. `resolve_run_request` maps shadow onto a real run
    /// mode, and copying that table across is the natural mistake — but a job's plan node has to
    /// WRITE `plan.json`, and a plan-only node writes nothing. A job demoted to shadow would do
    /// nothing at all while reporting that it was working, which is the failure a person only
    /// discovers in the morning.
    #[test]
    fn um_pedido_de_job_resolve_ou_recusa_com_motivo_proprio() {
        use crate::governance::autopilot::Mode;
        let roster = vec![
            summary("off", Mode::Off, Some("/repos/off")),
            summary("shadow", Mode::Shadow, Some("/repos/shadow")),
            summary("rootless", Mode::Active, None),
            summary("live", Mode::Active, Some("/repos/live")),
        ];

        assert_eq!(
            resolve_start(&roster, "nao-existe"),
            Err(StartRefusal::UnknownProject)
        );
        assert_eq!(
            resolve_start(&roster, "off"),
            Err(StartRefusal::NotActive(Mode::Off))
        );
        assert_eq!(
            resolve_start(&roster, "shadow"),
            Err(StartRefusal::NotActive(Mode::Shadow)),
            "shadow is plan-only, so it is a refusal and never a mode a job falls back to"
        );
        assert_eq!(
            resolve_start(&roster, "rootless"),
            Err(StartRefusal::NoRoot)
        );
        assert_eq!(
            resolve_start(&roster, "live"),
            Ok(ResolvedStart {
                project_root: "/repos/live".to_string()
            })
        );

        // The two refusals that share a status code still have to be told apart by the person
        // reading them, or "422" is all the answer they get.
        assert!(
            StartRefusal::NotActive(Mode::Shadow)
                .reason("live")
                .contains("shadow")
        );
        assert!(
            StartRefusal::NotActive(Mode::Off)
                .reason("live")
                .contains("off")
        );
        assert_ne!(
            StartRefusal::NotActive(Mode::Shadow).reason("live"),
            StartRefusal::NotActive(Mode::Off).reason("live")
        );
    }

    // ---- the whole walk ------------------------------------------------------------------------

    /// A repository with a real gate, so a walk measures something rather than waving items
    /// through. The commands used are `git`, which runs everywhere this daemon does and answers in
    /// milliseconds.
    fn walkable_repo(prefix: &str, gate: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        let container = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(base)
            .expect("create space-free tempdir");
        let repo = container.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create repository directory");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@x"],
            vec!["config", "user.name", "test"],
        ] {
            assert!(git_ok(&repo, &args));
        }
        std::fs::write(repo.join("seed.txt"), "seed\n").expect("seed the repository");
        for args in [vec!["add", "-A"], vec!["commit", "-m", "seed"]] {
            assert!(git_ok(&repo, &args));
        }
        // The gate, where a job reads it: the project's state directory under a stand-in home
        // that lives in the same container (see `with_home`). Written for both ids these walks use,
        // since the rules are keyed by the project and not by the folder.
        for project_id in ["nucleos", "project-a"] {
            crate::project::state::write_for_test(
                &container.path().join(WALK_HOME),
                project_id,
                crate::project::state::AUTOPILOT_FILE,
                &format!("gate_command: {gate}\nschedules: []\n"),
            );
        }
        (container, repo)
    }

    /// The stand-in for `~/.nucleos` inside a [`walkable_repo`]'s container.
    const WALK_HOME: &str = "nucleos-home";

    /// `state`, reading project state from the stand-in home [`walkable_repo`] wrote the gate into.
    fn with_home(mut state: AppState, container: &tempfile::TempDir) -> AppState {
        state.machine_config_root = Some(container.path().join(WALK_HOME));
        state
    }

    fn git_ok(repo: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git")
            .status
            .success()
    }

    /// **The proof of this slice: undoing a red merge takes back that merge and nothing else.**
    ///
    /// Two items with checkouts of their own. A lands and is kept; B lands and is rejected. The
    /// reset has to leave the branch holding A's file and not B's — and the reason it is not
    /// obvious is the ordinals. `footing_for`, which is what a sequential job reverts by, walks
    /// BACKWARDS THROUGH ORDINALS to the nearest item a gate agreed with. Here B is ordinal 0 and A
    /// is ordinal 1, and B merged second: reverting B by ordinal finds nothing before it and falls
    /// back to the job's `head_sha`, which takes A's merge with it.
    ///
    /// So the ordinals are deliberately the reverse of the merge order. That is not a contrived
    /// case — it is the ordinary one the moment items are allowed to finish at their own speed.
    #[tokio::test(flavor = "current_thread")]
    async fn reverting_a_red_merge_leaves_the_other_items_work_on_the_branch() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-merge-", "git --version");
        let trees = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(trees.path());
        let pool = test_pool().await;
        let job_id = seed_job_in(
            &pool,
            "nucleos",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();

        // The job's own checkout, on the job's branch. This is what the merges land in.
        let job_tree = crate::execution::worktree::create(
            &repo,
            crate::execution::worktree::Owner::Job(job_id),
        )
        .await
        .expect("the job's checkout");
        crate::execution::worktree::record(
            &pool,
            crate::execution::worktree::Owner::Job(job_id),
            "nucleos",
            &repo.to_string_lossy(),
            &job_tree.path.to_string_lossy(),
            &job_tree.branch,
            job_tree.base_sha.as_deref(),
        )
        .await
        .unwrap();

        // Ordinal 0 is the one that will go red, ordinal 1 the one that must survive it.
        seed_items(&pool, job_id, &["implemented", "implemented"]).await;
        let items: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();

        // Each item works in a checkout of its own, born where the job's branch stands, writes one
        // file there — and LEAVES IT UNCOMMITTED, which is what an implement node is told to do
        // and therefore what production actually produces.
        //
        // It used to commit by hand here, and that hand-commit is what hid the hole this test now
        // covers: nothing in the daemon committed an item's work, so every merge brought across an
        // empty branch and succeeded. The fixture was asserting a thing no code did.
        for (item_id, file) in [(items[0], "red.txt"), (items[1], "green.txt")] {
            let base = crate::execution::worktree::head_sha(&job_tree.path)
                .await
                .unwrap();
            let tree = crate::execution::worktree::adopt_or_create_at(
                &repo,
                crate::execution::worktree::Owner::Item(item_id),
                Some(&base),
            )
            .await
            .expect("the item's checkout");
            // Recorded, as `create_run_with` records it in production: the merge finds the checkout
            // to commit through this row, and an item whose tree is not on record has none.
            crate::execution::worktree::record(
                &pool,
                crate::execution::worktree::Owner::Item(item_id),
                "project-a",
                &repo.to_string_lossy(),
                &tree.path.to_string_lossy(),
                &tree.branch,
                tree.base_sha.as_deref(),
            )
            .await
            .expect("record the item's checkout");
            std::fs::write(tree.path.join(file), "work\n").expect("write the item's work");
        }

        // The green one lands first, and is kept.
        let state = test_state(pool.clone()).await;
        let state = with_home(state, &_container);
        assert_eq!(
            merge_item(&state, &load_job(&pool, job_id).await.unwrap(), 1).await,
            Step::Continued
        );
        assert!(job_tree.path.join("green.txt").exists());

        // Then the red one lands, on top of it.
        assert_eq!(
            merge_item(&state, &load_job(&pool, job_id).await.unwrap(), 0).await,
            Step::Continued
        );
        assert!(job_tree.path.join("red.txt").exists());

        // And is undone.
        let target = revert_point(&pool, &load_job(&pool, job_id).await.unwrap(), 0)
            .await
            .expect("a red merge knows where it started");
        crate::execution::worktree::revert_to(&job_tree.path, &target)
            .await
            .expect("put the branch back");

        assert!(
            !job_tree.path.join("red.txt").exists(),
            "the rejected merge is still on the branch"
        );
        assert!(
            job_tree.path.join("green.txt").exists(),
            "undoing item 0 took item 1's merge with it — which is what reverting by ordinal does"
        );
    }

    /// A job whose two items have both been merged into its branch, item 0 (`red.txt`) after
    /// item 1 (`green.txt`) — the ordinals deliberately the reverse of the merge order, as in
    /// `reverting_a_red_merge_leaves_the_other_items_work_on_the_branch`.
    ///
    /// The three tests below start from exactly this state, and it takes a repository, a worktree
    /// root and two checkouts to build, so it is built once. The caller holds `test_env_lock()`
    /// BEFORE calling and for as long as it uses the result; the fields are declared in the order
    /// they must be dropped (the environment override first, the directories it points at last).
    struct MergedJob {
        _env: WorktreeRootEnv,
        _trees: tempfile::TempDir,
        _container: tempfile::TempDir,
        pool: sqlx::SqlitePool,
        state: AppState,
        job_id: i64,
        tree: std::path::PathBuf,
    }

    async fn a_job_with_two_merged_items() -> MergedJob {
        let (container, repo) = walkable_repo("nucleos-job-red-merge-", "git --version");
        let trees = tempfile::tempdir().expect("worktree root");
        let env = WorktreeRootEnv::set(trees.path());
        let pool = test_pool().await;
        let job_id = seed_job_in(
            &pool,
            "nucleos",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();

        // The job's own checkout, on the job's branch. This is what the merges land in.
        let job_tree = crate::execution::worktree::create(
            &repo,
            crate::execution::worktree::Owner::Job(job_id),
        )
        .await
        .expect("the job's checkout");
        crate::execution::worktree::record(
            &pool,
            crate::execution::worktree::Owner::Job(job_id),
            "nucleos",
            &repo.to_string_lossy(),
            &job_tree.path.to_string_lossy(),
            &job_tree.branch,
            job_tree.base_sha.as_deref(),
        )
        .await
        .unwrap();

        seed_items(&pool, job_id, &["implemented", "implemented"]).await;
        let items: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();

        // Each item's work is left uncommitted in a checkout of its own, as an implement node
        // leaves it.
        for (item_id, file) in [(items[0], "red.txt"), (items[1], "green.txt")] {
            let base = crate::execution::worktree::head_sha(&job_tree.path)
                .await
                .unwrap();
            let tree = crate::execution::worktree::adopt_or_create_at(
                &repo,
                crate::execution::worktree::Owner::Item(item_id),
                Some(&base),
            )
            .await
            .expect("the item's checkout");
            crate::execution::worktree::record(
                &pool,
                crate::execution::worktree::Owner::Item(item_id),
                "project-a",
                &repo.to_string_lossy(),
                &tree.path.to_string_lossy(),
                &tree.branch,
                tree.base_sha.as_deref(),
            )
            .await
            .expect("record the item's checkout");
            std::fs::write(tree.path.join(file), "work\n").expect("write the item's work");
        }

        let state = test_state(pool.clone()).await;
        let state = with_home(state, &container);
        // The green one lands first, then the red one on top of it.
        for ordinal in [1, 0] {
            assert_eq!(
                merge_item(&state, &load_job(&pool, job_id).await.unwrap(), ordinal).await,
                Step::Continued
            );
        }
        assert!(job_tree.path.join("green.txt").exists());
        assert!(job_tree.path.join("red.txt").exists());

        MergedJob {
            _env: env,
            _trees: trees,
            _container: container,
            pool,
            state,
            job_id,
            tree: job_tree.path,
        }
    }

    /// What taking item 0's merge back must have done to the job's branch, and nothing more.
    fn assert_only_the_red_merge_was_taken_back(tree: &std::path::Path) {
        assert!(
            !tree.join("red.txt").exists(),
            "the rejected merge is still on the branch"
        );
        assert!(
            tree.join("green.txt").exists(),
            "undoing item 0 took item 1's merge with it"
        );
    }

    /// A merged item that goes red with a retry left is taken OFF the job branch, and asked for
    /// again.
    ///
    /// A sequential item that goes red with a retry left keeps its tree, because the tree is where
    /// the second node starts from. A merged item's work is on the JOB's branch, and the retry
    /// builds a fresh checkout for the item; leaving the first attempt on the branch would land the
    /// second on top of it. So the merge is reset away first, and only then does the item read
    /// `gate_failed` with a retry left.
    #[tokio::test(flavor = "current_thread")]
    async fn a_merged_item_gated_red_with_a_retry_left_is_taken_off_the_job_branch() {
        let _lock = crate::execution::worktree::test_env_lock();
        let job = a_job_with_two_merged_items().await;
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job.job_id)
            .execute(&job.pool)
            .await
            .unwrap();
        let row = load_job(&job.pool, job.job_id).await.unwrap();

        let step = record_gate(&job.state, &row, 0, red_gate()).await;

        assert_eq!(step, Step::Continued);
        assert_only_the_red_merge_was_taken_back(&job.tree);
        let (status, attempts): (String, i64) = sqlx::query_as(
            "SELECT status, gate_attempts FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job.job_id)
        .fetch_one(&job.pool)
        .await
        .unwrap();
        assert_eq!((status.as_str(), attempts), ("gate_failed", 1));

        // Item 1 has been gated green in the meantime, as the queue would have done.
        sqlx::query("UPDATE job_items SET status = 'passed' WHERE job_id = ? AND ordinal = 1")
            .bind(job.job_id)
            .execute(&job.pool)
            .await
            .unwrap();
        assert_eq!(
            next_step(&load_view(&job.pool, job.job_id).await.unwrap()),
            Next::SpawnImplement(vec![0]),
            "the queue has to come back to the same item"
        );
    }

    /// Out of retries, the same reset happens and the item is over: `gate_failed`, and a lesson
    /// queued for the distiller.
    ///
    /// The distill row is the proof that the settle is the one place the verdict is written: the
    /// `RunExhausted` cause is queued by the same write that records `gate_failed` once the
    /// attempts exceed the budget.
    #[tokio::test(flavor = "current_thread")]
    async fn a_merged_item_gated_red_out_of_retries_is_reverted_then_gate_failed() {
        let _lock = crate::execution::worktree::test_env_lock();
        let job = a_job_with_two_merged_items().await;
        sqlx::query("UPDATE jobs SET gate_retries = 0 WHERE id = ?")
            .bind(job.job_id)
            .execute(&job.pool)
            .await
            .unwrap();
        let row = load_job(&job.pool, job.job_id).await.unwrap();

        let step = record_gate(&job.state, &row, 0, red_gate()).await;

        assert_eq!(step, Step::Continued);
        assert_only_the_red_merge_was_taken_back(&job.tree);
        let (status, attempts): (String, i64) = sqlx::query_as(
            "SELECT status, gate_attempts FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job.job_id)
        .fetch_one(&job.pool)
        .await
        .unwrap();
        assert_eq!((status.as_str(), attempts), ("gate_failed", 1));
        assert_eq!(
            item_state_from(&status, attempts, 0),
            ItemState::GateFailed,
            "no retry is left, so this is the end of the item"
        );
        let queued: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue WHERE job_id = ? AND cause = ?")
                .bind(job.job_id)
                .bind(crate::distill::Cause::RunExhausted.as_str())
                .fetch_one(&job.pool)
                .await
                .unwrap();
        assert_eq!(queued, 1, "an exhausted item is a lesson for the distiller");
    }

    /// A job restarted while an item reads `reverted` finishes the settle and moves on.
    ///
    /// `reverted` is the window between putting the branch back and writing the verdict. A daemon
    /// that died inside it leaves the item there, and before the settle was a step of its own such
    /// a job waited on it for ever. `advance` has to put the branch back (the reset may or may not
    /// have happened; doing it again is harmless), write the verdict, and not stall.
    #[tokio::test(flavor = "current_thread")]
    async fn a_job_restarted_with_an_item_reverted_settles_it_and_moves_on() {
        let _lock = crate::execution::worktree::test_env_lock();
        let job = a_job_with_two_merged_items().await;
        sqlx::query(
            "UPDATE job_items SET status = 'reverted', gate_status = 'failed', gate_attempts = 1
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job.job_id)
        .execute(&job.pool)
        .await
        .unwrap();
        assert!(
            job.tree.join("red.txt").exists(),
            "the premise: the branch was not yet put back when the daemon died"
        );

        let step = advance(
            &job.state,
            &load_job(&job.pool, job.job_id).await.unwrap(),
            Utc::now(),
        )
        .await;

        assert_eq!(
            step,
            Step::Continued,
            "a restart must not stall on the item"
        );
        assert_only_the_red_merge_was_taken_back(&job.tree);
        assert_eq!(item_statuses(&job.pool, job.job_id).await[0], "gate_failed");
        assert_ne!(
            next_step(&load_view(&job.pool, job.job_id).await.unwrap()),
            Next::SettleRevert { ordinal: 0 },
            "settled once is settled"
        );
    }

    /// A job nobody scheduled: no rule on the row, and no rule in the line a person reads.
    ///
    /// `rule_name` was `&'a str` until a second caller appeared that has no rule to name. The
    /// column has accepted NULL since migration 0042, so this pins the type to the schema rather
    /// than widening anything — and it pins the feed line, which is the half a person actually
    /// sees. `job 7 started for rule 'None'` is the shape this exists to prevent: a line that reads
    /// like a bug in the scheduler for a job the scheduler never touched.
    #[tokio::test(flavor = "current_thread")]
    async fn um_job_sem_regra_nao_inventa_uma_no_registo_nem_no_feed() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-norule-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let state = with_home(state, &_container);

        let started = start(
            &state,
            &StartRequest {
                project_id: "nucleos",
                project_root: &repo.to_string_lossy(),
                rule_name: None,
                prompt: "build the thing",
                max_items: 3,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await;

        let JobStart::Started(job_id) = started else {
            panic!("a job with no rule must still start");
        };

        let rule_name: Option<String> =
            sqlx::query_scalar("SELECT rule_name FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            rule_name, None,
            "no rule asked for this job, so the column has to say so rather than carry a sentinel"
        );

        let (line, subject): (String, Option<String>) = sqlx::query_as(
            "SELECT summary, subject FROM feed WHERE kind = 'job_started' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(subject, Some(format!("job:{job_id}")));
        assert!(
            !line.contains("rule"),
            "a job with no rule must not name one: {line}"
        );
        // The positive half, without which the assertion above would pass on an empty line.
        assert!(
            line.starts_with(&format!("job {job_id} started on ")),
            "the line still has to say what started and where: {line}"
        );
    }

    /// Points worktree provisioning at a root of the test's own, and switches the disk floor off.
    ///
    /// **The floor measures the machine, not the code.** `runs::no_room_on_disk` refuses an item's
    /// checkout when the volume `NUCLEOS_WORKTREE_ROOT` lives on has less than 5 GiB free, and the
    /// checkouts these fixtures open are a few kilobytes — so the floor has nothing to say about
    /// what they exercise, and everything to say about whatever else is filling the disk. On
    /// 2026-09-13 that was the suite itself, several multi-GB cargo target trees growing under
    /// load: five team walks failed together with `last status waiting` (one of them again, alone,
    /// in a gate on 2026-09-14), each an item run refused as `Busy`, a job parked as `slot`, and
    /// forty parked passes spun through in milliseconds. Every one of them passed run by itself, and
    /// every one failed on demand with `NUCLEOS_MIN_FREE_DISK_GB=100000`. The refusal is still
    /// tested — in `runs.rs`, on purpose, with a floor no disk meets.
    ///
    /// Both variables come back as they were on drop. Callers hold `test_env_lock`, which is what
    /// makes a process-wide variable safe to set at all.
    struct WorktreeRootEnv {
        root: Option<std::ffi::OsString>,
        floor: Option<std::ffi::OsString>,
    }
    impl WorktreeRootEnv {
        fn set(path: &std::path::Path) -> Self {
            let root = std::env::var_os("NUCLEOS_WORKTREE_ROOT");
            let floor = std::env::var_os("NUCLEOS_MIN_FREE_DISK_GB");
            unsafe {
                std::env::set_var("NUCLEOS_WORKTREE_ROOT", path);
                std::env::set_var("NUCLEOS_MIN_FREE_DISK_GB", "0");
            }
            Self { root, floor }
        }
    }
    impl Drop for WorktreeRootEnv {
        fn drop(&mut self) {
            for (name, previous) in [
                ("NUCLEOS_WORKTREE_ROOT", &self.root),
                ("NUCLEOS_MIN_FREE_DISK_GB", &self.floor),
            ] {
                unsafe {
                    match previous {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    /// Lets the node a pass started finish before the next pass reads its status.
    ///
    /// Nodes run as spawned tasks, so without this every pass would find its own node still
    /// `running` and answer `Wait` forever — the walk would hang rather than fail, which is the
    /// least useful way for a test to be wrong.
    ///
    /// **On a deadline, and not a count.** It was 500 `yield_now()`s, which is a budget in scheduler
    /// turns rather than time — and how much time a turn is worth is exactly what a loaded machine
    /// changes. Two minutes of wall clock is purely a hang guard: the scripted nodes finish at once,
    /// so the ordinary case returns in milliseconds, and when it does fire it names the runs it
    /// gave up on.
    ///
    /// This is not what failed on 2026-09-13. Those walks ended `waiting`, a park, and a parked job
    /// has no node in flight for this to wait on; `why_it_stands` is where a park shows.
    async fn settle(state: &AppState, job_id: i64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let running: Vec<(i64, Option<String>, Option<i64>)> = sqlx::query_as(
                "SELECT id, stage, item_id FROM runs
                  WHERE job_id = ? AND status = 'running' ORDER BY id",
            )
            .bind(job_id)
            .fetch_all(&state.pool)
            .await
            .unwrap();
            if running.is_empty() {
                return;
            }
            if std::time::Instant::now() >= deadline {
                let named = running
                    .iter()
                    .map(|(id, stage, item)| {
                        let stage = stage.as_deref().unwrap_or("no stage");
                        match item {
                            Some(item) => format!("run {id} ({stage}, item {item})"),
                            None => format!("run {id} ({stage})"),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                panic!("a node never finished: {named} still running after two minutes");
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// What one walk cost, in the two units that matter.
    ///
    /// **Passes and never seconds.** The tick is 30 s and `MAX_STEPS_PER_PASS` is 4, so a job's wall
    /// clock is how many times the daemon has to come back to it. A stopwatch would measure this
    /// machine on this afternoon; passes measure the schedule, and answer the same on a busy laptop
    /// and an idle server.
    ///
    /// **And writing rounds, because passes alone cannot see what parallelism buys.** A pass stops
    /// as soon as it starts a node, so a batch of two costs one pass exactly as a batch of one does
    /// — and the merges and gates behind them are a queue either way. What the design shortens is
    /// the WRITING: `24 min -> 8 min (overlapped)` in the design's own table. Here the scripted
    /// agent returns instantly, so that term weighs nothing and the totals come out equal. Counting
    /// the passes in which an implement node actually started is how the overlapped term is
    /// measured at all.
    #[derive(Debug)]
    struct Walk {
        ending: String,
        /// Every pass the daemon made, which is the wall-clock term the tick dominates.
        passes: usize,
        /// The passes that started at least one implement node: how many rounds of WRITING the
        /// queue took. This is the number a ceiling of two is supposed to shrink.
        writing: usize,
        /// How many implement nodes each item cost, by ordinal. A two is an item that was asked
        /// again, which is the only durable trace an unhappy event leaves: the states it passed
        /// through are gone by the time the walk ends.
        attempts: Vec<i64>,
        /// The passes that ended with the job `waiting`, each with the reason it gave then. A park
        /// costs a pass and says nothing about the schedule, so a walk that is going to be compared
        /// with another must have none — `walk_measured` refuses one that does.
        parked: Vec<String>,
    }

    async fn walk_counting(state: &AppState, job_id: i64) -> Walk {
        let started = |pool: sqlx::SqlitePool| async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM runs WHERE job_id = ? AND stage = 'implement'",
            )
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        let mut writing = 0;
        let mut parked = Vec::new();
        for pass in 1..=40 {
            let before = started(state.pool.clone()).await;
            job_tick(state, Utc::now()).await;
            settle(state, job_id).await;
            if started(state.pool.clone()).await > before {
                writing += 1;
            }
            let status = job_status(&state.pool, job_id).await;
            if status == "waiting" {
                parked.push(format!(
                    "pass {pass}: {}",
                    why_it_stands(&state.pool, job_id).await
                ));
            }
            if !LIVE_STATUSES.contains(&status.as_str()) {
                let attempts = sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM runs
                      WHERE job_id = ? AND stage = 'implement' AND item_id IS NOT NULL
                      GROUP BY item_id ORDER BY item_id",
                )
                .bind(job_id)
                .fetch_all(&state.pool)
                .await
                .unwrap();
                return Walk {
                    ending: status,
                    passes: pass,
                    writing,
                    attempts,
                    parked,
                };
            }
        }
        panic!(
            "the job never reached an ending; last {} ({} of 40 passes ended parked)",
            why_it_stands(&state.pool, job_id).await,
            parked.len()
        );
    }

    /// Why a job is standing still, in words a failing assertion can print.
    ///
    /// A park leaves its reason in three places and the status is none of them: `wait_reason` on
    /// the job's row, a `job_waiting` line from `park`, and — when it was a run's provisioning that
    /// said no — a `worktree_provision_failed` line naming the wall. Until 2026-09-14 only the last
    /// told the slot from the disk, because a refused item parked its job as `slot` either way; the
    /// job now parks as `disk`, and the line is still the one with the numbers. The walks used to
    /// print the status alone, and on 2026-09-13 five of them failed with `last status waiting` and
    /// nothing else: the disk floor refusing every item, and not a word of it on screen.
    ///
    /// Newest first, and three lines at most. The pool is the test's own, so every line is this
    /// job's.
    async fn why_it_stands(pool: &sqlx::SqlitePool, job_id: i64) -> String {
        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(pool)
                .await
                .unwrap();
        let lines: Vec<(String, String)> = sqlx::query_as(
            "SELECT kind, summary FROM feed
              WHERE kind IN ('job_waiting', 'worktree_provision_failed')
              ORDER BY id DESC LIMIT 3",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let lines = if lines.is_empty() {
            "none".to_owned()
        } else {
            lines
                .iter()
                .map(|(kind, summary)| format!("[{kind}] {summary}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        format!(
            "status {status}, wait_reason {}, feed: {lines}",
            reason.as_deref().unwrap_or("none")
        )
    }

    /// A team of one, and a job it directs, over a real repository.
    ///
    /// The plan goes in through `plan_to_write`, so the queue this walks is one the daemon read off
    /// disk and put through `Graph::check` — files, dependencies and an agent id all validated —
    /// rather than three rows a fixture inserted.
    async fn start_directed_job(
        state: &AppState,
        runner: &crate::execution::runner::FakeCommandRunner,
        repo: &std::path::Path,
        plan: &str,
        max_parallel: i64,
        gate_retries: i64,
    ) -> i64 {
        seed_agent(&state.pool, "ana", "everything", "you are careful").await;
        seed_crew(&state.pool, "crew", "ana", &[]).await;
        sqlx::query("UPDATE teams SET max_parallel = ? WHERE id = 'crew'")
            .bind(max_parallel)
            .execute(&state.pool)
            .await
            .unwrap();

        *runner.plan_to_write.lock().unwrap() = Some(plan.to_owned());
        let root = repo.to_string_lossy().into_owned();
        let head_sha = crate::repo_trigger::current_branch_sha(repo, "HEAD", false).await;
        let job_id = crate::execution::job::start(
            state,
            &StartRequest {
                project_id: "project-a",
                project_root: &root,
                rule_name: Some("nightly"),
                prompt: "advance the backlog",
                max_items: 5,
                gate_each: true,
                // Off: a review node is one more pass in both configurations, so it cancels out of
                // the comparison while adding a node to every walk.
                review: false,
                gate_retries,
                head_sha: head_sha.as_deref(),
                max_rounds: None,
                budget_usd: None,
                team_id: Some("crew"),
            },
        )
        .await;
        match job_id {
            crate::execution::job::JobStart::Started(job_id) => job_id,
            other => panic!("the directed job did not start: {other:?}"),
        }
    }

    /// The plan three scripted items produce. Items 1 and 2 both declare `a.rs`, so the fold may
    /// never start them together; item 3 declares `b.rs` and may run beside either.
    const THREE_ITEMS: &str = r#"{"items":[
        {"description":"ITEM-ONE: write alpha","files":["a.rs"],"depends_on":[],"agent_id":"ana"},
        {"description":"ITEM-TWO: rewrite alpha","files":["a.rs"],"depends_on":[],"agent_id":"ana"},
        {"description":"ITEM-THREE: write beta","files":["b.rs"],"depends_on":[],"agent_id":"ana"}
    ]}"#;

    fn three_scripted_agents() -> Vec<(String, String, String)> {
        vec![
            (
                "ITEM-ONE".to_owned(),
                "a.rs".to_owned(),
                "alpha\n".to_owned(),
            ),
            (
                "ITEM-TWO".to_owned(),
                "a.rs".to_owned(),
                "alpha, again\n".to_owned(),
            ),
            (
                "ITEM-THREE".to_owned(),
                "b.rs".to_owned(),
                "beta\n".to_owned(),
            ),
        ]
    }

    /// **A job with a team runs its queue to the end, and the work is on the branch.**
    ///
    /// The first end-to-end walk of the parallel path, and the thing that made it worth building:
    /// every slice was green and the path as a whole did nothing, because nothing committed an
    /// item's work and an empty merge succeeds. A test that only ever asserted per-slice behaviour
    /// could not see that, and this one cannot miss it — the files either reach the job's branch
    /// or they do not.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_teams_job_walks_its_whole_queue_and_lands_the_work_on_the_branch() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-accept-whole-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);
        *runner.writes.lock().unwrap() = three_scripted_agents();

        let job_id = start_directed_job(&state, &runner, &repo, THREE_ITEMS, 2, 0).await;
        let walk = walk_counting(&state, job_id).await;

        assert_eq!(walk.ending, "completed", "the queue did not run to the end");
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["passed", "passed", "passed"]
        );

        let (job_tree, _) = job_worktree(&pool, job_id)
            .await
            .unwrap()
            .expect("the job's checkout");
        assert!(
            job_tree.join("b.rs").exists(),
            "item 3's work never reached the job's branch"
        );
        // Trimmed, because git rewrites the line ending on checkout under Windows and this
        // assertion is about WHOSE alpha is on the branch, not about newlines.
        assert_eq!(
            std::fs::read_to_string(job_tree.join("a.rs"))
                .expect("item 1 and 2 wrote alpha")
                .trim_end(),
            "alpha, again",
            "the later item's alpha is what should be on the branch"
        );
    }

    /// Ticks a job until it parks, and hands back the reason on its row and its last waiting line.
    ///
    /// Ten passes is far more than a plan node and a first item need; a job that has not parked by
    /// then is not going to, and the panic says where it stood instead.
    async fn tick_until_parked(state: &AppState, job_id: i64) -> (String, String) {
        for _ in 0..10 {
            job_tick(state, Utc::now()).await;
            settle(state, job_id).await;
            if job_status(&state.pool, job_id).await == "waiting" {
                let reason: Option<String> =
                    sqlx::query_scalar("SELECT wait_reason FROM jobs WHERE id = ?")
                        .bind(job_id)
                        .fetch_one(&state.pool)
                        .await
                        .unwrap();
                let line: String = sqlx::query_scalar(
                    "SELECT summary FROM feed WHERE kind = 'job_waiting' ORDER BY id DESC LIMIT 1",
                )
                .fetch_one(&state.pool)
                .await
                .expect("a park says so in the feed");
                return (reason.unwrap_or_default(), line);
            }
        }
        panic!(
            "the job never parked; last {}",
            why_it_stands(&state.pool, job_id).await
        );
    }

    /// **A job whose item is refused for disk waits for the disk, says how full, and comes back
    /// when there is room.**
    ///
    /// On 2026-09-14, with the floor raised on purpose, a job read `status waiting, wait_reason
    /// slot` and fed "another run holds the project's worktree slot" — and only the next line,
    /// `worktree_provision_failed`, said the disk was the wall. Whoever reads the job goes looking
    /// for a run holding a slot and finds none. The floor here is a million GiB, which no volume
    /// meets, so the answer does not depend on the machine.
    ///
    /// The second half is the resume: `wait_reason` is a note and not a latch, and a disk park has
    /// to lift exactly as a slot park does once the condition clears.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_job_refused_its_item_for_disk_waits_for_the_disk_and_resumes_when_there_is_room() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-disk-park-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);
        *runner.writes.lock().unwrap() = three_scripted_agents();

        let job_id = start_directed_job(&state, &runner, &repo, THREE_ITEMS, 2, 0).await;
        // After the guard, which switched the floor off and puts back what stood before it on drop,
        // so this bare `set_var` is undone with it. The plan node works in the job's own checkout
        // and is never asked; the first item is.
        unsafe { std::env::set_var("NUCLEOS_MIN_FREE_DISK_GB", "1000000") };

        let (reason, line) = tick_until_parked(&state, job_id).await;
        assert_eq!(
            reason, "disk",
            "a disk refusal is not slot contention: {line}"
        );
        assert!(
            line.contains("disk") && line.contains(" MiB free "),
            "the job's own line has to name the disk and how much room there was: {line}"
        );
        assert!(
            line.contains("at least 1024000000 MiB"),
            "and the floor it fell below: {line}"
        );
        assert!(
            !line.contains("slot"),
            "a reader told about a slot goes looking for a run that is not there: {line}"
        );

        unsafe { std::env::set_var("NUCLEOS_MIN_FREE_DISK_GB", "0") };
        // One pass first, and read before the walk: a job whose item starts again moves on either
        // way, so the ending alone cannot tell a park that lifted from a note left standing. A row
        // reading `implementing / disk` names a wall that is gone, which is `resume`'s to clear.
        job_tick(&state, Utc::now()).await;
        settle(&state, job_id).await;
        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(
            status, "waiting",
            "the park did not lift once there was room"
        );
        assert_eq!(
            reason, None,
            "a job that is moving again is not waiting for the disk (status {status})"
        );
        let items_started: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs
              WHERE job_id = ? AND stage = 'implement' AND item_id IS NOT NULL
                AND status != 'failed'",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            items_started > 0,
            "the refused item was never started again"
        );

        let walk = walk_counting(&state, job_id).await;
        assert_eq!(
            walk.ending, "completed",
            "the park did not lift once there was room"
        );
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["passed", "passed", "passed"]
        );
    }

    /// **The control: an item refused for a slot still waits for a slot.**
    ///
    /// The disk got a reason of its own; this is what keeps that change from bleeding into the
    /// contention it was split from. A ceiling of one, held by the job itself, is a project with no
    /// room for its first item.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_job_refused_its_item_for_a_slot_still_waits_for_a_slot() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-slot-park-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);
        *runner.writes.lock().unwrap() = three_scripted_agents();

        let job_id = start_directed_job(&state, &runner, &repo, THREE_ITEMS, 2, 0).await;
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = 1")
            .execute(&pool)
            .await
            .unwrap();

        let (reason, line) = tick_until_parked(&state, job_id).await;
        assert_eq!(reason, "slot", "{line}");
        assert!(
            line.contains("another run holds the project's worktree slot"),
            "the slot's sentence is unchanged, and the shell reads it by these words: {line}"
        );
    }

    /// One walk of the three items at a given ceiling, and how many passes it took.
    ///
    /// A fresh repository and a fresh database each time. Sharing either would let the first walk's
    /// branches and slots decide the second's, which is the one thing a comparison must not have.
    async fn passes_at(max_parallel: i64, tag: &str) -> Walk {
        walk_plan(max_parallel, tag, THREE_ITEMS, three_scripted_agents()).await
    }

    /// The same, for a plan and a cast of scripted agents of the caller's choosing.
    async fn walk_plan(
        max_parallel: i64,
        tag: &str,
        plan: &str,
        agents: Vec<(String, String, String)>,
    ) -> Walk {
        walk_measured(Measured {
            max_parallel,
            tag,
            plan,
            agents,
            gate: "git --version",
            gate_retries: 0,
            seed: &[],
        })
        .await
    }

    /// Everything one walk needs that is not the same for every walk.
    ///
    /// A struct because it is seven things and four of them are strings: a call with seven positional
    /// arguments is a call whose third and fourth get swapped once and never noticed.
    struct Measured<'a> {
        max_parallel: i64,
        tag: &'a str,
        plan: &'a str,
        agents: Vec<(String, String, String)>,
        /// The project's `gate_command`. `git --version` is the gate that always agrees.
        gate: &'a str,
        /// How many EXTRA implement runs a red gate may buy. Zero makes the first red terminal.
        gate_retries: i64,
        /// Files committed into the repository before the job starts, so the gate has something to
        /// measure that the queue did not create.
        seed: &'a [(&'a str, &'a str)],
    }

    async fn walk_measured(measured: Measured<'_>) -> Walk {
        let Measured {
            max_parallel,
            tag,
            plan,
            agents,
            gate,
            gate_retries,
            seed,
        } = measured;
        let (_container, repo) = walkable_repo(tag, gate);
        for (path, contents) in seed {
            std::fs::write(repo.join(path), contents).expect("seed a file the gate measures");
            assert!(git_ok(&repo, &["add", path]));
        }
        if !seed.is_empty() {
            assert!(git_ok(&repo, &["commit", "-m", "what the gate measures"]));
        }
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);
        *runner.writes.lock().unwrap() = agents;

        let job_id =
            start_directed_job(&state, &runner, &repo, plan, max_parallel, gate_retries).await;
        let walk = walk_counting(&state, job_id).await;

        // First, because every comparison these walks feed is a `<=` on passes. A park costs a
        // pass for a reason that is this machine's and not the schedule's, so one on either side
        // would flip the comparison — or, worse, satisfy it — and the assertion would still read
        // as a verdict on the design. A park that never lifts already fails in `walk_counting`;
        // this is the one that lifted and left only a larger number behind.
        assert!(
            walk.parked.is_empty(),
            "the walk at {max_parallel} parked, which is the machine refusing and not the \
             schedule: {:?}",
            walk.parked
        );
        assert_eq!(
            walk.ending, "completed",
            "the walk at {max_parallel} did not finish"
        );
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["passed", "passed", "passed"],
            "the walk at {max_parallel} did not do all the work"
        );
        walk
    }

    /// Three items whose declarations are honest and still collide.
    ///
    /// **A conflict cannot be produced by two items declaring the same file**, because that is
    /// exactly what the fold refuses to start together — so the design's own §5 names where a real
    /// one comes from: *"duas famílias que o predicado não vê"*. Two items that declare different
    /// work and both touch a REGISTRY the director never thought to name — a lock file, a module
    /// list, a migrations directory. Nobody lied; the declaration simply cannot see it.
    ///
    /// Items 1 and 2 declare `a.rs` and `b.rs`, so the fold starts them together, and both write
    /// `registry.txt`. The first to land is clean; the second conflicts.
    const THREE_ITEMS_ONE_REGISTRY: &str = r#"{"items":[
        {"description":"ITEM-ONE: alpha","files":["a.rs"],"depends_on":[],"agent_id":"ana"},
        {"description":"ITEM-TWO: beta","files":["b.rs"],"depends_on":[],"agent_id":"ana"},
        {"description":"ITEM-THREE: gamma","files":["c.rs"],"depends_on":[],"agent_id":"ana"}
    ]}"#;

    /// The scripted agents for the plan above, including what the conflicted one does second time.
    ///
    /// The last entry is keyed on the catch-up block `catch_up_the_item` appends to a retry's
    /// prompt, which is the one thing in there that says "you are being asked again because your
    /// merge conflicted". It is LAST because the entries are applied in order and a retry matches
    /// both its own marker and this one: a naive agent would rewrite its own line over the conflict
    /// markers and drop the other item's, which is a resolution git accepts and a person would not.
    fn three_agents_over_one_registry() -> Vec<(String, String, String)> {
        vec![
            (
                "ITEM-ONE".to_owned(),
                "a.rs".to_owned(),
                "alpha\n".to_owned(),
            ),
            (
                "ITEM-ONE".to_owned(),
                "registry.txt".to_owned(),
                "one\n".to_owned(),
            ),
            (
                "ITEM-TWO".to_owned(),
                "b.rs".to_owned(),
                "beta\n".to_owned(),
            ),
            (
                "ITEM-TWO".to_owned(),
                "registry.txt".to_owned(),
                "two\n".to_owned(),
            ),
            (
                "ITEM-THREE".to_owned(),
                "c.rs".to_owned(),
                "gamma\n".to_owned(),
            ),
            (
                "left STAGED".to_owned(),
                "registry.txt".to_owned(),
                "one\ntwo\n".to_owned(),
            ),
        ]
    }

    /// **The parallel path pays for a conflict the sequential one never meets, and still does not
    /// lose.** §7's first criterion, with its first unhappy event.
    ///
    /// The asymmetry is the point and it is not a flaw in the fixture. At a ceiling of one, items 1
    /// and 2 never overlap, so item 2's checkout is born AFTER item 1's registry line landed and
    /// its merge is clean — there is no conflict to have. Parallelism is what creates the event,
    /// which is exactly why the design budgets for it: the saving has to survive the cost it causes.
    ///
    /// Measured: five passes either way, and the writing goes from three rounds to two even with the
    /// conflicted item asked a second time. It fits in two because the retry batches with the item
    /// that had not started yet — the fold does not care that one of them is going round again.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn the_parallel_walk_pays_for_its_conflict_and_still_writes_in_fewer_rounds() {
        let _lock = crate::execution::worktree::test_env_lock();

        let sequential = walk_plan(
            1,
            "nucleos-accept-clash-seq-",
            THREE_ITEMS_ONE_REGISTRY,
            three_agents_over_one_registry(),
        )
        .await;
        let parallel = walk_plan(
            2,
            "nucleos-accept-clash-par-",
            THREE_ITEMS_ONE_REGISTRY,
            three_agents_over_one_registry(),
        )
        .await;

        assert!(
            parallel.passes <= sequential.passes,
            "the conflict made the parallel walk slower: {parallel:?} against {sequential:?}"
        );
        assert!(
            parallel.writing < sequential.writing,
            "the saving did not survive the conflict it caused: {parallel:?} against {sequential:?}"
        );
    }

    /// The gate that reads the tree rather than always agreeing.
    ///
    /// `git grep -q good HEAD` exits zero exactly while something on the branch still says `good`,
    /// which the seeded `marker.txt` does and an item that clobbers it stops doing. No shell and no
    /// script in the tree: `run_gate` splits the command into words and spawns the program itself,
    /// and `git` is the one program this suite already cannot run without.
    ///
    /// Steered by what the scripted agents WRITE, and never by the harness reaching in between
    /// passes. A gate a test flips by hand measures the test; this one measures the tree, which is
    /// what a gate is.
    ///
    /// **`-- marker.txt` is deliberately NOT on the end**, and the first attempt at this had it.
    /// `worktree_scripts` treats every relative path among the command's arguments as a gate script
    /// living in the tree, and `tampered_gate_script` then refuses to measure when the run rewrote
    /// one — correctly, and here fatally: naming the very file the queue clobbers turns every red
    /// gate into `gate_errored`, which is "the measurement did not happen" and not "the code is
    /// broken". The guard was right and the command was wrong.
    const GATE_READS_THE_MARKER: &str = "git grep -q good HEAD";

    /// Both unhappy events in one queue.
    ///
    /// Item 2 clobbers the registry that item 1 also touches, and neither declared it — the
    /// conflict. Item 3 clobbers the marker the gate reads, which nobody else touches, so it merges
    /// cleanly and is refused by the gate — the red. Two different ways for undeclared work to go
    /// wrong, and the design answers them differently: one is a question about two pieces of work,
    /// the other is a verdict on one.
    const THREE_ITEMS_TWO_EVENTS: &str = r#"{"items":[
        {"description":"ITEM-ONE: alpha","files":["a.rs"],"depends_on":[],"agent_id":"ana"},
        {"description":"ITEM-TWO: beta","files":["b.rs"],"depends_on":[],"agent_id":"ana"},
        {"description":"ITEM-THREE: gamma","files":["c.rs"],"depends_on":[],"agent_id":"ana"}
    ]}"#;

    /// The cast for the plan above, and the two last entries are the retries.
    ///
    /// `left STAGED` is the sentence `catch_up_the_item` appends when a merge conflicted; `attempted
    /// before` is the one `implement_prompt` appends when a gate said no. They are the only things
    /// in a prompt that say WHY an item is being asked again, they are disjoint, and they are last
    /// because entries are applied in order: a retry matches its own marker too, and what it wrote
    /// the first time is exactly what has to be overwritten.
    fn three_agents_with_two_unhappy_events() -> Vec<(String, String, String)> {
        vec![
            (
                "ITEM-ONE".to_owned(),
                "a.rs".to_owned(),
                "alpha\n".to_owned(),
            ),
            (
                "ITEM-ONE".to_owned(),
                "registry.txt".to_owned(),
                "one\n".to_owned(),
            ),
            (
                "ITEM-TWO".to_owned(),
                "b.rs".to_owned(),
                "beta\n".to_owned(),
            ),
            (
                "ITEM-TWO".to_owned(),
                "registry.txt".to_owned(),
                "two\n".to_owned(),
            ),
            (
                "ITEM-THREE".to_owned(),
                "c.rs".to_owned(),
                "gamma\n".to_owned(),
            ),
            // The work that breaks the build: the marker the gate reads, clobbered.
            (
                "ITEM-THREE".to_owned(),
                "marker.txt".to_owned(),
                "clobbered\n".to_owned(),
            ),
            // Asked again after a conflict: keep both lines.
            (
                "left STAGED".to_owned(),
                "registry.txt".to_owned(),
                "one\ntwo\n".to_owned(),
            ),
            // Asked again after a red gate: put the marker back.
            (
                "attempted before".to_owned(),
                "marker.txt".to_owned(),
                "good\n".to_owned(),
            ),
        ]
    }

    /// **Two unhappy events, and the parallel walk still does not lose.** §7's first criterion,
    /// whole.
    ///
    /// The design budgets sixteen minutes of saving against events that cost eight to twelve each,
    /// and says so plainly: *"um evento deixa-a em 8 ou 4; dois invertem o sinal"*. Two is therefore
    /// the number the criterion has to be tested at, and one would be a test that agreed with the
    /// design about the easy case.
    ///
    /// Only one of the two is symmetric. The RED gate is caused by item 3's own work, so both
    /// ceilings meet it. The CONFLICT exists only in the parallel walk: at a ceiling of one, item 2's
    /// checkout is born after item 1's line landed and there is nothing to conflict with.
    /// Parallelism causes that event, and the saving has to survive the cost it causes.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn two_unhappy_events_do_not_make_the_parallel_walk_the_slower_one() {
        let _lock = crate::execution::worktree::test_env_lock();

        let measured = |max_parallel: i64, tag: &'static str| async move {
            walk_measured(Measured {
                max_parallel,
                tag,
                plan: THREE_ITEMS_TWO_EVENTS,
                agents: three_agents_with_two_unhappy_events(),
                gate: GATE_READS_THE_MARKER,
                // One extra implement run per item. `0069` defaults this to zero deliberately, so a
                // job that wants a red gate to be answerable has to say so; this one does.
                gate_retries: 1,
                seed: &[("marker.txt", "good\n")],
            })
            .await
        };

        let sequential = measured(1, "nucleos-accept-two-seq-").await;
        let parallel = measured(2, "nucleos-accept-two-par-").await;

        assert_eq!(sequential.ending, "completed");
        assert_eq!(parallel.ending, "completed");

        // Both events really happened, or this is a comparison of two happy walks. A `2` is an item
        // asked a second time: item 3 by the gate in both walks, and item 2 by the conflict only
        // where there was parallelism to cause one.
        assert_eq!(
            sequential.attempts,
            vec![1, 1, 2],
            "the red gate did not fire in the sequential walk: {sequential:?}"
        );
        assert_eq!(
            parallel.attempts,
            vec![1, 2, 2],
            "the parallel walk did not meet both events: {parallel:?}"
        );
        assert!(
            parallel.passes <= sequential.passes,
            "two unhappy events made the parallel walk slower: {parallel:?} against {sequential:?}"
        );
        assert!(
            parallel.writing < sequential.writing,
            "the saving did not survive two unhappy events: {parallel:?} against {sequential:?}"
        );
    }

    /// **A conflict is a step, and the queue walks through it.** The first unhappy event of §7.
    ///
    /// Everything the design says about conflict happens here in one walk and nowhere else in the
    /// suite: the merge is refused and the job's checkout is put back clean, the item is put DOWN
    /// rather than failed, it is asked again in the checkout it already has, the job's branch is
    /// brought into that checkout with the conflict left staged, and what the second node writes is
    /// merged and gated.
    ///
    /// The registry file is the assertion that matters. `one\ntwo` on the branch means the two
    /// items' work was really reconciled; either line alone means one of them was silently dropped
    /// by a merge that reported success.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_conflict_over_a_file_nobody_declared_is_resolved_and_the_queue_finishes() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-accept-clash-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);
        *runner.writes.lock().unwrap() = three_agents_over_one_registry();

        let job_id =
            start_directed_job(&state, &runner, &repo, THREE_ITEMS_ONE_REGISTRY, 2, 0).await;
        let walk = walk_counting(&state, job_id).await;

        assert_eq!(walk.ending, "completed", "the conflict stopped the queue");
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["passed", "passed", "passed"]
        );

        // The item that conflicted was asked again, in a checkout it already had. Counted on the
        // runs because a status is transient and this is not: one implement node for the first
        // attempt, one for the resolution.
        assert_eq!(
            walk.attempts,
            vec![1, 2, 1],
            "exactly one item should have been asked twice"
        );

        let (job_tree, _) = job_worktree(&pool, job_id)
            .await
            .unwrap()
            .expect("the job's checkout");
        let registry = std::fs::read_to_string(job_tree.join("registry.txt"))
            .expect("the registry both items touched");
        let lines: Vec<&str> = registry.lines().map(str::trim_end).collect();
        assert_eq!(
            lines,
            vec!["one", "two"],
            "the resolution did not keep both items' work: {registry:?}"
        );
        for file in ["a.rs", "b.rs", "c.rs"] {
            assert!(
                job_tree.join(file).exists(),
                "{file} never reached the job's branch"
            );
        }
    }

    /// **The same work at two ceilings, and the parallel one is not slower.** §7's first criterion.
    ///
    /// Two numbers, because one of them cannot see the thing the design changes.
    ///
    /// **Total passes: not more.** That is the criterion as §7 words it, and the `<=` is the whole
    /// of it. The batch buys overlap on the writing; the merges and the gates behind it stay in a
    /// queue however many items were written at once, so a design that shortened the queue as well
    /// would be a different design. Asserting a strict `<` here was measured and is FALSE: three
    /// items cost five passes at either ceiling, because a pass stops as soon as it starts a node
    /// and the merges cost the same either way.
    ///
    /// **Writing rounds: strictly fewer.** This is the term the ceiling shrinks and the reason the
    /// first assertion is worth having rather than trivially true. §7 says so in as many words: a
    /// parallelism collapsed to one satisfies "not slower" by being the same thing, which is how an
    /// earlier draft of this design convinced itself it worked. Two independent items written in one
    /// round instead of two is the claim, and it is asserted on rounds the daemon really made.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn the_same_queue_at_two_ceilings_and_the_parallel_one_is_not_slower() {
        let _lock = crate::execution::worktree::test_env_lock();

        let sequential = passes_at(1, "nucleos-accept-seq-").await;
        let parallel = passes_at(2, "nucleos-accept-par-").await;

        assert!(
            parallel.passes <= sequential.passes,
            "the parallel walk took {parallel:?} against the sequential {sequential:?}"
        );
        assert!(
            parallel.writing < sequential.writing,
            "two independent items should be written in one round rather than two: {parallel:?} \
             against {sequential:?}"
        );
    }

    /// Drives a job to an ending, one pass per loop, exactly as the daemon's tick would.
    async fn walk(state: &AppState, job_id: i64) -> String {
        for _ in 0..20 {
            job_tick(state, Utc::now()).await;
            settle(state, job_id).await;
            let status = job_status(&state.pool, job_id).await;
            if !LIVE_STATUSES.contains(&status.as_str()) {
                return status;
            }
        }
        panic!(
            "the job never reached an ending; last {}",
            why_it_stands(&state.pool, job_id).await
        );
    }

    async fn start_job_for(
        state: &AppState,
        runner: &crate::execution::runner::FakeCommandRunner,
        repo: &std::path::Path,
        plan: &str,
    ) -> i64 {
        *runner.plan_to_write.lock().unwrap() = Some(plan.to_owned());
        let root = repo.to_string_lossy().into_owned();
        // Through the same function `POST /jobs` and the scheduler both use, and NOT `None`.
        // `head_sha` is the footing the first item reverts to, so a helper that left it empty would
        // send every walk below down the "no footing, stop instead" branch — testing the fallback
        // and never the behaviour.
        let head_sha = crate::repo_trigger::current_branch_sha(repo, "HEAD", false).await;
        let job_id = insert_job(
            &state.pool,
            &NewJob {
                project_id: "project-a",
                project_root: &root,
                rule_name: Some("nightly"),
                prompt: "advance the backlog",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: head_sha.as_deref(),
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        .expect("start a job");
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            &state.pool,
            owner,
            "project-a",
            &root,
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        job_id
    }

    #[tokio::test(flavor = "current_thread")]
    async fn f5_a_jobs_worktree_gets_the_projects_pinned_workflow() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = walkable_repo("nucleos-job-workflow-", "git --version");
        let root = tempfile::tempdir().unwrap();
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let mut state = with_home(test_state(pool.clone()).await, &container);
        let library = container.path().join("workflows");
        let bundle_dir = library.join("dev").join("1.0");
        std::fs::create_dir_all(bundle_dir.join(".ai/workflow")).unwrap();
        std::fs::write(bundle_dir.join("bundle.yaml"), "owns:\n  - .ai/workflow/\n").unwrap();
        std::fs::write(bundle_dir.join(".ai/workflow/rule.md"), "pinned").unwrap();
        let bundle = crate::workflows::read_bundle(&bundle_dir, "dev", "1.0")
            .unwrap()
            .unwrap();
        let pins = crate::project::state::file(
            state.machine_config_root.as_deref(),
            "nucleos",
            crate::project::state::PINS_FILE,
        )
        .unwrap();
        crate::workflows::install(&pins, &bundle).unwrap();
        state.workflow_library = Some(library);
        let project_root = repo.to_string_lossy().into_owned();

        let job_id = match start(
            &state,
            &StartRequest {
                project_id: "nucleos",
                project_root: &project_root,
                rule_name: None,
                prompt: "do it",
                max_items: 1,
                gate_each: true,
                review: false,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        {
            JobStart::Started(id) => id,
            other => panic!("job did not start: {other:?}"),
        };
        let (worktree, _) = job_worktree(&pool, job_id).await.unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(worktree.join(".ai/workflow/rule.md")).unwrap(),
            "pinned"
        );
    }

    /// The thesis of the whole feature, walked end to end for the first time: **one trigger
    /// produces a sequence of runs over one worktree**, so autonomous work is no longer capped by a
    /// single context window.
    ///
    /// Nothing here reaches around the machinery. The queue is written by the node into the handoff
    /// directory it learned about through its environment, and read back off disk by the daemon —
    /// so this covers the env plumbing, `prepare_artifacts`, the file contract of §5.2, the gate,
    /// and every transition between them. Seeding `job_items` directly would have exercised the
    /// parts and left the joins between them as the only place a bug could still live.
    #[tokio::test(flavor = "current_thread")]
    async fn one_trigger_becomes_a_sequence_of_runs_over_one_worktree() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-walk-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        let job_id = start_job_for(
            &state,
            &runner,
            &repo,
            r#"{"items":[{"description":"guard the cursor"},{"description":"retry on lock"}]}"#,
        )
        .await;

        assert_eq!(walk(&state, job_id).await, "completed");

        // Five runs from one trigger: spec, plan, two items, review. This number IS the feature —
        // one run per trigger being the ceiling is the whole thing the design exists to remove.
        //
        // `spec` leads, and it was four runs until it did. The brief is written before the queue,
        // because a queue built from a misread request is one that every node after it obeys.
        let stages: Vec<String> =
            sqlx::query_scalar("SELECT stage FROM runs WHERE job_id = ? ORDER BY id")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            stages,
            vec!["spec", "plan", "implement", "implement", "review"]
        );

        // One worktree, shared by all four. A second row would give the directory two owners and
        // let the GC collect it out from under a job still working in it.
        let trees: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worktrees")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(trees, 1);
        let cwds: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT cwd FROM runs WHERE job_id = ?")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(cwds.len(), 1, "every node ran in the same tree");

        // The queue came off disk, in order, and every item was actually measured.
        let items: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT description, status, gate_status FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, "guard the cursor");
        assert_eq!(items[1].0, "retry on lock");
        assert!(items.iter().all(|item| item.1 == "passed"));
        assert!(items.iter().all(|item| item.2.as_deref() == Some("passed")));

        let _ = crate::execution::worktree::remove(
            &repo,
            &root.path().join(format!("job-{job_id}")),
            &[],
        )
        .await;
    }

    /// Every gate the walk ran left one `verify_runs` row naming the job and the item.
    #[tokio::test(flavor = "current_thread")]
    async fn each_item_gate_is_recorded_in_verify_runs() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) =
            walkable_repo("nucleos-job-verify-", "git rev-parse --verify no-such-ref");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        let job_id = start_job_for(
            &state,
            &runner,
            &repo,
            r#"{"items":[{"description":"first"},{"description":"second"}]}"#,
        )
        .await;
        walk(&state, job_id).await;

        let gated: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM job_items WHERE job_id = ? AND gate_status IS NOT NULL",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let rows: Vec<(String, Option<i64>, String, Option<String>)> = sqlx::query_as(
            "SELECT origin, ordinal, status, project_id FROM verify_runs              WHERE origin = 'job_item' AND origin_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            gated > 0,
            "the walk must have gated something for this test to mean anything"
        );
        assert_eq!(rows.len() as i64, gated);
        for (origin, ordinal, status, project_id) in &rows {
            assert_eq!(origin, crate::verify::runs::ORIGIN_JOB_ITEM);
            assert!(ordinal.is_some());
            assert_eq!(status, crate::verify::runs::STATUS_FAILED);
            assert!(project_id.is_some());
        }
    }

    /// Walked rather than asserted against a seeded row: a red gate no longer ends the night.
    ///
    /// This test used to pin the opposite, and the sentence it carried — *"Never started. The
    /// partial stays on the branch and the second item is still there to do."* — was the honest
    /// description of a real cost. The reason letting item i+1 run was unsafe is that it would build
    /// on unmeasured work; the checkpoint removes that reason by putting the tree back to the
    /// footing item i started from, so the objection no longer applies and the remaining items —
    /// which were never the ones that broke — are worth doing.
    ///
    /// The verdict is untouched: the job still ends `gate_failed`, decided at the end instead of at
    /// the first red item.
    #[tokio::test(flavor = "current_thread")]
    async fn a_red_gate_reverts_the_item_and_lets_the_queue_carry_on() {
        let _lock = crate::execution::worktree::test_env_lock();
        // A command that runs everywhere and always fails, so this measures the chain's answer to a
        // red gate rather than to a missing binary — which is the other outcome entirely, and one
        // the design spends a whole section keeping apart from this one.
        let (_container, repo) =
            walkable_repo("nucleos-job-red-", "git rev-parse --verify no-such-ref");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        let job_id = start_job_for(
            &state,
            &runner,
            &repo,
            r#"{"items":[{"description":"first"},{"description":"second"}]}"#,
        )
        .await;

        assert_eq!(
            walk(&state, job_id).await,
            "gate_failed",
            "one red gate still decides what the job is called"
        );

        let items: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, gate_status FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        // Both, because this gate command fails for every item. The second one is the point: it ran.
        assert_eq!(items[0].0, "gate_failed");
        assert_eq!(items[0].1.as_deref(), Some("failed"));
        assert_eq!(
            items[1].0, "gate_failed",
            "the item after a red one has to have been attempted, not left pending"
        );
        let stages: Vec<String> =
            sqlx::query_scalar("SELECT stage FROM runs WHERE job_id = ? ORDER BY id")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        // No review, and deliberately: both items went red and were reverted, so the round put
        // nothing on the branch, and a review would read a diff of nothing — which is what job
        // 27's round-0 review did on 2026-09-14. See `ReviewState::Skipped`.
        assert_eq!(
            stages,
            vec!["spec", "plan", "implement", "implement"],
            "the whole queue runs, and a round in which nothing passed is not reviewed"
        );

        let _ = crate::execution::worktree::remove(
            &repo,
            &root.path().join(format!("job-{job_id}")),
            &[],
        )
        .await;
    }

    /// The plan node is the one node with no item to mark, so nothing else stops a second planner
    /// being started thirty seconds after the first.
    #[tokio::test]
    async fn a_planner_still_running_is_waited_for_rather_than_started_again() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_node(&pool, job_id, "plan", "running").await;

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(view.planning);
        assert_eq!(next_step(&view), Next::Wait);
    }

    /// A node paused for approval has not finished. Reading it as finished would let the job walk
    /// past work that never happened, or complete while its review sits on a question nobody has
    /// been shown.
    #[tokio::test]
    async fn a_node_paused_for_approval_is_still_in_flight() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_node(&pool, job_id, "plan", "awaiting_approval").await;

        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Wait
        );

        let reviewing = seed_job(&pool, "project-b", "reviewing").await.unwrap();
        seed_items(&pool, reviewing, &["passed"]).await;
        seed_node(&pool, reviewing, "review", "awaiting_approval").await;

        assert_eq!(
            next_step(&load_view(&pool, reviewing).await.unwrap()),
            Next::Wait
        );
    }

    // ---- parking and coming back ---------------------------------------------------------------

    /// The bug the `resume_status` column exists for. `waiting` overwrites the status underneath
    /// it, and `planning` is the one status that carries meaning the job cannot rebuild: without
    /// this, a job parked for budget while still planning would come back as
    /// planned-with-an-empty-queue — the honest "there was no work" night — and report itself
    /// complete having planned nothing.
    #[tokio::test]
    async fn a_job_parked_while_planning_still_plans_when_it_comes_back() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();

        wait(&pool, job_id, "budget").await.unwrap();
        // Even parked, before anything unparks it, the queue does not exist yet.
        assert!(!load_view(&pool, job_id).await.unwrap().planned);

        resume(&pool, job_id).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "planning");
        // Still the property this test is named for: it came back knowing it has NOT planned, which
        // is the whole of what `resume_status` protects. This job was parked before it had done
        // anything at all, so what it comes back to is the brief rather than the queue.
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnSpec
        );
    }

    /// Parking is re-applied every pass a brake is still on, so it has to survive being called on
    /// an already-parked job. Recording `waiting` as the stage to return to would leave the job
    /// with no way home.
    #[tokio::test]
    async fn parking_a_parked_job_does_not_lose_where_it_was() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        wait(&pool, job_id, "budget").await.unwrap();
        wait(&pool, job_id, "attention").await.unwrap();
        resume(&pool, job_id).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "implementing");
    }

    // ---- folding a finished node back into the job ---------------------------------------------

    #[tokio::test]
    async fn a_node_still_in_flight_is_not_reconciled() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "awaiting_approval").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ?")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(item_statuses(&pool, job_id).await, vec!["running"]);
    }

    /// The handoff window. A node's body writes `completed` BEFORE it inserts its context-handoff
    /// successor and moves the item onto it, so a pass in that gap saw a finished node and called
    /// the item implemented while the successor went on editing the tree. A registration still
    /// held is what says the body has not finished deciding.
    #[tokio::test]
    async fn a_node_whose_task_is_still_registered_is_not_reconciled() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "completed").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ?")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let task = tokio::spawn(tokio::time::sleep(std::time::Duration::from_secs(60)));
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(run_id, task.abort_handle());

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["running"],
            "an item was finished while its node's task could still hand it off"
        );

        // And once the task is gone, the same row is the ending it says it is.
        state.run_handles.lock().unwrap().remove(&run_id);
        task.abort();
        reconcile_nodes(&state, &job).await.unwrap();
        assert_eq!(item_statuses(&pool, job_id).await, vec!["implemented"]);
    }

    /// A pass reads a live job, then spends minutes in a gate or a launch before it writes. A
    /// cancel landing in between used to be overwritten by that write — `implementing` brought the
    /// job back to life and the next item started. Every status write now finds the job live or
    /// does nothing, and says which.
    #[tokio::test]
    async fn a_cancelled_job_is_not_revived_by_a_pass_that_read_it_live() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", STATUS_CANCELLED)
            .await
            .unwrap();

        assert!(
            !set_live_status(&pool, job_id, "implementing")
                .await
                .unwrap()
        );
        assert!(!set_live_status(&pool, job_id, "gating").await.unwrap());
        assert!(!retire(&pool, job_id, "gate_failed").await.unwrap());
        pause(&pool, job_id, "waiting", "budget").await.unwrap();

        let (status, resume_status): (String, Option<String>) =
            sqlx::query_as("SELECT status, resume_status FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            status, STATUS_CANCELLED,
            "the first ending is the one that stands"
        );
        assert_eq!(resume_status, None);

        // The live path is unchanged: a job still running moves, and the first ending wins it.
        let live = seed_job(&pool, "project-a", "implementing").await.unwrap();
        assert!(set_live_status(&pool, live, "gating").await.unwrap());
        assert!(retire(&pool, live, STATUS_CANCELLED).await.unwrap());
        assert!(!retire(&pool, live, "completed").await.unwrap());
    }

    /// The guard every status write carries is spelled out as SQL, so it is held against the
    /// constant the way `LIVE_WHERE_SQL` is: a live status missing from it would make that stage
    /// unwritable, and an extra one would let a write revive a job that ended.
    #[test]
    fn the_status_guard_names_every_live_status() {
        let guard = live_statuses_sql!();
        for status in LIVE_STATUSES {
            assert!(
                guard.contains(&format!("'{status}'")),
                "the status guard does not know `{status}`"
            );
        }
        let named = guard
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            named,
            LIVE_STATUSES.len(),
            "the guard names a status nothing drives"
        );
    }

    /// Only `completed` is done. A node that timed out, was cancelled or was interrupted leaves the
    /// tree holding edits no gate has measured, and calling any of those finished would let the
    /// next item build on top of them.
    #[tokio::test]
    async fn a_node_that_did_not_complete_stops_the_chain_at_its_item() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running", "pending"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "timed_out").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = 0")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["failed", "pending"]
        );
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Failed)
        );
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_item_failed".to_owned())
        );
    }

    #[tokio::test]
    async fn a_finished_node_hands_its_item_to_the_gate() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "completed").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ?")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(item_statuses(&pool, job_id).await, vec!["implemented"]);
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::RunGate { ordinal: 0 }
        );
    }

    // ---- the queue the planner produced --------------------------------------------------------

    /// The pair this whole module turns on. A planner that produced nothing failed; a planner that
    /// found nothing had a quiet, successful night. Collapsing them makes every crashed plan node
    /// look like a job well done, and teaches the reader to ignore the feed.
    #[tokio::test]
    async fn a_plan_node_that_wrote_no_file_fails_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "failed");
        assert!(item_statuses(&pool, job_id).await.is_empty());
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_plan_failed".to_owned())
        );
    }

    #[tokio::test]
    async fn a_planner_that_found_no_work_completes_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;
        write_plan(worktree.path(), r#"{"items": []}"#).await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "implementing");
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Completed)
        );
    }

    // ---- the replan node (Chunk 3, task 11) ------------------------------------------------------

    /// Seeds a job at the end of a round, with a landed replan node and the answer it wrote.
    async fn seed_closed_round(
        pool: &sqlx::SqlitePool,
        worktree: &std::path::Path,
        run_status: &str,
        answer: Option<&str>,
    ) -> i64 {
        let job_id = seed_job(pool, "project-a", "reviewing").await.unwrap();
        seed_worktree(pool, job_id, worktree).await;
        seed_items(pool, job_id, &["passed", "passed"]).await;
        sqlx::query("UPDATE jobs SET max_rounds = 5 WHERE id = ?")
            .bind(job_id)
            .execute(pool)
            .await
            .unwrap();
        seed_node(pool, job_id, "replan", run_status).await;
        if let Some(answer) = answer {
            write_plan(worktree, answer).await;
        }
        job_id
    }

    async fn round_counters(pool: &sqlx::SqlitePool, job_id: i64) -> (i64, i64, i64) {
        sqlx::query_as("SELECT round, dry_rounds, replan_done FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Starting a replan node must not make this round's finished review vanish.
    ///
    /// The review is scoped by "which replan opened this round", and reading that from the LATEST
    /// replan run is wrong in exactly one window: while a replan is deciding whether there is a next
    /// round, it has opened nothing. Its id moved the line anyway, the round's own review dropped
    /// out of view, and the job spawned a second one over a queue that had not changed.
    ///
    /// Measured on job 17, 2026-08-08 — run 900179, one wasted node per round. `jobs.replan_run_id`
    /// is the honest line because it is written where a replan's answer is acted on, not where the
    /// node is started.
    #[tokio::test]
    async fn a_replan_in_flight_does_not_make_this_rounds_review_look_unrun() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "reviewing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "passed"]).await;
        sqlx::query("UPDATE jobs SET max_rounds = 5 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_node(&pool, job_id, "review", "completed").await;

        // The round's review has landed, so the round closes into a replan.
        let before = load_view(&pool, job_id).await.unwrap();
        assert_eq!(before.review, ReviewState::Done);
        assert_eq!(next_step(&before), Next::SpawnReplan);

        // ...and now that node exists and is thinking. Nothing about the round changed.
        seed_node(&pool, job_id, "replan", "running").await;

        let during = load_view(&pool, job_id).await.unwrap();
        assert_eq!(
            during.review,
            ReviewState::Done,
            "the review this round already had was still had"
        );
        assert_eq!(
            next_step(&during),
            Next::Wait,
            "waiting for the replan, not paying for a second review of the same queue"
        );
    }

    /// A job whose round has run out of items, allowed to go round again after a red gate: the
    /// shape job 27 was in on 2026-09-14 when its round-0 review was spent on nothing.
    async fn a_round_run_out(
        pool: &sqlx::SqlitePool,
        worktree: &std::path::Path,
        items: &[&str],
        review: bool,
    ) -> i64 {
        let job_id = seed_job(pool, "project-a", "gating").await.unwrap();
        sqlx::query(
            "UPDATE jobs SET project_root = ?, rule_name = NULL, budget_usd = 25.0, max_rounds = 5,
                             review = ?
             WHERE id = ?",
        )
        .bind(worktree.to_string_lossy().into_owned())
        .bind(review)
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
        seed_worktree(pool, job_id, worktree).await;
        seed_items(pool, job_id, items).await;
        spend_the_red_items_gates(pool, job_id).await;
        job_id
    }

    /// The red items as `record_gate` leaves them: the attempt counted, so against a budget of no
    /// retries each one reads `GateFailed` and not `GateRetriable` — an item over, not one to redo.
    async fn spend_the_red_items_gates(pool: &sqlx::SqlitePool, job_id: i64) {
        sqlx::query(
            "UPDATE job_items SET gate_attempts = 1 WHERE job_id = ? AND status = 'gate_failed'",
        )
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
    }

    fn skips_said(told: &[String]) -> usize {
        told.iter()
            .filter(|line| line.contains("nothing on the branch for a review to judge"))
            .count()
    }

    /// A round in which nothing passed goes on to the replan without paying to review nothing.
    ///
    /// Job 27, 2026-09-14: round 0's only item ended `gate_failed`, its work was reverted, and the
    /// review (run 900513, $0.12) found `HEAD` at the base and reported "no work was done". The
    /// round now closes the way it would have after that review, and the feed says why it had none.
    #[tokio::test]
    async fn a_round_in_which_nothing_passed_goes_to_the_replan_without_a_review() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = a_round_run_out(&pool, worktree.path(), &["gate_failed"], true).await;

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(view.review, ReviewState::Skipped);
        assert_eq!(next_step(&view), Next::SpawnReplan);

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(
            stages_run(&pool, job_id).await,
            vec!["replan"],
            "the round was reviewed although nothing in it passed"
        );
        let told = feed_texts(&pool).await;
        assert_eq!(
            skips_said(&told),
            1,
            "the skip has to be said, once: {told:?}"
        );
        // Derived from the rows, so a second read — a restart — finds the same answer.
        assert_eq!(
            load_view(&pool, job_id).await.unwrap().review,
            ReviewState::Skipped
        );
    }

    /// The same for a job of one round: it ends where it would have ended after the review,
    /// `gate_failed`, without the review.
    #[tokio::test]
    async fn a_one_round_job_in_which_nothing_passed_ends_without_a_review() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "gating").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["gate_failed"]).await;
        spend_the_red_items_gates(&pool, job_id).await;

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(view.review, ReviewState::Skipped);
        assert_eq!(next_step(&view), Next::Finish(Outcome::GateFailed));

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert!(stages_run(&pool, job_id).await.is_empty());
        assert_eq!(job_status(&pool, job_id).await, "gate_failed");
        assert_eq!(
            feed_kinds(&pool).await,
            vec!["job_review_skipped", "job_finished"],
            "the reason there was no review belongs beside the ending it led to"
        );
    }

    /// One item that passed is a round with something to judge, and it is judged — whatever
    /// happened to the rest of it.
    #[tokio::test]
    async fn a_round_with_one_passed_item_still_gets_its_review() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id =
            a_round_run_out(&pool, worktree.path(), &["gate_failed", "passed"], true).await;

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(view.review, ReviewState::Pending);
        assert_eq!(next_step(&view), Next::SpawnReview);

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(stages_run(&pool, job_id).await, vec!["review"]);
        assert_eq!(skips_said(&feed_texts(&pool).await), 0);
    }

    /// A job that never wanted a review walks a round in which nothing passed exactly as it did: no
    /// review, and no line saying one was skipped, because none was owed.
    #[tokio::test]
    async fn a_job_with_review_off_walks_a_round_with_nothing_passed_as_it_did() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = a_round_run_out(&pool, worktree.path(), &["gate_failed"], false).await;

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(view.review, ReviewState::NotWanted);
        assert_eq!(next_step(&view), Next::SpawnReplan);

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(stages_run(&pool, job_id).await, vec!["replan"]);
        assert_eq!(skips_said(&feed_texts(&pool).await), 0);
    }

    /// The skip is said only once it is what decided the step. While items are still to run the
    /// view reads `Skipped` — nothing has passed yet — and has decided nothing.
    #[test]
    fn a_skipped_review_decides_only_once_the_round_has_nothing_left_to_run() {
        use ItemState::*;
        assert_eq!(
            next_step(&view(true, &[GateFailed], ReviewState::Skipped)),
            Next::Finish(Outcome::GateFailed)
        );
        assert!(the_skip_decided(&view(
            true,
            &[GateFailed],
            ReviewState::Skipped
        )));
        assert!(
            !the_skip_decided(&view(true, &[GateFailed, Pending], ReviewState::Skipped)),
            "an item still to run: the skip has decided nothing yet"
        );
        assert!(
            !the_skip_decided(&view(true, &[Cancelled], ReviewState::Skipped)),
            "a cancelled job ends at the short-circuit, review or no review"
        );
        assert!(!the_skip_decided(&view(
            true,
            &[GateFailed],
            ReviewState::NotWanted
        )));
    }

    /// A review node that has landed, with the stream it printed.
    async fn seed_review(pool: &sqlx::SqlitePool, job_id: i64, status: &str, stdout: &str) {
        let run_id = seed_node(pool, job_id, "review", status).await;
        sqlx::query("UPDATE runs SET stdout = ? WHERE id = ?")
            .bind(stdout)
            .bind(run_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// A review that never reached the model has judged nothing, and is run once more.
    ///
    /// Measured on job 26, 2026-09-13: review run 900483 ended on "Can't reach the API server —
    /// check your internet or DNS (ENOTFOUND)", read as `Done`, and twenty seconds later the replan
    /// opened the next round with no verdict on this one.
    #[tokio::test]
    async fn a_review_that_failed_on_a_transient_api_error_is_run_again() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = job_ready_to_review(&pool, worktree.path()).await;
        seed_review(
            &pool,
            job_id,
            "failed",
            crate::execution::runner::REVIEW_THAT_NEVER_REACHED_THE_API,
        )
        .await;

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(view.review, ReviewState::Retry);
        assert_eq!(next_step(&view), Next::SpawnReview);

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(stages_run(&pool, job_id).await, vec!["review", "review"]);
        let told = feed_texts(&pool).await;
        assert!(
            told.iter().any(|line| line.contains("run once more")),
            "a second review has to be said out loud, with why: {told:?}"
        );
    }

    /// Once per round, counted from the rows: a network that stays down costs one extra node, and
    /// then the round closes exactly as it did before.
    #[tokio::test]
    async fn a_second_review_that_fails_the_same_way_closes_the_round() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "reviewing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "passed"]).await;
        for _ in 0..2 {
            seed_review(
                &pool,
                job_id,
                "failed",
                crate::execution::runner::REVIEW_THAT_NEVER_REACHED_THE_API,
            )
            .await;
        }

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(
            view.review,
            ReviewState::Done,
            "the round already had its one retry"
        );
        assert_eq!(next_step(&view), Next::Finish(Outcome::Completed));
    }

    /// Every other way a review can end is a review that happened, as it always was: a failure the
    /// network had nothing to do with, and a transient-looking stream on a review that did not end
    /// `failed` — a `timed_out`, or a `cancelled` one a person or a hook stopped.
    #[tokio::test]
    async fn a_review_that_failed_for_any_other_reason_is_still_done() {
        let refused = r#"{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","api_error_status":400,"result":"API Error: 400"}"#;
        let max_turns = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"terminal_reason":"max_turns","result":""}"#;
        let transient = crate::execution::runner::REVIEW_THAT_NEVER_REACHED_THE_API;
        for (status, stdout) in [
            ("failed", refused),
            ("failed", max_turns),
            ("failed", ""),
            ("timed_out", transient),
            ("cancelled", transient),
        ] {
            let pool = test_pool().await;
            let job_id = seed_job(&pool, "project-a", "reviewing").await.unwrap();
            seed_items(&pool, job_id, &["passed"]).await;
            seed_review(&pool, job_id, status, stdout).await;

            let view = load_view(&pool, job_id).await.unwrap();
            assert_eq!(
                view.review,
                ReviewState::Done,
                "a review that ended {status}: {stdout}"
            );
            assert_eq!(next_step(&view), Next::Finish(Outcome::Completed));
        }
    }

    /// Ending #1. `completed` and not `stopped`, because the node that just looked at the work is
    /// the one saying the work is over — the best evidence this system can get for that claim.
    #[tokio::test]
    async fn a_replan_that_says_done_ends_the_job_completed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_closed_round(
            &pool,
            worktree.path(),
            "completed",
            Some(r#"{"done": true, "why": "the task is met"}"#),
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(round_counters(&pool, job_id).await.2, 1, "replan_done");
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Completed)
        );
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_replanned".to_owned())
        );
    }

    /// Ordinals CONTINUE across rounds. They are half `job_items`'s primary key, and the position in
    /// the loaded queue is the number `advance` binds into its lookups — restarting them per round
    /// would collide on the first insert and mislead on every one after.
    #[tokio::test]
    async fn a_replan_with_items_opens_the_next_round_past_the_old_ordinals() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_closed_round(
            &pool,
            worktree.path(),
            "completed",
            Some(r#"{"items": [{"description": "one more thing"}]}"#),
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        let (round, dry, done) = round_counters(&pool, job_id).await;
        assert_eq!(
            (round, dry, done),
            (1, 0, 0),
            "a round that added work is not dry"
        );
        assert_eq!(job_status(&pool, job_id).await, "implementing");

        let placed: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT ordinal, round FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(placed, vec![(0, 0), (1, 0), (2, 1)]);
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnImplement(vec![2])
        );
    }

    /// The whole reason `jobs.replan_run_id` exists, stated as the loop it prevents.
    ///
    /// A replan that produced nothing leaves every observable condition exactly as it found it: the
    /// queue is still fully terminal, `replan_done` is still 0, and the latest replan run is still
    /// the same one. Any DERIVED test for "already taken" therefore answers no on the next tick, and
    /// the round would keep advancing — one per tick, thirty seconds apart — until the ceiling ended
    /// a job that had done nothing at all.
    #[tokio::test]
    async fn a_dry_replan_is_counted_once_however_many_ticks_pass() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_closed_round(
            &pool,
            worktree.path(),
            "completed",
            Some(r#"{"items": []}"#),
        )
        .await;

        for _ in 0..3 {
            let job = load_job(&pool, job_id).await.unwrap();
            reconcile_nodes(&state, &job).await.unwrap();
        }

        let (round, dry, _) = round_counters(&pool, job_id).await;
        assert_eq!(
            (round, dry),
            (1, 1),
            "three ticks, one answer: a dry round is still one round"
        );
        // One short of the brake, so the job asks again rather than ending.
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnReplan
        );
    }

    /// The replan names the unfinished items it takes over, one-based as it was shown them.
    #[test]
    fn a_replan_item_names_the_unfinished_items_it_takes_over() {
        let planned = parse_plan(
            Some(
                br#"{"items": [{"description": "again", "replaces": [3, 1]}, {"description": "new"}]}"#,
            ),
            5,
            None,
        )
        .unwrap();
        assert_eq!(planned.items.len(), 2);
        // One-based as the prompt lists them, zero-based as the rows store them.
        assert_eq!(planned.replaces, vec![0, 2]);

        let nothing = parse_plan(
            Some(br#"{"items": [{"description": "x", "replaces": [0]}, {"description": "y"}]}"#),
            5,
            None,
        )
        .unwrap();
        assert!(nothing.replaces.is_empty(), "a zero names no item");
    }

    /// Read back as what it is. The fallback in `item_state_from` is `Pending`, so a status it did
    /// not know would come back as work to START - the one reading of a finished item that costs a
    /// second run of it.
    #[test]
    fn a_superseded_item_is_read_back_as_superseded_and_not_as_work_to_do() {
        assert_eq!(
            item_state_from(STATUS_SUPERSEDED, 0, 0),
            ItemState::Superseded
        );
        assert_eq!(ItemState::Superseded.claimable_as(), None);
    }

    /// Job 26, 2026-09-13: its round-0 item failed, round 1 redid it and passed the gate, and the job
    /// was reported `failed`. With the failure taken over, the ending is read from what is left.
    #[test]
    fn a_failure_a_later_round_took_over_no_longer_decides_the_ending() {
        let closing = |items: &[ItemState], replanned: Replan, max_rounds: i64| {
            view_in_round(
                items,
                ReviewState::Done,
                RoundState {
                    round: 1,
                    max_rounds,
                    replanned,
                    a_broken_item_may_go_round_again: true,
                    ..RoundState::default()
                },
            )
        };
        let taken_over = [ItemState::Superseded, ItemState::Passed];
        let not_taken_over = [ItemState::Failed, ItemState::Passed];

        // The replan confirmed the task is met: completed, which the failure used to forbid.
        assert_eq!(
            next_step(&closing(&taken_over, Replan::Done, 3)),
            Next::Finish(Outcome::Completed)
        );
        // Out of rounds with nobody confirming: stopped, the honest reading of green work that was
        // never declared finished - and no longer `failed`, which is what job 26 was told.
        assert_eq!(
            next_step(&closing(&taken_over, Replan::NotYet, 2)),
            Next::Finish(Outcome::Stopped)
        );
        // A failure nothing took over still decides, exactly as before.
        assert_eq!(
            next_step(&closing(&not_taken_over, Replan::NotYet, 2)),
            Next::Finish(Outcome::Failed)
        );
    }

    /// The next round marks what it takes over, and only what may be taken over: an unfinished item
    /// of an earlier round. A number naming finished work is ignored rather than trusted.
    #[tokio::test]
    async fn opening_a_round_supersedes_only_the_unfinished_items_it_names() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "reviewing").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["failed", "passed", "gate_failed"]).await;
        sqlx::query("UPDATE jobs SET max_rounds = 5 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_node(&pool, job_id, "replan", "completed").await;
        write_plan(
            worktree.path(),
            r#"{"items": [{"description": "item 1 again", "replaces": [1, 2]}, {"description": "new"}]}"#,
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            statuses,
            [
                STATUS_SUPERSEDED,
                "passed",
                "gate_failed",
                "pending",
                "pending"
            ],
            "item 1 taken over, item 2 was finished and stays, item 3 was not named and stays"
        );
    }

    /// Told how to take a failure over when there is one, and not burdened with it when there is not.
    #[test]
    fn a_replan_is_told_how_to_take_over_what_broke_and_only_when_something_did() {
        let broke = Unfinished {
            ordinal: 0,
            description: "the flaky test".to_owned(),
            gate_output: None,
        };
        let with = replan_prompt("t", 0, &[], "/wt/.nucleos", None, &[broke]);
        assert!(with.contains("\"replaces\": [N]"), "{with}");
        assert!(with.contains("\"replaces\": []"), "{with}");
        let without = replan_prompt("t", 0, &[], "/wt/.nucleos", None, &[]);
        assert!(!without.contains("replaces"), "{without}");
    }

    /// Job 26, 2026-09-13: a retry sent back for formatting alone ran the whole test suite through
    /// `| tail`, printed nothing for thirty minutes, and was killed as a hang with its fix unsaved.
    #[test]
    fn an_implement_node_is_told_the_gate_runs_the_suite_so_it_need_not() {
        let first = implement_prompt("x", 0, 1, "/wt/.nucleos", &[], None, None);
        assert!(
            first.contains("do not run the whole suite yourself"),
            "{first}"
        );
        assert!(first.contains("taken for a hang"), "{first}");
        let retry = implement_prompt(
            "x",
            0,
            1,
            "/wt/.nucleos",
            &[],
            Some("gates FAILED:\n  core: fmt"),
            None,
        );
        assert!(
            retry.contains("re-run that step, not the whole gate"),
            "{retry}"
        );
    }

    /// A replan that could not answer stops the job; it does not fail it.
    ///
    /// The rounds that ran are on the branch, gated green, and worth looking at. `failed` for want of
    /// a replan node teaches its reader to ignore the branch — which is the one thing the whole
    /// `Completed`/`Stopped` split exists to prevent.
    #[tokio::test]
    async fn a_replan_that_could_not_answer_stops_the_job_rather_than_failing_it() {
        for (run_status, answer) in [("failed", None), ("completed", None)] {
            let pool = test_pool().await;
            let state = test_state(pool.clone()).await;
            let worktree = tempfile::tempdir().unwrap();
            let job_id = seed_closed_round(&pool, worktree.path(), run_status, answer).await;

            let job = load_job(&pool, job_id).await.unwrap();
            reconcile_nodes(&state, &job).await.unwrap();

            assert_eq!(
                job_status(&pool, job_id).await,
                STATUS_STOPPED,
                "{run_status}"
            );
            assert!(feed_kinds(&pool).await.contains(&"job_stopped".to_owned()));
        }
    }

    /// Truncation is reported, never silent: a queue quietly cut from seven to five reads
    /// downstream as "the planner found five things", which is a different and wrong statement
    /// about the work.
    #[tokio::test]
    async fn a_plan_above_the_ceiling_is_cut_and_the_feed_says_by_how_much() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;
        let seven: Vec<String> = (0..7)
            .map(|n| format!(r#"{{"description":"item {n}"}}"#))
            .collect();
        write_plan(
            worktree.path(),
            &format!(r#"{{"items":[{}]}}"#, seven.join(",")),
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(item_statuses(&pool, job_id).await.len(), 5);
        let (summary, subject): (String, Option<String>) =
            sqlx::query_as("SELECT summary, subject FROM feed WHERE kind = 'job_planned'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(subject, Some(format!("job:{job_id}")));
        assert!(
            summary.contains('2'),
            "the feed must say what was left out: {summary}"
        );
    }

    /// The hint has to survive the gap between the plan node and the implement node, which is a
    /// database row and not a process — nothing of `PlannedItems` is still in memory by the time
    /// the item is spawned. An item with no hint stores NULL rather than `[]`: the column that says
    /// nothing is the one that reads downstream as "the planner did not say", and an empty array
    /// would read as "the planner said no files", which is a different claim.
    #[tokio::test]
    async fn planned_file_hints_are_stored_with_the_item() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;
        write_plan(
            worktree.path(),
            r#"{"items":[{"description":"hinted","files":["core/src/job.rs","core/src/runs.rs"]},
                        {"description":"unhinted"}]}"#,
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        let stored: Vec<Option<String>> =
            sqlx::query_scalar("SELECT files FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();

        let hinted: Vec<String> =
            serde_json::from_str(stored[0].as_deref().expect("a hinted item keeps its hint"))
                .expect("the hint is stored as a JSON array");
        assert_eq!(hinted, vec!["core/src/job.rs", "core/src/runs.rs"]);
        assert_eq!(stored[1], None, "no hint is NULL, not an empty array");
    }

    // ---- the brakes between nodes --------------------------------------------------------------

    async fn set_budget(pool: &sqlx::SqlitePool, window: Option<f64>, hourly: Option<f64>) {
        sqlx::query(
            "UPDATE autopilot_global
             SET budget_limit_usd = ?, budget_hourly_limit_usd = ?, budget_per_run_reserve_usd = 1.0",
        )
        .bind(window)
        .bind(hourly)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_job_run(pool: &sqlx::SqlitePool, job_id: i64, cost: f64) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, job_id, cost_usd, created_at, completed_at)
             VALUES ('a node', 'completed', 'worktree', ?, ?, ?, ?)",
        )
        .bind(job_id)
        .bind(cost)
        .bind(Utc::now().to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    /// A job's own allowance STOPS it and never parks it, which is the whole difference from the
    /// window brake above. A calendar window reopens; a task's allowance does not, so a job parked
    /// on it would sit at `waiting` until the four-hour ceiling swept it up, wearing a reason nobody
    /// could act on.
    #[tokio::test]
    async fn a_job_that_spent_its_own_allowance_stops_rather_than_waiting() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_job_run(&pool, job_id, 4.5).await;

        // The house limit is off, so anything that fires here is the job's own.
        sqlx::query("UPDATE jobs SET budget_usd = 5.0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let job = load_job(&pool, job_id).await.unwrap();
        assert!(
            matches!(brakes(&state, &job, Utc::now()).await, Brake::Stop { .. }),
            "spent 4.50 of 5.00 and the next node reserves 1.00"
        );

        // Room for another node: the test is about the NEXT one, never the one in flight.
        sqlx::query("UPDATE jobs SET budget_usd = 20.0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let job = load_job(&pool, job_id).await.unwrap();
        assert!(matches!(brakes(&state, &job, Utc::now()).await, Brake::Go));
    }

    /// A NULL allowance means "only the house limit governs", which is what every `graph:` rule has
    /// always meant. A job that spent plenty is untouched by a brake it never asked for.
    #[tokio::test]
    async fn a_job_with_no_allowance_of_its_own_is_governed_only_by_the_house() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_job_run(&pool, job_id, 500.0).await;

        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(job.budget_usd, None);
        assert!(matches!(brakes(&state, &job, Utc::now()).await, Brake::Go));
    }

    /// The house budget against the four shapes of job, and only one of them escapes it.
    ///
    /// The escape is the point of the change and the three that do NOT escape are the point of the
    /// test: an exemption whose condition is a conjunction fails silently in three directions if
    /// only the happy one is asserted.
    #[tokio::test]
    async fn only_a_commissioned_job_with_an_allowance_escapes_the_house_budget() {
        // A house limit already spent past, so anything the house governs stops here.
        async fn bench(rule: Option<&str>, allowance: Option<f64>) -> (AppState, JobRow) {
            let pool = test_pool().await;
            let state = test_state(pool.clone()).await;
            set_budget(&pool, Some(5.0), None).await;
            let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
            seed_job_run(&pool, job_id, 50.0).await;
            sqlx::query("UPDATE jobs SET rule_name = ?, budget_usd = ? WHERE id = ?")
                .bind(rule)
                .bind(allowance)
                .bind(job_id)
                .execute(&pool)
                .await
                .unwrap();
            let job = load_job(&pool, job_id).await.unwrap();
            (state, job)
        }

        // Nobody asked for it: the house governs however it was configured.
        let (state, job) = bench(Some("nightly"), Some(500.0)).await;
        assert!(
            !matches!(brakes(&state, &job, Utc::now()).await, Brake::Go),
            "a scheduled job walked past the house budget"
        );

        // Asked for, but named no number of its own — so the house limit is the ONLY bound there
        // is, and taking it away would leave nothing. This is the arm that keeps the exemption
        // safe rather than merely narrow.
        let (state, job) = bench(None, None).await;
        assert!(
            !matches!(brakes(&state, &job, Utc::now()).await, Brake::Go),
            "a job with no allowance of its own was left unbounded"
        );

        // Asked for AND given a number: the owner's spend decision, and it is the one that governs.
        // Its own allowance still has room, so this must go.
        let (state, job) = bench(None, Some(500.0)).await;
        assert!(
            matches!(brakes(&state, &job, Utc::now()).await, Brake::Go),
            "work the owner commissioned and funded was paced by the house anyway"
        );

        // And the exemption reaches the house budget and nothing else: the job's OWN allowance,
        // spent, still stops it.
        let (state, job) = bench(None, Some(10.0)).await;
        assert!(
            matches!(brakes(&state, &job, Utc::now()).await, Brake::Stop { .. }),
            "the exemption swallowed the job's own ceiling as well"
        );
    }

    async fn exclude(pool: &sqlx::SqlitePool, project_id: &str, low: i64, high: i64) {
        sqlx::query(
            "INSERT INTO fleet_exclusions
                 (project_id, job_low, job_high, proposal_id, created_at)
             VALUES (?, ?, ?, 1, '2026-08-15T00:00:00Z')",
        )
        .bind(project_id)
        .bind(low)
        .bind(high)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The whole journey, with nothing along it faked.
    ///
    /// Every other test here inserts the rule straight into `fleet_exclusions`, which is right for
    /// testing the brake and wrong for testing the FEATURE: those would pass unchanged if `propose`
    /// filed the wrong pair or `approve` wrote a row nothing reads. This one walks the path a person
    /// walks — ask, approve, watch one job hold and the other wait, and watch the wait end by itself
    /// — through the same functions the routes call.
    ///
    /// The two jobs are asked about BACKWARDS (`high` first) on purpose: the ordering that makes the
    /// tie-break work has to survive the request, not just the table.
    #[tokio::test]
    async fn asking_approving_and_waiting_is_one_unbroken_chain() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let low = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let high = seed_job(&pool, "project-a", "implementing").await.unwrap();

        // 1. Somebody draws the edge. Nothing about scheduling changes yet.
        let proposal = crate::governance::exclusion::propose(&pool, high, low, &[])
            .await
            .unwrap();
        crate::execution::concurrency::claim(
            &pool,
            "project-a",
            crate::execution::worktree::Owner::Job(low),
        )
        .await
        .unwrap();
        let high_row = load_job(&pool, high).await.unwrap();
        assert!(
            matches!(brakes(&state, &high_row, Utc::now()).await, Brake::Go),
            "a request that nobody has approved must not hold anything"
        );

        // 2. Somebody approves it. Only now is there a rule.
        let approved = crate::governance::exclusion::approve(&pool, proposal)
            .await
            .unwrap();
        assert!(
            matches!(approved, crate::governance::exclusion::Approved::Written(_)),
            "got {approved:?}"
        );

        // 3. The higher job waits, and the reason names what to wait for.
        let Brake::Park { reason, detail } = brakes(&state, &high_row, Utc::now()).await else {
            panic!("the higher job must wait while its partner holds a slot");
        };
        assert_eq!(reason, "excluded");
        assert!(detail.contains(&format!("job {low}")), "got: {detail}");
        // And it is written on the job itself, which is what the card reads.
        park(&state, &high_row, reason, &detail).await;
        let parked = load_job(&pool, high).await.unwrap();
        assert_eq!(parked.status, "waiting");
        assert_eq!(parked.wait_reason.as_deref(), Some("excluded"));

        // 4. The partner finishes. Nobody presses anything.
        crate::execution::concurrency::release(&pool, crate::execution::worktree::Owner::Job(low))
            .await
            .unwrap();
        resume(&pool, high).await.unwrap();
        let woken = load_job(&pool, high).await.unwrap();
        assert_eq!(
            woken.wait_reason, None,
            "the note outlives the pause it explains"
        );
        assert!(matches!(
            brakes(&state, &woken, Utc::now()).await,
            Brake::Go
        ));
    }

    /// One of the two waits on the other's SLOT, and it is always the same one.
    ///
    /// The asymmetry is the deadlock argument, not a detail of the query: the low id is never parked
    /// for a slot — only for a node already in flight, which ends by itself (the test below) — so
    /// of any two excluded jobs at least one is always free to run. Were the
    /// tie-break decided at read time, two reads that disagreed would park both — and two jobs
    /// somebody asked to SERIALISE, stopped forever on each other, is the one failure this feature
    /// must not be able to produce.
    #[tokio::test]
    async fn the_higher_job_waits_for_its_partner_and_the_lower_one_never_does() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let low = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let high = seed_job(&pool, "project-a", "implementing").await.unwrap();
        exclude(&pool, "project-a", low, high).await;
        crate::execution::concurrency::claim(
            &pool,
            "project-a",
            crate::execution::worktree::Owner::Job(low),
        )
        .await
        .unwrap();

        let high_row = load_job(&pool, high).await.unwrap();
        let Brake::Park { reason, detail } = brakes(&state, &high_row, Utc::now()).await else {
            panic!("the higher job must wait while its partner holds a slot");
        };
        assert_eq!(reason, "excluded");
        assert!(
            detail.contains(&format!("job {low}")),
            "the reason has to name what to wait for, got: {detail}"
        );

        // The other side of the same rule, at the same moment: never parked by a partner that is
        // merely live. Both hold slots for their whole lives, so a slot-wait would park both.
        let low_row = load_job(&pool, low).await.unwrap();
        assert!(matches!(
            brakes(&state, &low_row, Utc::now()).await,
            Brake::Go
        ));
    }

    /// The other start order. The higher job got going first and has a node running when the lower
    /// one comes to start its own.
    ///
    /// Waiting on the slot alone held the exclusion in one order only: the low side never looked,
    /// so it started a node beside the high side's running one — the very overlap the rule forbids.
    /// The low side waits for a node IN FLIGHT and never for a slot, which keeps the deadlock
    /// argument: a running node ends by itself, and the high side cannot start another while the
    /// low one lives.
    #[tokio::test]
    async fn the_lower_job_waits_for_a_node_its_partner_already_has_running() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let low = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let high = seed_job(&pool, "project-a", "implementing").await.unwrap();
        exclude(&pool, "project-a", low, high).await;
        for job in [low, high] {
            crate::execution::concurrency::claim(
                &pool,
                "project-a",
                crate::execution::worktree::Owner::Job(job),
            )
            .await
            .unwrap();
        }
        let node = seed_node(&pool, high, "implement", "running").await;

        let low_row = load_job(&pool, low).await.unwrap();
        let Brake::Park { reason, detail } = brakes(&state, &low_row, Utc::now()).await else {
            panic!("the lower job started a node beside its excluded partner's running one");
        };
        assert_eq!(reason, "excluded");
        assert!(detail.contains(&format!("job {high}")), "got: {detail}");

        // A node paused on a person is still a node in the tree.
        sqlx::query("UPDATE runs SET status = 'awaiting_approval' WHERE id = ?")
            .bind(node)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            brakes(&state, &low_row, Utc::now()).await,
            Brake::Park { .. }
        ));

        // The node lands: the lower job goes, and the higher one is the one that now waits.
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(node)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            brakes(&state, &low_row, Utc::now()).await,
            Brake::Go
        ));
        let high_row = load_job(&pool, high).await.unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Park { .. }
        ));
    }

    /// The brake lifts by itself, which is why it is a `Park` and not a `Stop`.
    ///
    /// Two ways for it to lift, and both are tested here because they fail differently: the partner
    /// gives its slot back, or somebody revokes the rule. A brake that needed a person to restart
    /// the job would make serialising two fronts of work cost more attention than doing them by
    /// hand.
    #[tokio::test]
    async fn the_wait_ends_when_the_slot_goes_back_or_the_rule_is_revoked() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let low = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let high = seed_job(&pool, "project-a", "implementing").await.unwrap();
        exclude(&pool, "project-a", low, high).await;
        crate::execution::concurrency::claim(
            &pool,
            "project-a",
            crate::execution::worktree::Owner::Job(low),
        )
        .await
        .unwrap();
        let high_row = load_job(&pool, high).await.unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Park { .. }
        ));

        crate::execution::concurrency::release(&pool, crate::execution::worktree::Owner::Job(low))
            .await
            .unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Go
        ));

        // And with the slot taken again, revoking the rule releases it just the same.
        crate::execution::concurrency::claim(
            &pool,
            "project-a",
            crate::execution::worktree::Owner::Job(low),
        )
        .await
        .unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Park { .. }
        ));
        sqlx::query("UPDATE fleet_exclusions SET revoked_at = '2026-08-15T01:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Go
        ));
    }

    /// Decision 9, and the whole reason `PauseKind` exists. The hourly brake lifts by itself, so
    /// the job comes back; the window ceiling does not lift before the period rolls over, so
    /// waiting for it would be a hang dressed up as patience.
    #[tokio::test]
    async fn an_hourly_brake_parks_a_job_and_a_spent_window_ends_it() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["pending"]).await;
        set_budget(&pool, None, Some(0.0)).await;

        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(advance(&state, &job, Utc::now()).await, Step::Stopped);
        assert_eq!(job_status(&pool, job_id).await, "waiting");
        let reason: Option<String> =
            sqlx::query_scalar("SELECT wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("budget"));

        set_budget(&pool, Some(0.0), None).await;
        resume(&pool, job_id).await.unwrap();
        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, STATUS_STOPPED);
        assert!(feed_kinds(&pool).await.contains(&"job_stopped".to_owned()));
    }

    #[tokio::test]
    async fn quota_brake_parks_with_wait_reason_quota_when_the_reset_comes_before_end_of_life() {
        let pool = test_pool().await;
        let reset = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let state = quota_state(pool.clone(), Some(&reset)).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["pending"]).await;

        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(advance(&state, &job, Utc::now()).await, Step::Stopped);
        assert_eq!(job_status(&pool, job_id).await, "waiting");
        let reason: Option<String> =
            sqlx::query_scalar("SELECT wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("quota"));
    }

    #[tokio::test]
    async fn quota_brake_stops_and_hands_back_the_partial_when_the_reset_is_after_end_of_life() {
        for reset in [
            Some((Utc::now() + chrono::Duration::hours(5)).to_rfc3339()),
            None,
        ] {
            let pool = test_pool().await;
            let state = quota_state(pool.clone(), reset.as_deref()).await;
            let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
            seed_items(&pool, job_id, &["pending"]).await;

            let job = load_job(&pool, job_id).await.unwrap();
            advance(&state, &job, Utc::now()).await;
            assert_eq!(job_status(&pool, job_id).await, STATUS_STOPPED);
            assert!(feed_kinds(&pool).await.contains(&"job_stopped".to_owned()));
        }
    }

    #[tokio::test]
    async fn quota_brake_holds_a_job_with_its_own_allowance() {
        let pool = test_pool().await;
        let reset = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let state = quota_state(pool.clone(), Some(&reset)).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET budget_usd = 10.0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["pending"]).await;

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;
        let reason: Option<String> =
            sqlx::query_scalar("SELECT wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("quota"));
    }

    /// The number in that line is what the night left behind, and a retriable item is work.
    ///
    /// An item whose gate went red with a retry still to spend is one the queue owed another
    /// implement run. Counting only `Pending` reports one item fewer than was really left, and the
    /// item it drops is precisely the one that had already cost money.
    #[tokio::test]
    async fn a_stopped_job_counts_a_retriable_item_among_the_unfinished() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        // The row a red gate with a retry left behind, written the way `record_gate` writes it: the
        // status still says `gate_failed`, and it is the attempt count read beside the job's budget
        // that makes the item retriable rather than finished with.
        seed_items(&pool, job_id, &["gate_failed", "pending"]).await;
        sqlx::query(
            "UPDATE job_items SET gate_status = 'failed', gate_attempts = 1
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        // The window ceiling, because it is the brake that stops rather than parks.
        set_budget(&pool, Some(0.0), None).await;

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, STATUS_STOPPED);
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'job_stopped'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            summary.contains("2 item(s) unfinished"),
            "the retriable item is work the queue still owed a run: {summary}"
        );
    }

    /// Decision 10. The brake used to be a check made once at admission, so a job admitted at 03:00
    /// would keep starting nodes at 08:00 with the owner at the keyboard.
    #[tokio::test]
    async fn the_attention_brake_refuses_the_next_node_but_never_the_gate() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        // `project_root` is the tempdir so the gate configuration below is genuinely absent rather
        // than unreadable — this test is about the brake, not about gate configuration.
        sqlx::query("UPDATE jobs SET project_root = ? WHERE id = ?")
            .bind(worktree.path().to_string_lossy().into_owned())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["implemented", "pending"]).await;
        sqlx::query(
            "INSERT INTO attention_heartbeats (scope, project_id, last_seen_at) VALUES ('project', 'project-a', ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        // The gate is a subprocess, not a run: it starts no CLI and answers to no brake. Refusing
        // to measure would leave the tree holding edits nothing verified, which is the state
        // decision 10 exists to avoid.
        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(advance(&state, &job, Utc::now()).await, Step::Continued);
        assert_eq!(item_statuses(&pool, job_id).await[0], "passed");

        // The next item, though, does not start.
        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;
        assert_eq!(job_status(&pool, job_id).await, "waiting");
        assert_eq!(item_statuses(&pool, job_id).await[1], "pending");
    }

    /// The same setup as the test above, with the brake switched off in the project's rules.
    ///
    /// The heartbeat is still fresh — the owner IS at the keyboard — so this asserts the switch and
    /// not the absence of a signal. What changes is only whether the job asks.
    #[tokio::test]
    async fn a_project_that_switched_the_brake_off_is_not_parked_by_a_present_owner() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        crate::project::state::write_for_test(
            worktree.path(),
            "project-a",
            crate::project::state::AUTOPILOT_FILE,
            "attention_brake: false\n",
        );
        let state = AppState {
            machine_config_root: Some(worktree.path().to_path_buf()),
            ..state
        };
        sqlx::query("UPDATE jobs SET project_root = ? WHERE id = ?")
            .bind(worktree.path().to_string_lossy().into_owned())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["implemented", "pending"]).await;
        sqlx::query(
            "INSERT INTO attention_heartbeats (scope, project_id, last_seen_at) VALUES ('project', 'project-a', ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(advance(&state, &job, Utc::now()).await, Step::Continued);

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        // What this asserts is the brake and only the brake: the job is not parked, and not parked
        // for this reason. Whether the next node then reaches `running` is the spawn path's
        // business — it provisions a checkout and starts a CLI, neither of which this bench sets
        // up — and it has its own tests. Asserting it here would have made this test fail for
        // reasons that have nothing to do with the switch it exists to cover.
        assert_ne!(
            job_status(&pool, job_id).await,
            "waiting",
            "the brake is off, so the owner being present must not park this job"
        );
        let reason: Option<String> =
            sqlx::query_scalar("SELECT wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(reason.as_deref(), Some("attention"));
    }

    /// Decision 14, and the reason parked time counts toward it: without that, parking would be a
    /// way around the ceiling and a starved job would hold a worktree and the project's slot for
    /// as long as the contention lasted.
    #[tokio::test]
    async fn the_life_ceiling_retires_a_job_that_is_still_parked() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["pending"]).await;
        wait(&pool, job_id, "slot").await.unwrap();
        let started = Utc::now() - MAX_JOB_LIFETIME - chrono::Duration::minutes(1);
        sqlx::query("UPDATE jobs SET created_at = ? WHERE id = ?")
            .bind(started.to_rfc3339())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        job_tick(&state, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, STATUS_EXPIRED);
        assert!(feed_kinds(&pool).await.contains(&"job_expired".to_owned()));
    }

    /// The life ceiling asks who commissioned the job, and answers with a different number.
    ///
    /// Three ages against one job, because the risk here is a change that reads as "commissioned
    /// work never expires": past four hours it must survive, past twelve it must not, and the
    /// message it leaves must name twelve rather than the constant the other branch uses.
    ///
    /// The job is held still by its OWN allowance, already spent past, and that choice is the
    /// test working at all. `drive` reads the ceiling before it unparks and long before it would
    /// start anything, and the job's budget is read after — so the job is driven for real, nothing
    /// is spawned, and the only thing that can retire it is the clock under test. The kill switch
    /// would have been the obvious way to hold it and is the wrong one: `job_tick` returns on it
    /// before `drive` is ever called, so the ceiling would never be reached and every arm here
    /// would pass for the wrong reason.
    #[tokio::test]
    async fn a_commissioned_job_is_kept_to_its_own_clock_and_not_the_nightly_one() {
        async fn tick_at(age: chrono::Duration) -> (sqlx::SqlitePool, i64) {
            let pool = test_pool().await;
            let state = test_state(pool.clone()).await;
            let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
            seed_items(&pool, job_id, &["pending"]).await;
            seed_job_run(&pool, job_id, 50.0).await;
            sqlx::query(
                "UPDATE jobs SET rule_name = NULL, budget_usd = 25.0, created_at = ? WHERE id = ?",
            )
            .bind((Utc::now() - age).to_rfc3339())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

            job_tick(&state, Utc::now()).await;
            (pool, job_id)
        }

        // Past the ceiling that governs work nobody asked for, and this job was asked for.
        let (pool, job_id) = tick_at(MAX_JOB_LIFETIME + chrono::Duration::hours(1)).await;
        assert_ne!(
            job_status(&pool, job_id).await,
            STATUS_EXPIRED,
            "a job the owner commissioned was retired by the nightly ceiling"
        );

        // A whole night in, and still inside its own.
        let (pool, job_id) = tick_at(chrono::Duration::hours(8)).await;
        assert_ne!(
            job_status(&pool, job_id).await,
            STATUS_EXPIRED,
            "eight hours is the span an overnight run is asked for, and it did not survive it"
        );

        // Past its own, where the backstop is the point: the exemption is a longer clock, not none.
        let (pool, job_id) =
            tick_at(COMMISSIONED_JOB_LIFETIME + chrono::Duration::minutes(1)).await;
        assert_eq!(job_status(&pool, job_id).await, STATUS_EXPIRED);
        let told = feed_texts(&pool).await;
        assert!(
            told.iter().any(|line| line.contains("12-hour")),
            "the job was told the wrong ceiling had retired it: {told:?}"
        );
    }

    /// The panic button stops the chain, not just the node. It does not mark anything terminal,
    /// deliberately: the read fails closed, so a database hiccup that retired every job in flight
    /// would be a far worse failure than the one the switch is for.
    #[tokio::test]
    async fn the_kill_switch_stops_the_chain_without_destroying_it() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["pending"]).await;
        sqlx::query("UPDATE autopilot_global SET kill_switch = 1")
            .execute(&pool)
            .await
            .unwrap();

        job_tick(&state, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, "implementing");
        assert_eq!(item_statuses(&pool, job_id).await, vec!["pending"]);
        assert!(feed_kinds(&pool).await.is_empty());
    }

    // ---- the gate --------------------------------------------------------------------------

    /// §7 of the design calls this the easiest mistake to make, because both readings are "did not
    /// pass". A `cargo` that would not start has to reach the user as "not measured", never as
    /// "your tests failed" — one is silence and the other is a verdict.
    #[tokio::test]
    async fn a_gate_that_could_not_run_ends_a_job_differently_from_one_that_failed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;

        let broken = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, broken, &["implemented"]).await;
        // A real place to put the red item's work back to: a checkout on record and the job's
        // footing at its HEAD. Without one the red gate ends `revert_failed`/`stopped`, which is a
        // third ending and not the one this test is about.
        let checkout = tempfile::tempdir().expect("a checkout for the red job");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@x"],
            vec!["config", "user.name", "test"],
            vec!["commit", "--allow-empty", "-m", "seed"],
        ] {
            assert!(git_ok(checkout.path(), &args));
        }
        let footing = crate::execution::worktree::head_sha(checkout.path())
            .await
            .expect("the checkout's HEAD");
        seed_worktree(&pool, broken, checkout.path()).await;
        sqlx::query("UPDATE jobs SET head_sha = ? WHERE id = ?")
            .bind(&footing)
            .bind(broken)
            .execute(&pool)
            .await
            .unwrap();
        let row = load_job(&pool, broken).await.unwrap();
        assert_eq!(
            record_gate(
                &state,
                &row,
                0,
                crate::gate::GateOutcome::Failed {
                    exit_code: 101,
                    output: "test failed".into(),
                },
            )
            .await,
            Step::Continued,
            "the premise: the red item was taken off the branch"
        );

        let unmeasured = seed_job(&pool, "project-b", "implementing").await.unwrap();
        seed_items(&pool, unmeasured, &["implemented"]).await;
        let row = load_job(&pool, unmeasured).await.unwrap();
        record_gate(
            &state,
            &row,
            0,
            crate::gate::GateOutcome::Errored {
                reason: "cargo not found".into(),
            },
        )
        .await;

        // The red job's item was reverted and marked `gate_failed`; the errored one was not
        // reverted at all (a gate that did not run judged nothing). The MARK is what these
        // assertions read.
        //
        // And there the two part company, which is the point of the test: silence ends the job
        // `gate_errored`, a verdict ends it `gate_failed`. Silence stops the job where it stands.
        // A verdict lets the queue carry on, and here the queue is spent, so the job goes where a
        // spent queue goes — without a review, because the round's only item did not pass and
        // there is nothing on the branch for one to judge (see `ReviewState::Skipped`). The
        // verdict with a review in front of it is pinned purely in
        // `one_red_gate_makes_the_whole_job_gate_failed_however_it_ends`.
        assert_eq!(
            next_step(&load_view(&pool, broken).await.unwrap()),
            Next::Finish(Outcome::GateFailed)
        );
        assert_eq!(
            next_step(&load_view(&pool, unmeasured).await.unwrap()),
            Next::Finish(Outcome::GateErrored)
        );
    }

    /// The retry budget is copied onto the job when it starts, or the config key is decoration.
    ///
    /// The join every other test in this chunk hangs off, and the only one that can fail on it.
    /// `GraphConfig::gate_retries()` can be right, `item_state_from` can be right, `record_gate` can
    /// be right, and every one of those assertions can pass while every real job runs on the column
    /// default of 0 — because nothing on the way IN ever carried the number across. The two
    /// pool-backed tests below set `jobs.gate_retries` by hand, so they cannot notice; this one goes
    /// through `NewJob` and reads the row back, which is the only shape that can.
    ///
    /// Copied rather than re-read, for the reason `0042_jobs.sql` already gives about `max_items`,
    /// `gate_each` and `review`: the project's `autopilot.yaml` can be edited mid-flight, and a job
    /// that
    /// changed shape between its own nodes would gate some items and not others with nothing
    /// recording why. A budget re-read at each step has that failure with a worse symptom — one item
    /// retried because the file said 2 this morning, and the item beside it dropped because it says
    /// 0 now, in the same night, under the same gate.
    #[tokio::test]
    async fn a_jobs_retry_budget_comes_from_the_rule_that_started_it() {
        let pool = test_pool().await;
        let job_id = insert_job(
            &pool,
            &NewJob {
                project_id: "project-a",
                project_root: "/project/a",
                rule_name: Some("nightly-backlog"),
                prompt: "pull from the todo list and advance what you can",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 2,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        .expect("start a job");

        let stored: i64 = sqlx::query_scalar("SELECT gate_retries FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            stored, 2,
            "the budget the rule asked for never reached the row the job actually runs on"
        );
    }

    /// A red gate with a retry left leaves the tree where it stands and asks for the item again.
    ///
    /// **No worktree row is seeded, and the absence is the assertion.** Today a red gate reverts the
    /// tree to the item's footing before it marks anything, and a job with no worktree on record
    /// takes the `!reverted` branch: it marks the item and stops the night, refusing to let the
    /// queue advance onto a tree it could not put back. A retry must not reach for any of that — the
    /// work it is about to redo is the work standing in the tree, and reverting it first would throw
    /// away everything the second node would otherwise start from, turning a near miss into a blank
    /// page. If the revert machinery still ran here, this job would come back with a dead queue
    /// instead of asking for item 1 again, and this test is what would notice.
    ///
    /// The count and the output are the other half. Without the count the budget never runs out and
    /// a hopeless item retries forever; without the output the second node is a second guess.
    #[tokio::test]
    async fn a_red_gate_with_a_retry_left_keeps_the_tree() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        // Set on the row rather than asked for at creation: `NewJob` carries no retry budget yet,
        // and a job is what the column says it is whatever wrote it.
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        // `implemented` is the status an item is really in when the gate measures it — `next_step`
        // answers `RunGate` for exactly that state and no other.
        seed_items(&pool, job_id, &["implemented"]).await;
        let row = load_job(&pool, job_id).await.unwrap();

        record_gate(
            &state,
            &row,
            0,
            crate::gate::GateOutcome::Failed {
                exit_code: 1,
                output: "boom".into(),
            },
        )
        .await;

        let (attempts, output): (i64, Option<String>) = sqlx::query_as(
            "SELECT gate_attempts, gate_output FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            attempts, 1,
            "an uncounted red gate is a budget that never runs out"
        );
        assert_eq!(
            output.as_deref(),
            Some("boom"),
            "the retrying node has to be able to read what the gate said"
        );

        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnImplement(vec![0]),
            "the queue has to come back to the same item, not stop and not step over it"
        );
    }

    /// Asking for the item again is not the same as starting it, and only one of those was tested.
    ///
    /// Every other test of this feature stops at `next_step`, which is pure and which answers
    /// `SpawnImplement { ordinal: 0 }` for a retriable item quite correctly. `advance` then has to
    /// CLAIM that item, and the claim is a compare-and-swap that names the status it expects:
    /// `WHERE ... AND status = 'pending'`. A retriable item's row says `gate_failed`, so the swap
    /// matched nothing, `rows_affected() == 0` took the branch that exists to stop two nodes starting
    /// on one tree, and the job stopped at the first red gate — with the rejected work still standing,
    /// because `record_gate` had correctly skipped the revert on the way in. That is worse than the
    /// behaviour the retry replaced: before it, the job at least reverted and carried on.
    ///
    /// So this test crosses from the pure half into the I/O half deliberately. It drives `advance`,
    /// not `next_step`, and it reads the two rows that prove a node actually started: the item is
    /// `running` and it has a run attached. The prompt is the third assertion, and it is what makes
    /// the whole chain load-bearing — output stored by `record_gate`, selected by `advance`, appended
    /// by `implement_prompt`, and handed to a node. Any link missing and this says so.
    /// **Two items of one job are worked on at the same time.** The criterion §7 says the whole
    /// design rests on.
    ///
    /// It exists because the other criterion cannot catch a no-op on its own. "The parallel run is
    /// not slower than the sequential one" is satisfied trivially by a parallelism that collapsed to
    /// one — same plan, same order, same timings — and that is exactly how an earlier draft of
    /// this design convinced itself it worked. This asks the question the timings cannot: were two
    /// nodes ever going at once.
    ///
    /// Deterministic, and it has to be: no sleeping, no wall clock, no ordering between threads. One
    /// pass of `advance` over a queue of three, and the rows afterwards say how many started. Three
    /// items and a ceiling of two, so the answer distinguishes "two" from both "one" (no
    /// parallelism) and "all of them" (no ceiling).
    ///
    /// Item 1 declares the same file as item 0 and is the control: it must NOT be in the batch, or
    /// the two that did start were chosen by counting rather than by the fold.
    #[tokio::test(flavor = "current_thread")]
    async fn two_items_of_a_team_are_started_in_one_pass_and_a_third_is_not() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-parallel-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        seed_agent(&pool, "ana", "migrations", "careful").await;
        seed_crew(&pool, "crew", "ana", &[]).await;
        sqlx::query("UPDATE teams SET max_parallel = 2 WHERE id = 'crew'")
            .execute(&pool)
            .await
            .unwrap();

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");

        // The queue a director's plan produces: three items, and item 1 sharing a file with item 0.
        for (ordinal, files) in [(0, r#"["a.rs"]"#), (1, r#"["a.rs"]"#), (2, r#"["b.rs"]"#)] {
            sqlx::query(
                "INSERT INTO job_items (job_id, ordinal, description, status, files, depends_on,
                                        agent_id)
                 VALUES (?, ?, 'an item', 'pending', ?, '[]', 'ana')",
            )
            .bind(job_id)
            .bind(ordinal as i64)
            .bind(files)
            .execute(&pool)
            .await
            .unwrap();
        }

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        // Read before the assertion so it can print it. `["pending", "pending", "pending"]` alone
        // is all 2026-09-13 left to go on, and it reads as the fold choosing nothing — when the
        // fold chose right, the first run it asked for was refused, and only the job's row and
        // the feed said why.
        let why = why_it_stands(&pool, job_id).await;
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["running", "pending", "running"],
            "one pass started items 0 and 2 together, and left 1 for the file it shares with 0 \
             (job: {why})"
        );

        // And they are two nodes, in two checkouts, not one node counted twice. `item_id` is what
        // says so: it is the owner of the worktree each of them opened its editor in.
        let items: Vec<i64> = sqlx::query_scalar(
            "SELECT runs.item_id FROM runs
              WHERE runs.job_id = ? AND runs.item_id IS NOT NULL ORDER BY runs.item_id",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(items.len(), 2, "two runs, one per started item: {items:?}");
        assert_ne!(items[0], items[1]);

        let trees: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM worktrees WHERE owner_kind = 'item' AND removed_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(trees, 2, "each item is writing in a checkout of its own");
    }

    /// An item's node reads its agent's memory and its team's, and no other agent's or team's.
    #[tokio::test(flavor = "current_thread")]
    async fn a_team_items_node_is_briefed_with_its_agents_and_its_teams_memory() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-loadout-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        seed_agent(&pool, "ana", "migrations", "careful").await;
        seed_crew(&pool, "crew", "ana", &[]).await;
        // The foreign owners have rows too, so the isolation below is proven by scope filtering
        // and not by the owner check dropping a link whose row is missing.
        seed_agent(&pool, "bob", "frontend", "bold").await;
        seed_crew(&pool, "other-crew", "bob", &[]).await;

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");

        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, files, depends_on,
                                    agent_id)
             VALUES (?, 0, 'zanzibar quokka item', 'pending', '[\"src/a.rs\"]', '[]', 'ana')",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        // "zanzibar" is in every row, so FTS gives it no weight: each in-scope note also needs a term of its own that the task text carries ("ana" for the agent, "quokka" for the team).
        // The foreign rows carry the task's terms too ("zanzibar quokka ana"), so ranking alone
        // could not keep them out: only the scope chain can.
        let mut knowledge_ids = Vec::new();
        for (scope_kind, scope_id, title) in [
            ("agent", "ana", "zanzibar ana-note"),
            ("team", "crew", "zanzibar quokka crew-note"),
            ("agent", "bob", "zanzibar quokka ana bob-note"),
            ("team", "other-crew", "zanzibar quokka other-crew-note"),
        ] {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO knowledge
                   (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
                 VALUES ('semantic', ?, ?, 'owner', 'memory', ?, 'body', 'active',
                         '2026-08-19T00:00:00+00:00')
                 RETURNING id",
            )
            .bind(scope_kind)
            .bind(scope_id)
            .bind(title)
            .fetch_one(&pool)
            .await
            .unwrap();
            knowledge_ids.push(id);
        }
        let (ana_id, crew_id, bob_id, other_id) = (
            knowledge_ids[0],
            knowledge_ids[1],
            knowledge_ids[2],
            knowledge_ids[3],
        );

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        let run_id: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let run_id = run_id.expect("the item's node started, so it has a run to read");
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        // The foreign rows first: `crew-note` is a substring of `other-crew-note`, so the positive
        // check below only means something once the other team's row is known to be absent.
        assert!(!prompt.contains("bob-note"), "{prompt}");
        assert!(!prompt.contains("other-crew-note"), "{prompt}");
        assert!(prompt.contains("ana-note"), "{prompt}");
        assert!(prompt.contains("crew-note"), "{prompt}");

        // The trace says the same thing as the prompt: nothing of the foreign rows was even a
        // candidate for this run, shown or not.
        let foreign: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM run_knowledge WHERE run_id = ? AND knowledge_id IN (?, ?)",
        )
        .bind(run_id)
        .bind(bob_id)
        .bind(other_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            foreign, 0,
            "another agent's or team's row reached the trace"
        );
        for (mine, name) in [(ana_id, "ana's"), (crew_id, "the team's")] {
            let traced: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM run_knowledge WHERE run_id = ? AND knowledge_id = ?",
            )
            .bind(run_id)
            .bind(mine)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(traced, 1, "{name} row {mine} is missing from the trace");
        }
    }

    /// A team job in `repo` with its worktree recorded: the setup the loadout tests below share.
    async fn loadout_team_job(pool: &sqlx::SqlitePool, repo: &std::path::Path) -> i64 {
        let job_id = seed_job_in(
            pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(job_id)
            .execute(pool)
            .await
            .unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        job_id
    }

    /// One pending item of `job_id`, owned by `agent`, touching `files`.
    async fn loadout_pending_item(
        pool: &sqlx::SqlitePool,
        job_id: i64,
        ordinal: i64,
        agent: &str,
        files: &str,
    ) {
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, files, depends_on,
                                    agent_id)
             VALUES (?, ?, 'a loadout item', 'pending', ?, '[]', ?)",
        )
        .bind(job_id)
        .bind(ordinal)
        .bind(files)
        .bind(agent)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The run the item at `ordinal` started, once the pass that claims it has been made.
    async fn loadout_item_run(pool: &sqlx::SqlitePool, job_id: i64, ordinal: i64) -> i64 {
        let run_id: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM job_items WHERE job_id = ? AND ordinal = ?")
                .bind(job_id)
                .bind(ordinal)
                .fetch_one(pool)
                .await
                .unwrap();
        run_id.expect("the item's node started, so it has a run to read")
    }

    /// The tools a run's `run_loadout` row froze, and the owners it names.
    async fn loadout_row_of(
        pool: &sqlx::SqlitePool,
        run_id: i64,
    ) -> (Option<String>, Option<String>, Vec<String>) {
        let (agent, team, tools): (Option<String>, Option<String>, String) =
            sqlx::query_as("SELECT agent_id, team_id, tools FROM run_loadout WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(pool)
                .await
                .expect("the node recorded a run_loadout row before it started");
        let mut tools: Vec<String> = serde_json::from_str(&tools).expect("tools is a JSON array");
        tools.sort();
        (agent, team, tools)
    }

    /// An item's node records the loadout it was launched with, and launches with exactly that.
    ///
    /// Ana owns a folder ref and bob owns a file ref. Ana's node sees her folder in the prompt
    /// and as `--add-dir`, never bob's file, and the row, the allow-list the runner was handed and
    /// the prompt all come out of the one resolution.
    #[tokio::test(flavor = "current_thread")]
    async fn loadout_a_team_items_node_records_its_loadout_and_launches_with_it() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-loadout-run-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let files = tempfile::tempdir().expect("managed files root");
        let files_root = std::fs::canonicalize(files.path()).unwrap();
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;
        let mut state = with_home(state, &_container);
        state.files_root = Some(files_root.clone());

        seed_agent(&pool, "ana", "migrations", "careful").await;
        seed_crew(&pool, "crew", "ana", &[]).await;
        seed_agent(&pool, "bob", "frontend", "bold").await;
        seed_crew(&pool, "other-crew", "bob", &[]).await;

        let ana_dir = files_root.join("ana-docs");
        std::fs::create_dir_all(&ana_dir).unwrap();
        std::fs::write(ana_dir.join("guide.md"), "ana's guide").unwrap();
        let bob_file = files_root.join("bob-secrets.txt");
        std::fs::write(&bob_file, "bob's file").unwrap();
        for (owner, path, kind) in [("ana", &ana_dir, "dir"), ("bob", &bob_file, "file")] {
            sqlx::query(
                "INSERT INTO context_refs (owner_kind, owner_id, path, kind, note, created_at)
                 VALUES ('agent', ?, ?, ?, NULL, '2026-10-09T00:00:00Z')",
            )
            .bind(owner)
            .bind(path.to_string_lossy().into_owned())
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }

        let job_id = loadout_team_job(&pool, &repo).await;
        loadout_pending_item(&pool, job_id, 0, "ana", r#"["src/a.rs"]"#).await;

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;
        let run_id = loadout_item_run(&pool, job_id, 0).await;

        // The row: the full effective set (the box's base, nothing approved), and the owners it was
        // resolved for.
        let mut base: Vec<String> = crate::mcp_tools::JOB_NODE_BASE
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect();
        base.sort();
        let (agent, team, tools) = loadout_row_of(&pool, run_id).await;
        assert_eq!(agent.as_deref(), Some("ana"));
        assert_eq!(team.as_deref(), Some("crew"));
        assert_eq!(tools, base, "no approvals: exactly the job-node base");

        // The prompt: ana's ref is in the index, bob's is nowhere.
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            prompt.contains("Context files you were given"),
            "the index heading is missing: {prompt}"
        );
        assert!(
            prompt.contains(&ana_dir.to_string_lossy().into_owned()),
            "{prompt}"
        );
        assert!(!prompt.contains("bob-secrets"), "{prompt}");

        // The launch: the runner is handed the same set the row froze, and ana's folder as a
        // directory to add. The launch happens on a task, so wait for it as `runs.rs` does.
        let mut launch = None;
        for _ in 0..200 {
            if let Some(seen) = runner.last_job_mcp.lock().unwrap().clone() {
                launch = Some(seen);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (_config, _job, allowed) = launch.expect("the runner never received an MCP launch");
        let mut allowed = allowed.expect("a job node launches with an allow-list");
        allowed.sort();
        assert_eq!(allowed, base, "the launch must carry the loadout's tools");

        let refs = crate::context_refs::spawn_refs(&pool, &files_root, Some("ana"), Some("crew"))
            .await
            .unwrap();
        let expected_dirs = crate::context_refs::add_dirs(&refs);
        assert_eq!(expected_dirs.len(), 1, "only ana's folder is a dir ref");
        let dirs = runner
            .last_add_dirs
            .lock()
            .unwrap()
            .clone()
            .expect("the launch recorded its add_dirs");
        assert_eq!(dirs, expected_dirs);
    }

    /// An extra an owner approved reaches that agent's node and no other: two agents' items start
    /// in one pass, and only the approved one's row lists `web_read`.
    #[tokio::test(flavor = "current_thread")]
    async fn loadout_an_approved_extra_reaches_only_its_agents_node() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-loadout-extra-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        seed_agent(&pool, "ana", "migrations", "careful").await;
        seed_agent(&pool, "bob", "frontend", "bold").await;
        seed_crew(&pool, "crew", "ana", &["bob"]).await;
        sqlx::query("UPDATE teams SET max_parallel = 2 WHERE id = 'crew'")
            .execute(&pool)
            .await
            .unwrap();

        // Bob's `web_read` is approved. Ana's is only proposed, which must grant nothing.
        for (agent, status) in [("bob", "active"), ("ana", "proposed")] {
            sqlx::query(
                "INSERT INTO loadout_tools
                     (owner_kind, owner_id, tool, status, source, reason, run_id, created_at)
                 VALUES ('agent', ?, 'web_read', ?, 'request', 'why', NULL,
                         '2026-10-09T00:00:00Z')",
            )
            .bind(agent)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        }

        let job_id = loadout_team_job(&pool, &repo).await;
        // Disjoint files, so one pass starts both items.
        loadout_pending_item(&pool, job_id, 0, "ana", r#"["a.rs"]"#).await;
        loadout_pending_item(&pool, job_id, 1, "bob", r#"["b.rs"]"#).await;

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;
        let ana_run = loadout_item_run(&pool, job_id, 0).await;
        let bob_run = loadout_item_run(&pool, job_id, 1).await;

        let (_, _, ana_tools) = loadout_row_of(&pool, ana_run).await;
        let (_, _, bob_tools) = loadout_row_of(&pool, bob_run).await;
        assert!(
            !ana_tools.iter().any(|tool| tool == "web_read"),
            "a proposed extra reached ana's node, or bob's approval did: {ana_tools:?}"
        );
        assert!(
            bob_tools.iter().any(|tool| tool == "web_read"),
            "bob's approved extra is missing from his node: {bob_tools:?}"
        );
        // Everything else is the base on both sides, so the extra is the only difference.
        let without_extra: Vec<&String> = bob_tools
            .iter()
            .filter(|tool| *tool != "web_read")
            .collect();
        assert_eq!(without_extra.len(), ana_tools.len());
        let bob_lists = crate::tool_loadout::run_lists_tool(&pool, bob_run, "web_read").await;
        let ana_lists = crate::tool_loadout::run_lists_tool(&pool, ana_run, "web_read").await;
        assert!(bob_lists.unwrap());
        assert!(!ana_lists.unwrap());
    }

    /// A real repository and a real worktree, because a node that cannot be provisioned would leave
    /// the item back where it started for a reason that has nothing to do with the claim — and would
    /// look exactly like the defect this pins.
    #[tokio::test(flavor = "current_thread")]
    async fn a_retriable_item_is_claimed_and_started_not_merely_asked_for() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-retryclaim-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        // The row a red gate with a retry left leaves behind, written the way `record_gate` writes
        // it: the status still says `gate_failed`, and it is the count beside the job's budget that
        // makes it retriable. Seeded rather than gated so that what fails here can only be the claim.
        seed_items(&pool, job_id, &["gate_failed"]).await;
        sqlx::query(
            "UPDATE job_items SET gate_status = 'failed', gate_attempts = 1, gate_output = ?
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind("FAILED tests/test_cursor.py::test_guard")
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["running"],
            "the retry was asked for and never claimed, so the job stopped on an unreverted tree"
        );
        let run_id: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let run_id = run_id.expect("the retried item has a node of its own attached to it");

        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            prompt.contains("FAILED tests/test_cursor.py::test_guard"),
            "the node that has to answer the gate was not told what it said: {prompt}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_retried_item_that_fails_without_a_gate_is_judged_by_its_run_before_and_after_a_replan()
     {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-retryverdict-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["gate_failed"]).await;
        sqlx::query(
            "UPDATE job_items SET gate_status = 'failed', gate_attempts = 1
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        let (status, gate_status, run_id): (String, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT status, gate_status, run_id FROM job_items
                 WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "running");
        assert_eq!(
            gate_status, None,
            "the old attempt's gate was still attached"
        );
        let run_id = run_id.expect("the retried item has a node of its own attached to it");

        // `interrupted` is what a restart writes, and a restarted daemon holds no task for the
        // run. Drop this one's registration too, or `reconcile_nodes` reads the row as still
        // inside its handoff window and leaves the item running.
        if let Some(task) = state
            .run_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&run_id)
        {
            task.abort();
        }
        sqlx::query("UPDATE runs SET status = 'interrupted', exit_code = NULL WHERE id = ?")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        let job = load_job(&pool, job_id).await.unwrap();
        assert!(matches!(
            reconcile_nodes(&state, &job).await.unwrap(),
            Reconciled::KeepGoing
        ));
        assert_eq!(item_statuses(&pool, job_id).await, vec!["failed"]);

        let (status, gate_status, gate_attempts): (String, Option<String>, i64) = sqlx::query_as(
            "SELECT status, gate_status, gate_attempts FROM job_items
                 WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let gate_retries: i64 = sqlx::query_scalar("SELECT gate_retries FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let (run_status, exit_code): (String, Option<i64>) =
            sqlx::query_as("SELECT status, exit_code FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let run = Some((run_status.as_str(), exit_code));

        assert_eq!(
            crate::brief::item_verdict(
                item_state_from(&status, gate_attempts, gate_retries),
                gate_status.as_deref(),
                false,
                run,
            ),
            Some(crate::brief::Verdict::NoOutcome)
        );
        assert_eq!(
            crate::brief::item_verdict(
                item_state_from(STATUS_SUPERSEDED, gate_attempts, gate_retries),
                gate_status.as_deref(),
                false,
                run,
            ),
            Some(crate::brief::Verdict::NoOutcome)
        );
    }

    #[tokio::test]
    async fn a_released_retry_claim_keeps_its_red_gate() {
        let pool = test_pool().await;
        let retry_job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, retry_job_id, &["running"]).await;
        sqlx::query(
            "UPDATE job_items SET gate_status = NULL, gate_attempts = 1
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind(retry_job_id)
        .execute(&pool)
        .await
        .unwrap();
        let retry_job = load_job(&pool, retry_job_id).await.unwrap();

        release_item(
            &pool,
            &retry_job,
            Some(ItemClaim {
                ordinal: 0,
                held: "gate_failed",
            }),
        )
        .await;

        let retry: (String, Option<String>) = sqlx::query_as(
            "SELECT status, gate_status FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(retry_job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retry, ("gate_failed".into(), Some("failed".into())));

        let pending_job_id = seed_job(&pool, "project-b", "implementing").await.unwrap();
        seed_items(&pool, pending_job_id, &["running"]).await;
        let pending_job = load_job(&pool, pending_job_id).await.unwrap();

        release_item(
            &pool,
            &pending_job,
            Some(ItemClaim {
                ordinal: 0,
                held: "pending",
            }),
        )
        .await;

        let pending: (String, Option<String>) = sqlx::query_as(
            "SELECT status, gate_status FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(pending_job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, ("pending".into(), None));
    }

    /// What a red gate that cannot put its branch back must leave behind, whatever the reason.
    ///
    /// The item is `revert_failed` and not `gate_failed`: `gate_failed` reads as "the work was
    /// rejected and taken off the branch", and here it was rejected and is still ON it. The job
    /// stops, because every later item would build on work a gate has just called broken. The red
    /// gate is counted anyway, and the feed line says in words that the branch was not restored.
    async fn assert_reverts_nothing_and_stops(pool: &sqlx::SqlitePool, job_id: i64, step: Step) {
        assert_eq!(
            step,
            Step::Stopped,
            "a tree that could not be put back still ends the pass"
        );
        assert_eq!(
            item_statuses(pool, job_id).await,
            vec!["revert_failed"],
            "an item still on the branch must not read as one taken off it"
        );
        assert_eq!(job_status(pool, job_id).await, "stopped");
        let attempts: i64 = sqlx::query_scalar(
            "SELECT gate_attempts FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(
            attempts, 1,
            "the red gate happened, and it is counted whether or not it bought anything"
        );
        assert!(
            feed_texts(pool)
                .await
                .iter()
                .any(|text| text.contains("could not be put back")),
            "the feed has to say the branch was not restored"
        );
        assert!(
            feed_kinds(pool)
                .await
                .iter()
                .any(|kind| kind == "job_gate_failed"),
            "the line goes out under the existing gate-failed kind"
        );
    }

    /// A job with a single item the gate is about to call red, and nothing to spend on a retry.
    ///
    /// `gate_retries = 0` is every job written before migration 0068, so this is the regression the
    /// retry is most likely to break: with no budget the first red gate is final.
    async fn a_job_with_one_item_about_to_go_red(pool: &sqlx::SqlitePool) -> i64 {
        let job_id = seed_job(pool, "project-a", "implementing").await.unwrap();
        // Said out loud rather than left to the column default, because it is the premise: this is
        // a job with nothing to spend.
        sqlx::query("UPDATE jobs SET gate_retries = 0 WHERE id = ?")
            .bind(job_id)
            .execute(pool)
            .await
            .unwrap();
        seed_items(pool, job_id, &["implemented"]).await;
        job_id
    }

    fn red_gate() -> crate::gate::GateOutcome {
        crate::gate::GateOutcome::Failed {
            exit_code: 1,
            output: "boom".into(),
        }
    }

    /// Out of retries and no worktree on record: nothing was put back, and the item says so.
    ///
    /// Rewrites `a_red_gate_out_of_retries_still_stops_when_it_cannot_revert`, which asserted
    /// `gate_failed` and so called an item rejected and taken off the branch when neither was true
    /// of the second half.
    #[tokio::test]
    async fn a_red_gate_that_cannot_be_reverted_marks_the_item_and_stops_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = a_job_with_one_item_about_to_go_red(&pool).await;
        let row = load_job(&pool, job_id).await.unwrap();

        let step = record_gate(&state, &row, 0, red_gate()).await;

        assert_reverts_nothing_and_stops(&pool, job_id, step).await;
    }

    /// A worktree on record is not a place to go back TO: with no footing there is no revision to
    /// reset to, and the honest response is to leave the tree alone and say so.
    ///
    /// The job's `head_sha` is NULL (a job written before the column, or one whose head could not
    /// be read) and no item has a checkpoint, so `revert_point` answers nothing.
    #[tokio::test]
    async fn a_red_gate_with_no_revert_point_is_revert_failed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = a_job_with_one_item_about_to_go_red(&pool).await;
        let tmp = tempfile::tempdir().expect("a directory for the job");
        seed_worktree(&pool, job_id, tmp.path()).await;
        let row = load_job(&pool, job_id).await.unwrap();
        assert!(
            revert_point(&pool, &row, 0).await.is_none(),
            "the premise: nothing knows where this item started"
        );

        let step = record_gate(&state, &row, 0, red_gate()).await;

        assert_reverts_nothing_and_stops(&pool, job_id, step).await;
    }

    /// A footing and a worktree, and git still cannot do it: the reset itself fails.
    ///
    /// The worktree row points at a plain directory that is no checkout, so `revert_to` errors. An
    /// error from the reset must land where a missing revert point does, not on `gate_failed`.
    #[tokio::test]
    async fn a_red_gate_whose_reset_fails_is_revert_failed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = a_job_with_one_item_about_to_go_red(&pool).await;
        let tmp = tempfile::tempdir().expect("a directory that is not a checkout");
        seed_worktree(&pool, job_id, tmp.path()).await;
        // Any 40-hex string: it only has to exist so that a footing does.
        sqlx::query("UPDATE jobs SET head_sha = ? WHERE id = ?")
            .bind("0123456789abcdef0123456789abcdef01234567")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let row = load_job(&pool, job_id).await.unwrap();
        assert!(
            revert_point(&pool, &row, 0).await.is_some(),
            "the premise: there is a revision to go back to"
        );

        let step = record_gate(&state, &row, 0, red_gate()).await;

        assert_reverts_nothing_and_stops(&pool, job_id, step).await;
    }

    #[tokio::test]
    async fn the_last_red_gate_of_an_item_credits_its_briefing_and_a_retriable_one_does_not() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["implemented"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "completed").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = 0")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let knowledge_id = seed_shown_item_trace(&pool, job_id, 0, run_id).await;
        let job = load_job(&pool, job_id).await.unwrap();

        record_gate(
            &state,
            &job,
            0,
            crate::gate::GateOutcome::Failed {
                exit_code: 1,
                output: "red".into(),
            },
        )
        .await;

        assert_eq!(knowledge_counts(&pool, knowledge_id).await, (0, 0, 0));
        assert_eq!(trace_credited_at(&pool, run_id, knowledge_id).await, None);

        record_gate(
            &state,
            &job,
            0,
            crate::gate::GateOutcome::Failed {
                exit_code: 1,
                output: "red".into(),
            },
        )
        .await;

        assert_eq!(knowledge_counts(&pool, knowledge_id).await, (1, 1, 0));
        assert!(
            trace_credited_at(&pool, run_id, knowledge_id)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_green_gate_credits_the_items_briefing_green() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["implemented"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "completed").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = 0")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let knowledge_id = seed_shown_item_trace(&pool, job_id, 0, run_id).await;
        let job = load_job(&pool, job_id).await.unwrap();

        record_gate(&state, &job, 0, crate::gate::GateOutcome::Passed).await;

        assert_eq!(knowledge_counts(&pool, knowledge_id).await, (1, 1, 1));
    }

    /// Every `distill_queue` row as `(cause, job_id, item_id, project_id)`, oldest first.
    async fn distill_rows(
        pool: &sqlx::SqlitePool,
    ) -> Vec<(String, Option<i64>, Option<i64>, String)> {
        sqlx::query_as("SELECT cause, job_id, item_id, project_id FROM distill_queue ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// A job that ends without completing is queued for the distiller, once; a completed one is
    /// not, because it is the land that carries it there.
    #[tokio::test]
    async fn a_job_that_does_not_complete_is_queued_for_distillation_once() {
        let pool = test_pool().await;
        let failed = seed_job(&pool, "project-a", "implementing").await.unwrap();

        assert!(retire(&pool, failed, "failed").await.unwrap());
        assert!(!retire(&pool, failed, "failed").await.unwrap());

        let completed = seed_job(&pool, "project-b", "implementing").await.unwrap();
        assert!(retire(&pool, completed, "completed").await.unwrap());

        assert_eq!(
            distill_rows(&pool).await,
            vec![(
                "job_failed".to_string(),
                Some(failed),
                None,
                "project-a".to_string()
            )],
            "one row for the job that did not complete, none for the one that did"
        );
    }

    /// A green gate on an item that had gone red before is a recovery worth learning from.
    #[tokio::test]
    async fn a_gate_that_passes_after_a_red_queues_a_recovery() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["implemented"]).await;
        sqlx::query(
            "UPDATE job_items SET gate_attempts = 1, gate_status = 'failed'
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        let item_id: i64 =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let job = load_job(&pool, job_id).await.unwrap();

        record_gate(&state, &job, 0, crate::gate::GateOutcome::Passed).await;

        assert_eq!(
            distill_rows(&pool).await,
            vec![(
                "gate_recovered".to_string(),
                Some(job_id),
                Some(item_id),
                "project-a".to_string()
            )]
        );
    }

    /// A red gate that takes an item past the job's retries is an exhaustion; the first red of a
    /// job with a retry left is not.
    #[tokio::test]
    async fn an_item_out_of_gate_retries_queues_its_exhaustion() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET gate_retries = 0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["implemented"]).await;
        let item_id: i64 =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        // A real revert point, so the red item ends `gate_failed` (its work taken off the
        // branch). Without one it ends `revert_failed` and the job is retired `stopped`, which
        // queues the JOB's failure instead and is not what this test is about.
        let checkout = tempfile::tempdir().expect("a checkout for the job");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@x"],
            vec!["config", "user.name", "test"],
            vec!["commit", "--allow-empty", "-m", "seed"],
        ] {
            assert!(git_ok(checkout.path(), &args));
        }
        let footing = crate::execution::worktree::head_sha(checkout.path())
            .await
            .expect("the checkout's HEAD");
        seed_worktree(&pool, job_id, checkout.path()).await;
        sqlx::query("UPDATE jobs SET head_sha = ? WHERE id = ?")
            .bind(&footing)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let job = load_job(&pool, job_id).await.unwrap();

        let step = record_gate(
            &state,
            &job,
            0,
            crate::gate::GateOutcome::Failed {
                exit_code: 1,
                output: "boom".into(),
            },
        )
        .await;

        // `Continued`, not the `Stopped` this asserted when its fixture had no revert point: the
        // item was reverted and the job carries on to its ending, rather than stopping on a branch
        // it could not put back.
        assert_eq!(step, Step::Continued);
        assert_eq!(item_statuses(&pool, job_id).await, vec!["gate_failed"]);
        assert_eq!(
            distill_rows(&pool).await,
            vec![(
                "run_exhausted".to_string(),
                Some(job_id),
                Some(item_id),
                "project-a".to_string()
            )]
        );
    }

    /// An exhausted item whose branch cannot be put back is `revert_failed`, and its own
    /// RunExhausted verdict is queued all the same (the job's failure is a separate row).
    #[tokio::test]
    async fn a_revert_failed_item_out_of_retries_still_queues_its_exhaustion() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = a_job_with_one_item_about_to_go_red(&pool).await;
        let item_id: i64 =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let row = load_job(&pool, job_id).await.unwrap();

        let step = record_gate(&state, &row, 0, red_gate()).await;

        assert_reverts_nothing_and_stops(&pool, job_id, step).await;
        let exhausted: Vec<_> = distill_rows(&pool)
            .await
            .into_iter()
            .filter(|row| row.0 == "run_exhausted" && row.2 == Some(item_id))
            .collect();
        assert_eq!(
            exhausted,
            vec![(
                "run_exhausted".to_string(),
                Some(job_id),
                Some(item_id),
                "project-a".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn an_item_passed_without_a_gate_is_credited_by_its_run() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET gate_each = 0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["implemented", "implemented"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "completed").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = 0")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let knowledge_id = seed_shown_item_trace(&pool, job_id, 0, run_id).await;
        let job = load_job(&pool, job_id).await.unwrap();

        gate_item(&state, &job, 0, 2).await;

        assert_eq!(knowledge_counts(&pool, knowledge_id).await, (1, 1, 1));
    }

    #[tokio::test]
    async fn the_job_tick_credits_what_a_crash_left_uncredited() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "gate_failed").await.unwrap();
        seed_items(&pool, job_id, &["gate_failed"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "completed").await;
        sqlx::query(
            "UPDATE job_items
                SET run_id = ?, gate_status = 'failed', gate_attempts = 1
              WHERE job_id = ? AND ordinal = 0",
        )
        .bind(run_id)
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        let knowledge_id = seed_shown_item_trace(&pool, job_id, 0, run_id).await;

        job_tick(&state, Utc::now()).await;

        assert_eq!(knowledge_counts(&pool, knowledge_id).await, (1, 1, 0));
    }

    /// `gate_after_each_item: false` buys back the intermediate suite runs. The final gate runs
    /// regardless, because a job that measured nothing hands back a partial nobody can trust.
    #[tokio::test]
    async fn the_last_item_is_gated_even_when_per_item_gating_is_off() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        crate::project::state::write_for_test(
            worktree.path(),
            "project-a",
            crate::project::state::AUTOPILOT_FILE,
            "gate_command: definitely-not-a-real-binary\n",
        );
        let state = AppState {
            machine_config_root: Some(worktree.path().to_path_buf()),
            ..state
        };
        sqlx::query("UPDATE jobs SET gate_each = 0, project_root = ? WHERE id = ?")
            .bind(worktree.path().to_string_lossy().into_owned())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["implemented", "implemented"]).await;

        let job = load_job(&pool, job_id).await.unwrap();
        gate_item(&state, &job, 0, 2).await;
        // Not measured, so no verdict is recorded against it — the item passes on the strength of
        // the final gate that is still to come.
        assert_eq!(item_statuses(&pool, job_id).await[0], "passed");
        let verdicts: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT gate_status FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(verdicts[0], None);

        gate_item(&state, &job, 1, 2).await;
        // A command that will not start is `errored`, and the distinction is what proves the last
        // item was actually measured rather than waved through like the first.
        assert_eq!(item_statuses(&pool, job_id).await[1], "gate_errored");
    }

    // ---- intermediate gates by scope -----------------------------------------------------------

    /// The last item is the one measurement a partial can be trusted on, so a test map never
    /// narrows it: the decision is `Full` whatever the map and the executor say.
    #[test]
    fn the_last_item_keeps_the_full_gate_even_with_a_test_map() {
        assert_eq!(item_gate(true, true, true, false), ItemGate::Full);
        assert_eq!(item_gate(true, true, false, false), ItemGate::Full);
        assert_eq!(item_gate(true, false, true, false), ItemGate::Full);
        assert_eq!(item_gate(true, false, false, false), ItemGate::Full);
    }

    /// With the scoped-final switch (and the post-merge gate behind it) the last item takes the
    /// same decision as an intermediate one: scope only with a trusted map and an executor.
    #[test]
    fn the_last_item_gates_by_scope_only_with_scoped_final() {
        assert_eq!(item_gate(true, true, true, true), ItemGate::Scope);
        assert_eq!(item_gate(true, false, true, true), ItemGate::Full);
        assert_eq!(item_gate(true, true, false, true), ItemGate::Full);
        assert_eq!(item_gate(true, false, false, true), ItemGate::Full);
        // Off: the same inputs give the full gate.
        assert_eq!(item_gate(true, true, true, false), ItemGate::Full);
    }

    #[test]
    fn an_intermediate_item_with_a_trusted_map_gates_by_scope() {
        assert_eq!(item_gate(false, true, true, false), ItemGate::Scope);
    }

    /// Never measure less than today without a map the work being measured cannot have rewritten,
    /// and a running executor to measure it with.
    #[test]
    fn an_intermediate_item_falls_back_to_the_full_gate_without_map_or_executor() {
        assert_eq!(item_gate(false, false, true, false), ItemGate::Full);
        assert_eq!(item_gate(false, true, false, false), ItemGate::Full);
        assert_eq!(item_gate(false, false, false, false), ItemGate::Full);
    }

    #[test]
    fn a_job_tree_map_that_differs_from_the_project_roots_is_not_trusted() {
        let map = "version: 1
tests:
  groups:
    core:
      paths: [core/]
      check: git --version
      command: git --version
";
        let write = |dir: &std::path::Path, text: &str| {
            std::fs::write(dir.join(crate::tests_map::MAP_FILE), text).unwrap();
        };

        // Byte-identical and valid: trusted.
        let root = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        write(root.path(), map);
        write(tree.path(), map);
        assert!(map_is_trusted(tree.path(), root.path()));

        // Still a valid map, but not the project's: the work may have narrowed it.
        write(tree.path(), &map.replace("core/", "core/only/"));
        assert!(!map_is_trusted(tree.path(), root.path()));

        // Absent from the job tree.
        let bare = tempfile::tempdir().unwrap();
        assert!(!map_is_trusted(bare.path(), root.path()));

        // Invalid, even when both sides agree on it.
        let broken_root = tempfile::tempdir().unwrap();
        let broken_tree = tempfile::tempdir().unwrap();
        write(broken_root.path(), "version: [");
        write(broken_tree.path(), "version: [");
        assert!(!map_is_trusted(broken_tree.path(), broken_root.path()));
    }

    #[test]
    fn a_map_whose_command_script_differs_from_the_project_roots_is_not_trusted() {
        let map = "version: 1
tests:
  groups:
    core:
      paths: [core/]
      check: git --version
      command: sh scripts/gates.sh core
";
        let root = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        for (dir, script) in [
            (
                root.path(),
                "echo a
",
            ),
            (
                tree.path(),
                "echo a
",
            ),
        ] {
            std::fs::create_dir_all(dir.join("scripts")).unwrap();
            std::fs::write(dir.join(crate::tests_map::MAP_FILE), map).unwrap();
            std::fs::write(dir.join("scripts/gates.sh"), script).unwrap();
        }
        // The map and its script agree with the project's: trusted.
        assert!(map_is_trusted(tree.path(), root.path()));

        // Same map, but the script it runs was rewritten.
        std::fs::write(
            tree.path().join("scripts/gates.sh"),
            "echo rewritten
",
        )
        .unwrap();
        assert!(!map_is_trusted(tree.path(), root.path()));
    }

    #[test]
    fn an_implement_node_is_told_to_verify_its_own_files_through_the_daemon() {
        let prompt = implement_prompt("x", 0, 2, "/wt/.nucleos", &[], None, None);
        assert!(prompt.contains("verify_status"), "{prompt}");
        assert!(prompt.contains("scope own"), "{prompt}");
        assert!(
            prompt.contains("files set to the paths you changed"),
            "{prompt}"
        );
        assert!(prompt.contains("tell each one to call verify"), "{prompt}");
        assert!(prompt.contains("kind test"), "{prompt}");
        // What the older prompt tests fix stays true.
        assert!(
            prompt.contains("do not run the whole suite yourself"),
            "{prompt}"
        );
        assert!(prompt.contains("taken for a hang"), "{prompt}");
        assert!(prompt.contains("the job commits for you"), "{prompt}");
        // The old claim that the whole suite runs the moment the node stops is no longer true of
        // an intermediate item.
        assert!(!prompt.contains("runs the whole test suite"), "{prompt}");
    }

    // ---- surviving a restart -------------------------------------------------------------------

    /// Decision 11. The discriminator cannot be liveness: by the time this runs, the startup pass
    /// over `runs` has marked every run `interrupted`, so "has no live node" is true of every job
    /// — including the ones that died in the gap between two nodes, which are the recoverable ones.
    #[tokio::test]
    async fn a_crash_keeps_the_job_only_while_its_repository_has_not_moved() {
        let pool = test_pool().await;
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();

        // No git repository at that path at all, so HEAD cannot be read: not the same as knowing it
        // stayed put, and the safe direction is to stop. What the job finished is on its branch.
        let unreadable = seed_job_in(&pool, "project-a", "implementing", &root, Some("abc123"))
            .await
            .unwrap();
        assert_eq!(reconcile_orphaned_jobs(&pool).await.unwrap(), 1);
        assert_eq!(job_status(&pool, unreadable).await, STATUS_INTERRUPTED);
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_interrupted".to_owned())
        );

        // A job that never recorded a HEAD cannot prove anything either.
        let unrecorded = seed_job_in(&pool, "project-b", "implementing", &root, None)
            .await
            .unwrap();
        reconcile_orphaned_jobs(&pool).await.unwrap();
        assert_eq!(job_status(&pool, unrecorded).await, STATUS_INTERRUPTED);
    }

    #[tokio::test]
    async fn a_finished_job_is_not_reconsidered_after_a_restart() {
        let pool = test_pool().await;
        let done = seed_job(&pool, "project-a", "completed").await.unwrap();

        assert_eq!(reconcile_orphaned_jobs(&pool).await.unwrap(), 0);
        assert_eq!(job_status(&pool, done).await, "completed");
    }

    // ---- what each node is told ----------------------------------------------------------------

    /// The plan node's whole contract, and the one place an empty queue has to be named as a
    /// legitimate answer — otherwise a model asked to plan will find something to plan.
    #[test]
    fn the_plan_node_is_told_the_file_is_the_only_thing_read() {
        let prompt = plan_prompt("advance the backlog", 5, "/wt/.nucleos", None, false);
        assert!(prompt.contains("advance the backlog"));
        assert!(prompt.contains("/wt/.nucleos/plan.json"));
        assert!(prompt.contains("at most 5"));
        assert!(prompt.contains(r#"{"items": []}"#));
    }

    /// The hint is asked for, and asked for as optional. A planner told to name files without being
    /// told it may decline will name some for every item, and a confident wrong list is worse than
    /// no list: the implement node is told to start there, so an invented path spends a whole
    /// context window in the wrong place.
    #[test]
    fn plan_prompt_asks_for_optional_file_hints() {
        let prompt = plan_prompt("advance the backlog", 5, "/wt/.nucleos", None, false);

        assert!(
            prompt.contains(r#""files""#),
            "the shape must name the field: {prompt}"
        );
        let lowered = prompt.to_lowercase();
        assert!(
            lowered.contains("optional") || lowered.contains("if you"),
            "naming files must be offered, not required: {prompt}"
        );
        assert!(
            lowered.contains("best effort")
                || lowered.contains("best-effort")
                || lowered.contains("guess"),
            "the planner must be told a partial answer is acceptable: {prompt}"
        );
    }

    /// Two halves of one contract. With a hint the node is told where to start *and* told the list
    /// is not a boundary — an implement node that treats a planner's guess as the full extent of the
    /// change leaves the tree half-edited. With no hint the prompt is byte-for-byte what it has
    /// always been, so a job planned before this existed is not silently given a different brief.
    #[test]
    fn implement_prompt_names_the_hinted_files_as_a_possibly_incomplete_list() {
        let hints = [
            "core/src/job.rs".to_owned(),
            "core/migrations/0052.sql".to_owned(),
        ];
        let hinted = implement_prompt("write the thing", 0, 3, "/wt/.nucleos", &hints, None, None);

        assert!(hinted.contains("core/src/job.rs"));
        assert!(hinted.contains("core/migrations/0052.sql"));
        assert!(
            hinted.to_lowercase().contains("incomplete"),
            "the list is a hint, not a boundary: {hinted}"
        );

        let bare = implement_prompt("write the thing", 0, 3, "/wt/.nucleos", &[], None, None);
        assert_eq!(
            bare,
            "You are item 1 of 3 in an autonomous job. The working tree already holds the work \
             of the earlier items; this is the only one you do.\n\n\
             write the thing\n\n\
             The full queue is in /wt/.nucleos/plan.json for context. Do not start another item \
             and do not edit that file. Your work is verified by the daemon after you finish, so \
             leave the tree building, and do not run the whole suite yourself. While you work, \
             check your change with the verify tool: kind test (or check for a quick compile), \
             scope own, and files set to the paths you changed. It runs only the tests those \
             files select, and answers within about 45 seconds with the result or a ticket and \
             its progress. While a ticket is not done, call verify_status with it instead of \
             starting the same check again. If the project has no test map, own runs nothing and \
             says so; then run the narrowest test that tells you your change works. If you hand \
             part of this item to subagents, tell each one to call verify with scope own and the \
             files it changed. A command of yours that prints nothing for long is taken for a \
             hang, and it ends this item. Leave it UNCOMMITTED: the job commits for you once the \
             gate agrees, and \
             committing by hand stops this item to ask permission for something already arranged."
        );
        // Said twice on purpose: the equality above is the guarantee, and this says what it is a
        // guarantee *of* — an unhinted node is never told about a hint mechanism it has no hint for,
        // and never invited to wonder which files were meant.
        let lowered = bare.to_lowercase();
        assert!(
            !lowered.contains("hint"),
            "no hint, no hint paragraph: {bare}"
        );
        assert!(
            !lowered.contains("incomplete"),
            "nothing to be incomplete: {bare}"
        );
        assert!(
            !lowered.contains("start with"),
            "no files to start with: {bare}"
        );

        // The paragraph is appended, so everything the node was told before it is still there.
        assert!(hinted.starts_with(&bare));
    }

    /// A retry is only worth a run if the node is told what the gate said.
    ///
    /// §5.4 keeps nodes from resuming each other's sessions on purpose, so the second implement node
    /// knows nothing about the first — including that there WAS a first. Without the gate's output
    /// travelling in the prompt, a retry is a second independent guess at the same item, which is a
    /// whole run spent to rediscover what the gate already printed. That is the difference between
    /// a retry and repeating yourself.
    ///
    /// The other half is the `None` call, and it is the half that protects every job that is not
    /// retrying: an item on its first attempt is given the brief it has always been given, with
    /// nothing in it about a previous gate it never had. The byte-for-byte guarantee lives in
    /// `implement_prompt_names_the_hinted_files_as_a_possibly_incomplete_list` above, which pins the
    /// unhinted prompt as a literal; here the claim is that the output is APPENDED, so everything
    /// the node was told before it is still there and in the same place.
    #[test]
    fn the_retry_prompt_carries_the_gate_output() {
        let first = implement_prompt("write the thing", 0, 3, "/wt/.nucleos", &[], None, None);
        let retry = implement_prompt(
            "write the thing",
            0,
            3,
            "/wt/.nucleos",
            &[],
            Some("FAILED test_x"),
            None,
        );

        assert!(
            retry.contains("FAILED test_x"),
            "the retrying node was not told what failed: {retry}"
        );
        assert!(
            !first.contains("FAILED test_x"),
            "a first attempt must carry no account of a gate it never faced: {first}"
        );
        assert!(
            retry.starts_with(&first),
            "the gate's output is appended to the brief, not substituted for part of it: {retry}"
        );
    }

    /// The bound that keeps a megabyte of test runner out of the database and out of the next
    /// prompt, and the boundary walk that keeps that bound from being a crash.
    ///
    /// Two claims, and the second is the one with teeth. `gate.rs` caps what it captures at 1 MiB,
    /// which is the right bound against a runaway suite and far too much to paste into a retry — so
    /// `record_gate` stores the TAIL and nothing else. The tail and not the head: a runner prints
    /// its failures and its summary line last, and that is the part a retry can act on.
    ///
    /// The second claim is that the cut lands where a character ends. Slicing a `str` by a raw byte
    /// index panics, `record_gate` runs inside the daemon, and a gate prints whatever the project's
    /// tools print — an accented test name, a `✗`, a path off a non-ASCII branch. Nothing asserted
    /// this until now, which is worse than it sounds: the guard and its absence look identical from
    /// outside, because both pass every ASCII gate anybody has ever run. The failure was reserved
    /// for the first non-ASCII one.
    #[test]
    fn the_stored_gate_output_is_a_tail_that_never_cuts_a_character_in_half() {
        let short = "FAILED test_x\ntest result: FAILED. 1 failed";
        assert_eq!(
            gate_output_tail(short),
            short,
            "an output already inside the bound must arrive whole"
        );

        let long = format!("{}test result: FAILED. 3 failed", "noise\n".repeat(20_000));
        let kept = gate_output_tail(&long);
        assert!(
            kept.len() <= GATE_OUTPUT_TAIL,
            "kept {} bytes, over the {GATE_OUTPUT_TAIL}-byte bound",
            kept.len()
        );
        assert!(
            kept.ends_with("test result: FAILED. 3 failed"),
            "the summary line is the part a retry acts on, and it did not survive: {kept}"
        );

        // `€` is three bytes and 4096 is not a multiple of three, so the naive cut lands INSIDE a
        // character here. Chosen rather than stumbled upon: a two-byte character divides 4096
        // evenly, so an implementation with no walk at all would pass a test written with `é`.
        let accented = "€".repeat(2_000);
        let walked = gate_output_tail(&accented);
        assert!(
            accented.ends_with(walked),
            "the walk moved the cut past the end of the output"
        );
        assert!(
            GATE_OUTPUT_TAIL - walked.len() < 4,
            "the walk skipped {} bytes looking for a boundary, which is more than one character",
            GATE_OUTPUT_TAIL - walked.len()
        );
    }

    /// §5.4: the review node's independence is structural, not requested. It is never given a
    /// builder's session because no builder session is kept for it to resume.
    #[test]
    fn the_review_node_is_given_a_diff_and_no_reasoning() {
        let with_base = review_prompt("t", Some("deadbeef"), "/wt/.nucleos", false, false);
        assert!(with_base.contains("git diff deadbeef..HEAD"));

        // No recorded base: git would not answer when the job started. Asking for the branch's own
        // commits is worse than naming a sha and better than reviewing a guess.
        let without = review_prompt("t", None, "/wt/.nucleos", false, false);
        assert!(without.contains("git log --oneline"));
    }

    /// The review is given the task as it was asked, and the spec when there is one, and is told to
    /// judge the diff against them and not only against the queue.
    ///
    /// Job 27, 2026-09-14: the task said a change that merely widens a test's timing margin until
    /// it stops failing "loses that property and is not a fix", the spec restated it under
    /// `Not this`, and the plan told the implementer to widen the bound anyway. The review was
    /// handed the diff and the queue and nothing else, so it checked one against the other and
    /// found them agreeing. A flaw that comes from the queue cannot be caught by a reader whose
    /// only yardstick is the queue.
    #[test]
    fn the_review_judges_the_diff_against_the_task_and_not_only_the_queue() {
        let task = "a change that merely widens the margin until the test stops failing loses that \
                    property and is not a fix";
        let with = review_prompt(task, Some("deadbeef"), "/wt/.nucleos", true, false);

        assert!(
            with.contains(task),
            "the review has to see what was actually asked, word for word: {with}"
        );
        assert!(
            with.contains("/wt/.nucleos/spec.md"),
            "the review was not sent to the brief that exists: {with}"
        );
        for section in ["Done means", "Not this"] {
            assert!(
                with.contains(section),
                "the review has to be pointed at `{section}`, the part a diff can break while \
                 satisfying every item of the queue: {with}"
            );
        }
        assert!(
            with.contains("judged against the task"),
            "the review has to judge against the task, not only against the queue: {with}"
        );
        assert!(
            with.contains("the fault is the queue's"),
            "a flaw that comes from the plan has to be reportable as the plan's: {with}"
        );

        let without = review_prompt(task, Some("deadbeef"), "/wt/.nucleos", false, false);
        assert!(without.contains(task));
        assert!(without.contains("judged against the task"));
        assert!(
            !without.contains("spec.md"),
            "the review was sent to a brief that was never written: {without}"
        );

        // And the rules the node already had are all still there.
        assert!(with.contains("git diff deadbeef..HEAD"));
        assert!(with.contains("/wt/.nucleos/plan.json"));
        assert!(with.contains("Change no files"));
        assert!(with.contains("Read, Grep and Glob"));
    }

    /// The wiring the prompt test cannot see: the node that is started is handed the job's own
    /// prompt, and is sent to the brief because the spec node left one on disk.
    #[tokio::test]
    async fn the_review_node_is_handed_the_jobs_task_and_the_brief_on_disk() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = job_ready_to_review(&pool, worktree.path()).await;
        std::fs::write(
            worktree
                .path()
                .join(crate::execution::worktree::ARTIFACTS_DIR)
                .join(SPEC_FILE),
            "## Done means\n\n## Not this\n",
        )
        .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        let prompt: String =
            sqlx::query_scalar("SELECT prompt FROM runs WHERE job_id = ? AND stage = 'review'")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            prompt.contains("pull from the todo list and advance what you can"),
            "the review node was not handed the task it is to judge against: {prompt}"
        );
        assert!(
            prompt.contains(&format!("{}/{SPEC_FILE}", artifacts_for(worktree.path()))),
            "the review node was not sent to the brief the spec node wrote: {prompt}"
        );
    }

    // -- The council in front of the review node ---------------------------------------------

    /// A roster that would advise, with the consumers the caller asks for.
    ///
    /// Cloud seats only: a local seat would be refused by `resolve_seat` against `NoAssistants`,
    /// and these tests are about the job's side of the seam rather than about who may sit.
    fn advising_roster(job_review: bool) -> crate::config::CouncilConfig {
        crate::config::CouncilConfig {
            timeout_seconds: 1,
            rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
            consumers: crate::config::CouncilConsumers {
                job_review,
                proposal_advice: false,
            },
            chairman: crate::config::SeatSpec {
                kind: Some(crate::config::SeatKind::Cloud),
                model_ref: Some("the-chairman".to_string()),
                agent: None,
            },
            members: vec![crate::config::SeatSpec {
                kind: Some(crate::config::SeatKind::Cloud),
                model_ref: Some("a-member".to_string()),
                agent: None,
            }],
        }
    }

    /// The same state, with a council runtime swapped in. `token` is what decides whether
    /// `council::start` can get past its second line -- see `a_council_that_will_not_start...`.
    async fn state_with_council(
        pool: sqlx::SqlitePool,
        config: Option<crate::config::CouncilConfig>,
        token: Option<&str>,
    ) -> AppState {
        let state = test_state(pool).await;
        AppState {
            council: std::sync::Arc::new(crate::council::CouncilRuntime::new(
                config,
                token.map(str::to_string),
            )),
            ..state
        }
    }

    /// A council row written by hand, so a job's side of the seam can be walked without spending a
    /// deliberation. A synthesis is put where the real thing puts it: the transcript of the run in
    /// `chairman_run_id`, which is not a column and must not be treated as one.
    async fn seed_council(pool: &sqlx::SqlitePool, id: &str, status: &str) {
        sqlx::query(
            "INSERT INTO council_runs
               (id, created_at, question, status, stage, anon_seed, chairman_kind, chairman_ref)
             VALUES (?, ?, 'what should the reviewer look at', ?, 3, ?, 'cloud', 'the-chairman')",
        )
        .bind(id)
        .bind(Utc::now().to_rfc3339())
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .expect("insert a council");
    }

    /// Settles a seeded council with the text its chairman wrote.
    async fn settle_council(pool: &sqlx::SqlitePool, id: &str, synthesis: &str) {
        let run_id: i64 = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at, stdout)
             VALUES ('project-a', 'synthesize', 'completed', 'assistant', ?, ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(synthesis)
        .execute(pool)
        .await
        .expect("insert the chairman's run")
        .last_insert_rowid();
        sqlx::query("UPDATE council_runs SET status = ?, chairman_run_id = ? WHERE id = ?")
            .bind(crate::council::STATUS_DONE)
            .bind(run_id)
            .bind(id)
            .execute(pool)
            .await
            .expect("settle the council");
    }

    /// A job with everything done but its review, and a directory to hand work through.
    async fn job_ready_to_review(pool: &sqlx::SqlitePool, worktree: &std::path::Path) -> i64 {
        let job_id = seed_job(pool, "project-a", "reviewing").await.unwrap();
        sqlx::query("UPDATE jobs SET project_root = ? WHERE id = ?")
            .bind(worktree.to_string_lossy().into_owned())
            .bind(job_id)
            .execute(pool)
            .await
            .unwrap();
        seed_worktree(pool, job_id, worktree).await;
        seed_items(pool, job_id, &["passed", "passed"]).await;
        job_id
    }

    async fn stages_run(pool: &sqlx::SqlitePool, job_id: i64) -> Vec<String> {
        sqlx::query_scalar("SELECT stage FROM runs WHERE job_id = ? ORDER BY id")
            .bind(job_id)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn review_council_of(pool: &sqlx::SqlitePool, job_id: i64) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>("SELECT review_council_id FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn councils_held(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM council_runs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn council_file_in(worktree: &std::path::Path) -> std::path::PathBuf {
        worktree
            .join(crate::execution::worktree::ARTIFACTS_DIR)
            .join(COUNCIL_FILE)
    }

    /// **The non-regression test, and the one that matters most in this file.** Both consumers ship
    /// off, and off has to mean the pipeline that ran yesterday: the review node starts on the same
    /// pass, with a prompt that names no council, and nothing is convened or written down.
    #[tokio::test]
    async fn a_job_with_council_review_off_walks_exactly_as_it_did() {
        let pool = test_pool().await;
        // The shipped state: no roster at all, which is what `CouncilRuntime::default()` is.
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = job_ready_to_review(&pool, worktree.path()).await;

        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnReview
        );
        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(stages_run(&pool, job_id).await, vec!["review"]);
        assert_eq!(councils_held(&pool).await, 0);
        assert_eq!(review_council_of(&pool, job_id).await, None);
        assert_ne!(job_status(&pool, job_id).await, "waiting");

        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE job_id = ?")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            !prompt.contains(COUNCIL_FILE),
            "a review with no council must not be sent to a file that is not there: {prompt}"
        );
    }

    /// A configured council that refuses to start is a council that never happened, and the job
    /// walks on. Deterministic because the refusal is `Unavailable` -- a runtime holding a roster
    /// and no daemon key, which `council::start` turns down before any row exists.
    #[tokio::test]
    async fn a_council_that_will_not_start_does_not_hold_the_job() {
        let pool = test_pool().await;
        let state = state_with_council(pool.clone(), Some(advising_roster(true)), None).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = job_ready_to_review(&pool, worktree.path()).await;

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(stages_run(&pool, job_id).await, vec!["review"]);
        assert_eq!(councils_held(&pool).await, 0);
        assert_eq!(review_council_of(&pool, job_id).await, None);
        assert_ne!(
            job_status(&pool, job_id).await,
            "waiting",
            "a council that could not convene must never be able to park a job"
        );
    }

    /// The whole of the parked path: the job waits while the council deliberates, and the tick
    /// after it settles writes the synthesis beside the queue and starts the node.
    ///
    /// `waiting` and not a status of its own, deliberately -- see `LIVE_STATUSES`, which is
    /// duplicated in `LIVE_JOBS_SQL`, in a migration's index and in `concurrency.rs`. A status that
    /// falls out of that list stops being ticked while still holding a concurrency slot.
    #[tokio::test]
    async fn a_council_review_parks_the_job_and_unparks_it_when_the_council_settles() {
        let pool = test_pool().await;
        let state =
            state_with_council(pool.clone(), Some(advising_roster(true)), Some("key")).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = job_ready_to_review(&pool, worktree.path()).await;

        // A council already convened for this job and still thinking. Seeded rather than spent:
        // this test is about what the JOB does with a council, not about the council.
        seed_council(&pool, "council-1", crate::council::STATUS_RUNNING).await;
        sqlx::query("UPDATE jobs SET review_council_id = 'council-1' WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;
        assert_eq!(job_status(&pool, job_id).await, "waiting");
        let reason: Option<String> =
            sqlx::query_scalar("SELECT wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some(WAIT_COUNCIL));
        assert!(
            stages_run(&pool, job_id).await.is_empty(),
            "the review must not start while the council it is going to read is still writing"
        );

        // The council settles. `drive` is what a tick actually calls, and it unparks before it
        // re-reads anything -- so the whole pass is driven rather than `advance` alone.
        settle_council(&pool, "council-1", "watch the cursor").await;
        let job = load_job(&pool, job_id).await.unwrap();
        drive(&state, job, Utc::now()).await;

        let written = std::fs::read_to_string(council_file_in(worktree.path()))
            .expect("the synthesis lands where `review_prompt` sends the node");
        assert_eq!(written, "watch the cursor");
        assert_eq!(stages_run(&pool, job_id).await, vec!["review"]);
        let prompt: String =
            sqlx::query_scalar("SELECT prompt FROM runs WHERE job_id = ? AND stage = 'review'")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(prompt.contains(COUNCIL_FILE));

        // And the latch held: the column is not cleared when the council settles, precisely so a
        // pass that comes back here cannot convene a second deliberation.
        assert_eq!(
            review_council_of(&pool, job_id).await.as_deref(),
            Some("council-1")
        );
        assert_eq!(councils_held(&pool).await, 1);
    }

    /// Four ways a council ends with nothing to say -- errored, cancelled, gone from the record
    /// entirely, and settled with no transcript to read -- and none of them may hold the job. The
    /// review runs unadvised, which is exactly what a job with no council configured does.
    #[tokio::test]
    async fn a_council_that_failed_does_not_hold_the_job() {
        for (label, status) in [
            ("errored", Some(crate::council::STATUS_ERROR)),
            ("cancelled", Some(crate::council::STATUS_CANCELLED)),
            ("pruned", None),
            ("done with no synthesis", Some(crate::council::STATUS_DONE)),
        ] {
            let pool = test_pool().await;
            let state =
                state_with_council(pool.clone(), Some(advising_roster(true)), Some("key")).await;
            let worktree = tempfile::tempdir().unwrap();
            let job_id = job_ready_to_review(&pool, worktree.path()).await;
            if let Some(status) = status {
                seed_council(&pool, "council-1", status).await;
            }
            sqlx::query("UPDATE jobs SET review_council_id = 'council-1' WHERE id = ?")
                .bind(job_id)
                .execute(&pool)
                .await
                .unwrap();

            let job = load_job(&pool, job_id).await.unwrap();
            advance(&state, &job, Utc::now()).await;

            assert_eq!(
                stages_run(&pool, job_id).await,
                vec!["review"],
                "a council that {label} must not stop the review from happening"
            );
            assert_ne!(job_status(&pool, job_id).await, "waiting", "{label}");
            assert!(
                !council_file_in(worktree.path()).exists(),
                "a council that {label} writes no advice, and the node must not be told there is any"
            );
        }
    }

    /// A prompt naming a file that is not there is an instruction that fails by omission: the node
    /// reads nothing, gets an error nobody warned it about, and either hunts for the file or
    /// decides the whole paragraph was wrong. The consumer is off by default, so absent is the
    /// COMMON case here rather than the edge one.
    #[test]
    fn the_review_prompt_names_the_council_file_only_when_it_exists() {
        let without = review_prompt("t", Some("deadbeef"), "/wt/.nucleos", false, false);
        assert!(!without.contains(COUNCIL_FILE));

        let with = review_prompt("t", Some("deadbeef"), "/wt/.nucleos", false, true);
        assert!(with.contains(&format!("/wt/.nucleos/{COUNCIL_FILE}")));
        // Named as advice, in the same paragraph. The gate holds ship/no-ship and the panel never
        // saw this branch, so a node told to read a synthesis must not read it as a verdict.
        assert!(with.contains("ADVICE"));
        assert!(
            with.contains("the diff is what is real"),
            "the node is told which of the two to believe when they disagree: {with}"
        );
        // And nothing else about the prompt moved.
        assert!(with.contains("git diff deadbeef..HEAD"));
        assert!(with.contains("/wt/.nucleos/plan.json"));
        assert!(with.contains("Read, Grep and Glob"));
    }

    /// The words an owner would leave on a job in flight, and the item they arrive beside.
    ///
    /// Distinctive strings on purpose. `seed_items` writes `'an item'` for every description, which
    /// appears inside `implement_prompt`'s own boilerplate often enough that asserting on it would
    /// pass whether or not the item's brief survived.
    const A_NOTE: &str = "when you get to item 3, update the docs too";
    const AN_ITEM: &str = "rename the cursor helper";

    /// Drives the production briefing seam through run creation and the item-keyed trace write.
    #[tokio::test(flavor = "current_thread")]
    async fn a_node_leaves_the_trace_of_its_briefing_once_its_run_exists() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-brief-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        seed_items(&pool, job_id, &["pending"]).await;
        sqlx::query("UPDATE job_items SET description = ? WHERE job_id = ? AND ordinal = 0")
            .bind(AN_ITEM)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_active_knowledge(&pool, "the cursor helper lives in ui").await;

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        let (item_id, run_id): (i64, Option<i64>) =
            sqlx::query_as("SELECT id, run_id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let run_id = run_id.expect("the item's node started, so its briefing has a run to name");
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(prompt.contains("the cursor helper lives in ui"));

        let trace: Vec<(bool, Option<i64>, f64, Option<String>)> = sqlx::query_as(
            "SELECT shown, item_id, s_fts, credited_at
               FROM run_knowledge
              WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(trace.len(), 1);
        assert!(trace[0].0);
        assert_eq!(trace[0].1, Some(item_id));
        assert!(trace[0].2 > 0.0);
        assert_eq!(trace[0].3, None);

        for _ in 0..250 {
            if runner.last_prompt.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let launched = runner
            .last_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("the node's CLI launch should receive its prompt");
        assert_eq!(
            launched
                .matches("Earlier work on this project left the notes below")
                .count(),
            1,
            "a job node is briefed by spawn_node and must not be briefed again by create_run_with",
        );
    }

    /// The one seam a job's words have to cross, driven end to end.
    ///
    /// Every other test of this feature stops inside `notes`, which is pure or nearly so and which
    /// will happily store and return a note nobody ever reads. The wiring is the part that can be
    /// missing while all of that passes: `advance` selects the item, builds the brief and hands it
    /// to `spawn_node`, and unless the pending notes are appended THERE the owner's sentence is a
    /// row in a table with no reader. A node is born with a clean context window and no steering
    /// channel — `create_job_node_run` passes `steerable: false` on purpose — so the prompt is the
    /// only door, and this test opens it from the outside.
    ///
    /// It asserts on the STORED prompt, read back out of `runs`, and not on a string the test built.
    /// The chain is long — note left, queue read, text rendered, brief appended, run created, row
    /// written — and a test that composed those pieces itself would be checking its own arithmetic
    /// while any one of the links could be missing.
    ///
    /// The item's own description is asserted for beside it, because "the note reached the prompt"
    /// is satisfied by a `render` that returned the note INSTEAD of the brief. A node whose item
    /// vanished would go and do what the note said and nothing else, which is the failure the whole
    /// wording of `render` is written to avoid.
    ///
    /// And the queue is asserted empty afterwards. Delivery is what makes a note a message rather
    /// than a standing order: unconsumed, one sentence typed at midnight would be appended to item
    /// 4, item 5, the replan and the review, each of them reading it as something newly said about
    /// the work in front of it.
    ///
    /// A real repository and a real worktree, for the reason
    /// `a_retriable_item_is_claimed_and_started_not_merely_asked_for` gives: a node that could not
    /// be provisioned leaves the item exactly where a node that was never told about the note would,
    /// and the two would be indistinguishable here.
    #[tokio::test(flavor = "current_thread")]
    async fn a_note_left_on_a_job_reaches_the_next_nodes_prompt() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-note-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;
        let state = with_home(state, &_container);

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::execution::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        seed_items(&pool, job_id, &["pending"]).await;
        sqlx::query("UPDATE job_items SET description = ? WHERE job_id = ? AND ordinal = 0")
            .bind(AN_ITEM)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let note_id = crate::notes::leave(&pool, job_id, A_NOTE, "helena")
            .await
            .expect("the owner leaves a note on a job already running");

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        let run_id: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let run_id = run_id.expect("the item's node started, so there is a prompt to look at");
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert!(
            prompt.contains(A_NOTE),
            "the note never reached the node it was left for: {prompt}"
        );
        assert!(
            prompt.contains(AN_ITEM),
            "the note took the place of the item's brief instead of being added to it: {prompt}"
        );
        assert_eq!(
            crate::notes::pending(&pool, job_id)
                .await
                .unwrap()
                .iter()
                .map(|note| note.id)
                .collect::<Vec<_>>(),
            Vec::<i64>::new(),
            "note {note_id} is still queued after being read out, so every later node of this job \
             gets told the same thing again"
        );
    }

    /// A node that never started was told nothing, and the note has to still be waiting.
    ///
    /// `spawn_node` has an exit before the run exists, and it is not an exotic one: the item claim
    /// is a compare-and-swap that stops two passes starting two nodes on one tree, and losing it is
    /// the ordinary outcome of a second pass arriving while the first is still working. The `Busy`
    /// and provisioning-failure arms below it end the same way — no run, no prompt, nobody told.
    ///
    /// So the order inside `spawn_node` is the behaviour: read the queue, render, append, create the
    /// run, and only THEN mark the notes delivered. A `mark_delivered` at the top reads as harmless
    /// — the words were rendered, after all — and loses the owner's sentence in exactly the case
    /// nothing reports. The run is never created, the prompt is thrown away, the note is gone from
    /// the queue, and the owner learns about it by watching the job finish without doing what they
    /// asked.
    ///
    /// `spawn_node` is called directly rather than through `advance`, because `advance` derives the
    /// status it claims against from the view it just loaded and therefore cannot lose the swap
    /// inside one pass. Driving the seam itself is the only way to stand in the moment this is about.
    /// The run count is asserted as well as the note, because "no node started" is this test's
    /// premise and a premise the test assumed rather than checked would make the rest of it vacuous.
    #[tokio::test]
    async fn a_note_is_not_consumed_by_a_node_that_failed_to_start() {
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        // The race the compare-and-swap exists to lose: another pass claimed this item first, so its
        // row already says `running` and the claim below — which expects `pending`, the status the
        // caller last saw — matches nothing.
        seed_items(&pool, job_id, &["running"]).await;

        let note_id = crate::notes::leave(&pool, job_id, A_NOTE, "helena")
            .await
            .expect("the owner leaves a note on a job already running");

        let job = load_job(&pool, job_id).await.unwrap();
        let step = spawn_node(
            &state,
            &job,
            "implement",
            "the brief this node would have been given".to_owned(),
            Some(ItemClaim {
                ordinal: 0,
                held: "pending",
            }),
            (PathBuf::from("/project/a/worktree"), "nucleos/job".into()),
            NoRoom::Park,
        )
        .await;

        assert_eq!(
            step,
            Step::Stopped,
            "an item that could not be claimed has to end the pass"
        );
        let started: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE job_id = ?")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            started, 0,
            "the premise of this test is that no node started, and one did"
        );

        assert_eq!(
            crate::notes::pending(&pool, job_id)
                .await
                .unwrap()
                .iter()
                .map(|note| note.id)
                .collect::<Vec<_>>(),
            vec![note_id],
            "the note was spent on a node that never read it, and nothing will say so"
        );
    }

    // ---- which gate an item takes, driven through `gate_item_with` -----------------------------

    /// A job whose project root and worktree are one real repository carrying a test map, cut at
    /// its first commit and standing on its second, with a running verify executor on the same pool.
    struct ScopeJob {
        pool: sqlx::SqlitePool,
        state: AppState,
        job: JobRow,
        executor: std::sync::Arc<crate::verify::exec::Executor>,
        _repo: tempfile::TempDir,
        _machine: tempfile::TempDir,
    }

    /// `rostered` is whether the repository is a known project's root; without it `submit` refuses
    /// the scope request (`project_for_worktree` finds no project) before anything is queued.
    async fn scope_job(rostered: bool) -> ScopeJob {
        scope_job_with(rostered, "gate_command: git --version\n").await
    }

    /// `scope_job` with the machine's `autopilot.yaml` text chosen by the test.
    async fn scope_job_with(rostered: bool, rules: &str) -> ScopeJob {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::create_dir_all(root.join("core")).unwrap();
        std::fs::write(root.join("core/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(
            root.join(crate::tests_map::MAP_FILE),
            "version: 1
tests:
  groups:
    core:
      paths: [core/]
      check: git --version
      command: git --version
",
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@x"],
            vec!["config", "user.name", "test"],
            vec!["add", "-A"],
            vec!["commit", "-q", "-m", "one"],
        ] {
            assert!(git_ok(root, &args), "git {args:?}");
        }
        let base = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        std::fs::write(root.join("core/a.rs"), "fn a() { let _ = 1; }\n").unwrap();
        assert!(git_ok(root, &["commit", "-q", "-am", "two"]));

        let machine = tempfile::tempdir().unwrap();
        crate::project::state::write_for_test(
            machine.path(),
            "project-a",
            crate::project::state::AUTOPILOT_FILE,
            rules,
        );
        let pool = test_pool().await;
        let state = AppState {
            machine_config_root: Some(machine.path().to_path_buf()),
            ..test_state(pool.clone()).await
        };
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let root_text = root.to_string_lossy().into_owned();
        sqlx::query("UPDATE jobs SET project_root = ? WHERE id = ?")
            .bind(&root_text)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        crate::execution::worktree::record(
            &pool,
            crate::execution::worktree::Owner::Job(job_id),
            "project-a",
            &root_text,
            &root_text,
            "main",
            Some(&base),
        )
        .await
        .unwrap();
        if rostered {
            sqlx::query(
                "INSERT INTO autopilot_state (project_id, mode, project_root) \
                 VALUES ('project-a', 'active', ?)",
            )
            .bind(&root_text)
            .execute(&pool)
            .await
            .unwrap();
        }
        seed_items(&pool, job_id, &["implemented", "implemented"]).await;

        let executor = crate::verify::exec::Executor::new(
            pool.clone(),
            crate::config::VerifyConfig::default(),
            Some(machine.path().to_path_buf()),
        );
        tokio::spawn(crate::verify::exec::run_executor(executor.clone()));
        let job = load_job(&pool, job_id).await.unwrap();
        ScopeJob {
            pool,
            state,
            job,
            executor,
            _repo: repo,
            _machine: machine,
        }
    }

    async fn scope_requests(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT caller FROM verify_requests ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn full_gate_rows(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM verify_runs WHERE origin = 'job_item'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The last item is the measurement a partial is trusted on: it takes the full gate even when
    /// an executor is installed and the map is trusted, and queues nothing with the executor.
    #[tokio::test]
    async fn gate_item_sends_the_last_item_through_the_full_gate_despite_a_trusted_map() {
        let f = scope_job(true).await;

        gate_item_with(&f.state, &f.job, 1, 2, Some(f.executor.clone())).await;

        assert_eq!(scope_requests(&f.pool).await, Vec::<String>::new());
        assert_eq!(full_gate_rows(&f.pool).await, 1);
    }

    /// An intermediate item with an installed executor and a trusted map is measured by `scope`:
    /// a verify request is made in the job's name, and the full gate does not run.
    #[tokio::test]
    async fn gate_item_sends_an_intermediate_item_with_a_trusted_map_through_scope() {
        let f = scope_job(true).await;

        gate_item_with(&f.state, &f.job, 0, 2, Some(f.executor.clone())).await;

        assert_eq!(
            scope_requests(&f.pool).await,
            vec![format!("job:{}", f.job.id)]
        );
        assert_eq!(full_gate_rows(&f.pool).await, 0);
    }

    /// A scope request that cannot even be submitted measured nothing, so the item takes today's
    /// full gate instead of being recorded as errored.
    #[tokio::test]
    async fn gate_item_falls_back_to_the_full_gate_when_scope_cannot_be_submitted() {
        let f = scope_job(false).await;

        gate_item_with(&f.state, &f.job, 0, 2, Some(f.executor.clone())).await;

        assert_eq!(scope_requests(&f.pool).await, Vec::<String>::new());
        assert_eq!(full_gate_rows(&f.pool).await, 1);
        assert_eq!(item_statuses(&f.pool, f.job.id).await[0], "passed");
    }

    const SCOPED_FINAL_RULES: &str =
        "gate_command: git --version\ngate_after_land: true\nscoped_final_gate: true\n";

    async fn scoped_feed(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'job_gate_scoped' ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// With `scoped_final_gate` and `gate_after_land` both on, the last item is measured by
    /// `scope` in the job's name, the full gate does not run, and the feed says which gate ran.
    #[tokio::test]
    async fn gate_item_sends_the_last_item_through_scope_with_scoped_final_and_gate_after_land() {
        let f = scope_job_with(true, SCOPED_FINAL_RULES).await;

        gate_item_with(&f.state, &f.job, 1, 2, Some(f.executor.clone())).await;

        assert_eq!(
            scope_requests(&f.pool).await,
            vec![format!("job:{}", f.job.id)]
        );
        assert_eq!(full_gate_rows(&f.pool).await, 0);
        let said = scoped_feed(&f.pool).await;
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("verify scope"), "{}", said[0]);
    }

    /// The switch alone is not enough: without `gate_after_land` there is no net behind a partial
    /// measurement, so the last item keeps the full gate and says nothing new.
    #[tokio::test]
    async fn gate_item_keeps_the_full_gate_on_the_last_item_without_gate_after_land() {
        let f = scope_job_with(
            true,
            "gate_command: git --version\nscoped_final_gate: true\n",
        )
        .await;

        gate_item_with(&f.state, &f.job, 1, 2, Some(f.executor.clone())).await;

        assert_eq!(scope_requests(&f.pool).await, Vec::<String>::new());
        assert_eq!(full_gate_rows(&f.pool).await, 1);
        assert!(scoped_feed(&f.pool).await.is_empty());
    }

    /// The map is the project's own, but a script its group commands run was rewritten in the job
    /// tree: the last item takes the full gate, with no scope request.
    #[tokio::test]
    async fn gate_item_keeps_the_full_gate_on_the_last_item_when_a_map_script_was_rewritten() {
        let f = scope_job_with(true, SCOPED_FINAL_RULES).await;
        let root = PathBuf::from(&f.job.project_root);
        let tree = tempfile::tempdir().unwrap();
        let map = std::fs::read_to_string(root.join(crate::tests_map::MAP_FILE))
            .unwrap()
            .replace(
                "command: git --version",
                "command: sh scripts/gates.sh core",
            );
        for (dir, script) in [
            (
                root.as_path(),
                "echo configured
",
            ),
            (
                tree.path(),
                "echo rewritten
",
            ),
        ] {
            std::fs::create_dir_all(dir.join("scripts")).unwrap();
            std::fs::write(dir.join(crate::tests_map::MAP_FILE), &map).unwrap();
            std::fs::write(dir.join("scripts/gates.sh"), script).unwrap();
        }
        sqlx::query("UPDATE worktrees SET path = ? WHERE owner_kind = 'job' AND owner_id = ?")
            .bind(tree.path().to_string_lossy().into_owned())
            .bind(f.job.id)
            .execute(&f.pool)
            .await
            .unwrap();

        gate_item_with(&f.state, &f.job, 1, 2, Some(f.executor.clone())).await;

        assert_eq!(scope_requests(&f.pool).await, Vec::<String>::new());
        assert_eq!(full_gate_rows(&f.pool).await, 1);
        assert!(scoped_feed(&f.pool).await.is_empty());
    }

    /// A scope request that cannot be submitted measured nothing: the last item takes the full
    /// gate, and the feed says that the full gate is what measured it.
    #[tokio::test]
    async fn gate_item_falls_back_to_the_full_gate_on_the_last_item_when_scope_cannot_be_submitted()
    {
        let f = scope_job_with(false, SCOPED_FINAL_RULES).await;

        gate_item_with(&f.state, &f.job, 1, 2, Some(f.executor.clone())).await;

        assert_eq!(scope_requests(&f.pool).await, Vec::<String>::new());
        assert_eq!(full_gate_rows(&f.pool).await, 1);
        let said = scoped_feed(&f.pool).await;
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("full gate"), "{}", said[0]);
    }

    /// The last item's scope ran nothing over a map the project root still vouches for: "no test
    /// covers this diff" is a verdict there, so the item is committed like a green one (status
    /// `passed` and `checkpoint_sha` set in one write) while `gate_status` stays empty and no full
    /// gate runs. The recorded base is moved to HEAD so the cumulative diff is empty.
    #[tokio::test]
    async fn gate_item_records_a_checkpoint_when_the_last_item_scope_ran_nothing() {
        let f = scope_job_with(true, SCOPED_FINAL_RULES).await;
        let head = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&f.job.project_root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        sqlx::query("UPDATE worktrees SET base_sha = ? WHERE owner_kind = 'job' AND owner_id = ?")
            .bind(&head)
            .bind(f.job.id)
            .execute(&f.pool)
            .await
            .unwrap();

        gate_item_with(&f.state, &f.job, 1, 2, Some(f.executor.clone())).await;

        assert_eq!(
            scope_requests(&f.pool).await,
            vec![format!("job:{}", f.job.id)]
        );
        assert_eq!(full_gate_rows(&f.pool).await, 0);
        assert_eq!(item_statuses(&f.pool, f.job.id).await[1], "passed");
        let checkpoint: Option<String> = sqlx::query_scalar(
            "SELECT checkpoint_sha FROM job_items WHERE job_id = ? AND ordinal = 1",
        )
        .bind(f.job.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            checkpoint,
            Some(head),
            "a trusted nothing-ran last item checkpoints the tree"
        );
        let gate_status: Option<String> = sqlx::query_scalar(
            "SELECT gate_status FROM job_items WHERE job_id = ? AND ordinal = 1",
        )
        .bind(f.job.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            gate_status, None,
            "nothing was measured, so no gate verdict is recorded"
        );
    }

    // ---- a job still being provisioned --------------------------------------------------------

    /// `start` inserts the row live before `git worktree add` runs, so a tick can find a job that
    /// has no worktree YET. That job is being born, not broken: the pass waits for it.
    #[tokio::test]
    async fn a_job_still_being_provisioned_is_not_failed_for_having_no_worktree() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(advance(&state, &job, Utc::now()).await, Step::Stopped);

        assert_eq!(
            job_status(&pool, job_id).await,
            "planning",
            "a tick inside `start`'s window failed a job whose worktree was still being made"
        );
        assert!(!feed_kinds(&pool).await.contains(&"job_failed".to_owned()));
        assert!(stages_run(&pool, job_id).await.is_empty());
    }

    /// The grace is a grace and not a pass: a job that has gone without a worktree for longer than
    /// any `start` takes is one whose start is gone, and it is failed so its slot comes back.
    #[tokio::test]
    async fn a_job_with_no_worktree_past_the_provisioning_grace_is_failed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        let later = Utc::now() + PROVISIONING_GRACE + chrono::Duration::minutes(1);
        assert_eq!(advance(&state, &job, later).await, Step::Stopped);

        assert_eq!(job_status(&pool, job_id).await, "failed");
        assert!(feed_kinds(&pool).await.contains(&"job_failed".to_owned()));
    }

    /// A worktree read that errors is a database hiccup, not a fact about the job. The pass is
    /// skipped and the job is left live for the next tick — even past the grace, so this is the
    /// error arm being measured and not the grace.
    #[tokio::test]
    async fn a_worktree_that_cannot_be_read_skips_the_pass_rather_than_failing_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        // Nothing REFERENCES worktrees (migrations 0044, 0096), so this drops no other row; every
        // read of it from here on is an error.
        sqlx::query("DROP TABLE worktrees")
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        let later = Utc::now() + PROVISIONING_GRACE + chrono::Duration::minutes(1);
        assert_eq!(advance(&state, &job, later).await, Step::Stopped);

        assert_eq!(
            job_status(&pool, job_id).await,
            "planning",
            "a failed read of the worktree table failed the job"
        );
        assert!(!feed_kinds(&pool).await.contains(&"job_failed".to_owned()));
    }

    /// PURE: the window itself, and what an unreadable timestamp falls back to.
    #[test]
    fn still_provisioning_is_the_grace_after_creation_and_no_longer() {
        let created = Utc::now();
        let stamp = created.to_rfc3339();
        assert!(still_provisioning(&stamp, created));
        assert!(still_provisioning(&stamp, created + PROVISIONING_GRACE));
        assert!(!still_provisioning(
            &stamp,
            created + PROVISIONING_GRACE + chrono::Duration::seconds(1)
        ));
        assert!(!still_provisioning("not a timestamp", created));
    }
}
