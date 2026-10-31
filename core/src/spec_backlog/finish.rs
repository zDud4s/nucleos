//! Finishing the backlog: one tree per spec, in dependency order (RED: tests only, GREEN fills in).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::ToolPolicy;
    use crate::spec_backlog::audit::{AuditReport, SpecLineage};
    use crate::spec_backlog::model::{
        spec_id, CriterionKind, CriterionStatus, DepSource, NewCriterion, SpecKind, SpecStatus,
    };
    use crate::spec_backlog::store;
    use crate::spec_backlog::verdict::{NotDoneReason, Verdict, VerifyState};
    use sqlx::SqlitePool;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const SHA: &str = "sha-1";

    fn path_of(name: &str) -> String {
        format!(".ai/specs/2026-01-01-{name}.md")
    }

    /// Registers a spec in `status` and returns its id.
    async fn add_spec(pool: &SqlitePool, name: &str, status: SpecStatus) -> String {
        let path = path_of(name);
        let spec = store::upsert_by_path(pool, None, &path, name, SHA).await.unwrap();
        if status == SpecStatus::Done {
            store::set_audit_result(pool, &spec.id, SpecStatus::Done, None, SHA).await.unwrap();
        } else if status != SpecStatus::Discovered {
            store::set_extraction_result(pool, &spec.id, SpecKind::Spec, status, None)
                .await
                .unwrap();
        }
        spec.id
    }

    fn cfg() -> FinishConfig {
        FinishConfig {
            max_parallel: 2,
            max_rounds: 2,
            integration_branch: "master".to_owned(),
            project_root: "/repo".to_owned(),
            owned_paths: BTreeMap::new(),
            worktree: std::env::temp_dir(),
        }
    }

    /// Gives every listed spec its own, disjoint owned path.
    fn disjoint(ids: &[&String]) -> BTreeMap<String, Vec<String>> {
        ids.iter()
            .enumerate()
            .map(|(n, id)| ((*id).clone(), vec![format!("core/src/area{n}/")]))
            .collect()
    }

    type Log = Arc<Mutex<Vec<String>>>;

    struct FakeLauncher {
        log: Log,
        requests: Mutex<Vec<TreeRequest>>,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        delay: Duration,
    }

    impl FakeLauncher {
        fn new(log: &Log) -> Self {
            Self {
                log: log.clone(),
                requests: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                delay: Duration::from_millis(40),
            }
        }

        fn requests(&self) -> Vec<TreeRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl TreeLauncher for FakeLauncher {
        async fn run_tree(&self, request: &TreeRequest) -> Result<TreeOutcome, String> {
            self.requests.lock().unwrap().push(request.clone());
            self.log.lock().unwrap().push(format!("tree:{}", request.spec_id));
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(TreeOutcome {
                run_ids: vec![format!("run-{}-r{}", request.spec_id, request.round)],
                lineage_roots: vec![format!("root-{}", request.id)],
            })
        }
    }

    /// Extract and audit stand-ins that write to the store like the real ones would.
    struct FakePreparer {
        pool: SqlitePool,
        log: Log,
        /// Specs whose audit ends Done; every other audit ends Audited.
        passes: Vec<String>,
        lineages: Mutex<Vec<(String, SpecLineage)>>,
    }

    impl FakePreparer {
        fn new(pool: &SqlitePool, log: &Log, passes: &[&String]) -> Self {
            Self {
                pool: pool.clone(),
                log: log.clone(),
                passes: passes.iter().map(|id| (*id).clone()).collect(),
                lineages: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl SpecPreparer for FakePreparer {
        async fn extract(&self, spec_id: &str) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("extract:{spec_id}"));
            let item = NewCriterion {
                text: "it works".to_owned(),
                kind: CriterionKind::Behavior,
                verify_hint: None,
                status: CriterionStatus::Open,
            };
            store::replace_criteria(&self.pool, spec_id, SHA, &[item]).await.unwrap();
            store::set_extraction_result(
                &self.pool,
                spec_id,
                SpecKind::Spec,
                SpecStatus::Extracted,
                None,
            )
            .await
            .unwrap();
            Ok(())
        }

        async fn audit(&self, spec_id: &str, lineage: &SpecLineage) -> Result<AuditReport, String> {
            self.log.lock().unwrap().push(format!("audit:{spec_id}"));
            self.lineages.lock().unwrap().push((spec_id.to_owned(), lineage.clone()));
            let done = self.passes.iter().any(|id| id == spec_id);
            let status = if done { SpecStatus::Done } else { SpecStatus::Audited };
            store::set_audit_result(&self.pool, spec_id, status, Some("fake audit"), SHA)
                .await
                .unwrap();
            Ok(AuditReport {
                reviewer_run_id: "reviewer-1".to_owned(),
                verify: if done { VerifyState::Green } else { VerifyState::Red },
                verdict: if done {
                    Verdict::Done
                } else {
                    Verdict::NotDone(vec![NotDoneReason::VerifyNotGreen])
                },
                status,
            })
        }
    }

    fn scope_all() -> FinishScope {
        FinishScope::All { project_id: None }
    }

    async fn status_of(pool: &SqlitePool, id: &str) -> SpecStatus {
        store::get_spec(pool, id).await.unwrap().unwrap().status
    }

    #[tokio::test]
    async fn creates_one_tree_per_spec() {
        let pool = crate::testdb::fresh_pool().await;
        let a = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let b = add_spec(&pool, "beta", SpecStatus::Extracted).await;
        let c = add_spec(&pool, "gamma", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&a, &b, &c]);
        let mut config = cfg();
        config.owned_paths = disjoint(&[&a, &b, &c]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        let requests = launcher.requests();
        assert_eq!(requests.len(), 3, "exactly one tree per spec");
        let mut specs: Vec<_> = requests.iter().map(|r| r.spec_id.clone()).collect();
        specs.sort();
        let mut expected = vec![a.clone(), b.clone(), c.clone()];
        expected.sort();
        assert_eq!(specs, expected);
        let mut ids: Vec<_> = requests.iter().map(|r| r.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3, "every tree has its own id");
        let mut branches: Vec<_> = requests.iter().map(|r| r.feature_branch.clone()).collect();
        branches.sort();
        branches.dedup();
        assert_eq!(branches.len(), 3, "every tree has its own branch");
        for request in &requests {
            assert_ne!(request.feature_branch, "master", "never a tree over master");
            assert_eq!(request.project_root, "/repo");
            assert_eq!(request.round, 1);
            assert!(request.goal.contains(".ai/specs/2026-01-01-"), "goal names the spec file");
        }
        assert_eq!(report.trees.len(), 3);
        for id in [&a, &b, &c] {
            assert_eq!(status_of(&pool, id).await, SpecStatus::Done);
            let spec = store::get_spec(&pool, id).await.unwrap().unwrap();
            assert!(spec.tree_id.is_some(), "the spec records its tree");
        }
    }

    #[tokio::test]
    async fn respects_dependency_order() {
        let pool = crate::testdb::fresh_pool().await;
        // Inserted in the reverse of the order they must run in.
        let a = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let b = add_spec(&pool, "beta", SpecStatus::Extracted).await;
        let c = add_spec(&pool, "gamma", SpecStatus::Extracted).await;
        store::add_dep(&pool, &a, &b, DepSource::Owner).await.unwrap();
        store::add_dep(&pool, &b, &c, DepSource::Extracted).await.unwrap();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&a, &b, &c]);
        let mut config = cfg();
        config.max_parallel = 3;
        config.owned_paths = disjoint(&[&a, &b, &c]);

        finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        let order: Vec<_> = launcher.requests().iter().map(|r| r.spec_id.clone()).collect();
        assert_eq!(order, vec![c, b, a]);
        assert_eq!(launcher.max_in_flight.load(Ordering::SeqCst), 1, "a chain never overlaps");
    }

    #[tokio::test]
    async fn skips_specs_already_done() {
        let pool = crate::testdb::fresh_pool().await;
        let done = add_spec(&pool, "finished", SpecStatus::Done).await;
        let todo = add_spec(&pool, "todo", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&done, &todo]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        let launched: Vec<_> = launcher.requests().iter().map(|r| r.spec_id.clone()).collect();
        assert_eq!(launched, vec![todo]);
        assert_eq!(report.skipped, vec![done.clone()]);
        assert!(
            !log.lock().unwrap().iter().any(|entry| entry.ends_with(&done)),
            "a done spec is neither extracted, audited nor launched"
        );
    }

    #[tokio::test]
    async fn extracts_and_audits_discovered_specs_before_any_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let ready = add_spec(&pool, "ready", SpecStatus::Extracted).await;
        let fresh_one = add_spec(&pool, "fresh-one", SpecStatus::Discovered).await;
        let fresh_two = add_spec(&pool, "fresh-two", SpecStatus::Discovered).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        // Nothing passes the pre-tree audit, so every spec needs a tree.
        let prep = FakePreparer::new(&pool, &log, &[]);
        let mut config = cfg();
        config.max_rounds = 1;
        config.owned_paths = disjoint(&[&ready, &fresh_one, &fresh_two]);

        finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        let log = log.lock().unwrap().clone();
        let first_tree = log.iter().position(|e| e.starts_with("tree:")).expect("trees ran");
        for fresh in [&fresh_one, &fresh_two] {
            let extract = log.iter().position(|e| *e == format!("extract:{fresh}"));
            let audit = log.iter().position(|e| *e == format!("audit:{fresh}"));
            let (extract, audit) = (extract.expect("extracted"), audit.expect("audited"));
            assert!(extract < audit, "extract before audit for {fresh}");
            assert!(audit < first_tree, "audit of {fresh} before any tree");
        }
        assert!(
            !log.iter().any(|e| *e == format!("extract:{ready}")),
            "an already extracted spec is not extracted again"
        );
    }

    #[tokio::test]
    async fn runs_independent_specs_in_parallel_up_to_the_budget() {
        let pool = crate::testdb::fresh_pool().await;
        let ids = [
            add_spec(&pool, "one", SpecStatus::Extracted).await,
            add_spec(&pool, "two", SpecStatus::Extracted).await,
            add_spec(&pool, "three", SpecStatus::Extracted).await,
            add_spec(&pool, "four", SpecStatus::Extracted).await,
        ];
        let refs: Vec<&String> = ids.iter().collect();
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &refs);
        let mut config = cfg();
        config.max_parallel = 2;
        config.owned_paths = disjoint(&refs);

        finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 4);
        assert_eq!(
            launcher.max_in_flight.load(Ordering::SeqCst),
            2,
            "independent specs overlap, but never beyond the budget"
        );
    }

    #[tokio::test]
    async fn serialises_specs_with_overlapping_paths() {
        let pool = crate::testdb::fresh_pool().await;
        let a = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let b = add_spec(&pool, "beta", SpecStatus::Extracted).await;
        let c = add_spec(&pool, "gamma", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&a, &b, &c]);
        let mut config = cfg();
        config.max_parallel = 4;
        // a and b write under the same directory; c has an unknown scope (no entry).
        config.owned_paths.insert(a.clone(), vec!["core/src/shared/".to_owned()]);
        config.owned_paths.insert(b.clone(), vec!["core/src/shared/mod.rs".to_owned()]);

        finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(launcher.requests().len(), 3);
        assert_eq!(
            launcher.max_in_flight.load(Ordering::SeqCst),
            1,
            "overlapping and unknown scopes never run together"
        );
    }

    #[tokio::test]
    async fn third_failed_round_becomes_needs_owner() {
        let pool = crate::testdb::fresh_pool().await;
        let stubborn = add_spec(&pool, "stubborn", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        // The audit never passes.
        let prep = FakePreparer::new(&pool, &log, &[]);

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();

        let rounds: Vec<u32> = launcher.requests().iter().map(|r| r.round).collect();
        assert_eq!(rounds, vec![1, 2], "at most two rounds, never a third tree");
        assert_eq!(status_of(&pool, &stubborn).await, SpecStatus::NeedsOwner);
        let spec = store::get_spec(&pool, &stubborn).await.unwrap().unwrap();
        assert!(spec.status_reason.is_some_and(|r| !r.is_empty()), "a reason is recorded");
        assert_eq!(report.needs_owner, vec![stubborn.clone()]);
        assert!(report.done.is_empty());
        // The second round's audit sees the lineage of both trees.
        let lineages = prep.lineages.lock().unwrap();
        let (_, last) = lineages.last().unwrap();
        assert!(last.run_ids.iter().any(|r| r.ends_with("-r1")));
        assert!(last.run_ids.iter().any(|r| r.ends_with("-r2")));
    }

    #[tokio::test]
    async fn finish_all_with_nothing_to_do_is_a_noop() {
        let pool = crate::testdb::fresh_pool().await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[]);

        let empty = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();
        assert_eq!(empty, FinishReport::default());

        let done = add_spec(&pool, "finished", SpecStatus::Done).await;
        let again = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &cfg(),
            scope_all(),
        )
        .await
        .unwrap();
        assert!(again.trees.is_empty());
        assert_eq!(again.skipped, vec![done.clone()]);
        assert!(log.lock().unwrap().is_empty(), "nothing was started");
        assert_eq!(status_of(&pool, &done).await, SpecStatus::Done);
    }

    #[tokio::test]
    async fn never_a_tree_over_the_integration_branch() {
        let pool = crate::testdb::fresh_pool().await;
        let risky = add_spec(&pool, "risky", SpecStatus::Extracted).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&risky]);
        let mut config = cfg();
        // The branch derived for the spec is exactly the integration branch.
        config.integration_branch = derived_branch(&path_of("risky"));

        let report = finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert!(launcher.requests().is_empty(), "no tree is created over the integration branch");
        assert_eq!(report.refused, vec![risky.clone()]);
        assert_ne!(status_of(&pool, &risky).await, SpecStatus::Done);
        assert!(derived_branch(&path_of("risky")).starts_with("spec/"));
    }

    #[tokio::test]
    async fn spec_files_are_never_altered() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("2026-01-01-sealed.md");
        let bytes = b"# Sealed\n\n- [ ] it works\n".to_vec();
        std::fs::write(&file, &bytes).unwrap();
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "sealed", SpecStatus::Discovered).await;
        let log = Log::default();
        let launcher = FakeLauncher::new(&log);
        let prep = FakePreparer::new(&pool, &log, &[&id]);
        let mut config = cfg();
        config.worktree = dir.path().to_path_buf();
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();

        finish_with(
            &pool,
            &FinishDeps { launcher: &launcher, preparer: &prep },
            &config,
            scope_all(),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&file).unwrap(), bytes, "the spec file is byte-identical");
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "finish creates nothing beside the spec");
        let source = include_str!("finish.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for needle in ["fs::write", "OpenOptions", "File::create", "remove_file", "rename("] {
            assert!(!production.contains(needle), "production code must not use {needle}");
        }
    }

    #[test]
    fn real_reviewer_adapter_requests_a_no_tools_run() {
        let request = no_tools_run_request("review this", Some("proj"), Some("/repo"));

        assert_eq!(request.prompt, "review this");
        assert_eq!(request.project_id.as_deref(), Some("proj"));
        assert_eq!(request.cwd.as_deref(), Some("/repo"));
        assert_eq!(request.tool_policy, ToolPolicy::None, "the reviewer has no tools at all");
        assert!(request.wait_for_final_text, "the adapter waits for the final text");
        assert!(!request.steerable, "nobody talks to a reviewer after it starts");
    }

    #[tokio::test]
    async fn extraction_run_ids_are_persisted() {
        let pool = crate::testdb::fresh_pool().await;
        let id = add_spec(&pool, "alpha", SpecStatus::Extracted).await;
        let other = add_spec(&pool, "beta", SpecStatus::Extracted).await;

        record_extraction_run(&pool, &id, "ex-1").await.unwrap();
        record_extraction_run(&pool, &id, "ex-2").await.unwrap();
        record_extraction_run(&pool, &id, "ex-1").await.unwrap();
        record_extraction_run(&pool, &other, "ex-9").await.unwrap();

        assert_eq!(extraction_run_ids(&pool, &id).await.unwrap(), vec!["ex-1", "ex-2"]);
        assert_eq!(extraction_run_ids(&pool, &other).await.unwrap(), vec!["ex-9"]);
        assert!(extraction_run_ids(&pool, "unknown").await.unwrap().is_empty());

        // A later audit counts them as part of the spec's own lineage.
        let outcome = TreeOutcome {
            run_ids: vec!["tree-run".to_owned()],
            lineage_roots: vec!["tree-root".to_owned()],
        };
        let lineage = lineage_for(&pool, &id, &[outcome]).await.unwrap();
        for expected in ["ex-1", "ex-2", "tree-run"] {
            assert!(lineage.run_ids.iter().any(|r| r == expected), "missing {expected}");
        }
        assert!(!lineage.run_ids.iter().any(|r| r == "ex-9"));
        assert!(lineage.lineage_roots.iter().any(|r| r == "tree-root"));
        let _ = spec_id(None, "unused");
    }
}
