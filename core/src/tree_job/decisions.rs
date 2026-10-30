//! Decisions a tree job asks its owner for: parking a branch on a question, and resuming it from
//! the answer. Each one is a row of `decisions` (migration 0263).

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset};
    use sqlx::SqlitePool;

    use super::*;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::store::{self, TreeJob};

    const CREATED: &str = "2026-10-30T10:00:00+00:00";
    /// Long before any clock reading a test can take, so a rewritten `at` cannot pass unnoticed.
    const OLD: &str = "2020-01-01T00:00:00+00:00";
    const T1: &str = "2026-10-30T11:00:00+00:00";
    const T2: &str = "2026-10-30T12:00:00+00:00";
    const T3: &str = "2026-10-30T13:00:00+00:00";

    const TREE: &str = "job-1";
    const ROOT: &str = "job-1-root";
    const BRANCH: &str = "api";
    const NODE: &str = "node-api";
    const OTHER_BRANCH: &str = "ui";
    const OTHER_NODE: &str = "node-ui";
    const QUESTION: &str = "pick A or B";
    const ANSWER: &str = "Pick A";
    const CHANNEL: &str = "telegram";

    // ----- fixtures -----

    async fn seed_node(
        pool: &SqlitePool,
        id: &str,
        kind: &str,
        parent_id: Option<&str>,
        depth: i64,
    ) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status,
                                     tree_job_id, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, 'Root', 'goal', 'running', ?, ?, ?)",
        )
        .bind(id)
        .bind(ROOT)
        .bind(parent_id)
        .bind(depth)
        .bind(kind)
        .bind(TREE)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Stores the job `TREE` with one branch per `(name, state, node id)`, and the node rows behind
    /// the job and its branches under the same ids.
    async fn seed_tree(pool: &SqlitePool, branches: &[(&str, &str, &str)]) {
        seed_node(pool, ROOT, "root", None, 0).await;
        let mut manifest = Manifest::new(TREE, "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        for &(name, state, node_id) in branches {
            seed_node(pool, node_id, "orchestrator", Some(ROOT), 1).await;
            let mut branch = Branch::new(name, &[format!("core/src/{name}.rs")], "core", None);
            branch.state = state.to_owned();
            branch.node_id = Some(node_id.to_owned());
            manifest.branches.insert(name.to_owned(), branch);
        }
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: ROOT.to_owned(),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: CREATED.to_owned(),
            updated_at: CREATED.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
    }

    /// The job `TREE` with one branch `BRANCH` in `state`, behind the node `NODE`.
    async fn seed_one_branch(pool: &SqlitePool, state: &str) {
        seed_tree(pool, &[(BRANCH, state, NODE)]).await;
    }

    /// The job `TREE` with the branches `BRANCH` and `OTHER_BRANCH`, in these states.
    async fn seed_two_branches(pool: &SqlitePool, state: &str, other: &str) {
        let branches = [(BRANCH, state, NODE), (OTHER_BRANCH, other, OTHER_NODE)];
        seed_tree(pool, &branches).await;
    }

    async fn job(pool: &SqlitePool) -> TreeJob {
        store::load(pool, TREE).await.unwrap().unwrap()
    }

    async fn branch_of(pool: &SqlitePool, name: &str) -> Branch {
        job(pool).await.manifest.branches[name].clone()
    }

    /// The row as the database holds it, read past the module's own `list` and `open`.
    async fn stored(pool: &SqlitePool, id: &str) -> Decision {
        sqlx::query_as(
            "SELECT id, tree_id, node_id, question, answer, origin, at, status \
             FROM decisions WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn decision_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM decisions")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn feed_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Every tree line in the feed, oldest first: `(kind, summary)`.
    async fn tree_lines(pool: &SqlitePool) -> Vec<(String, String)> {
        sqlx::query_as("SELECT kind, summary FROM feed WHERE kind LIKE 'work_tree_%' ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    fn row(
        id: &str,
        at: &str,
        status: &str,
        answer_text: Option<&str>,
        origin: Option<&str>,
    ) -> Decision {
        Decision {
            id: id.to_owned(),
            tree_id: Some(TREE.to_owned()),
            node_id: Some(NODE.to_owned()),
            question: format!("question of {id}"),
            answer: answer_text.map(str::to_owned),
            origin: origin.map(str::to_owned),
            at: at.to_owned(),
            status: status.to_owned(),
        }
    }

    async fn seed_decision(pool: &SqlitePool, decision: &Decision) {
        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&decision.id)
        .bind(&decision.tree_id)
        .bind(&decision.node_id)
        .bind(&decision.question)
        .bind(&decision.answer)
        .bind(&decision.origin)
        .bind(&decision.at)
        .bind(&decision.status)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn backdate(pool: &SqlitePool, id: &str) {
        sqlx::query("UPDATE decisions SET at = ? WHERE id = ?")
            .bind(OLD)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// The columns of an index, in index order.
    async fn index_columns(pool: &SqlitePool, index: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
            .bind(index)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    fn instant(at: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(at).unwrap()
    }

    /// Open and answered rows, deliberately not inserted in the order they read back. `b-answered`
    /// and `d-open` share a time, so `id` decides between them.
    fn mixed_rows() -> Vec<Decision> {
        vec![
            row("c-open", T3, STATUS_OPEN, None, Some(ORIGIN_AGENT)),
            row("a-answered", T1, STATUS_ANSWERED, Some("yes"), Some(CHANNEL)),
            row("d-open", T2, STATUS_OPEN, None, Some(ORIGIN_AGENT)),
            row("b-answered", T2, STATUS_ANSWERED, Some("no"), Some("shell")),
        ]
    }

    fn ids(decisions: &[Decision]) -> Vec<&str> {
        decisions
            .iter()
            .map(|decision| decision.id.as_str())
            .collect()
    }

    // ----- the migration -----

    #[tokio::test]
    async fn migration_gives_decisions_the_exact_shape() {
        let pool = crate::testdb::fresh_pool().await;

        let columns: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT name, type, \"notnull\", pk FROM pragma_table_info('decisions') ORDER BY cid",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        // `id` reads as nullable: SQLite only implies NOT NULL on an INTEGER PRIMARY KEY.
        let expected: Vec<(String, String, i64, i64)> = [
            ("id", 0, 1),
            ("tree_id", 0, 0),
            ("node_id", 0, 0),
            ("question", 1, 0),
            ("answer", 0, 0),
            ("origin", 0, 0),
            ("at", 1, 0),
            ("status", 1, 0),
        ]
        .into_iter()
        .map(|(name, not_null, pk)| (name.to_owned(), "TEXT".to_owned(), not_null, pk))
        .collect();
        assert_eq!(columns, expected);

        assert_eq!(
            index_columns(&pool, "decisions_status").await,
            ["status", "at"]
        );
        assert_eq!(index_columns(&pool, "decisions_tree").await, ["tree_id"]);
    }

    // ----- park -----

    #[tokio::test]
    async fn park_writes_an_open_decision_with_origin_agent() {
        let pool = crate::testdb::fresh_pool().await;
        seed_two_branches(&pool, "building", "red").await;

        let parked = park(&pool, TREE, NODE, QUESTION).await.unwrap();

        assert_eq!(STATUS_OPEN, "open");
        assert_eq!(ORIGIN_AGENT, "agent");
        assert!(!parked.id.is_empty());
        assert_eq!(parked.tree_id.as_deref(), Some(TREE));
        assert_eq!(parked.node_id.as_deref(), Some(NODE));
        assert_eq!(parked.question, QUESTION);
        assert_eq!(parked.answer, None);
        assert_eq!(parked.origin.as_deref(), Some(ORIGIN_AGENT));
        assert_eq!(parked.status, STATUS_OPEN);
        instant(&parked.at);
        assert_eq!(stored(&pool, &parked.id).await, parked);

        let other = park(&pool, TREE, OTHER_NODE, "pick C or D").await.unwrap();
        assert_ne!(other.id, parked.id);
        assert_eq!(decision_rows(&pool).await, 2);
    }

    #[tokio::test]
    async fn park_moves_the_branch_to_parked_and_records_prev_state() {
        // Two states, so that a `prev_state` written as a constant cannot pass.
        for state in ["building", "red"] {
            let pool = crate::testdb::fresh_pool().await;
            seed_two_branches(&pool, state, "fixing").await;

            let parked = park(&pool, TREE, NODE, QUESTION).await.unwrap();

            let branch = branch_of(&pool, BRANCH).await;
            assert_eq!(branch.state, "parked");
            let record = branch.park.expect("the branch holds its park record");
            assert!(record.is_object(), "{record}");
            assert_eq!(record["prev_state"].as_str(), Some(state));
            assert_eq!(record["decision_id"].as_str(), Some(parked.id.as_str()));

            let other = branch_of(&pool, OTHER_BRANCH).await;
            assert_eq!(other.state, "fixing");
            assert_eq!(other.park, None);
        }
    }

    #[tokio::test]
    async fn park_appends_a_work_tree_parked_feed_line_with_the_question() {
        let pool = crate::testdb::fresh_pool().await;
        seed_one_branch(&pool, "building").await;

        park(&pool, TREE, NODE, QUESTION).await.unwrap();

        let lines = tree_lines(&pool).await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].0, "work_tree_parked");
        assert!(
            lines[0].1.contains(&format!("precisa de: {QUESTION}")),
            "{}",
            lines[0].1
        );
    }

    #[tokio::test]
    async fn park_of_an_unknown_tree_or_node_is_no_branch_and_writes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        seed_one_branch(&pool, "building").await;
        let before = job(&pool).await;

        // The last pair names a real node of the tree that no branch stands behind.
        for (tree_id, node_id) in [("no-such-tree", NODE), (TREE, "no-such-node"), (TREE, ROOT)] {
            let error = park(&pool, tree_id, node_id, QUESTION).await.unwrap_err();

            assert!(matches!(error, DecisionError::NoBranch(_)), "{error:?}");
            assert_eq!(decision_rows(&pool).await, 0);
            assert_eq!(feed_rows(&pool).await, 0);
            assert_eq!(job(&pool).await, before);
        }
    }

    #[tokio::test]
    async fn park_of_a_branch_the_engine_refuses_writes_nothing() {
        for state in ["merged", "dropped", "parked"] {
            let pool = crate::testdb::fresh_pool().await;
            seed_one_branch(&pool, state).await;
            let before = job(&pool).await;

            let error = park(&pool, TREE, NODE, QUESTION).await.unwrap_err();

            assert!(
                matches!(error, DecisionError::Engine(_)),
                "{state}: {error:?}"
            );
            assert_eq!(decision_rows(&pool).await, 0, "{state}");
            assert_eq!(feed_rows(&pool).await, 0, "{state}");
            assert_eq!(job(&pool).await, before, "{state}");
        }
    }

    // ----- answer -----

    #[tokio::test]
    async fn answer_records_text_origin_and_time() {
        let pool = crate::testdb::fresh_pool().await;
        seed_one_branch(&pool, "building").await;
        let parked = park(&pool, TREE, NODE, QUESTION).await.unwrap();
        // Whole-second clocks can read the same twice, which would hide an `at` left as it was.
        backdate(&pool, &parked.id).await;

        let answered = answer(&pool, &parked.id, ANSWER, CHANNEL).await.unwrap();

        assert_eq!(STATUS_ANSWERED, "answered");
        assert_eq!(answered.id, parked.id);
        assert_eq!(answered.question, QUESTION);
        assert_eq!(answered.answer.as_deref(), Some(ANSWER));
        assert_eq!(answered.origin.as_deref(), Some(CHANNEL));
        assert_eq!(answered.status, STATUS_ANSWERED);
        assert!(
            instant(&answered.at) >= instant(&parked.at),
            "{}",
            answered.at
        );
        assert!(instant(&answered.at) > instant(OLD), "{}", answered.at);
        assert_eq!(stored(&pool, &parked.id).await, answered);
    }

    #[tokio::test]
    async fn answer_resumes_the_branch_to_its_prev_state_and_clears_the_park() {
        for state in ["building", "red"] {
            let pool = crate::testdb::fresh_pool().await;
            seed_one_branch(&pool, state).await;
            let parked = park(&pool, TREE, NODE, QUESTION).await.unwrap();
            assert_eq!(branch_of(&pool, BRANCH).await.state, "parked");

            answer(&pool, &parked.id, ANSWER, CHANNEL).await.unwrap();

            let branch = branch_of(&pool, BRANCH).await;
            assert_eq!(branch.state, state);
            assert_eq!(branch.park, None);
        }
    }

    #[tokio::test]
    async fn answer_of_an_unknown_id_is_not_found() {
        let pool = crate::testdb::fresh_pool().await;
        seed_one_branch(&pool, "building").await;
        let parked = park(&pool, TREE, NODE, QUESTION).await.unwrap();
        let before = job(&pool).await;

        let error = answer(&pool, "no-such-decision", ANSWER, CHANNEL)
            .await
            .unwrap_err();

        assert!(matches!(error, DecisionError::NotFound(_)), "{error:?}");
        assert_eq!(stored(&pool, &parked.id).await, parked);
        assert_eq!(job(&pool).await, before);
    }

    #[tokio::test]
    async fn a_second_answer_is_refused_and_keeps_the_first() {
        let pool = crate::testdb::fresh_pool().await;
        seed_one_branch(&pool, "building").await;
        let first = park(&pool, TREE, NODE, QUESTION).await.unwrap();
        answer(&pool, &first.id, ANSWER, CHANNEL).await.unwrap();
        // Backdated so that a refused answer rewriting `at` cannot land on the same clock reading.
        backdate(&pool, &first.id).await;
        let kept = stored(&pool, &first.id).await;
        assert_eq!(kept.answer.as_deref(), Some(ANSWER));

        let error = answer(&pool, &first.id, "Pick B", "shell")
            .await
            .unwrap_err();

        assert!(
            matches!(error, DecisionError::AlreadyDecided(_)),
            "{error:?}"
        );
        assert_eq!(stored(&pool, &first.id).await, kept);
        assert_eq!(branch_of(&pool, BRANCH).await.state, "building");

        // Parked again on a new question: the spent decision must not resume the branch.
        let second = park(&pool, TREE, NODE, "pick C or D").await.unwrap();
        let before = job(&pool).await;

        let error = answer(&pool, &first.id, "Pick B", "shell")
            .await
            .unwrap_err();

        assert!(
            matches!(error, DecisionError::AlreadyDecided(_)),
            "{error:?}"
        );
        assert_eq!(stored(&pool, &first.id).await, kept);
        assert_eq!(stored(&pool, &second.id).await, second);
        assert_eq!(job(&pool).await, before);
        assert_eq!(branch_of(&pool, BRANCH).await.state, "parked");
    }

    #[tokio::test]
    async fn answer_without_a_tree_only_updates_the_row() {
        let pool = crate::testdb::fresh_pool().await;
        seed_one_branch(&pool, "building").await;
        let before = job(&pool).await;
        let treeless = Decision {
            tree_id: None,
            node_id: None,
            ..row("treeless", OLD, STATUS_OPEN, None, None)
        };
        seed_decision(&pool, &treeless).await;

        let answered = answer(&pool, &treeless.id, ANSWER, CHANNEL).await.unwrap();

        assert_eq!(answered.tree_id, None);
        assert_eq!(answered.node_id, None);
        assert_eq!(answered.question, treeless.question);
        assert_eq!(answered.answer.as_deref(), Some(ANSWER));
        assert_eq!(answered.origin.as_deref(), Some(CHANNEL));
        assert_eq!(answered.status, STATUS_ANSWERED);
        assert!(instant(&answered.at) > instant(OLD), "{}", answered.at);
        assert_eq!(stored(&pool, &treeless.id).await, answered);
        assert_eq!(job(&pool).await, before);
    }

    // ----- list and open -----

    #[tokio::test]
    async fn list_is_one_list_ordered_by_time_with_origin() {
        let pool = crate::testdb::fresh_pool().await;
        let rows = mixed_rows();
        for decision in &rows {
            seed_decision(&pool, decision).await;
        }

        let listed = list(&pool).await.unwrap();

        assert_eq!(ids(&listed), ["a-answered", "b-answered", "d-open", "c-open"]);
        // Whole rows, so that `origin` and the rest are read back and not only ordered.
        let expected = [
            rows[1].clone(),
            rows[3].clone(),
            rows[2].clone(),
            rows[0].clone(),
        ];
        assert_eq!(listed, expected);
    }

    #[tokio::test]
    async fn open_lists_only_undecided_rows() {
        let pool = crate::testdb::fresh_pool().await;
        assert_eq!(open(&pool).await.unwrap(), Vec::<Decision>::new());
        let rows = mixed_rows();
        for decision in &rows {
            seed_decision(&pool, decision).await;
        }

        let undecided = open(&pool).await.unwrap();

        assert_eq!(ids(&undecided), ["d-open", "c-open"]);
        assert_eq!(undecided, [rows[2].clone(), rows[0].clone()]);
    }
}
