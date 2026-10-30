//! Tree job routes (`/trees*`): the local RPC through which nodes record their state (spec R4).
//!
//! Every refusal is `{error}`, and a field sent as `null` reads as an absent one, the conventions
//! of `http/nodes.rs`. A transition is idempotent per node, transition and key. A manifest is
//! written whole, so every handler that changes one holds a lock on its job.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use sqlx::SqlitePool;
    use tokio::task::JoinHandle;
    use tower::ServiceExt;

    use super::{branch_admission, overlap_outside_base_chain, routes};
    use crate::execution::admission::AdmissionBook;
    use crate::state::AppState;
    use crate::tree_job::budget::{Admission, check_admission};
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::node_run::NodeRunError;
    use crate::tree_job::store::{self, TreeJob, launch_recovering};

    const CREATED: &str = "2026-10-30T10:00:00+00:00";
    const JOB: &str = "job-1";
    const OTHER_JOB: &str = "job-2";
    const BRANCH: &str = "api";
    const NODE: &str = "node-api";
    const KEY: &str = "key-1";
    /// A file a docs branch may own, so that the routes admit its branch whatever the disk holds.
    const SHARED: &str = ".ai/plans/shared.md";
    const OTHER_DOCS: &str = ".ai/plans/other.md";
    const CODE_PATH: &str = "core/src/shared.rs";
    const OTHER_CODE_PATH: &str = "core/src/other.rs";

    const CORE: &str = "core";
    const DOCS: &str = "docs";
    /// A code layer measured at no cost: the route then needs only the floor free on the real disk.
    const FREE_LAYER: &str = "free";
    /// A code layer no real disk can hold: the route refuses it whatever the disk holds.
    const HUGE_LAYER: &str = "huge";
    const HUGE_GB: f64 = 1e12;

    const FLOOR_GB: f64 = 15.0;
    const CAP: usize = 4;
    const PLENTY_GB: f64 = 10_000.0;
    /// `budget::SEED_COST_GB`, spelled out so that these sums read on their own.
    const SEED_GB: f64 = 8.0;

    /// `OPEN_STATES` in `tree_resources.py`: the states that hold a slot and grow on disk.
    const LIVE_STATES: [&str; 7] = [
        "open",
        "building",
        "gating",
        "green",
        "red",
        "reviewing",
        "fixing",
    ];
    const NOT_LIVE_STATES: [&str; 4] = ["queued", "parked", "merged", "dropped"];

    // ----- fixtures -----

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn branch(name: &str, state: &str, layer: &str, paths: &[&str], base: Option<&str>) -> Branch {
        let mut branch = Branch::new(name, &strings(paths), layer, base);
        branch.state = state.to_owned();
        branch
    }

    fn with_node(mut branch: Branch, node_id: &str) -> Branch {
        branch.node_id = Some(node_id.to_owned());
        branch
    }

    fn manifest_of(id: &str, branches: Vec<Branch>, costs: &[(&str, f64)]) -> Manifest {
        let mut manifest = Manifest::new(id, "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        for branch in branches {
            manifest.branches.insert(branch.name.clone(), branch);
        }
        for (layer, cost) in costs {
            manifest.costs_gb.insert((*layer).to_owned(), json!(cost));
        }
        manifest
    }

    fn book(capacity: u32) -> AdmissionBook {
        AdmissionBook::new(
            capacity,
            Duration::from_secs(900),
            Duration::from_secs(7200),
        )
    }

    fn sizes(entries: &[(&str, f64)]) -> BTreeMap<String, f64> {
        entries
            .iter()
            .map(|(name, size)| ((*name).to_owned(), *size))
            .collect()
    }

    /// A new `core` branch against a free book, the default floor and the default cap.
    fn admit(manifest: &Manifest, free_disk_gb: f64, live_sizes: &[(&str, f64)]) -> Admission {
        branch_admission(
            &book(8),
            manifest,
            CORE,
            free_disk_gb,
            FLOOR_GB,
            CAP,
            &sizes(live_sizes),
        )
    }

    fn assert_refused_naming(admission: &Admission, word: &str) {
        assert!(!admission.admit, "expected a refusal, got {admission:?}");
        assert!(
            admission.reasons.iter().any(|reason| reason.contains(word)),
            "no reason names {word:?}: {admission:?}"
        );
    }

    fn assert_admitted(admission: &Admission) {
        assert!(admission.admit, "expected an admission, got {admission:?}");
        assert!(admission.reasons.is_empty(), "{admission:?}");
    }

    /// Runs `work` on a thread of its own, so that a walk that never ends fails instead of hanging
    /// the whole test binary.
    fn within_five_seconds<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the walk did not terminate")
    }

    /// A state whose fake agent stays alive for the test's length, as in `node_run`'s tests.
    async fn test_state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    /// The same, on a pool of several connections, so that two requests really can interleave.
    async fn shared_state() -> AppState {
        let mut state = test_state().await;
        state.pool = crate::testdb::fresh_shared_pool().await;
        state
    }

    fn app(state: &AppState) -> Router {
        routes().with_state(state.clone())
    }

    async fn send(app: &Router, method: &str, uri: &str, body: Body) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(body)
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
        send(app, "POST", uri, Body::from(body.to_string())).await
    }

    async fn post_raw(app: &Router, uri: &str, body: &'static str) -> (StatusCode, Value) {
        send(app, "POST", uri, Body::from(body)).await
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
        send(app, "GET", uri, Body::empty()).await
    }

    fn spawn_post(app: &Router, uri: String, body: Value) -> JoinHandle<(StatusCode, Value)> {
        let app = app.clone();
        tokio::spawn(async move { post(&app, &uri, body).await })
    }

    fn branches_uri(job: &str) -> String {
        format!("/trees/{job}/branches")
    }

    fn transition_uri(job: &str, node: &str) -> String {
        format!("/trees/{job}/nodes/{node}/transition")
    }

    async fn move_branch(
        app: &Router,
        job: &str,
        node: &str,
        transition: &str,
        key: &str,
    ) -> (StatusCode, Value) {
        post(
            app,
            &transition_uri(job, node),
            json!({ "transition": transition, "key": key }),
        )
        .await
    }

    /// Asserts a refusal of exactly `status` with a `{error}` body.
    fn assert_error(response: &(StatusCode, Value), status: StatusCode) {
        assert_eq!(response.0, status, "{:?}", response.1);
        assert!(
            response.1["error"].is_string(),
            "no error text: {:?}",
            response.1
        );
    }

    fn assert_error_naming(response: &(StatusCode, Value), status: StatusCode, name: &str) {
        assert_error(response, status);
        let error = response.1["error"].as_str().unwrap();
        assert!(error.contains(name), "{error:?} does not name {name:?}");
    }

    /// A first call may answer `replayed: false` or leave the field out; it may not claim a replay.
    fn assert_first_call(response: &(StatusCode, Value)) {
        assert_eq!(response.0, StatusCode::OK, "{:?}", response.1);
        assert_ne!(response.1["replayed"], json!(true), "{:?}", response.1);
    }

    fn assert_replayed(response: &(StatusCode, Value)) {
        assert_eq!(response.0, StatusCode::OK, "{:?}", response.1);
        assert_eq!(response.1["replayed"], json!(true), "{:?}", response.1);
    }

    fn tree_body(id: &str) -> Value {
        json!({
            "id": id,
            "goal": "Ship it",
            "feature_branch": "feat/tree",
            "project_root": project_root(),
        })
    }

    /// A directory that exists on the disk the tests run on, with no project config in it, so that
    /// the handler reads real free space and the default floor and cap.
    fn project_root() -> String {
        std::env::temp_dir().to_string_lossy().replace('\\', "/")
    }

    fn docs_branch_body(name: &str, paths: &[&str]) -> Value {
        json!({ "name": name, "paths": paths, "layer": DOCS })
    }

    async fn seed_node(pool: &SqlitePool, id: &str, kind: &str, root_id: &str, job: &str) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, tree_job_id,
                                     created_at, updated_at)
             VALUES (?, ?, 0, ?, 'title', 'goal', 'running', ?, ?, ?)",
        )
        .bind(id)
        .bind(root_id)
        .bind(kind)
        .bind(job)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Stores a job in `state`, its root node, and a node row for each branch that names one.
    async fn seed_job(
        pool: &SqlitePool,
        id: &str,
        state: &str,
        branches: Vec<Branch>,
        costs: &[(&str, f64)],
    ) {
        let mut manifest = manifest_of(id, branches, costs);
        manifest.state = state.to_owned();
        manifest.project_root = project_root();
        let root_node_id = format!("{id}-root");
        seed_node(pool, &root_node_id, "root", &root_node_id, id).await;
        for node_id in manifest.branches.values().filter_map(|b| b.node_id.as_ref()) {
            seed_node(pool, node_id, "orchestrator", &root_node_id, id).await;
        }
        let job = TreeJob {
            id: id.to_owned(),
            root_node_id,
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: state.to_owned(),
            manifest,
            contract: None,
            created_at: CREATED.to_owned(),
            updated_at: CREATED.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
    }

    /// One job holding one code branch `api`, queued, behind the node `node-api`.
    async fn seed_one_branch(pool: &SqlitePool, state: &str) {
        let branch = with_node(branch(BRANCH, state, CORE, &[CODE_PATH], None), NODE);
        seed_job(pool, JOB, "open", vec![branch], &[]).await;
    }

    async fn loaded(pool: &SqlitePool, id: &str) -> TreeJob {
        store::load(pool, id).await.unwrap().unwrap()
    }

    async fn branch_state(pool: &SqlitePool, job: &str, name: &str) -> String {
        loaded(pool, job).await.manifest.branches[name].state.clone()
    }

    async fn job_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_jobs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn run_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn transition_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_run_id(pool: &SqlitePool, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    // ----- branch_admission: growth -----

    #[test]
    fn admission_refuses_when_the_disk_fits_the_new_branch_alone_but_not_after_growth() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let free = FLOOR_GB + SEED_GB;
        // The trap: the plain check, which knows nothing of live branches, would let it through.
        assert!(check_admission(&book(8), &manifest, CORE, free, FLOOR_GB).admit);

        let admission: Admission = admit(&manifest, free, &[]);

        assert_refused_naming(&admission, "disk");
        assert_eq!(admission.cost_gb, SEED_GB);
    }

    #[test]
    fn admission_counts_an_absent_size_as_zero_so_the_whole_cost_is_growth() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let sizes = [("someone-else", SEED_GB)];

        assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB * 2.0, &sizes));
        assert_refused_naming(&admit(&manifest, FLOOR_GB + SEED_GB * 2.0 - 1.0, &sizes), "disk");
    }

    #[test]
    fn admission_lets_a_measured_branch_near_its_cost_grow_only_by_what_is_left() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let sizes = [("a", SEED_GB - 0.5)];

        assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB + 0.5, &sizes));
        assert_refused_naming(&admit(&manifest, FLOOR_GB + SEED_GB, &sizes), "disk");
    }

    /// `max(0, cost - size)`: a branch larger than its cost is no credit towards the next one.
    #[test]
    fn admission_takes_a_branch_past_its_cost_as_no_growth_and_no_credit() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[], None)],
            &[],
        );
        let sizes = [("a", 100.0)];

        assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB, &sizes));
        assert_refused_naming(&admit(&manifest, FLOOR_GB + SEED_GB - 1.0, &sizes), "disk");
    }

    #[test]
    fn admission_charges_growth_at_the_live_branch_layer_cost_not_the_new_one() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", "small", &[], None)],
            &[("small", 2.0), ("large", 20.0)],
        );
        let call = |free: f64| {
            branch_admission(
                &book(8),
                &manifest,
                "large",
                free,
                FLOOR_GB,
                CAP,
                &BTreeMap::new(),
            )
        };

        let admitted = call(FLOOR_GB + 20.0 + 2.0);
        assert_admitted(&admitted);
        assert_eq!(admitted.cost_gb, 20.0);
        assert_refused_naming(&call(FLOOR_GB + 20.0 + 1.0), "disk");
    }

    #[test]
    fn admission_sums_the_growth_of_every_live_branch() {
        let manifest = manifest_of(
            "tree-1",
            vec![
                branch("a", "building", CORE, &[], None),
                branch("b", "fixing", CORE, &[], None),
            ],
            &[],
        );
        let sizes = [("a", SEED_GB - 2.0)];
        let enough = FLOOR_GB + SEED_GB + 2.0 + SEED_GB;

        assert_admitted(&admit(&manifest, enough, &sizes));
        assert_refused_naming(&admit(&manifest, enough - 1.0, &sizes), "disk");
    }

    #[test]
    fn admission_counts_growth_for_every_live_state() {
        for state in LIVE_STATES {
            let manifest =
                manifest_of("tree-1", vec![branch("a", state, CORE, &[], None)], &[]);

            let admission = admit(&manifest, FLOOR_GB + SEED_GB, &[]);

            assert!(!admission.admit, "{state}: {admission:?}");
        }
    }

    /// Per `compute_budget`: `parked` and `queued` are not in `OPEN_STATES`, so like `merged` and
    /// `dropped` they neither grow nor take a slot.
    #[test]
    fn admission_counts_no_growth_for_a_branch_that_is_not_live() {
        for state in NOT_LIVE_STATES {
            let manifest =
                manifest_of("tree-1", vec![branch("a", state, CORE, &[], None)], &[]);

            assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB, &[]));
        }
    }

    #[test]
    fn admission_counts_no_growth_for_a_docs_branch_in_any_state() {
        for state in ["queued", "building", "done", "dropped"] {
            let manifest =
                manifest_of("tree-1", vec![branch("a", state, DOCS, &[], None)], &[]);

            assert_admitted(&admit(&manifest, FLOOR_GB + SEED_GB, &[]));
        }
    }

    // ----- branch_admission: cap, book, docs -----

    fn live_branches(count: usize, state: &str, layer: &str) -> Vec<Branch> {
        (0..count)
            .map(|index| branch(&format!("b{index}"), state, layer, &[], None))
            .collect()
    }

    #[test]
    fn admission_refuses_at_the_cap_naming_max_live_branches() {
        let manifest = manifest_of("tree-1", live_branches(CAP, "building", CORE), &[]);

        let admission = admit(&manifest, PLENTY_GB, &[]);

        assert_refused_naming(&admission, "max_live_branches");
    }

    #[test]
    fn admission_admits_one_below_the_cap() {
        let manifest = manifest_of("tree-1", live_branches(CAP - 1, "building", CORE), &[]);

        assert_admitted(&admit(&manifest, PLENTY_GB, &[]));
    }

    #[test]
    fn admission_takes_the_cap_it_is_given() {
        let manifest = manifest_of("tree-1", live_branches(1, "building", CORE), &[]);

        let admission = branch_admission(
            &book(8),
            &manifest,
            CORE,
            PLENTY_GB,
            FLOOR_GB,
            1,
            &BTreeMap::new(),
        );

        assert_refused_naming(&admission, "max_live_branches");
    }

    #[test]
    fn admission_does_not_count_a_branch_that_is_not_live_towards_the_cap() {
        for state in NOT_LIVE_STATES {
            let manifest = manifest_of("tree-1", live_branches(CAP, state, CORE), &[]);

            assert_admitted(&admit(&manifest, PLENTY_GB, &[]));
        }
    }

    #[test]
    fn admission_does_not_count_a_docs_branch_towards_the_cap() {
        let manifest = manifest_of("tree-1", live_branches(CAP, "building", DOCS), &[]);

        assert_admitted(&admit(&manifest, PLENTY_GB, &[]));
    }

    #[test]
    fn admission_refuses_when_the_book_has_no_free_slot() {
        let manifest = manifest_of("tree-1", Vec::new(), &[]);
        let full = book(1);
        full.request(1, "gate", 1);

        let admission = branch_admission(
            &full,
            &manifest,
            CORE,
            PLENTY_GB,
            FLOOR_GB,
            CAP,
            &BTreeMap::new(),
        );

        assert_refused_naming(&admission, "slot");
    }

    #[test]
    fn admission_always_admits_a_docs_layer() {
        let manifest = manifest_of("tree-1", live_branches(CAP, "building", CORE), &[]);
        let full = book(1);
        full.request(1, "gate", 1);

        let admission =
            branch_admission(&full, &manifest, DOCS, 0.0, FLOOR_GB, CAP, &BTreeMap::new());

        assert_admitted(&admission);
        assert_eq!(admission.cost_gb, 0.0);
    }

    // ----- overlap_outside_base_chain -----

    fn holder(name: &str, base: Option<&str>) -> Branch {
        branch(name, "building", CORE, &[CODE_PATH], base)
    }

    fn overlap(manifest: &Manifest, base: Option<&str>) -> Option<String> {
        overlap_outside_base_chain(manifest, &strings(&[CODE_PATH]), base)
    }

    #[test]
    fn overlap_with_the_base_is_exempt() {
        let manifest = manifest_of("tree-1", vec![holder("a", None)], &[]);

        assert_eq!(overlap(&manifest, Some("a")), None);
    }

    #[test]
    fn overlap_with_the_base_of_the_base_is_exempt() {
        let manifest = manifest_of(
            "tree-1",
            vec![holder("grand", None), holder("parent", Some("grand"))],
            &[],
        );

        assert_eq!(overlap(&manifest, Some("parent")), None);
    }

    #[test]
    fn overlap_with_a_stranger_names_it() {
        let manifest = manifest_of("tree-1", vec![holder("z", None)], &[]);

        assert_eq!(overlap(&manifest, None), Some("z".to_owned()));
    }

    /// `overlapping_branch` answers only the first overlap by name, so filtering its answer against
    /// the chain would see `a`, exempt it, and let `z` through.
    #[test]
    fn overlap_finds_the_stranger_behind_a_chain_branch_that_sorts_first() {
        let manifest = manifest_of("tree-1", vec![holder("a", None), holder("z", None)], &[]);
        let paths = strings(&[CODE_PATH]);
        assert_eq!(
            crate::tree_job::engine::overlapping_branch(&manifest, &paths),
            Some("a".to_owned())
        );

        assert_eq!(overlap(&manifest, Some("a")), Some("z".to_owned()));
    }

    #[test]
    fn overlap_does_not_exempt_a_sibling_on_the_same_base() {
        let manifest = manifest_of(
            "tree-1",
            vec![holder("base", None), holder("sibling", Some("base"))],
            &[],
        );

        assert_eq!(overlap(&manifest, Some("base")), Some("sibling".to_owned()));
    }

    #[test]
    fn overlap_walk_terminates_on_a_cycle_and_still_finds_a_stranger() {
        let manifest = manifest_of(
            "tree-1",
            vec![
                holder("a", Some("b")),
                holder("b", Some("a")),
                holder("z", None),
            ],
            &[],
        );

        let found = within_five_seconds(move || overlap(&manifest, Some("a")));

        assert_eq!(found, Some("z".to_owned()));
    }

    #[test]
    fn overlap_walk_terminates_on_a_branch_based_on_itself() {
        let manifest = manifest_of("tree-1", vec![holder("a", Some("a"))], &[]);

        let found = within_five_seconds(move || overlap(&manifest, Some("a")));

        assert_eq!(found, None);
    }

    #[test]
    fn overlap_with_an_unknown_base_exempts_nothing() {
        let manifest = manifest_of("tree-1", vec![holder("a", None)], &[]);

        assert_eq!(overlap(&manifest, Some("ghost")), Some("a".to_owned()));
    }

    #[test]
    fn overlap_keeps_the_chain_up_to_a_base_that_names_a_missing_branch() {
        let manifest = manifest_of("tree-1", vec![holder("a", Some("ghost"))], &[]);

        assert_eq!(overlap(&manifest, Some("a")), None);
    }

    #[test]
    fn overlap_with_no_base_exempts_nothing() {
        let manifest = manifest_of("tree-1", vec![holder("a", None)], &[]);

        assert_eq!(overlap(&manifest, None), Some("a".to_owned()));
    }

    #[test]
    fn overlap_ignores_a_branch_that_released_its_paths() {
        for state in ["merged", "dropped"] {
            let manifest = manifest_of(
                "tree-1",
                vec![branch("a", state, CORE, &[CODE_PATH], None)],
                &[],
            );

            assert_eq!(overlap(&manifest, None), None, "{state}");
        }
    }

    #[test]
    fn overlap_ignores_paths_that_do_not_overlap() {
        let manifest = manifest_of(
            "tree-1",
            vec![branch("a", "building", CORE, &[OTHER_CODE_PATH], None)],
            &[],
        );

        assert_eq!(overlap(&manifest, None), None);
    }

    // ----- POST /trees -----

    #[tokio::test]
    async fn post_trees_opens_a_root_saves_the_job_and_links_the_node() {
        let state = test_state().await;
        let app = app(&state);

        let response = post(&app, "/trees", tree_body(JOB)).await;

        assert_first_call(&response);
        let body = &response.1;
        assert_eq!(body["id"], json!(JOB));
        assert_eq!(body["state"], json!("open"));
        let root_node_id = body["root_node_id"].as_str().expect("a root node id");
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.root_node_id, root_node_id);
        assert_eq!(job.goal, "Ship it");
        assert_eq!(job.feature_branch.as_deref(), Some("feat/tree"));
        assert_eq!(job.state, "open");
        assert_eq!(job.manifest.tree_id, JOB);
        assert_eq!(job.manifest.goal, "Ship it");
        assert_eq!(job.manifest.feature_branch, "feat/tree");
        assert_eq!(job.manifest.project_root, project_root());
        let (kind, depth, root_id, tree_job_id): (String, i64, String, Option<String>) =
            sqlx::query_as("SELECT kind, depth, root_id, tree_job_id FROM work_nodes WHERE id = ?")
                .bind(root_node_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(kind, "root");
        assert_eq!(depth, 0);
        assert_eq!(root_id, root_node_id);
        assert_eq!(tree_job_id.as_deref(), Some(JOB));
    }

    #[tokio::test]
    async fn post_trees_keeps_the_optional_fields() {
        let state = test_state().await;
        let mut body = tree_body(JOB);
        body["project_id"] = json!("project-1");
        body["contract"] = json!("the contract");
        body["dirty_owner_files"] = json!(["notes.txt"]);

        let response = post(&app(&state), "/trees", body).await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.project_id.as_deref(), Some("project-1"));
        assert_eq!(job.contract.as_deref(), Some("the contract"));
        assert_eq!(job.manifest.dirty_owner_files, strings(&["notes.txt"]));
    }

    #[tokio::test]
    async fn post_trees_reads_null_optional_fields_as_absent() {
        let state = test_state().await;
        let mut body = tree_body(JOB);
        body["project_id"] = Value::Null;
        body["contract"] = Value::Null;
        body["dirty_owner_files"] = Value::Null;

        let response = post(&app(&state), "/trees", body).await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.project_id, None);
        assert_eq!(job.contract, None);
        assert!(job.manifest.dirty_owner_files.is_empty());
    }

    #[tokio::test]
    async fn post_trees_replay_returns_the_same_job_and_opens_no_second_root() {
        let state = test_state().await;
        let app = app(&state);
        let first = post(&app, "/trees", tree_body(JOB)).await;
        let added = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;
        assert_first_call(&added);

        let replay = post(&app, "/trees", tree_body(JOB)).await;

        assert_replayed(&replay);
        assert_eq!(replay.1["id"], json!(JOB));
        assert_eq!(replay.1["root_node_id"], first.1["root_node_id"]);
        assert_eq!(replay.1["state"], json!("open"));
        assert_eq!(job_rows(&state.pool).await, 1);
        assert_eq!(node_rows(&state.pool).await, 1);
        // A replay must not rewrite the job from the request body.
        assert!(loaded(&state.pool, JOB).await.manifest.branches.contains_key("notes"));
    }

    #[tokio::test]
    async fn post_trees_refuses_a_missing_or_null_required_field_with_400() {
        let state = test_state().await;
        let app = app(&state);

        for field in ["id", "goal", "feature_branch", "project_root"] {
            let mut absent = tree_body(JOB);
            absent.as_object_mut().unwrap().remove(field);
            let mut null = tree_body(JOB);
            null[field] = Value::Null;

            for body in [absent, null] {
                let response = post(&app, "/trees", body).await;

                assert_error_naming(&response, StatusCode::BAD_REQUEST, field);
            }
        }
        assert_eq!(job_rows(&state.pool).await, 0);
        assert_eq!(node_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn post_trees_refuses_malformed_json_with_400() {
        let state = test_state().await;

        let response = post_raw(&app(&state), "/trees", "{not json").await;

        assert_error(&response, StatusCode::BAD_REQUEST);
        assert_eq!(job_rows(&state.pool).await, 0);
    }

    // ----- GET /trees/{id} -----

    #[tokio::test]
    async fn get_tree_returns_the_job_with_its_manifest() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let (status, body) = get(&app(&state), &format!("/trees/{JOB}")).await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["id"], json!(JOB));
        assert_eq!(body["root_node_id"], json!(format!("{JOB}-root")));
        assert_eq!(body["state"], json!("open"));
        assert_eq!(body["goal"], json!("Ship it"));
        assert_eq!(body["manifest"]["tree_id"], json!(JOB));
        assert_eq!(
            body["manifest"]["branches"][BRANCH]["node_id"],
            json!(NODE)
        );
    }

    #[tokio::test]
    async fn get_tree_of_an_unknown_id_is_404() {
        let state = test_state().await;

        let response = get(&app(&state), "/trees/no-such-job").await;

        assert_error(&response, StatusCode::NOT_FOUND);
    }

    // ----- POST /trees/{id}/branches -----

    #[tokio::test]
    async fn post_branch_adds_a_queued_branch_that_a_read_shows() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        let response = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_first_call(&response);
        let (status, read) = get(&app, &format!("/trees/{JOB}")).await;
        assert_eq!(status, StatusCode::OK, "{read:?}");
        let added = &read["manifest"]["branches"]["notes"];
        assert_eq!(added["state"], json!("queued"));
        assert_eq!(added["layer"], json!(DOCS));
        assert_eq!(added["paths"], json!([SHARED]));
        assert_eq!(added["base"], Value::Null);
    }

    /// The one route test that needs the real disk: `FLOOR_GB` free where the tests run.
    #[tokio::test]
    async fn post_branch_admits_a_code_branch_through_the_handler_sources() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "open", Vec::new(), &[(FREE_LAYER, 0.0)]).await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "api", "paths": [CODE_PATH], "layer": FREE_LAYER }),
        )
        .await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.manifest.branches["api"].state, "queued");
        assert_eq!(job.manifest.branches["api"].layer, FREE_LAYER);
    }

    #[tokio::test]
    async fn post_branch_replay_of_an_identical_body_adds_no_duplicate() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("base", "queued", DOCS, &[OTHER_DOCS], None)],
            &[],
        )
        .await;
        let body = json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "base" });
        assert_first_call(&post(&app, &branches_uri(JOB), body.clone()).await);
        let before = loaded(&state.pool, JOB).await.manifest;

        let replay = post(&app, &branches_uri(JOB), body).await;

        assert_replayed(&replay);
        assert_eq!(loaded(&state.pool, JOB).await.manifest, before);
    }

    #[tokio::test]
    async fn post_branch_replay_reads_a_null_base_as_absent() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;
        let first = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;
        assert_first_call(&first);

        let replay = post(
            &app,
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": null }),
        )
        .await;

        assert_replayed(&replay);
    }

    /// The replay answers before admission: a branch already in the manifest does not ask for room
    /// again, even when a new one of its layer would now be refused.
    #[tokio::test]
    async fn post_branch_replay_answers_even_when_admission_would_now_refuse() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("big", "queued", HUGE_LAYER, &[CODE_PATH], None)],
            &[(HUGE_LAYER, HUGE_GB)],
        )
        .await;

        let replay = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "big", "paths": [CODE_PATH], "layer": HUGE_LAYER }),
        )
        .await;

        assert_replayed(&replay);
    }

    #[tokio::test]
    async fn post_branch_with_the_same_name_and_another_body_is_409() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("base", "queued", DOCS, &[OTHER_DOCS], None)],
            &[],
        )
        .await;
        let added = post(
            &app,
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;
        assert_first_call(&added);
        let before = loaded(&state.pool, JOB).await.manifest;

        for body in [
            json!({ "name": "notes", "paths": [".ai/plans/third.md"], "layer": DOCS }),
            json!({ "name": "notes", "paths": [SHARED], "layer": FREE_LAYER }),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "base" }),
        ] {
            let response = post(&app, &branches_uri(JOB), body.clone()).await;

            assert_error(&response, StatusCode::CONFLICT);
            assert_ne!(response.1["replayed"], json!(true), "{body:?}");
        }
        assert_eq!(loaded(&state.pool, JOB).await.manifest, before);
    }

    #[tokio::test]
    async fn post_branch_overlapping_a_stranger_is_409_naming_it() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("stranger-z", "queued", DOCS, &[SHARED], None)],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_error_naming(&response, StatusCode::CONFLICT, "stranger-z");
        assert!(!loaded(&state.pool, JOB).await.manifest.branches.contains_key("notes"));
    }

    #[tokio::test]
    async fn post_branch_overlapping_its_base_chain_is_200() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![
                branch("grand", "queued", DOCS, &[SHARED], None),
                branch("parent", "queued", DOCS, &[SHARED], Some("grand")),
            ],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "parent" }),
        )
        .await;

        assert_first_call(&response);
        let job = loaded(&state.pool, JOB).await;
        assert_eq!(job.manifest.branches["notes"].base.as_deref(), Some("parent"));
    }

    #[tokio::test]
    async fn post_branch_finds_the_stranger_behind_a_chain_branch_that_sorts_first() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![
                branch("chain-a", "queued", DOCS, &[SHARED], None),
                branch("stranger-z", "queued", DOCS, &[SHARED], None),
            ],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "chain-a" }),
        )
        .await;

        assert_error_naming(&response, StatusCode::CONFLICT, "stranger-z");
    }

    #[tokio::test]
    async fn post_branch_overlapping_a_merged_branch_is_200() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            vec![branch("old", "merged", CORE, &[SHARED], None)],
            &[],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_first_call(&response);
    }

    #[tokio::test]
    async fn post_branch_on_an_unknown_job_is_404() {
        let state = test_state().await;

        let response = post(
            &app(&state),
            &branches_uri("no-such-job"),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_error(&response, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_branch_on_a_finished_job_is_409_and_adds_nothing() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "finished", Vec::new(), &[]).await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_error(&response, StatusCode::CONFLICT);
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    #[tokio::test]
    async fn post_branch_with_an_unknown_base_is_422() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "notes", "paths": [SHARED], "layer": DOCS, "base": "ghost" }),
        )
        .await;

        assert_error_naming(&response, StatusCode::UNPROCESSABLE_ENTITY, "ghost");
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    /// A single tracked path among allowed ones is enough, or a docs branch could reach the tree.
    #[tokio::test]
    async fn post_branch_of_the_docs_layer_with_a_tracked_path_is_422() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        for paths in [vec![CODE_PATH], vec![SHARED, CODE_PATH]] {
            let response = post(
                &app,
                &branches_uri(JOB),
                docs_branch_body("notes", &paths),
            )
            .await;

            assert_error_naming(&response, StatusCode::UNPROCESSABLE_ENTITY, CODE_PATH);
        }
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    /// A layer measured beyond any real disk, so this refusal holds whatever the machine has free.
    #[tokio::test]
    async fn post_branch_not_admitted_for_disk_is_409_with_reasons() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "open", Vec::new(), &[(HUGE_LAYER, HUGE_GB)]).await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "big", "paths": [CODE_PATH], "layer": HUGE_LAYER }),
        )
        .await;

        assert_error(&response, StatusCode::CONFLICT);
        let reasons = response.1["reasons"].as_array().expect("a reasons array");
        assert!(
            reasons.iter().any(|reason| reason.as_str().is_some_and(|text| text.contains("disk"))),
            "{reasons:?}"
        );
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    /// Four live branches at no cost: the default cap refuses whatever the disk holds.
    #[tokio::test]
    async fn post_branch_not_admitted_at_the_cap_is_409_with_reasons() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            live_branches(CAP, "building", FREE_LAYER),
            &[(FREE_LAYER, 0.0)],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            json!({ "name": "api", "paths": [CODE_PATH], "layer": FREE_LAYER }),
        )
        .await;

        assert_error(&response, StatusCode::CONFLICT);
        let reasons = response.1["reasons"].as_array().expect("a reasons array");
        assert!(
            reasons
                .iter()
                .any(|reason| reason.as_str().is_some_and(|text| text.contains("max_live_branches"))),
            "{reasons:?}"
        );
    }

    #[tokio::test]
    async fn post_branch_admits_a_docs_layer_at_the_cap() {
        let state = test_state().await;
        seed_job(
            &state.pool,
            JOB,
            "open",
            live_branches(CAP, "building", FREE_LAYER),
            &[(FREE_LAYER, 0.0)],
        )
        .await;

        let response = post(
            &app(&state),
            &branches_uri(JOB),
            docs_branch_body("notes", &[SHARED]),
        )
        .await;

        assert_first_call(&response);
    }

    #[tokio::test]
    async fn post_branch_refuses_a_missing_or_null_required_field_with_400() {
        let state = test_state().await;
        let app = app(&state);
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        for field in ["name", "paths", "layer"] {
            let mut absent = docs_branch_body("notes", &[SHARED]);
            absent.as_object_mut().unwrap().remove(field);
            let mut null = docs_branch_body("notes", &[SHARED]);
            null[field] = Value::Null;

            for body in [absent, null] {
                let response = post(&app, &branches_uri(JOB), body).await;

                assert_error_naming(&response, StatusCode::BAD_REQUEST, field);
            }
        }
        assert!(loaded(&state.pool, JOB).await.manifest.branches.is_empty());
    }

    #[tokio::test]
    async fn post_branch_refuses_malformed_json_with_400() {
        let state = test_state().await;
        seed_job(&state.pool, JOB, "open", Vec::new(), &[]).await;

        let response = post_raw(&app(&state), &branches_uri(JOB), "{not json").await;

        assert_error(&response, StatusCode::BAD_REQUEST);
    }

    // ----- POST /trees/{id}/nodes/{n}/transition: branch states -----

    #[tokio::test]
    async fn transition_moves_the_branch_of_the_node_and_persists_it() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let (status, body) = move_branch(&app(&state), JOB, NODE, "open", KEY).await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body["result"].is_string(), "{body:?}");
        assert_eq!(body["replayed"], json!(false));
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "open");
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tree_transitions
             WHERE node_id = ? AND transition = 'open' AND key = ?",
        )
        .bind(NODE)
        .bind(KEY)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn transition_replay_returns_the_stored_result_and_does_not_move_the_branch_back() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;
        let (_, first) = move_branch(&app, JOB, NODE, "open", KEY).await;
        let (status, _) = move_branch(&app, JOB, NODE, "building", "key-2").await;
        assert_eq!(status, StatusCode::OK);

        // `building` to `open` is illegal, so a replay that re-applied would answer 409.
        let (status, replay) = move_branch(&app, JOB, NODE, "open", KEY).await;

        assert_eq!(status, StatusCode::OK, "{replay:?}");
        assert_eq!(replay["replayed"], json!(true));
        assert_eq!(replay["result"], first["result"]);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
        assert_eq!(transition_rows(&state.pool).await, 2);
    }

    /// Spec R4: the key is per node and transition, so one key may serve two transitions.
    #[tokio::test]
    async fn transition_with_the_same_key_and_another_transition_is_no_replay() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;
        move_branch(&app, JOB, NODE, "open", KEY).await;

        let (status, body) = move_branch(&app, JOB, NODE, "building", KEY).await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["replayed"], json!(false));
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
    }

    #[tokio::test]
    async fn transition_that_is_illegal_is_409_and_spends_no_key() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;

        let refused = move_branch(&app, JOB, NODE, "merged", KEY).await;

        assert_error(&refused, StatusCode::CONFLICT);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "queued");
        assert_eq!(transition_rows(&state.pool).await, 0);

        // Walked to where `merged` is legal, the very key that was refused now succeeds.
        for (index, step) in ["open", "building", "gating", "green"].into_iter().enumerate() {
            let (status, body) =
                move_branch(&app, JOB, NODE, step, &format!("step-{index}")).await;
            assert_eq!(status, StatusCode::OK, "{step}: {body:?}");
        }
        let (status, body) = move_branch(&app, JOB, NODE, "merged", KEY).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["replayed"], json!(false));
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "merged");
    }

    /// `apply_transition` narrows a parked branch to the state it was parked from; the plain
    /// `can_transition` table would let it resume anywhere parkable.
    #[tokio::test]
    async fn transition_of_a_parked_branch_goes_through_apply_transition() {
        let state = test_state().await;
        let app = app(&state);
        let mut parked = with_node(branch(BRANCH, "parked", CORE, &[CODE_PATH], None), NODE);
        parked.park = Some(json!({ "prev_state": "building" }));
        seed_job(&state.pool, JOB, "open", vec![parked], &[]).await;

        let refused = move_branch(&app, JOB, NODE, "open", KEY).await;
        assert_error(&refused, StatusCode::CONFLICT);

        let (status, body) = move_branch(&app, JOB, NODE, "building", KEY).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
    }

    /// `done` is no member of `BRANCH_STATES`, yet the engine knows it for the docs layer alone.
    #[tokio::test]
    async fn transition_to_done_is_known_and_legal_only_on_a_docs_branch() {
        let state = test_state().await;
        let app = app(&state);
        let docs = with_node(branch("notes", "building", DOCS, &[SHARED], None), "node-notes");
        let code = with_node(branch(BRANCH, "building", CORE, &[CODE_PATH], None), NODE);
        seed_job(&state.pool, JOB, "open", vec![docs, code], &[]).await;

        let (status, body) = move_branch(&app, JOB, "node-notes", "done", KEY).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(branch_state(&state.pool, JOB, "notes").await, "done");

        let refused = move_branch(&app, JOB, NODE, "done", KEY).await;
        assert_error(&refused, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn transition_with_an_unknown_name_is_422_and_spends_no_key() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let response = move_branch(&app(&state), JOB, NODE, "teleport", KEY).await;

        assert_error(&response, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "queued");
        assert_eq!(transition_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn transition_on_a_node_of_another_job_is_404_and_moves_nothing() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;
        let other = with_node(
            branch("web", "queued", CORE, &[OTHER_CODE_PATH], None),
            "node-web",
        );
        seed_job(&state.pool, OTHER_JOB, "open", vec![other], &[]).await;

        let response = move_branch(&app(&state), JOB, "node-web", "open", KEY).await;

        assert_error(&response, StatusCode::NOT_FOUND);
        assert_eq!(branch_state(&state.pool, OTHER_JOB, "web").await, "queued");
        assert_eq!(transition_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn transition_on_an_unknown_node_or_job_is_404() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;

        for (job, node) in [(JOB, "no-such-node"), ("no-such-job", NODE)] {
            let response = move_branch(&app, job, node, "open", KEY).await;

            assert_error(&response, StatusCode::NOT_FOUND);
        }
        assert_eq!(transition_rows(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn transition_refuses_a_missing_or_null_required_field_with_400() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "queued").await;

        for body in [
            json!({ "key": KEY }),
            json!({ "transition": null, "key": KEY }),
            json!({ "transition": "open" }),
            json!({ "transition": "open", "key": null }),
        ] {
            let response = post(&app, &transition_uri(JOB, NODE), body).await;

            assert_error(&response, StatusCode::BAD_REQUEST);
        }
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "queued");
    }

    #[tokio::test]
    async fn transition_refuses_malformed_json_with_400() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "queued").await;

        let response = post_raw(&app(&state), &transition_uri(JOB, NODE), "{not json").await;

        assert_error(&response, StatusCode::BAD_REQUEST);
    }

    // ----- POST /trees/{id}/nodes/{n}/transition: launch -----

    fn launch_body(key: &str) -> Value {
        json!({ "transition": "launch", "key": key, "prompt": "do it", "mode": "shadow" })
    }

    /// `create.rs:388`: worktree mode with no project and no cwd is `CreateRunError::Invalid`,
    /// raised before any run row exists.
    fn failing_launch_body(key: &str) -> Value {
        json!({ "transition": "launch", "key": key, "prompt": "do it", "mode": "worktree" })
    }

    /// The run id a launch answered, which is the id `launch` stores as the transition's result.
    fn launched_run(response: &(StatusCode, Value)) -> i64 {
        assert_eq!(response.0, StatusCode::OK, "{:?}", response.1);
        response.1["result"]
            .as_str()
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("no run id in {:?}", response.1))
    }

    #[tokio::test]
    async fn launch_starts_one_run_and_links_it_on_the_node() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;

        let response = post(
            &app(&state),
            &transition_uri(JOB, NODE),
            launch_body(KEY),
        )
        .await;

        let run_id = launched_run(&response);
        assert_eq!(run_rows(&state.pool).await, 1);
        assert_eq!(node_run_id(&state.pool, NODE).await, Some(run_id.to_string()));
        let (prompt, mode): (String, String) =
            sqlx::query_as("SELECT prompt, mode FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(prompt, "do it");
        assert_eq!(mode, "shadow");
    }

    #[tokio::test]
    async fn launch_replay_returns_the_same_run_and_creates_no_second() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "building").await;
        let first = launched_run(&post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await);

        let replay = launched_run(&post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await);

        assert_eq!(replay, first);
        assert_eq!(run_rows(&state.pool).await, 1);
    }

    /// Decision 4: plain `apply` or `launch` would leave an empty reservation and poison the key.
    #[tokio::test]
    async fn launch_whose_create_fails_refuses_and_the_same_key_then_starts_the_run() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "building").await;

        let (status, body) =
            post(&app, &transition_uri(JOB, NODE), failing_launch_body(KEY)).await;

        assert!(
            status.is_client_error() || status.is_server_error(),
            "{status}: {body:?}"
        );
        assert!(body["error"].is_string(), "{body:?}");
        assert_eq!(run_rows(&state.pool).await, 0);
        assert_eq!(node_run_id(&state.pool, NODE).await, None);

        let retried = post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await;

        let run_id = launched_run(&retried);
        assert_eq!(run_rows(&state.pool).await, 1);
        assert_eq!(node_run_id(&state.pool, NODE).await, Some(run_id.to_string()));

        let replay = post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await;

        assert_eq!(launched_run(&replay), run_id);
        assert_eq!(run_rows(&state.pool).await, 1);
    }

    #[tokio::test]
    async fn launch_without_a_prompt_is_422_and_spends_no_key() {
        let state = test_state().await;
        let app = app(&state);
        seed_one_branch(&state.pool, "building").await;

        for body in [
            json!({ "transition": "launch", "key": KEY, "mode": "shadow" }),
            json!({ "transition": "launch", "key": KEY, "mode": "shadow", "prompt": null }),
        ] {
            let response = post(&app, &transition_uri(JOB, NODE), body).await;

            assert_error_naming(&response, StatusCode::UNPROCESSABLE_ENTITY, "prompt");
        }
        assert_eq!(run_rows(&state.pool).await, 0);
        assert_eq!(transition_rows(&state.pool).await, 0);

        let response = post(&app, &transition_uri(JOB, NODE), launch_body(KEY)).await;

        launched_run(&response);
    }

    #[tokio::test]
    async fn launch_on_a_node_of_another_job_is_404_and_creates_no_run() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;
        let other = with_node(
            branch("web", "building", CORE, &[OTHER_CODE_PATH], None),
            "node-web",
        );
        seed_job(&state.pool, OTHER_JOB, "open", vec![other], &[]).await;

        let response = post(
            &app(&state),
            &transition_uri(JOB, "node-web"),
            launch_body(KEY),
        )
        .await;

        assert_error(&response, StatusCode::NOT_FOUND);
        assert_eq!(run_rows(&state.pool).await, 0);
        assert_eq!(node_run_id(&state.pool, "node-web").await, None);
    }

    #[tokio::test]
    async fn launch_does_not_move_the_branch() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;

        let response = post(
            &app(&state),
            &transition_uri(JOB, NODE),
            launch_body(KEY),
        )
        .await;

        launched_run(&response);
        assert_eq!(branch_state(&state.pool, JOB, BRANCH).await, "building");
    }

    /// The route and the recovering launch must read one key space, or a route replay could start a
    /// second run. This is also what types the sibling leaf's signature.
    #[tokio::test]
    async fn launch_route_and_launch_recovering_share_the_key() {
        let state = test_state().await;
        seed_one_branch(&state.pool, "building").await;
        let routed = launched_run(
            &post(
                &app(&state),
                &transition_uri(JOB, NODE),
                launch_body(KEY),
            )
            .await,
        );

        let direct: Result<i64, NodeRunError> = launch_recovering(
            &state,
            NODE,
            KEY,
            "do it".to_owned(),
            None,
            None,
            "shadow",
        )
        .await;

        assert_eq!(direct.unwrap(), routed);
        assert_eq!(run_rows(&state.pool).await, 1);
    }

    // ----- concurrency -----

    const ROUNDS: usize = 20;

    /// A load and a whole-document write with no lock between them would lose one of the two moves.
    /// Several rounds, because one round may happen not to overlap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_transitions_on_two_branches_both_survive() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            let (left_node, right_node) = (format!("left-{round}"), format!("right-{round}"));
            let left = with_node(branch("left", "queued", CORE, &[CODE_PATH], None), &left_node);
            let right = with_node(
                branch("right", "queued", CORE, &[OTHER_CODE_PATH], None),
                &right_node,
            );
            seed_job(&state.pool, &job, "open", vec![left, right], &[]).await;
            let body = json!({ "transition": "open", "key": KEY });

            let (first, second) = tokio::join!(
                spawn_post(&app, transition_uri(&job, &left_node), body.clone()),
                spawn_post(&app, transition_uri(&job, &right_node), body),
            );

            assert_eq!(first.unwrap().0, StatusCode::OK, "round {round}");
            assert_eq!(second.unwrap().0, StatusCode::OK, "round {round}");
            let manifest = loaded(&state.pool, &job).await.manifest;
            assert_eq!(manifest.branches["left"].state, "open", "round {round}");
            assert_eq!(manifest.branches["right"].state, "open", "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_transitions_with_one_key_move_the_branch_once() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            let node = format!("node-{round}");
            let only = with_node(branch(BRANCH, "queued", CORE, &[CODE_PATH], None), &node);
            seed_job(&state.pool, &job, "open", vec![only], &[]).await;
            let body = json!({ "transition": "open", "key": KEY });

            let (first, second) = tokio::join!(
                spawn_post(&app, transition_uri(&job, &node), body.clone()),
                spawn_post(&app, transition_uri(&job, &node), body),
            );

            let (first, second) = (first.unwrap(), second.unwrap());
            assert_eq!(first.0, StatusCode::OK, "round {round}: {:?}", first.1);
            assert_eq!(second.0, StatusCode::OK, "round {round}: {:?}", second.1);
            assert_eq!(first.1["result"], second.1["result"], "round {round}");
            assert_ne!(first.1["replayed"], second.1["replayed"], "round {round}");
            assert_eq!(branch_state(&state.pool, &job, BRANCH).await, "open");
        }
    }

    /// Docs branches, so that the outcome turns on the overlap and not on the disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_branches_with_overlapping_paths_admit_at_most_one() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            seed_job(&state.pool, &job, "open", Vec::new(), &[]).await;

            let (first, second) = tokio::join!(
                spawn_post(&app, branches_uri(&job), docs_branch_body("left", &[SHARED])),
                spawn_post(&app, branches_uri(&job), docs_branch_body("right", &[SHARED])),
            );

            let mut statuses = [first.unwrap().0, second.unwrap().0];
            statuses.sort();
            assert_eq!(
                statuses,
                [StatusCode::OK, StatusCode::CONFLICT],
                "round {round}"
            );
            let manifest = loaded(&state.pool, &job).await.manifest;
            assert_eq!(manifest.branches.len(), 1, "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_branches_with_separate_paths_both_survive() {
        let state = shared_state().await;
        let app = app(&state);

        for round in 0..ROUNDS {
            let job = format!("job-{round}");
            seed_job(&state.pool, &job, "open", Vec::new(), &[]).await;

            let (first, second) = tokio::join!(
                spawn_post(&app, branches_uri(&job), docs_branch_body("left", &[SHARED])),
                spawn_post(&app, branches_uri(&job), docs_branch_body("right", &[OTHER_DOCS])),
            );

            assert_eq!(first.unwrap().0, StatusCode::OK, "round {round}");
            assert_eq!(second.unwrap().0, StatusCode::OK, "round {round}");
            let manifest = loaded(&state.pool, &job).await.manifest;
            assert_eq!(manifest.branches.len(), 2, "round {round}");
        }
    }
}
