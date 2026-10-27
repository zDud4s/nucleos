use super::*;
use crate::embed::fake::FakeEmbedder;
use crate::map::intent::Extractor;
use chrono::{Duration, Utc};

pub(super) const MODEL: &str = "fake-model";
pub(super) async fn pool() -> SqlitePool {
    crate::testdb::fresh_pool().await
}
pub(super) fn embedder() -> FakeEmbedder {
    FakeEmbedder::new(MODEL, Vec::new(), Some(vec![1.0, 0.0]))
}
pub(super) fn job(n: i64) -> String {
    format!(r#"[{{"t":"job","id":{n}}},{{"t":"run","id":{}}}]"#, n * 100)
}

/// A distiller row with its vector stored under the fake model.
pub(super) async fn seed(
    pool: &SqlitePool,
    scope: (&str, Option<&str>),
    status: &str,
    title: &str,
    evidence: &str,
    vector: &[f32],
) -> i64 {
    let id = sqlx::query(
        "INSERT INTO knowledge (layer, scope_kind, scope_id, source, kind, title, body, fingerprint, evidence, distill_cause, status, created_at)
         VALUES ('semantic', ?, ?, 'distiller', 'memory', ?, 'body', ?, ?, 'job_failed', ?, ?)",
    )
    .bind(scope.0)
    .bind(scope.1)
    .bind(title)
    .bind(crate::knowledge::title_fingerprint(title))
    .bind(evidence)
    .bind(status)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    crate::embed::store_in(&mut pool.acquire().await.unwrap(), id, MODEL, vector)
        .await
        .unwrap();
    id
}
/// Three candidates in three projects, one job each, identical vectors.
pub(super) async fn trio(pool: &SqlitePool) -> [i64; 3] {
    let mut ids = [0; 3];
    for (i, project) in ["alpha", "bravo", "charlie"].into_iter().enumerate() {
        ids[i] = seed(
            pool,
            ("project", Some(project)),
            "proposed",
            "Run the suite with Git usr/bin on PATH",
            &job(i as i64 + 1),
            &[1.0, 0.0],
        )
        .await;
    }
    ids
}
pub(super) fn answering(stdout: &str) -> crate::execution::runner::FakeCommandRunner {
    crate::execution::runner::FakeCommandRunner {
        canned: std::sync::Mutex::new(Some(crate::execution::runner::RunOutcome {
            exit_code: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
            session_id: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
            model_requested: false,
        })),
        ..Default::default()
    }
}
/// A brain that is down: every launch fails (so a call turns a pass into `Failed`).
pub(super) fn down() -> crate::execution::runner::FakeCommandRunner {
    crate::execution::runner::FakeCommandRunner {
        fail_times: std::sync::Mutex::new(99),
        ..Default::default()
    }
}

#[tokio::test]
async fn groups_hold_only_live_distiller_rows_that_do_not_quote_the_owner() {
    let pool = pool().await;
    let [a, b, c] = trio(&pool).await;
    let quoting = seed(
        &pool,
        ("project", Some("alpha")),
        "proposed",
        "Quoted",
        &job(9),
        &[1.0, 0.0],
    )
    .await;
    sqlx::query("INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at) VALUES (?, 'proposed', 'proposed', 'quoted_owner_text', 'x')")
        .bind(quoting)
        .execute(&pool)
        .await
        .unwrap();
    let refused = seed(
        &pool,
        ("project", Some("alpha")),
        "rejected",
        "No",
        &job(8),
        &[1.0, 0.0],
    )
    .await;
    let machine = seed(
        &pool,
        ("machine", None),
        "active",
        "Already general",
        &job(7),
        &[1.0, 0.0],
    )
    .await;
    let gate = seed(
        &pool,
        ("project", Some("alpha")),
        "active",
        "Counter",
        &job(6),
        &[1.0, 0.0],
    )
    .await;
    sqlx::query("UPDATE knowledge SET source = 'gate' WHERE id = ?")
        .bind(gate)
        .execute(&pool)
        .await
        .unwrap();

    let groups = form_groups(&pool, MODEL, Utc::now()).await.unwrap();
    let ids: Vec<i64> = groups
        .iter()
        .flat_map(|g| g.members.iter().map(|m| m.id))
        .collect();
    assert_eq!(ids, vec![a, b, c]);
    assert!(
        ![quoting, refused, machine, gate]
            .iter()
            .any(|x| ids.contains(x))
    );
}

#[tokio::test]
async fn rows_older_than_ninety_days_and_rows_without_a_vector_are_left_out() {
    let pool = pool().await;
    let [a, ..] = trio(&pool).await;
    sqlx::query("UPDATE knowledge SET created_at = ? WHERE id = ?")
        .bind((Utc::now() - Duration::days(91)).to_rfc3339())
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO knowledge (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
         VALUES ('semantic','project','delta','distiller','memory','No vector','b','proposed',?)",
    )
    .bind(Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();
    let groups = form_groups(&pool, MODEL, Utc::now()).await.unwrap();
    assert_eq!(groups.iter().map(|g| g.members.len()).sum::<usize>(), 2);
}

#[test]
fn the_answer_is_empty_or_one_semantic_or_procedural_lesson() {
    assert!(parse_lessons("[]").unwrap().is_empty());
    let one = parse_lessons(
        "```json\n[{\"layer\":\"procedural\",\"title\":\" T \",\"body\":\"B\",\"about\":\"owner\"},\
        {\"layer\":\"semantic\",\"title\":\"Second\",\"body\":\"B\"}]\n```",
    )
    .unwrap();
    assert!(
        one.len() == 1 && one[0].owner && one[0].title == "T",
        "one lesson per group"
    );
    for bad in ["not json", r#"{"layer":"semantic"}"#] {
        assert!(parse_lessons(bad).is_err(), "{bad}");
    }
    for dropped in [
        r#"[{"layer":"episodic","title":"t","body":"b"}]"#,
        r#"[{"layer":"semantic","title":"","body":"b"}]"#,
    ] {
        assert!(parse_lessons(dropped).unwrap().is_empty(), "{dropped}");
    }
}

pub(super) async fn generalised(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE distill_cause = 'generalised'")
        .fetch_one(pool)
        .await
        .unwrap()
}
const LESSON: &str = r#"[{"layer":"procedural","title":"Put Git usr/bin on PATH before the suite","body":"b","about":"work"}]"#;

#[tokio::test]
async fn three_near_identical_candidates_in_three_projects_make_one_machine_proposal() {
    let pool = pool().await;
    let [a, b, c] = trio(&pool).await;
    // The fake embeds the lesson at [1,0] too: cosine 1.0 against every member. Only excluding the group's own
    // members from the fence lets the lesson be written.
    assert_eq!(
        pass(
            &pool,
            Extractor::Cli(&answering(LESSON)),
            &embedder(),
            Utc::now()
        )
        .await
        .unwrap(),
        Outcome::Done
    );

    type Row = (String, Option<String>, String, Option<String>, Option<String>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT scope_kind, scope_id, status, evidence, about FROM knowledge WHERE distill_cause = 'generalised'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    let (kind, scope, status, evidence, about) = rows.into_iter().next().unwrap();
    assert_eq!(
        (kind.as_str(), scope, status.as_str(), about),
        ("machine", None, "proposed", None)
    );
    for id in [a, b, c] {
        assert!(
            evidence
                .as_deref()
                .unwrap()
                .contains(&format!(r#""id":{id}"#))
        );
    }
    let (pending, vectors): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM proposals p JOIN knowledge k ON k.proposal_id = p.id WHERE k.distill_cause = 'generalised' AND p.status = 'pending'),
                (SELECT COUNT(*) FROM knowledge_embeddings e JOIN knowledge k ON k.id = e.knowledge_id WHERE k.distill_cause = 'generalised')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((pending, vectors), (1, 1));
}

#[tokio::test]
async fn one_job_is_one_subject_so_no_model_is_asked() {
    let pool = pool().await;
    for i in 0..3 {
        seed(
            &pool,
            ("project", Some("alpha")),
            "proposed",
            &format!("Same job {i}"),
            &job(1),
            &[1.0, 0.0],
        )
        .await;
    }
    // A call would be Failed.
    assert_eq!(
        pass(&pool, Extractor::Cli(&down()), &embedder(), Utc::now())
            .await
            .unwrap(),
        Outcome::Done
    );
}

#[tokio::test]
async fn owner_proof_needs_two_conversation_subjects() {
    let pool = pool().await;
    for chat in 1..=3 {
        seed(
            &pool,
            ("observation", Some(&format!("alpha:chat:{chat}"))),
            "observed",
            &format!("Prefers small diffs {chat}"),
            &format!(r#"[{{"t":"chat","id":{chat}}}]"#),
            &[1.0, 0.0],
        )
        .await;
    }
    let runner = answering(
        r#"[{"layer":"semantic","title":"Prefers small diffs","body":"b","about":"owner"}]"#,
    );
    pass(&pool, Extractor::Cli(&runner), &embedder(), Utc::now())
        .await
        .unwrap();
    let (kind, about): (String, Option<String>) = sqlx::query_as(
        "SELECT scope_kind, about FROM knowledge WHERE distill_cause = 'generalised'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), about.as_deref()),
        ("machine", Some("owner"))
    );
}

#[tokio::test]
async fn a_lesson_the_owner_refused_or_reverted_does_not_come_back_and_a_live_one_is_reconfirmed() {
    // (status of the old row, has a vector): with one the vector fence stops it (other words); without one only the
    // fingerprint fence can (a refusal that was never embedded).
    for (status, embedded) in [
        ("rejected", true),
        ("reverted", true),
        ("rejected", false),
        ("reverted", false),
        ("active", true),
    ] {
        let pool = pool().await;
        trio(&pool).await;
        let old = seed(
            &pool,
            ("machine", None),
            status,
            "Put Git usr/bin on PATH before the suite",
            "[]",
            &[1.0, 0.0],
        )
        .await;
        if embedded {
            sqlx::query("UPDATE knowledge SET title = 'Older wording', fingerprint = 'title:older wording' WHERE id = ?")
                .bind(old)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            sqlx::query("DELETE FROM knowledge_embeddings WHERE knowledge_id = ?")
                .bind(old)
                .execute(&pool)
                .await
                .unwrap();
        }
        let out = pass(
            &pool,
            Extractor::Cli(&answering(LESSON)),
            &embedder(),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(
            (out, generalised(&pool).await),
            (Outcome::Done, 0),
            "{status} embedded={embedded}"
        );
        let reconfirmed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ? AND note = 'reconfirmed'",
        )
        .bind(old)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            reconfirmed,
            i64::from(status == "active"),
            "only a live row is reconfirmed"
        );
    }
}

async fn seal(pool: &SqlitePool) -> (Option<String>, Option<String>) {
    let get = |k: &'static str| async move {
        sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
            .bind(k)
            .fetch_optional(pool)
            .await
            .unwrap()
    };
    (
        get("distiller.generalised_at").await,
        get("distiller.generalised_attempts").await,
    )
}
async fn attempts(pool: &SqlitePool) -> Option<String> {
    seal(pool).await.1
}

#[tokio::test]
async fn without_an_embedder_nothing_runs_and_nothing_is_sealed() {
    let pool = pool().await;
    trio(&pool).await;
    run_if_due(&pool, Extractor::Cli(&down()), None, Utc::now()).await;
    assert_eq!(seal(&pool).await, (None, None));
}

#[tokio::test]
async fn zero_groups_is_a_success_that_seals_for_a_day() {
    let pool = pool().await;
    let now = Utc::now();
    run_if_due(&pool, Extractor::Cli(&down()), Some(&embedder()), now).await;
    assert!(seal(&pool).await.0.is_some());
    assert_eq!(attempts(&pool).await.as_deref(), Some("0"));
    // Sealed: a call inside the day is a no-op even with eligible rows.
    trio(&pool).await;
    run_if_due(
        &pool,
        Extractor::Cli(&down()),
        Some(&embedder()),
        now + Duration::hours(1),
    )
    .await;
    assert_eq!(attempts(&pool).await.as_deref(), Some("0"));
}

#[tokio::test]
async fn a_brain_that_is_down_or_answers_garbage_fails_until_the_third_attempt_then_waits_a_day() {
    for garbage in [false, true] {
        let pool = pool().await;
        trio(&pool).await;
        let now = Utc::now();
        for (minutes, expected) in [
            (0, "1"),
            (5, "1" /* not due yet */),
            (11, "2"),
            (40, "0" /* spent: the day */),
        ] {
            let at = now + Duration::minutes(minutes);
            if garbage {
                run_if_due(
                    &pool,
                    Extractor::Cli(&answering("not json")),
                    Some(&embedder()),
                    at,
                )
                .await
            } else {
                run_if_due(&pool, Extractor::Cli(&down()), Some(&embedder()), at).await
            }
            assert_eq!(
                attempts(&pool).await.as_deref(),
                Some(expected),
                "garbage={garbage} +{minutes}m"
            );
        }
        assert_eq!(generalised(&pool).await, 0);
    }
}

#[tokio::test]
async fn the_same_group_is_not_asked_on_day_two_and_a_new_subject_asks_again() {
    let pool = pool().await;
    trio(&pool).await;
    let now = Utc::now();
    // Success with nothing to write.
    run_if_due(
        &pool,
        Extractor::Cli(&answering("[]")),
        Some(&embedder()),
        now,
    )
    .await;
    let keys: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_meta WHERE key LIKE 'generalise.group.%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(keys, 1, "an empty answer still leaves a memory");

    // Day 2, brain down: not asked, so the attempt is still a success. A new row of an OLD job is no new subject.
    run_if_due(
        &pool,
        Extractor::Cli(&down()),
        Some(&embedder()),
        now + Duration::hours(25),
    )
    .await;
    seed(
        &pool,
        ("project", Some("alpha")),
        "proposed",
        "Same job again",
        &job(1),
        &[1.0, 0.0],
    )
    .await;
    run_if_due(
        &pool,
        Extractor::Cli(&down()),
        Some(&embedder()),
        now + Duration::hours(50),
    )
    .await;
    assert_eq!(attempts(&pool).await.as_deref(), Some("0"));
    // A new job is: asked again, and the brain was down.
    seed(
        &pool,
        ("project", Some("alpha")),
        "proposed",
        "New job",
        &job(4),
        &[1.0, 0.0],
    )
    .await;
    run_if_due(
        &pool,
        Extractor::Cli(&down()),
        Some(&embedder()),
        now + Duration::hours(75),
    )
    .await;
    assert_eq!(attempts(&pool).await.as_deref(), Some("1"));
}

#[tokio::test]
async fn memory_of_a_group_that_no_longer_exists_is_pruned() {
    let pool = pool().await;
    sqlx::query("INSERT INTO schema_meta (key, value) VALUES ('generalise.group.999', '{\"h\":\"x\",\"s\":[]}')")
        .execute(&pool)
        .await
        .unwrap();
    run_if_due(
        &pool,
        Extractor::Cli(&down()),
        Some(&embedder()),
        Utc::now(),
    )
    .await;
    let left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_meta WHERE key = 'generalise.group.999'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(left, 0);
}

fn near(id: i64) -> NearNote {
    NearNote { id, cosine: 0.9 }
}

/// An active note with a stored vector under `model`.
async fn embedded_note(pool: &SqlitePool, text: &str, model: &str, v: [f32; 2]) -> i64 {
    let id = crate::owner_notes::create(pool, text, "shell")
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO owner_note_embeddings (note_id, model, dim, vector, text_hash, created_at)
         VALUES (?, ?, 2, ?, 'h', 't')",
    )
    .bind(id)
    .bind(model)
    .bind(crate::embed::encode(&v))
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Three rows of ONE job in one project: a single subject, so only notes can make the group eligible.
async fn one_job_trio(pool: &SqlitePool) {
    for i in 0..3 {
        seed(
            pool,
            ("project", Some("alpha")),
            "proposed",
            &format!("Same job {i}"),
            &job(1),
            &[1.0, 0.0],
        )
        .await;
    }
}

fn group_of(subjects: &[Subject]) -> Group {
    Group {
        members: Vec::new(),
        subjects: subjects.iter().cloned().collect(),
        centroid: vec![1.0, 0.0],
        min_id: 1,
    }
}

#[test]
fn a_nearby_note_counts_as_a_distinct_subject_named_note_id() {
    let subjects = subjects_with_notes(
        &group_of(&[Subject::Job(7)]).subjects,
        &[near(11), near(12)],
    );
    let keys: BTreeSet<String> = subjects.iter().map(Subject::key).collect();
    assert_eq!(
        keys,
        ["job:7", "note:11", "note:12"]
            .map(String::from)
            .into_iter()
            .collect()
    );
    assert!(
        subjects.len() >= plan::MIN_SUBJECTS,
        "one row plus two notes reach MIN_SUBJECTS"
    );
}

#[tokio::test]
async fn notes_alone_do_not_form_a_group() {
    let pool = pool().await;
    for text in ["one", "two", "three"] {
        embedded_note(&pool, text, MODEL, [1.0, 0.0]).await;
    }
    let groups = form_groups(&pool, MODEL, Utc::now()).await.unwrap();
    assert!(groups.is_empty());
    assert_eq!(
        pass(&pool, Extractor::Cli(&down()), &embedder(), Utc::now())
            .await
            .unwrap(),
        Outcome::Done,
        "no group, so no model call"
    );
}

#[tokio::test]
async fn notes_near_applies_the_floor_the_cap_and_the_active_filter() {
    let pool = pool().await;
    let mut kept = Vec::new();
    for i in 0..7 {
        kept.push(embedded_note(&pool, &format!("note {i}"), MODEL, [1.0, 0.01 * i as f32]).await);
    }
    let archived = embedded_note(&pool, "gone", MODEL, [1.0, 0.0]).await;
    sqlx::query("UPDATE owner_notes SET state = 'archived' WHERE id = ?")
        .bind(archived)
        .execute(&pool)
        .await
        .unwrap();
    let below = embedded_note(&pool, "far", MODEL, [0.0, 1.0]).await;
    let group = group_of(&[Subject::Job(1)]);
    let got: Vec<i64> = notes_near(&pool, &group, MODEL)
        .await
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(got.len(), MAX_NOTES_PER_GROUP);
    assert!(!got.contains(&archived) && !got.contains(&below));
}

#[tokio::test]
async fn near_notes_make_a_one_job_group_eligible_and_prove_about_owner() {
    let pool = pool().await;
    one_job_trio(&pool).await;
    embedded_note(&pool, "I like short answers", MODEL, [1.0, 0.0]).await;
    embedded_note(&pool, "keep replies brief", MODEL, [1.0, 0.0]).await;
    let runner = answering(
        r#"[{"layer":"semantic","title":"Prefers brief replies","body":"Keep answers short.","about":"owner"}]"#,
    );
    pass(&pool, Extractor::Cli(&runner), &embedder(), Utc::now())
        .await
        .unwrap();
    let (kind, about, evidence): (String, Option<String>, String) = sqlx::query_as(
        "SELECT scope_kind, about, evidence FROM knowledge WHERE distill_cause = 'generalised'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), about.as_deref()),
        ("machine", Some("owner"))
    );
    let entries: Vec<serde_json::Value> = serde_json::from_str(&evidence).unwrap();
    assert_eq!(entries.iter().filter(|e| e["t"] == "note").count(), 2);
    assert!(
        !evidence.contains("short answers"),
        "ids only, never the note text"
    );
}

#[tokio::test]
async fn notes_do_not_count_as_projects() {
    let pool = pool().await;
    one_job_trio(&pool).await;
    embedded_note(&pool, "one", MODEL, [1.0, 0.0]).await;
    embedded_note(&pool, "two", MODEL, [1.0, 0.0]).await;
    let runner =
        answering(r#"[{"layer":"semantic","title":"Same job lesson","body":"b","about":"work"}]"#);
    pass(&pool, Extractor::Cli(&runner), &embedder(), Utc::now())
        .await
        .unwrap();
    let (kind, scope, about): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT scope_kind, scope_id, about FROM knowledge WHERE distill_cause = 'generalised'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), scope.as_deref(), about),
        ("project", Some("alpha"), None)
    );
}

#[test]
fn evidence_carries_note_ids_without_text_and_stays_within_20_entries() {
    let ids: Vec<i64> = (1..=18).collect();
    let ev = evidence_json(&ids, &[near(11), near(12), near(13)]);
    let v: serde_json::Value = serde_json::from_str(&ev).unwrap();
    let entries = v.as_array().unwrap();
    assert_eq!(entries.len(), 20);
    assert!(
        entries
            .iter()
            .any(|e| e == &serde_json::json!({"t": "note", "id": 11}))
    );
    assert!(!ev.contains("text"));
    let none = evidence_json(&ids, &[]);
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(&none)
            .unwrap()
            .len(),
        18
    );
}

