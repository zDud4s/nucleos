//! Spec backlog storage: reads and writes over `specs`, `spec_criteria`, `spec_deps` and
//! `spec_questions`, and the owner's single list of decisions.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_backlog::model::{
        spec_id, CriterionKind, CriterionStatus, DepSource, NewCriterion, Spec, SpecStatus,
    };
    use sqlx::SqlitePool;

    const PATH: &str = ".ai/specs/2026-01-01-alpha.md";
    const OTHER_PATH: &str = ".ai/specs/2026-01-02-beta.md";
    /// Every upsert path must hold for both: the unique index over a nullable `project_id` treats
    /// NULLs as distinct, so it catches nothing in the `None` case.
    const PROJECTS: [Option<&str>; 2] = [Some("proj-1"), None];
    const OLD: &str = "2020-01-01T00:00:00+00:00";

    async fn exec(pool: &SqlitePool, sql: &'static str, binds: &[&str]) {
        let mut query = sqlx::query(sql);
        for value in binds {
            query = query.bind((*value).to_owned());
        }
        query.execute(pool).await.unwrap();
    }

    async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn open_items(texts: &[&str]) -> Vec<NewCriterion> {
        texts
            .iter()
            .map(|text| NewCriterion {
                text: (*text).to_owned(),
                kind: CriterionKind::Behavior,
                verify_hint: None,
                status: CriterionStatus::Open,
            })
            .collect()
    }

    async fn seed(pool: &SqlitePool, project: Option<&str>, path: &str) -> Spec {
        upsert_by_path(pool, project, path, "Title", "sha-1")
            .await
            .unwrap()
    }

    async fn criteria_texts(pool: &SqlitePool, spec: &str) -> Vec<String> {
        list_criteria(pool, spec)
            .await
            .unwrap()
            .into_iter()
            .map(|criterion| criterion.text)
            .collect()
    }

    async fn mark_first_criterion_met(pool: &SqlitePool, spec: &str) {
        exec(
            pool,
            "UPDATE spec_criteria SET status = 'met', evidence = 'seen in run r1', \
             reviewer_run_id = 'r1', checked_at = '2026-10-30T10:00:00+00:00' \
             WHERE spec_id = ? AND ordinal = 1",
            &[spec],
        )
        .await;
    }

    async fn source_of(pool: &SqlitePool, spec: &str, target: &str) -> Vec<DepSource> {
        list_deps(pool, spec)
            .await
            .unwrap()
            .into_iter()
            .filter(|dep| dep.spec_id == spec && dep.depends_on == target)
            .map(|dep| dep.source)
            .collect()
    }

    async fn recreate_decisions(pool: &SqlitePool) {
        exec(pool, "DROP TABLE IF EXISTS decisions", &[]).await;
        exec(
            pool,
            "CREATE TABLE decisions (id TEXT PRIMARY KEY, tree_id TEXT, node_id TEXT, \
             question TEXT NOT NULL, answer TEXT, origin TEXT, at TEXT NOT NULL, \
             status TEXT NOT NULL)",
            &[],
        )
        .await;
    }

    #[tokio::test]
    async fn upsert_by_path_keeps_id() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let expected_id = spec_id(project, PATH);
            assert!(get_spec(&pool, &expected_id).await.unwrap().is_none());

            let first = upsert_by_path(&pool, project, PATH, "Alpha", "sha-1")
                .await
                .unwrap();
            assert_eq!(first.id, expected_id);
            assert_eq!(first.project_id.as_deref(), project);
            assert_eq!(first.path, PATH);
            assert_eq!(first.content_sha, "sha-1");
            assert_eq!(first.status, SpecStatus::Discovered);

            // Backdated, so a delete-and-insert cannot pass by landing on the same clock reading.
            exec(
                &pool,
                "UPDATE specs SET created_at = ? WHERE id = ?",
                &[OLD, first.id.as_str()],
            )
            .await;

            for sha in ["sha-1", "sha-2"] {
                let again = upsert_by_path(&pool, project, PATH, "Alpha", sha)
                    .await
                    .unwrap();
                assert_eq!(again.id, first.id, "{project:?} {sha}");
                assert_eq!(again.created_at, OLD, "{project:?} {sha}");
                assert_eq!(again.content_sha, sha, "{project:?} {sha}");
            }

            assert_eq!(count(&pool, "SELECT COUNT(*) FROM specs").await, 1);
            let stored = get_spec(&pool, &first.id).await.unwrap().unwrap();
            assert_eq!(stored.created_at, OLD);
            assert_eq!(stored.content_sha, "sha-2");
        }
    }

    #[tokio::test]
    async fn same_path_in_two_projects_are_two_specs() {
        let pool = crate::testdb::fresh_pool().await;
        let one = seed(&pool, Some("proj-1"), PATH).await;
        let two = seed(&pool, Some("proj-2"), PATH).await;
        let none = seed(&pool, None, PATH).await;
        assert_ne!(one.id, two.id);
        assert_ne!(one.id, none.id);
        assert_ne!(two.id, none.id);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM specs").await, 3);

        // A change in one project reaches neither the other project nor the project-less row.
        upsert_by_path(&pool, Some("proj-1"), PATH, "Title", "sha-2")
            .await
            .unwrap();
        for (id, project, sha) in [
            (&one.id, Some("proj-1"), "sha-2"),
            (&two.id, Some("proj-2"), "sha-1"),
            (&none.id, None, "sha-1"),
        ] {
            let stored = get_spec(&pool, id).await.unwrap().unwrap();
            assert_eq!(stored.project_id.as_deref(), project);
            assert_eq!(stored.content_sha, sha, "{project:?}");
        }
    }

    #[tokio::test]
    async fn changed_content_sha_marks_criteria_stale() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let spec = seed(&pool, project, PATH).await;
            let other = seed(&pool, project, OTHER_PATH).await;
            replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["first", "second"]))
                .await
                .unwrap();
            replace_criteria(&pool, &other.id, "sha-1", &open_items(&["untouched"]))
                .await
                .unwrap();
            mark_first_criterion_met(&pool, &spec.id).await;
            exec(&pool, "UPDATE specs SET status = 'audited'", &[]).await;

            let updated = upsert_by_path(&pool, project, PATH, "Title", "sha-2")
                .await
                .unwrap();
            assert_eq!(updated.id, spec.id);
            assert_eq!(updated.content_sha, "sha-2");
            assert_eq!(updated.status, SpecStatus::Discovered);
            let stored = get_spec(&pool, &spec.id).await.unwrap().unwrap();
            assert_eq!(stored.status, SpecStatus::Discovered);

            let criteria = list_criteria(&pool, &spec.id).await.unwrap();
            assert_eq!(criteria.len(), 2);
            for criterion in &criteria {
                assert_eq!(criterion.status, CriterionStatus::Stale, "{}", criterion.text);
                assert_eq!(criterion.spec_sha, "sha-1", "{}", criterion.text);
            }

            // The change belongs to this spec only.
            let bystander = get_spec(&pool, &other.id).await.unwrap().unwrap();
            assert_eq!(bystander.status, SpecStatus::Audited);
            let untouched = list_criteria(&pool, &other.id).await.unwrap();
            assert_eq!(untouched.len(), 1);
            assert_eq!(untouched[0].status, CriterionStatus::Open);
        }
    }

    #[tokio::test]
    async fn unchanged_content_sha_keeps_status_and_criteria() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let spec = seed(&pool, project, PATH).await;
            replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["first", "second"]))
                .await
                .unwrap();
            mark_first_criterion_met(&pool, &spec.id).await;
            exec(
                &pool,
                "UPDATE specs SET status = 'audited', status_reason = 'all met' WHERE id = ?",
                &[spec.id.as_str()],
            )
            .await;

            let again = upsert_by_path(&pool, project, PATH, "Title", "sha-1")
                .await
                .unwrap();
            assert_eq!(again.status, SpecStatus::Audited);
            assert_eq!(again.status_reason.as_deref(), Some("all met"));

            let criteria = list_criteria(&pool, &spec.id).await.unwrap();
            assert_eq!(criteria.len(), 2);
            assert_eq!(criteria[0].status, CriterionStatus::Met);
            assert_eq!(criteria[0].evidence.as_deref(), Some("seen in run r1"));
            assert_eq!(criteria[0].reviewer_run_id.as_deref(), Some("r1"));
            assert_eq!(criteria[1].status, CriterionStatus::Open);
        }
    }

    #[tokio::test]
    async fn upsert_keeps_tree_and_audit_fields() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let spec = seed(&pool, project, PATH).await;
            exec(
                &pool,
                "UPDATE specs SET tree_id = 'tree-1', last_audited_sha = 'sha-0' WHERE id = ?",
                &[spec.id.as_str()],
            )
            .await;

            for sha in ["sha-1", "sha-2"] {
                let again = upsert_by_path(&pool, project, PATH, "Title", sha)
                    .await
                    .unwrap();
                assert_eq!(again.tree_id.as_deref(), Some("tree-1"), "{sha}");
                assert_eq!(again.last_audited_sha.as_deref(), Some("sha-0"), "{sha}");
            }
        }
    }

    #[tokio::test]
    async fn criteria_replace_is_atomic() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["keep-1", "keep-2"]))
            .await
            .unwrap();
        let before = list_criteria(&pool, &spec.id).await.unwrap();
        exec(
            &pool,
            "CREATE TRIGGER spec_criteria_boom BEFORE INSERT ON spec_criteria \
             WHEN NEW.text = 'boom' BEGIN SELECT RAISE(ABORT, 'boom'); END",
            &[],
        )
        .await;

        let failed = replace_criteria(
            &pool,
            &spec.id,
            "sha-2",
            &open_items(&["new-1", "boom", "new-3"]),
        )
        .await;
        assert!(failed.is_err());

        let after = list_criteria(&pool, &spec.id).await.unwrap();
        assert_eq!(after.len(), before.len());
        for (kept, old) in after.iter().zip(&before) {
            assert_eq!(kept.id, old.id);
            assert_eq!(kept.ordinal, old.ordinal);
            assert_eq!(kept.text, old.text);
            assert_eq!(kept.spec_sha, "sha-1");
        }
        assert_eq!(criteria_texts(&pool, &spec.id).await, ["keep-1", "keep-2"]);

        // The failure leaves no transaction open behind it: the next replace goes through.
        replace_criteria(&pool, &spec.id, "sha-2", &open_items(&["new-1"]))
            .await
            .unwrap();
        assert_eq!(criteria_texts(&pool, &spec.id).await, ["new-1"]);
    }

    #[tokio::test]
    async fn replace_criteria_numbers_from_one_and_clears_stale() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let other = seed(&pool, None, OTHER_PATH).await;
        replace_criteria(&pool, &other.id, "sha-1", &open_items(&["bystander"]))
            .await
            .unwrap();
        replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["a", "b", "c"]))
            .await
            .unwrap();
        let ordinals: Vec<i64> = list_criteria(&pool, &spec.id)
            .await
            .unwrap()
            .iter()
            .map(|criterion| criterion.ordinal)
            .collect();
        assert_eq!(ordinals, [1, 2, 3]);

        // The list follows the ordinal, not the insertion order.
        exec(
            &pool,
            "UPDATE spec_criteria SET ordinal = 10 - ordinal WHERE spec_id = ?",
            &[spec.id.as_str()],
        )
        .await;
        assert_eq!(criteria_texts(&pool, &spec.id).await, ["c", "b", "a"]);

        upsert_by_path(&pool, None, PATH, "Title", "sha-2")
            .await
            .unwrap();
        let extracted = [
            NewCriterion {
                text: "x".to_owned(),
                kind: CriterionKind::Test,
                verify_hint: Some("cargo test x".to_owned()),
                status: CriterionStatus::Open,
            },
            NewCriterion {
                text: "y".to_owned(),
                kind: CriterionKind::Decision,
                verify_hint: None,
                status: CriterionStatus::Doubt,
            },
        ];
        replace_criteria(&pool, &spec.id, "sha-2", &extracted)
            .await
            .unwrap();

        let criteria = list_criteria(&pool, &spec.id).await.unwrap();
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[0].ordinal, 1);
        assert_eq!(criteria[0].spec_id, spec.id);
        assert_eq!(criteria[0].text, "x");
        assert_eq!(criteria[0].kind, CriterionKind::Test);
        assert_eq!(criteria[0].verify_hint.as_deref(), Some("cargo test x"));
        assert_eq!(criteria[0].status, CriterionStatus::Open);
        assert_eq!(criteria[1].ordinal, 2);
        assert_eq!(criteria[1].text, "y");
        assert_eq!(criteria[1].kind, CriterionKind::Decision);
        assert_eq!(criteria[1].verify_hint, None);
        assert_eq!(criteria[1].status, CriterionStatus::Doubt);
        for criterion in &criteria {
            assert_eq!(criterion.spec_sha, "sha-2");
            assert_eq!(criterion.evidence, None);
            assert_eq!(criterion.reviewer_run_id, None);
            assert_eq!(criterion.checked_at, None);
        }
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM spec_criteria WHERE status = 'stale'"
            )
            .await,
            0
        );

        // Only this spec's rows were replaced.
        assert_eq!(criteria_texts(&pool, &other.id).await, ["bystander"]);
    }

    #[tokio::test]
    async fn deps_roundtrip() {
        let pool = crate::testdb::fresh_pool().await;
        let a = seed(&pool, None, ".ai/specs/a.md").await.id;
        let b = seed(&pool, None, ".ai/specs/b.md").await.id;
        let c = seed(&pool, None, ".ai/specs/c.md").await.id;
        add_dep(&pool, &a, &b, DepSource::Extracted).await.unwrap();
        add_dep(&pool, &a, &c, DepSource::Owner).await.unwrap();

        assert_eq!(list_deps(&pool, &a).await.unwrap().len(), 2);
        assert_eq!(source_of(&pool, &a, &b).await, [DepSource::Extracted]);
        assert_eq!(source_of(&pool, &a, &c).await, [DepSource::Owner]);

        // The edge has a direction.
        assert!(list_deps(&pool, &b).await.unwrap().is_empty());

        // The same pair again changes nothing.
        add_dep(&pool, &a, &b, DepSource::Extracted).await.unwrap();
        assert_eq!(list_deps(&pool, &a).await.unwrap().len(), 2);
        assert_eq!(source_of(&pool, &a, &b).await, [DepSource::Extracted]);

        // The same pair under the other source still neither errors nor duplicates. Which source
        // wins is not pinned, so it is not asserted.
        add_dep(&pool, &a, &b, DepSource::Owner).await.unwrap();
        assert_eq!(list_deps(&pool, &a).await.unwrap().len(), 2);
        assert_eq!(source_of(&pool, &a, &b).await.len(), 1);
    }

    #[tokio::test]
    async fn questions_roundtrip() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let other = seed(&pool, None, OTHER_PATH).await;

        let asked = add_question(&pool, &spec.id, "Which port?").await.unwrap();
        let pending = add_question(&pool, &spec.id, "Which port?").await.unwrap();
        let elsewhere = add_question(&pool, &other.id, "Which host?").await.unwrap();
        assert_ne!(asked, pending);

        let questions = list_questions(&pool, &spec.id).await.unwrap();
        assert_eq!(questions.len(), 2);
        let open = questions.iter().find(|q| q.id == asked).unwrap();
        assert_eq!(open.spec_id, spec.id);
        assert_eq!(open.question, "Which port?");
        assert_eq!(open.answer, None);
        assert_eq!(open.status, "open");
        assert_eq!(open.answered_at, None);
        assert!(chrono::DateTime::parse_from_rfc3339(&open.created_at).is_ok());
        let created_at = open.created_at.clone();

        answer_question(&pool, &asked, "8080").await.unwrap();

        let questions = list_questions(&pool, &spec.id).await.unwrap();
        let answered = questions.iter().find(|q| q.id == asked).unwrap();
        assert_eq!(answered.answer.as_deref(), Some("8080"));
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.created_at, created_at);
        let answered_at = answered.answered_at.as_deref().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(answered_at).is_ok());

        // Answering one question leaves its neighbours alone.
        let neighbour = questions.iter().find(|q| q.id == pending).unwrap();
        assert_eq!(neighbour.status, "open");
        assert_eq!(neighbour.answer, None);
        let others = list_questions(&pool, &other.id).await.unwrap();
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].id, elsewhere);
        assert_eq!(others[0].status, "open");
    }

    #[tokio::test]
    async fn list_owner_decisions_merges_spec_questions_and_decisions_when_table_exists() {
        let pool = crate::testdb::fresh_pool().await;
        recreate_decisions(&pool).await;
        let spec = seed(&pool, None, PATH).await;

        let q1 = add_question(&pool, &spec.id, "First?").await.unwrap();
        let q2 = add_question(&pool, &spec.id, "Third?").await.unwrap();
        answer_question(&pool, &q2, "yes").await.unwrap();
        let (t10, t11, t12, t13) = (
            "2026-10-30T10:00:00+00:00",
            "2026-10-30T11:00:00+00:00",
            "2026-10-30T12:00:00+00:00",
            "2026-10-30T13:00:00+00:00",
        );
        let set_created = "UPDATE spec_questions SET created_at = ? WHERE id = ?";
        exec(&pool, set_created, &[t10, q1.as_str()]).await;
        exec(&pool, set_created, &[t12, q2.as_str()]).await;
        // `origin` here is the table's own column, a different thing from the merged list's origin.
        exec(
            &pool,
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status) \
             VALUES ('d-1', 'tree-1', 'node-1', 'Second?', NULL, 'agent', ?, 'open')",
            &[t11],
        )
        .await;
        exec(
            &pool,
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status) \
             VALUES ('d-2', NULL, NULL, 'Fourth?', 'no', 'agent', ?, 'answered')",
            &[t13],
        )
        .await;

        let rows = list_owner_decisions(&pool).await.unwrap();

        // Interleaved on purpose: neither table first would give this order.
        let order: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|row| (row.origin.as_str(), row.id.as_str(), row.created_at.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("spec_question", q1.as_str(), t10),
                ("tree_decision", "d-1", t11),
                ("spec_question", q2.as_str(), t12),
                ("tree_decision", "d-2", t13),
            ]
        );

        assert_eq!(rows[0].subject, spec.id);
        assert_eq!(rows[0].question, "First?");
        assert_eq!(rows[0].answer, None);
        assert_eq!(rows[0].status, "open");
        assert_eq!(rows[0].answered_at, None);

        assert_eq!(rows[1].subject, "tree-1");
        assert_eq!(rows[1].question, "Second?");
        assert_eq!(rows[1].answer, None);
        assert_eq!(rows[1].status, "open");
        assert_eq!(rows[1].answered_at, None);

        assert_eq!(rows[2].subject, spec.id);
        assert_eq!(rows[2].question, "Third?");
        assert_eq!(rows[2].answer.as_deref(), Some("yes"));
        assert_eq!(rows[2].status, "answered");
        assert!(rows[2].answered_at.is_some());

        // A decision with no tree is not lost; its subject may be empty, so that is not asserted.
        assert_eq!(rows[3].question, "Fourth?");
        assert_eq!(rows[3].answer.as_deref(), Some("no"));
        assert_eq!(rows[3].status, "answered");
        assert_eq!(rows[3].answered_at, None);
    }

    #[tokio::test]
    async fn list_owner_decisions_without_decisions_table() {
        let pool = crate::testdb::fresh_pool().await;
        exec(&pool, "DROP TABLE IF EXISTS decisions", &[]).await;
        assert!(list_owner_decisions(&pool).await.unwrap().is_empty());

        let spec = seed(&pool, None, PATH).await;
        let later = add_question(&pool, &spec.id, "Later?").await.unwrap();
        let earlier = add_question(&pool, &spec.id, "Earlier?").await.unwrap();
        exec(
            &pool,
            "UPDATE spec_questions SET created_at = ? WHERE id = ?",
            &[OLD, earlier.as_str()],
        )
        .await;

        let rows = list_owner_decisions(&pool).await.unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, [earlier.as_str(), later.as_str()]);
        for row in &rows {
            assert_eq!(row.origin, "spec_question");
            assert_eq!(row.subject, spec.id);
            assert_eq!(row.status, "open");
        }
    }
}
