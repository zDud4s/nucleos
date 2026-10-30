//! Gates, merge and land of a tree job (spec R12), with the reviewer's findings kept as data (L40).
//!
//! The state decisions are pure over a `Manifest` or a `Branch`, so they can be tested without a
//! daemon. The async parts wrap them around the verify gate, the VCS queue and `land::submit`, and
//! persist the job themselves. Merge and land are each two steps, submit and settle, because the
//! queue runs the operation later and a daemon that restarts in between has to be able to resume.

#[cfg(test)]
mod tests {
    // A process-wide guard held across awaits on purpose: it serialises mutation of the shared
    // NUCLEOS_WORKTREE_ROOT override, as in `land::tests` and `git_exec::tests`.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use sqlx::SqlitePool;

    use crate::config::VerifyConfig;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use crate::execution::git_exec::tests::{WorktreeRootEnv, sha_of};
    use crate::execution::git_exec::{GitExecutor, OPERATION_TIMEOUT};
    use crate::execution::vcs::{Origin, ResolvedRepo, drain_once};
    use crate::execution::worktree::test_env_lock;
    use crate::gate::GateOutcome;
    use crate::tree_job::engine::branch_node_may_close;
    use crate::tree_job::model::{Branch, JOB_TERMINAL_STATE, Manifest};
    use crate::tree_job::store::{self, TreeJob};
    use crate::verify::ScopeVerdict;
    use crate::verify::exec::Executor;

    const STAMP: &str = "2026-10-30T10:00:00+00:00";
    const SETTLE_WAIT: Duration = Duration::from_secs(5);

    // ---- fixtures: manifests -------------------------------------------------------------------

    fn branch_at(name: &str, state: &str) -> Branch {
        let mut branch = Branch::new(name, &[format!("{name}/**")], "backend", None);
        branch.state = state.to_owned();
        branch.git_branch = Some(format!("tree/{name}"));
        branch
    }

    fn docs_branch_at(name: &str, state: &str) -> Branch {
        let mut branch = Branch::new(name, &[format!(".ai/{name}/**")], "docs", None);
        branch.state = state.to_owned();
        branch
    }

    fn manifest_with(branches: Vec<Branch>) -> Manifest {
        manifest_on("C:/nowhere", branches)
    }

    fn manifest_on(project_root: &str, branches: Vec<Branch>) -> Manifest {
        let mut manifest = Manifest::new("job-1", "feat/tree", "Ship it", project_root, &[]);
        for branch in branches {
            manifest.branches.insert(branch.name.clone(), branch);
        }
        manifest
    }

    fn job_from(manifest: Manifest) -> TreeJob {
        TreeJob {
            id: manifest.tree_id.clone(),
            root_node_id: format!("root-of-{}", manifest.tree_id),
            project_id: Some("alpha".to_owned()),
            feature_branch: Some(manifest.feature_branch.clone()),
            goal: manifest.goal.clone(),
            state: manifest.state.clone(),
            manifest,
            contract: None,
            created_at: STAMP.to_owned(),
            updated_at: STAMP.to_owned(),
        }
    }

    async fn saved_job(pool: &SqlitePool, manifest: Manifest) -> TreeJob {
        let job = job_from(manifest);
        store::save(pool, &job).await.unwrap();
        job
    }

    async fn stored(pool: &SqlitePool, id: &str) -> TreeJob {
        store::load(pool, id).await.unwrap().expect("the job is stored")
    }

    async fn listed_ids(pool: &SqlitePool) -> Vec<String> {
        store::list_non_terminal(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect()
    }

    fn finding(id: &str, state: &str) -> Value {
        json!({"id": id, "state": state})
    }

    fn reviewed(mut branch: Branch, findings: Value) -> Branch {
        branch.reviews.push(json!({"at": STAMP, "findings": findings}));
        branch
    }

    fn passed() -> ScopeVerdict {
        ScopeVerdict::Measured(GateOutcome::Passed)
    }

    fn failed() -> ScopeVerdict {
        ScopeVerdict::Measured(GateOutcome::Failed {
            exit_code: 3,
            output: "boom at the end".to_owned(),
        })
    }

    fn errored() -> ScopeVerdict {
        ScopeVerdict::Measured(GateOutcome::Errored {
            reason: "did not finish in time".to_owned(),
        })
    }

    fn undecided_verdicts() -> Vec<ScopeVerdict> {
        vec![errored(), ScopeVerdict::NothingRan, ScopeVerdict::Unavailable]
    }

    // ---- fixtures: repositories and the queue --------------------------------------------------

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn porcelain(dir: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["status", "--porcelain"])
            .output()
            .expect("git should start");
        assert!(output.status.success());
        String::from_utf8(output.stdout).expect("utf-8")
    }

    fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
        std::fs::write(repo.join(file), text).expect("write");
        assert!(git_in(repo, &["add", "-A"]));
        assert!(git_in(repo, &["commit", "-q", "-m", message]));
    }

    fn fresh_repo(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let container = space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        (container, repo)
    }

    /// `master`, the job's `feat/tree`, and `tree/api` one commit ahead of it. The main checkout
    /// stands on `feat/tree`, the target of a branch merge.
    fn repo_with_a_tree_branch(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "tree/api"]));
        commit_file(&repo, "api.txt", "from the branch\n", "api");
        assert!(git_in(&repo, &["checkout", "-q", "feat/tree"]));
        (container, repo)
    }

    /// `tree/api` and `feat/tree` each change `seed.txt`, so merging one into the other conflicts.
    /// The main checkout stands on `feat/tree`.
    fn repo_with_a_conflicting_tree_branch(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "tree/api"]));
        commit_file(&repo, "seed.txt", "theirs\n", "theirs");
        assert!(git_in(&repo, &["checkout", "-q", "feat/tree"]));
        commit_file(&repo, "seed.txt", "ours\n", "ours");
        (container, repo)
    }

    /// `feat/tree` one commit ahead of `master`, with the main checkout on `master`.
    fn repo_with_a_feature_to_land(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        commit_file(&repo, "feature.txt", "from the tree\n", "feature");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        (container, repo)
    }

    /// `feat/tree` and `master` each change `seed.txt`, so the landing conflicts in the queue.
    fn repo_with_a_conflicting_feature(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        commit_file(&repo, "seed.txt", "theirs\n", "theirs");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        commit_file(&repo, "seed.txt", "ours\n", "ours");
        (container, repo)
    }

    /// A pool, a project whose integration branch is `master`, and the worktree root the queue's
    /// integration worktrees go under. The caller holds `test_env_lock` for as long as this lives.
    struct Harness {
        pool: SqlitePool,
        resolved: ResolvedRepo,
        _roots: tempfile::TempDir,
        _env: WorktreeRootEnv,
    }

    async fn harness(repo: &Path) -> Harness {
        let roots = space_free_tempdir("nucleos-landing-wt-");
        let env = WorktreeRootEnv::set(roots.path());
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES (?, 'active', ?, 'master')",
        )
        .bind("alpha")
        .bind(repo.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .expect("seed the project's roster row");
        Harness {
            pool,
            resolved: ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha"),
            _roots: roots,
            _env: env,
        }
    }

    /// For a test that never reaches git: nothing here is on disk.
    fn synthetic_repo() -> ResolvedRepo {
        ResolvedRepo::synthetic("alpha", "C:/nowhere", "alpha")
    }

    fn manifest_for_repo(repo: &Path, branches: Vec<Branch>) -> Manifest {
        manifest_on(&repo.to_string_lossy(), branches)
    }

    async fn drain(pool: &SqlitePool) {
        assert!(
            drain_once(pool, "alpha", &GitExecutor::default()).await,
            "the queue had a request to run"
        );
    }

    async fn request_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn land_deadline() -> Instant {
        Instant::now() + OPERATION_TIMEOUT
    }

    // ---- findings as data ----------------------------------------------------------------------

    #[test]
    fn record_findings_appends_a_review_holding_the_reviewers_list() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let list = json!([
            {"id": "f1", "state": "open", "file": "core/src/a.rs", "note": "wrong"},
            {"id": "f2", "state": "defer"},
        ]);

        record_findings(&mut manifest, "api", list.clone()).unwrap();

        let reviews = &manifest.branches["api"].reviews;
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0]["findings"], list);
        chrono::DateTime::parse_from_rfc3339(reviews[0]["at"].as_str().unwrap())
            .expect("a review is stamped, as tree.py's run_review does");
    }

    #[test]
    fn each_recording_appends_one_more_review() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);

        record_findings(&mut manifest, "api", json!([finding("f1", "open")])).unwrap();
        record_findings(&mut manifest, "api", json!([finding("f1", "fixed")])).unwrap();

        assert_eq!(manifest.branches["api"].reviews.len(), 2);
    }

    #[test]
    fn an_empty_list_records_a_clean_review() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);

        record_findings(&mut manifest, "api", json!([])).unwrap();

        let branch = &manifest.branches["api"];
        assert_eq!(branch.reviews.len(), 1);
        assert_eq!(branch.reviews[0]["findings"], json!([]));
        assert!(open_findings(branch).is_empty());
    }

    #[tokio::test]
    async fn recorded_findings_survive_a_store_update_and_load() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;
        let list = json!([{"id": "f1", "state": "open", "note": "wrong"}]);

        record_findings(&mut job.manifest, "api", list.clone()).unwrap();
        store::update(&pool, &job).await.unwrap();

        let loaded = stored(&pool, "job-1").await;
        assert_eq!(loaded.manifest.branches["api"].reviews[0]["findings"], list);
        assert_eq!(open_findings(&loaded.manifest.branches["api"]).len(), 1);
    }

    #[test]
    fn findings_for_an_unknown_branch_are_refused_without_change() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let before = manifest.clone();

        let error =
            record_findings(&mut manifest, "ghost", json!([finding("f1", "open")])).unwrap_err();

        assert!(matches!(error, LandingError::UnknownBranch(_)), "{error:?}");
        assert!(error.to_string().contains("ghost"), "{error}");
        assert_eq!(manifest, before);
    }

    #[test]
    fn findings_that_are_not_a_list_are_refused_without_change() {
        for not_a_list in [
            json!({"id": "f1", "state": "open"}),
            json!("f1"),
            json!(7),
            Value::Null,
        ] {
            let mut manifest = manifest_with(vec![branch_at("api", "green")]);
            let before = manifest.clone();

            let error = record_findings(&mut manifest, "api", not_a_list.clone()).unwrap_err();

            assert!(
                matches!(error, LandingError::InvalidFindings(_)),
                "{not_a_list}: {error:?}"
            );
            assert_eq!(manifest, before, "{not_a_list}");
        }
    }

    /// The good finding ahead of the bad one must not be kept either: the list is taken whole.
    #[test]
    fn a_finding_without_an_id_is_refused_without_change() {
        for bad in [
            json!({"state": "open"}),
            json!({"id": "", "state": "open"}),
            json!({"id": "   ", "state": "open"}),
            json!({"id": 7, "state": "open"}),
            json!("f1"),
        ] {
            let mut manifest = manifest_with(vec![branch_at("api", "green")]);
            let before = manifest.clone();

            let error = record_findings(
                &mut manifest,
                "api",
                json!([finding("good", "open"), bad.clone()]),
            )
            .unwrap_err();

            assert!(
                matches!(error, LandingError::InvalidFindings(_)),
                "{bad}: {error:?}"
            );
            assert_eq!(manifest, before, "{bad}");
        }
    }

    #[test]
    fn a_duplicate_id_in_one_list_is_refused_without_change() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let before = manifest.clone();

        let error = record_findings(
            &mut manifest,
            "api",
            json!([finding("f1", "open"), finding("f1", "fixed")]),
        )
        .unwrap_err();

        assert!(matches!(error, LandingError::InvalidFindings(_)), "{error:?}");
        assert!(error.to_string().contains("f1"), "{error}");
        assert_eq!(manifest, before);
    }

    /// `read_findings` (`tree.py:626`) has no default state: a finding must carry one of
    /// `FINDING_STATES`, or the whole list is refused. A default of anything but `open` would let a
    /// finding that forgot its state stop blocking the merge.
    #[test]
    fn a_finding_without_a_known_state_is_refused_without_change() {
        for bad in [
            json!({"id": "f1"}),
            json!({"id": "f1", "state": "closed"}),
            json!({"id": "f1", "state": 1}),
        ] {
            let mut manifest = manifest_with(vec![branch_at("api", "green")]);
            let before = manifest.clone();

            let error = record_findings(&mut manifest, "api", json!([bad.clone()])).unwrap_err();

            assert!(
                matches!(error, LandingError::InvalidFindings(_)),
                "{bad}: {error:?}"
            );
            assert_eq!(manifest, before, "{bad}");
        }
    }

    #[test]
    fn every_finding_state_of_tree_py_is_accepted() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let list = json!([
            finding("a", "open"),
            finding("b", "fixed"),
            finding("c", "drop"),
            finding("d", "defer"),
        ]);

        record_findings(&mut manifest, "api", list.clone()).unwrap();

        assert_eq!(manifest.branches["api"].reviews[0]["findings"], list);
    }

    /// `run_review` (`tree.py:651-655`) folds a new list over the last review's findings. Without
    /// that, a reviewer who simply does not mention an open finding would close it, and open it for
    /// the merge. A finding that is mentioned takes the new values and keeps the keys it had.
    #[test]
    fn a_finding_the_next_review_does_not_mention_stays_open() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        record_findings(
            &mut manifest,
            "api",
            json!([
                {"id": "f1", "state": "open", "note": "kept"},
                {"id": "f2", "state": "open"},
            ]),
        )
        .unwrap();

        record_findings(&mut manifest, "api", json!([finding("f2", "fixed")])).unwrap();

        let branch = &manifest.branches["api"];
        let open = open_findings(branch);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["id"], "f1");
        let last = branch.reviews[1]["findings"].as_array().unwrap();
        assert_eq!(last.len(), 2);

        record_findings(&mut manifest, "api", json!([finding("f1", "fixed")])).unwrap();

        let branch = &manifest.branches["api"];
        assert!(open_findings(branch).is_empty());
        let last = branch.reviews[2]["findings"].as_array().unwrap();
        let f1 = last.iter().find(|one| one["id"] == "f1").unwrap();
        assert_eq!(f1["note"], "kept");
    }

    #[test]
    fn a_branch_with_no_reviews_has_no_open_findings() {
        assert!(open_findings(&branch_at("api", "green")).is_empty());
    }

    #[test]
    fn open_findings_returns_only_the_open_ones() {
        let branch = reviewed(
            branch_at("api", "green"),
            json!([
                finding("a", "open"),
                finding("b", "fixed"),
                finding("c", "drop"),
                finding("d", "defer"),
                finding("e", "open"),
            ]),
        );

        let ids: Vec<Value> = open_findings(&branch)
            .into_iter()
            .map(|one| one["id"].clone())
            .collect();

        assert_eq!(ids, vec![json!("a"), json!("e")]);
    }

    #[test]
    fn open_findings_reads_only_the_last_review() {
        let closed_later = reviewed(
            reviewed(branch_at("api", "green"), json!([finding("a", "open")])),
            json!([finding("b", "fixed")]),
        );
        assert!(open_findings(&closed_later).is_empty());

        let opened_later = reviewed(
            reviewed(branch_at("api", "green"), json!([finding("a", "fixed")])),
            json!([finding("b", "open")]),
        );
        let open = open_findings(&opened_later);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["id"], "b");
    }

    // ---- the gate ------------------------------------------------------------------------------

    #[test]
    fn a_passed_verdict_moves_the_branch_to_green() {
        for from in ["building", "gating", "fixing", "red", "reviewing"] {
            let mut branch = branch_at("api", from);

            let result = apply_gate_verdict(&mut branch, &passed()).unwrap();

            assert_eq!(result, GateResult::Green, "from {from}");
            assert_eq!(branch.state, "green", "from {from}");
            assert_eq!(branch.gates.len(), 1, "from {from}");
            assert_eq!(branch.gates[0]["verdict"], "green");
            assert_eq!(branch.gates[0]["exit"], 0);
            chrono::DateTime::parse_from_rfc3339(branch.gates[0]["at"].as_str().unwrap())
                .expect("a gate entry is stamped, as tree_resources.py's run_gate does");
        }
    }

    #[test]
    fn a_failed_verdict_moves_the_branch_to_red_and_keeps_the_exit_and_the_tail() {
        for from in ["building", "gating", "fixing", "green", "reviewing"] {
            let mut branch = branch_at("api", from);

            let result = apply_gate_verdict(&mut branch, &failed()).unwrap();

            assert_eq!(result, GateResult::Red, "from {from}");
            assert_eq!(branch.state, "red", "from {from}");
            assert_eq!(branch.gates.len(), 1, "from {from}");
            assert_eq!(branch.gates[0]["verdict"], "red");
            assert_eq!(branch.gates[0]["exit"], 3);
            assert!(
                branch.gates[0]["log_tail"]
                    .as_str()
                    .unwrap()
                    .contains("boom at the end")
            );
        }
    }

    /// `apply_transition` refuses `green` to `green` and `red` to `red`, and a repeated verdict is
    /// still a legal gate (`can_transition`'s doc, `run_gate` has no source guard).
    #[test]
    fn a_repeated_verdict_keeps_the_state_and_still_appends_its_entry() {
        let mut green = branch_at("api", "green");
        assert_eq!(
            apply_gate_verdict(&mut green, &passed()).unwrap(),
            GateResult::Green
        );
        assert_eq!(green.state, "green");
        assert_eq!(green.gates.len(), 1);

        let mut red = branch_at("api", "red");
        assert_eq!(
            apply_gate_verdict(&mut red, &failed()).unwrap(),
            GateResult::Red
        );
        assert_eq!(red.state, "red");
        assert_eq!(red.gates.len(), 1);
    }

    /// An undecided gate is never green: `Errored`, `NothingRan` and `Unavailable` measured nothing
    /// that could bless the work, so the branch stays where it was and the entry says why.
    #[test]
    fn an_undecided_verdict_leaves_the_state_and_records_why() {
        for from in ["building", "gating", "fixing", "red", "reviewing"] {
            for verdict in undecided_verdicts() {
                let mut branch = branch_at("api", from);

                let result = apply_gate_verdict(&mut branch, &verdict).unwrap();

                let GateResult::Undecided { reason } = result else {
                    panic!("{verdict:?} from {from} must be undecided, got {result:?}");
                };
                assert!(!reason.trim().is_empty(), "{verdict:?}");
                assert_eq!(branch.state, from, "{verdict:?}");
                assert_eq!(branch.gates.len(), 1, "{verdict:?}");
                assert_eq!(branch.gates[0]["verdict"], "undecided", "{verdict:?}");
                assert_eq!(branch.gates[0]["reason"], json!(reason), "{verdict:?}");
            }
        }
    }

    #[test]
    fn an_errored_verdict_records_the_reason_the_gate_gave() {
        let mut branch = branch_at("api", "building");

        let result = apply_gate_verdict(&mut branch, &errored()).unwrap();

        let GateResult::Undecided { reason } = result else {
            panic!("expected undecided, got {result:?}");
        };
        assert!(reason.contains("did not finish in time"), "{reason}");
    }

    #[test]
    fn each_verdict_appends_one_gate_entry_in_order() {
        let mut branch = branch_at("api", "building");

        apply_gate_verdict(&mut branch, &failed()).unwrap();
        apply_gate_verdict(&mut branch, &ScopeVerdict::NothingRan).unwrap();
        apply_gate_verdict(&mut branch, &passed()).unwrap();

        let verdicts: Vec<&Value> = branch.gates.iter().map(|entry| &entry["verdict"]).collect();
        assert_eq!(
            verdicts,
            vec![&json!("red"), &json!("undecided"), &json!("green")]
        );
        assert_eq!(branch.state, "green");
    }

    #[test]
    fn a_gate_on_a_branch_that_may_not_gate_is_refused_without_change() {
        let refused = [
            branch_at("api", "queued"),
            branch_at("api", "open"),
            branch_at("api", "merged"),
            branch_at("api", "parked"),
            branch_at("api", "dropped"),
            docs_branch_at("api", "done"),
        ];
        for original in refused {
            for verdict in [passed(), failed(), ScopeVerdict::NothingRan] {
                let mut branch = original.clone();

                let error = apply_gate_verdict(&mut branch, &verdict).unwrap_err();

                assert!(
                    matches!(error, LandingError::Refused(_)),
                    "{}: {error:?}",
                    original.state
                );
                assert!(
                    error.to_string().contains(&original.state),
                    "the refusal names the state: {error}"
                );
                assert_eq!(branch, original);
            }
        }
    }

    /// `merged` comes only out of a settled merge, never out of a gate.
    #[test]
    fn no_verdict_moves_a_branch_to_merged() {
        for verdict in [
            passed(),
            failed(),
            errored(),
            ScopeVerdict::NothingRan,
            ScopeVerdict::Unavailable,
        ] {
            let mut branch = branch_at("api", "green");
            apply_gate_verdict(&mut branch, &verdict).unwrap();
            assert_ne!(branch.state, "merged", "{verdict:?}");
        }
    }

    /// A tree job id is TEXT and `gate_job_scope` takes an `i64` that only ever labels the
    /// `verify_requests.caller` row as `job:{id}`. So it is derived from the text: no database read,
    /// the same for the life of the job (an implicit rowid would move on VACUUM), and not a small
    /// integer that would read as a row of the `jobs` table.
    #[test]
    fn the_scope_caller_id_is_derived_from_the_text_id_and_is_stable() {
        let first = scope_caller_id("5d6616a7-71f0-4414-a717-99d697a4c6d0");

        assert_eq!(first, scope_caller_id("5d6616a7-71f0-4414-a717-99d697a4c6d0"));
        assert_ne!(first, scope_caller_id("204d71d1-6b8b-4606-8e2b-d612bdbc5e00"));
        assert!(first > i64::from(u32::MAX), "{first}");
        assert!(scope_caller_id("") > i64::from(u32::MAX));
        assert!(scope_caller_id("job-1") > i64::from(u32::MAX));
    }

    fn idle_executor(pool: &SqlitePool, machine: &Path) -> std::sync::Arc<Executor> {
        Executor::new(
            pool.clone(),
            VerifyConfig::default(),
            Some(machine.to_path_buf()),
        )
    }

    /// `gate_branch` over a live `Executor` whose request cannot even be submitted, the one verdict
    /// reachable with no unit run (`a_job_gate_scope_that_cannot_be_submitted_is_unavailable`).
    #[tokio::test]
    async fn gate_branch_persists_an_unavailable_verdict_and_leaves_the_state() {
        let pool = crate::testdb::fresh_pool().await;
        let machine = tempfile::tempdir().unwrap();
        let not_a_worktree = tempfile::tempdir().unwrap();
        let executor = idle_executor(&pool, machine.path());
        let mut branch = branch_at("api", "building");
        branch.worktree = Some(not_a_worktree.path().to_string_lossy().into_owned());
        let mut job = saved_job(&pool, manifest_with(vec![branch])).await;

        let result = gate_branch(&executor, &mut job, "api", Duration::from_secs(5))
            .await
            .unwrap();

        assert!(matches!(result, GateResult::Undecided { .. }), "{result:?}");
        let loaded = stored(&pool, "job-1").await;
        assert_eq!(loaded.manifest, job.manifest, "the job was persisted");
        let branch = &loaded.manifest.branches["api"];
        assert_eq!(branch.state, "building");
        assert_eq!(branch.gates.len(), 1);
    }

    #[tokio::test]
    async fn gate_branch_refuses_a_branch_it_cannot_gate_without_change() {
        let pool = crate::testdb::fresh_pool().await;
        let machine = tempfile::tempdir().unwrap();
        let executor = idle_executor(&pool, machine.path());
        let mut no_worktree = branch_at("api", "building");
        no_worktree.worktree = None;
        let mut parked = branch_at("ui", "parked");
        parked.worktree = Some(machine.path().to_string_lossy().into_owned());
        let mut job = saved_job(&pool, manifest_with(vec![no_worktree, parked])).await;
        let before = job.manifest.clone();

        for name in ["api", "ui", "ghost"] {
            let result = gate_branch(&executor, &mut job, name, Duration::from_secs(5)).await;
            assert!(result.is_err(), "{name}: {result:?}");
            assert_eq!(job.manifest, before, "{name}");
        }
        assert_eq!(stored(&pool, "job-1").await.manifest, before);
    }

    // ---- merge: the decision -------------------------------------------------------------------

    #[test]
    fn a_green_branch_with_no_open_findings_may_merge() {
        let manifest = manifest_with(vec![branch_at("api", "green")]);

        assert_eq!(
            check_may_merge(&manifest, "api").unwrap(),
            MergeDecision::Proceed {
                source: "tree/api".to_owned(),
                target: "feat/tree".to_owned(),
            }
        );
    }

    #[test]
    fn findings_that_are_not_open_do_not_block_a_merge() {
        let branch = reviewed(
            branch_at("api", "green"),
            json!([
                finding("a", "fixed"),
                finding("b", "drop"),
                finding("c", "defer"),
            ]),
        );
        let manifest = manifest_with(vec![branch]);

        assert!(matches!(
            check_may_merge(&manifest, "api").unwrap(),
            MergeDecision::Proceed { .. }
        ));
    }

    #[test]
    fn every_state_but_green_refuses_a_merge_and_names_itself() {
        for state in [
            "queued",
            "open",
            "building",
            "gating",
            "red",
            "reviewing",
            "fixing",
            "parked",
            "dropped",
        ] {
            let manifest = manifest_with(vec![branch_at("api", state)]);

            let error = check_may_merge(&manifest, "api").unwrap_err();

            assert!(matches!(error, LandingError::Refused(_)), "{state}: {error:?}");
            assert!(error.to_string().contains(state), "{state}: {error}");
        }
    }

    #[test]
    fn an_open_finding_refuses_a_merge_and_is_named() {
        let branch = reviewed(
            branch_at("api", "green"),
            json!([finding("f1", "open"), finding("f2", "fixed")]),
        );
        let manifest = manifest_with(vec![branch]);

        let error = check_may_merge(&manifest, "api").unwrap_err();

        assert!(matches!(error, LandingError::Refused(_)), "{error:?}");
        assert!(error.to_string().contains("f1"), "{error}");
        assert!(!error.to_string().contains("f2"), "{error}");
    }

    /// Through `record_findings` rather than a review written by hand: what the reviewer recorded
    /// is what stops the merge, whichever state the recording leaves the branch in.
    #[test]
    fn a_finding_the_reviewer_recorded_stops_the_merge() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);

        record_findings(&mut manifest, "api", json!([finding("f1", "open")])).unwrap();

        assert!(matches!(
            check_may_merge(&manifest, "api"),
            Err(LandingError::Refused(_))
        ));
    }

    /// `run_merge` (`tree.py:693`): a stacked branch merges only after the branch it stands on.
    #[test]
    fn a_branch_whose_base_is_not_merged_refuses_a_merge_and_names_the_base() {
        let mut stacked = branch_at("api", "green");
        stacked.base = Some("db".to_owned());

        let waiting = manifest_with(vec![stacked.clone(), branch_at("db", "green")]);
        let error = check_may_merge(&waiting, "api").unwrap_err();
        assert!(matches!(error, LandingError::Refused(_)), "{error:?}");
        assert!(error.to_string().contains("db"), "{error}");

        let ready = manifest_with(vec![stacked, branch_at("db", "merged")]);
        assert!(matches!(
            check_may_merge(&ready, "api").unwrap(),
            MergeDecision::Proceed { .. }
        ));
    }

    #[test]
    fn a_green_branch_without_a_git_branch_refuses_a_merge() {
        let mut branch = branch_at("api", "green");
        branch.git_branch = None;
        let manifest = manifest_with(vec![branch]);

        assert!(matches!(
            check_may_merge(&manifest, "api"),
            Err(LandingError::Refused(_))
        ));
    }

    /// `can_transition`: a docs branch closes on `done` and never merges.
    #[test]
    fn a_docs_branch_refuses_a_merge() {
        let mut branch = docs_branch_at("notes", "green");
        branch.git_branch = Some("tree/notes".to_owned());
        let manifest = manifest_with(vec![branch]);

        assert!(matches!(
            check_may_merge(&manifest, "notes"),
            Err(LandingError::Refused(_))
        ));
    }

    #[test]
    fn a_merge_of_an_unknown_branch_is_refused() {
        let manifest = manifest_with(vec![branch_at("api", "green")]);

        assert!(matches!(
            check_may_merge(&manifest, "ghost"),
            Err(LandingError::UnknownBranch(_))
        ));
    }

    fn merged_branch(name: &str, sha: &str) -> Branch {
        let mut branch = branch_at(name, "merged");
        branch.extra.insert("merge_sha".to_owned(), json!(sha));
        branch.extra.insert("merged_at".to_owned(), json!(STAMP));
        branch
    }

    #[test]
    fn an_already_merged_branch_answers_with_its_stored_sha() {
        let manifest = manifest_with(vec![merged_branch("api", "abc123")]);

        assert_eq!(
            check_may_merge(&manifest, "api").unwrap(),
            MergeDecision::AlreadyMerged {
                sha: "abc123".to_owned()
            }
        );
    }

    // ---- merge: through the queue --------------------------------------------------------------

    #[tokio::test]
    async fn a_refused_merge_enqueues_nothing_and_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(
            &pool,
            manifest_with(vec![
                branch_at("api", "red"),
                reviewed(branch_at("ui", "green"), json!([finding("f1", "open")])),
            ]),
        )
        .await;
        let before = job.manifest.clone();

        for name in ["api", "ui"] {
            let result =
                submit_merge(&pool, &synthetic_repo(), &mut job, name, Origin::Daemon).await;
            assert!(
                matches!(result, Err(LandingError::Refused(_))),
                "{name}: {result:?}"
            );
        }

        assert_eq!(request_count(&pool).await, 0);
        assert_eq!(job.manifest, before);
        assert_eq!(stored(&pool, "job-1").await.manifest, before);
    }

    #[tokio::test]
    async fn a_replayed_merge_returns_the_stored_sha_and_enqueues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![merged_branch("api", "abc123")])).await;
        let before = job.manifest.clone();

        let result = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert_eq!(
            result,
            MergeSubmission::AlreadyMerged {
                sha: "abc123".to_owned()
            }
        );
        assert_eq!(request_count(&pool).await, 0);
        assert_eq!(job.manifest, before);
    }

    #[tokio::test]
    async fn a_green_branch_merges_through_the_queue_into_the_feature_branch() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_tree_branch("nucleos-landing-merge-");
        let h = harness(&repo).await;
        let tip = sha_of(&repo, "tree/api");
        let master = sha_of(&repo, "master");
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        let submitted = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert!(
            matches!(submitted, MergeSubmission::Queued { .. }),
            "{submitted:?}"
        );
        assert_eq!(
            job.manifest.branches["api"].state, "green",
            "a queued merge is not a merge yet"
        );
        assert_eq!(request_count(&h.pool).await, 1);

        drain(&h.pool).await;
        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT)
            .await
            .unwrap();

        let MergeSettlement::Merged { sha } = settled else {
            panic!("expected a merge, got {settled:?}");
        };
        assert_eq!(sha, sha_of(&repo, "feat/tree"));
        assert_eq!(sha_of(&repo, &format!("{sha}^2")), tip);
        assert_eq!(sha_of(&repo, "master"), master, "only the target moved");

        let branch = &job.manifest.branches["api"];
        assert_eq!(branch.state, "merged");
        assert_eq!(branch.extra["merge_sha"], json!(sha));
        chrono::DateTime::parse_from_rfc3339(branch.extra["merged_at"].as_str().unwrap())
            .expect("merged_at is RFC 3339");
        assert!(branch_node_may_close(branch));

        assert_eq!(
            stored(&h.pool, "job-1").await.manifest,
            job.manifest,
            "the merge was persisted"
        );

        let replay = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        assert_eq!(replay, MergeSubmission::AlreadyMerged { sha });
        assert_eq!(request_count(&h.pool).await, 1, "the replay enqueued nothing");
    }

    /// The pattern of `merge_item`: an implement node leaves its tree uncommitted, and a merge of
    /// the branch without a checkpoint would bring an empty branch across and call it success.
    #[tokio::test]
    async fn a_merge_commits_what_the_branch_worktree_left_uncommitted() {
        let _lock = test_env_lock();
        let (container, repo) = repo_with_a_tree_branch("nucleos-landing-checkpoint-");
        let h = harness(&repo).await;
        let worktree = container.path().join("api-tree");
        assert!(git_in(
            &repo,
            &["worktree", "add", "-q", &worktree.to_string_lossy(), "tree/api"]
        ));
        std::fs::write(worktree.join("wip.txt"), "not committed yet\n").expect("write");
        let mut branch = branch_at("api", "green");
        branch.worktree = Some(worktree.to_string_lossy().into_owned());
        let mut job = saved_job(&h.pool, manifest_for_repo(&repo, vec![branch])).await;

        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;
        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT)
            .await
            .unwrap();

        assert!(
            matches!(settled, MergeSettlement::Merged { .. }),
            "{settled:?}"
        );
        assert!(
            git_in(&repo, &["cat-file", "-e", "feat/tree:wip.txt"]),
            "the worktree's uncommitted file reached the target"
        );
    }

    /// A checkpoint that fails means the work exists only in a working tree, so nothing is queued.
    #[tokio::test]
    async fn a_checkpoint_that_fails_queues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let not_a_worktree = tempfile::tempdir().unwrap();
        let mut branch = branch_at("api", "green");
        branch.worktree = Some(not_a_worktree.path().to_string_lossy().into_owned());
        let mut job = saved_job(&pool, manifest_with(vec![branch])).await;

        let result = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon).await;

        assert!(result.is_err(), "{result:?}");
        assert_eq!(request_count(&pool).await, 0);
        assert_eq!(job.manifest.branches["api"].state, "green");
    }

    /// A daemon restarted between submit and settle replays the submit, and a second row would run
    /// the same merge twice.
    #[tokio::test]
    async fn a_second_submit_while_one_is_queued_enqueues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;

        let first = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        let second = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(request_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn the_queued_merge_survives_in_the_stored_job() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_tree_branch("nucleos-landing-resume-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;

        // As boot would: the job comes back from the store and nothing else is remembered.
        let mut resumed = stored(&h.pool, "job-1").await;
        let settled = settle_merge(&h.pool, &mut resumed, "api", SETTLE_WAIT)
            .await
            .unwrap();

        assert!(
            matches!(settled, MergeSettlement::Merged { .. }),
            "{settled:?}"
        );
    }

    #[tokio::test]
    async fn settling_a_merge_the_queue_has_not_run_is_pending_and_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;
        submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        let before = job.manifest.clone();

        let settled = settle_merge(&pool, &mut job, "api", Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(settled, MergeSettlement::Pending);
        assert_eq!(job.manifest, before);
    }

    /// `merged` is reachable only through a merge the queue ran: there is no settling without one.
    #[tokio::test]
    async fn settling_a_branch_with_no_queued_merge_is_refused_without_change() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;
        let before = job.manifest.clone();

        let result = settle_merge(&pool, &mut job, "api", Duration::ZERO).await;

        assert!(matches!(result, Err(LandingError::Refused(_))), "{result:?}");
        assert_eq!(job.manifest, before);
    }

    #[tokio::test]
    async fn a_conflict_is_aborted_leaves_the_target_clean_and_is_recorded() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_tree_branch("nucleos-landing-conflict-");
        let h = harness(&repo).await;
        let target = sha_of(&repo, "feat/tree");
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;
        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT)
            .await
            .unwrap();

        let MergeSettlement::NotMerged { reason } = settled else {
            panic!("expected a refusal, got {settled:?}");
        };
        assert!(!reason.trim().is_empty());

        assert_eq!(sha_of(&repo, "feat/tree"), target, "the target did not move");
        assert_eq!(porcelain(&repo), "", "the target's checkout is clean");
        assert!(
            !repo.join(".git").join("MERGE_HEAD").exists(),
            "no merge was left half done"
        );

        let branch = &job.manifest.branches["api"];
        assert_ne!(branch.state, "merged");
        assert!(!branch.extra.contains_key("merge_sha"));
        assert!(!branch.extra.contains_key("merged_at"));
        assert!(!branch_node_may_close(branch));
        assert_eq!(branch.extra["merge_failure"], json!(reason));
        assert_eq!(
            stored(&h.pool, "job-1").await.manifest,
            job.manifest,
            "the reason was persisted"
        );
    }

    /// The branch's gate still stands and the tree has no `conflicted` state, so it stays `green`;
    /// and the dead request must not answer the next submit, or the branch could never merge.
    #[tokio::test]
    async fn after_a_conflict_the_branch_is_green_and_a_new_submit_queues_a_new_request() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_tree_branch("nucleos-landing-retry-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        let first = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;
        settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT)
            .await
            .unwrap();
        assert_eq!(job.manifest.branches["api"].state, "green");

        let second = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert!(matches!(second, MergeSubmission::Queued { .. }), "{second:?}");
        assert_ne!(first, second);
        assert_eq!(request_count(&h.pool).await, 2);
    }

    // ---- land: the decision --------------------------------------------------------------------

    #[test]
    fn a_tree_of_merged_branches_may_land() {
        let manifest = manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "merged")]);

        check_may_land(&manifest).unwrap();
    }

    #[test]
    fn dropped_branches_and_finished_docs_do_not_stop_a_landing() {
        let manifest = manifest_with(vec![
            branch_at("api", "merged"),
            branch_at("old", "dropped"),
            docs_branch_at("notes", "done"),
        ]);

        check_may_land(&manifest).unwrap();
    }

    #[test]
    fn a_tree_with_nothing_merged_may_not_land() {
        for branches in [
            vec![],
            vec![branch_at("old", "dropped")],
            vec![docs_branch_at("notes", "done")],
            vec![branch_at("old", "dropped"), docs_branch_at("notes", "done")],
        ] {
            let manifest = manifest_with(branches);

            assert!(
                matches!(check_may_land(&manifest), Err(LandingError::Refused(_))),
                "{:?}",
                manifest.branches.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_parked_branch_blocks_the_landing_and_is_named() {
        let manifest = manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "parked")]);

        let error = check_may_land(&manifest).unwrap_err();

        assert!(matches!(error, LandingError::Refused(_)), "{error:?}");
        assert!(error.to_string().contains("ui"), "{error}");
    }

    #[test]
    fn a_branch_still_at_work_blocks_the_landing_and_is_named() {
        for state in [
            "queued",
            "open",
            "building",
            "gating",
            "green",
            "red",
            "reviewing",
            "fixing",
        ] {
            let manifest =
                manifest_with(vec![branch_at("api", "merged"), branch_at("ui", state)]);

            let error = check_may_land(&manifest).unwrap_err();

            assert!(matches!(error, LandingError::Refused(_)), "{state}: {error:?}");
            assert!(error.to_string().contains("ui"), "{state}: {error}");
        }
    }

    /// `done` closes a docs branch only: on a code branch it is not one of the landing states.
    #[test]
    fn a_code_branch_marked_done_blocks_the_landing() {
        let manifest = manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "done")]);

        let error = check_may_land(&manifest).unwrap_err();

        assert!(error.to_string().contains("ui"), "{error}");
    }

    // ---- land: through `land::submit` ----------------------------------------------------------

    #[tokio::test]
    async fn a_landing_the_rules_refuse_enqueues_nothing_and_stays_listed() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(
            &pool,
            manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "green")]),
        )
        .await;

        let result = land_job(
            &pool,
            &synthetic_repo(),
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await;

        assert!(matches!(result, Err(LandingError::Refused(_))), "{result:?}");
        assert_eq!(request_count(&pool).await, 0);
        assert!(!job.manifest.extra.contains_key("land_id"));
        assert_eq!(listed_ids(&pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn a_landing_submits_and_records_the_land_id_and_the_job_is_not_over_yet() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-land-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        let id = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();

        assert_eq!(job.manifest.extra["land_id"], json!(id));
        assert_eq!(request_count(&h.pool).await, 1);
        let loaded = stored(&h.pool, "job-1").await;
        assert_eq!(loaded.manifest.extra["land_id"], json!(id), "persisted");
        assert_ne!(job.state, JOB_TERMINAL_STATE, "admitted is not landed");
        assert_eq!(listed_ids(&h.pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn a_second_landing_answers_with_the_recorded_id_and_enqueues_nothing() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-replay-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        let first = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        let second = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(request_count(&h.pool).await, 1);
    }

    /// `feat/tree` is `master`, so `land::submit` answers `LandRefusal::NothingToLand`.
    #[tokio::test]
    async fn a_land_refusal_keeps_the_job_live_and_returns_and_records_the_reason() {
        let _lock = test_env_lock();
        let (_container, repo) = fresh_repo("nucleos-landing-refusal-");
        assert!(git_in(&repo, &["branch", "feat/tree"]));
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        let result = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await;

        let Err(LandingError::LandRefused(reason)) = result else {
            panic!("expected a land refusal, got {result:?}");
        };
        assert!(reason.contains("nothing to land"), "{reason}");
        assert_eq!(job.state, "open");
        assert_eq!(job.manifest.state, "open");
        assert_eq!(job.manifest.extra["land_refusal"], json!(reason));
        assert!(!job.manifest.extra.contains_key("land_id"));
        assert_eq!(request_count(&h.pool).await, 0);

        let loaded = stored(&h.pool, "job-1").await;
        assert_eq!(loaded.state, "open");
        assert_eq!(
            loaded.manifest.extra["land_refusal"],
            json!(reason),
            "persisted"
        );
        assert_eq!(listed_ids(&h.pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn a_landed_job_is_finished_and_leaves_the_list() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-finish-");
        let h = harness(&repo).await;
        let feature = sha_of(&repo, "feat/tree");
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        drain(&h.pool).await;
        let settled = settle_land(&h.pool, &mut job, SETTLE_WAIT).await.unwrap();

        assert!(
            matches!(settled, LandSettlement::Finished { .. }),
            "{settled:?}"
        );
        assert_eq!(sha_of(&repo, "master^2"), feature);
        assert_eq!(job.state, JOB_TERMINAL_STATE);
        assert_eq!(job.manifest.state, JOB_TERMINAL_STATE);
        let loaded = stored(&h.pool, "job-1").await;
        assert_eq!(loaded.state, JOB_TERMINAL_STATE);
        assert_eq!(loaded.manifest.state, JOB_TERMINAL_STATE);
        assert!(listed_ids(&h.pool).await.is_empty());
    }

    /// `land::submit` only admits. A landing that conflicts in the queue has not landed anything,
    /// so finishing the job at admission would strand its work with nothing left to resume it.
    #[tokio::test]
    async fn a_landing_that_fails_in_the_queue_keeps_the_job_live_and_records_why() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_feature("nucleos-landing-land-conflict-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        drain(&h.pool).await;
        let settled = settle_land(&h.pool, &mut job, SETTLE_WAIT).await.unwrap();

        let LandSettlement::NotLanded { reason } = settled else {
            panic!("expected no landing, got {settled:?}");
        };
        assert!(!reason.trim().is_empty());
        assert_ne!(job.state, JOB_TERMINAL_STATE);
        assert_eq!(job.manifest.extra["land_failure"], json!(reason));
        assert_eq!(
            stored(&h.pool, "job-1").await.manifest,
            job.manifest,
            "persisted"
        );
        assert_eq!(listed_ids(&h.pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn settling_a_landing_the_queue_has_not_run_is_pending_and_changes_nothing() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-pending-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;
        land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        let before = job.clone();

        let settled = settle_land(&h.pool, &mut job, Duration::ZERO).await.unwrap();

        assert_eq!(settled, LandSettlement::Pending);
        assert_eq!(job, before);
    }

    #[tokio::test]
    async fn settling_a_job_with_no_landing_is_refused_and_does_not_finish_it() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "merged")])).await;

        let result = settle_land(&pool, &mut job, Duration::ZERO).await;

        assert!(matches!(result, Err(LandingError::Refused(_))), "{result:?}");
        assert_ne!(job.state, JOB_TERMINAL_STATE);
        assert_eq!(listed_ids(&pool).await, ["job-1"]);
    }

    /// Decision of the packet: a parked job is not over, it resumes at boot. Only `finished` ends a
    /// job, whatever `TERMINAL_STATES` says about a branch.
    #[tokio::test]
    async fn a_parked_job_cannot_land_past_its_parked_branch_and_stays_listed() {
        let pool = crate::testdb::fresh_pool().await;
        let mut manifest =
            manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "parked")]);
        manifest.state = "parked".to_owned();
        let mut job = saved_job(&pool, manifest).await;

        let error = land_job(
            &pool,
            &synthetic_repo(),
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("ui"), "{error}");
        assert_eq!(job.state, "parked");
        assert_eq!(listed_ids(&pool).await, ["job-1"]);
    }
}