#[test]
fn the_note_text_is_scrubbed_and_sits_under_do_not_quote() {
    let group = group_of(&[Subject::Job(1)]);
    let with = dossier(&group, &["call me at sk-ABCDEF1234567890XYZ".to_owned()]);
    assert!(with.contains("do not quote"));
    assert!(!with.contains("sk-ABCDEF1234567890XYZ"));
    assert!(!dossier(&group, &[]).contains("OWNER CONTEXT"));
}

#[tokio::test]
async fn a_lesson_that_quotes_a_nearby_note_gets_the_quoted_owner_text_event() {
    const NOTE: &str = "the staging database is restored from the friday backup every week";
    for quotes in [true, false] {
        let pool = pool().await;
        one_job_trio(&pool).await;
        embedded_note(&pool, NOTE, MODEL, [1.0, 0.0]).await;
        embedded_note(&pool, "an unrelated second note", MODEL, [1.0, 0.0]).await;
        let body = if quotes {
            NOTE
        } else {
            "Restore staging regularly."
        };
        let runner = answering(&format!(
            r#"[{{"layer":"semantic","title":"Staging restore","body":"{body}","about":"owner"}}]"#
        ));
        pass(&pool, Extractor::Cli(&runner), &embedder(), Utc::now())
            .await
            .unwrap();
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge_events e JOIN knowledge k ON k.id = e.knowledge_id
              WHERE k.distill_cause = 'generalised' AND e.note = 'quoted_owner_text'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(events, i64::from(quotes));
        let status: String =
            sqlx::query_scalar("SELECT status FROM knowledge WHERE distill_cause = 'generalised'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "proposed");
    }
}

#[tokio::test]
async fn a_new_note_is_a_new_subject_for_the_group_memory() {
    let pool = pool().await;
    one_job_trio(&pool).await;
    let now = Utc::now();
    embedded_note(&pool, "one", MODEL, [1.0, 0.0]).await;
    embedded_note(&pool, "two", MODEL, [1.0, 0.0]).await;
    run_if_due(
        &pool,
        Extractor::Cli(&answering("[]")),
        Some(&embedder()),
        now,
    )
    .await;
    // Same subjects a day later: not asked, so a down brain still succeeds.
    run_if_due(
        &pool,
        Extractor::Cli(&down()),
        Some(&embedder()),
        now + Duration::hours(25),
    )
    .await;
    assert_eq!(attempts(&pool).await.as_deref(), Some("0"));
    // A third note is a new subject: asked again, and the brain was down.
    embedded_note(&pool, "three", MODEL, [1.0, 0.0]).await;
    run_if_due(
        &pool,
        Extractor::Cli(&down()),
        Some(&embedder()),
        now + Duration::hours(50),
    )
    .await;
    assert_eq!(attempts(&pool).await.as_deref(), Some("1"));
}

#[tokio::test]
async fn three_weeks_with_the_same_rhythm_are_three_subjects_and_reach_the_pass() {
    let pool = pool().await;
    for week in ["2026-W41", "2026-W42", "2026-W43"] {
        seed(
            &pool,
            ("observation", Some(&format!("week:{week}"))),
            "observed",
            &format!("Mon 07:00 gym {week}"),
            "[]",
            &[1.0, 0.0],
        )
        .await;
    }
    let groups = form_groups(&pool, MODEL, Utc::now()).await.unwrap();
    assert_eq!(groups.len(), 1);
    let keys: BTreeSet<String> = groups[0].subjects.iter().map(Subject::key).collect();
    assert_eq!(
        keys,
        ["week:2026-W41", "week:2026-W42", "week:2026-W43"]
            .map(String::from)
            .into_iter()
            .collect()
    );
    let runner = answering(
        r#"[{"layer":"semantic","title":"Trains on Monday mornings","body":"b","about":"owner"}]"#,
    );
    pass(&pool, Extractor::Cli(&runner), &embedder(), Utc::now())
        .await
        .unwrap();
    let (kind, about): (String, Option<String>) = sqlx::query_as(
        "SELECT scope_kind, about FROM knowledge WHERE distill_cause = 'generalised'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), about.as_deref()),
        ("machine", Some("owner"))
    );
}

#[tokio::test]
async fn observations_of_one_week_are_one_subject_below_the_minimum() {
    let pool = pool().await;
    for i in 0..3 {
        seed(
            &pool,
            ("observation", Some("week:2026-W41")),
            "observed",
            &format!("Mon 07:00 gym {i}"),
            "[]",
            &[1.0, 0.0],
        )
        .await;
    }
    // A model call would turn the pass into `Failed`.
    assert_eq!(
        pass(&pool, Extractor::Cli(&down()), &embedder(), Utc::now())
            .await
            .unwrap(),
        Outcome::Done
    );
}
