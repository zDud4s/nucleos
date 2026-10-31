//! Criteria extraction over registered, immutable spec files.

use std::fs;

use sqlx::SqlitePool;

use super::criteria::parse;
use super::model::{SpecKind, SpecStatus};
use super::prompts::{extraction_prompt, split_sections};
use super::scan::{ScanRoots, content_sha, resolve_stored_path};
use super::store::{self, StoreError};

#[async_trait::async_trait]
pub trait ExtractionRunner: Send + Sync {
    async fn run(&self, prompt: String) -> Result<String, String>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExtractOutcome {
    Stored { kind: SpecKind, criteria: usize },
    NeedsOwner { error: String },
}

#[derive(Debug)]
pub enum ExtractError {
    Store(StoreError),
    Io(std::io::Error),
    NotFound(String),
    SpecChanged { spec_id: String },
    Runner(String),
}

impl From<StoreError> for ExtractError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<std::io::Error> for ExtractError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub async fn extract(
    pool: &SqlitePool,
    roots: &ScanRoots,
    spec_id: &str,
    runner: &dyn ExtractionRunner,
) -> Result<ExtractOutcome, ExtractError> {
    let spec = store::get_spec(pool, spec_id)
        .await?
        .ok_or_else(|| ExtractError::NotFound(spec_id.to_owned()))?;
    let path = resolve_stored_path(roots, &spec.path)
        .ok_or_else(|| ExtractError::NotFound(spec_id.to_owned()))?;
    let bytes = fs::read(path)?;
    if content_sha(&bytes) != spec.content_sha {
        return Err(ExtractError::SpecChanged { spec_id: spec.id });
    }
    let text = String::from_utf8_lossy(&bytes);
    let known_paths = store::list_paths_by_project(pool, spec.project_id.as_deref()).await?;
    let mut kind = None;
    let mut criteria = Vec::new();
    let mut dependencies = Vec::new();
    let mut questions = Vec::new();
    for section in split_sections(&text) {
        let first = runner
            .run(extraction_prompt(&spec.path, &section, None))
            .await
            .map_err(ExtractError::Runner)?;
        let extraction = match parse(&first, &known_paths) {
            Ok(value) => value,
            Err(first_error) => {
                let retry = runner
                    .run(extraction_prompt(
                        &spec.path,
                        &section,
                        Some(&first_error.to_string()),
                    ))
                    .await
                    .map_err(ExtractError::Runner)?;
                match parse(&retry, &known_paths) {
                    Ok(value) => value,
                    Err(error) => {
                        let error = error.to_string();
                        store::set_extraction_result(
                            pool,
                            &spec.id,
                            spec.kind,
                            SpecStatus::NeedsOwner,
                            Some(&error),
                        )
                        .await?;
                        store::add_question(
                            pool,
                            &spec.id,
                            &format!("Extraction needs owner input: {error}"),
                        )
                        .await?;
                        return Ok(ExtractOutcome::NeedsOwner { error });
                    }
                }
            }
        };
        kind.get_or_insert(extraction.kind);
        criteria.extend(extraction.criteria);
        dependencies.extend(extraction.depends_on);
        questions.extend(extraction.questions);
    }
    let kind = kind.unwrap_or(SpecKind::Unknown);
    let count = criteria.len();
    store::replace_criteria(pool, &spec.id, &spec.content_sha, &criteria).await?;
    for dependency in dependencies {
        store::add_dep(
            pool,
            &spec.id,
            &super::model::spec_id(spec.project_id.as_deref(), &dependency),
            super::model::DepSource::Extracted,
        )
        .await?;
    }
    for question in questions {
        store::add_question(pool, &spec.id, &question).await?;
    }
    store::set_extraction_result(pool, &spec.id, kind, SpecStatus::Extracted, None).await?;
    Ok(ExtractOutcome::Stored {
        kind,
        criteria: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_backlog::criteria::ParseError;
    use crate::spec_backlog::model::{
        CriterionKind, CriterionStatus, Dep, DepSource, NewCriterion, Spec, SpecKind, SpecStatus,
        spec_id,
    };
    use crate::spec_backlog::prompts::{MAX_SPEC_CHARS, split_sections};
    use crate::spec_backlog::scan::{ScanRoots, content_sha};
    use crate::spec_backlog::store::{
        get_spec, list_criteria, list_deps, list_questions, replace_criteria, upsert_by_path,
    };
    use serde_json::{Value, json};
    use sqlx::SqlitePool;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    const PROJECT: Option<&str> = Some("proj-1");
    /// Renders as `ParseError::NoJsonBlock`.
    const NOT_JSON: &str = "I could not read the spec, sorry.";
    /// Renders as `ParseError::NoCriteria`.
    const NO_CRITERIA: &str =
        r#"{"kind": "spec", "criteria": [], "depends_on": [], "questions": []}"#;

    struct FakeRunner {
        answers: Mutex<VecDeque<Result<String, String>>>,
        prompts: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new(answers: Vec<Result<String, String>>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                prompts: Mutex::new(Vec::new()),
            }
        }

        fn answering(answers: &[String]) -> Self {
            Self::new(answers.iter().cloned().map(Ok).collect())
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ExtractionRunner for FakeRunner {
        async fn run(&self, prompt: String) -> Result<String, String> {
            self.prompts.lock().unwrap().push(prompt);
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("a run beyond the queued answers")
        }
    }

    fn fenced(value: Value) -> String {
        format!("Here is the extraction.\n\n```json\n{value}\n```\n")
    }

    fn criterion(text: &str) -> Value {
        json!({"text": text, "kind": "behavior", "verify_hint": null})
    }

    fn spec_answer(texts: &[&str]) -> String {
        let criteria: Vec<Value> = texts.iter().map(|text| criterion(text)).collect();
        fenced(json!({"kind": "spec", "criteria": criteria, "depends_on": [], "questions": []}))
    }

    fn empty_answer(kind: &str) -> String {
        fenced(json!({"kind": kind, "criteria": [], "depends_on": [], "questions": []}))
    }

    fn roots_at(project: &Path) -> ScanRoots {
        ScanRoots {
            project_root: Some(project.to_path_buf()),
            state_dir: None,
        }
    }

    /// Writes the file under `.ai/specs` and registers it with its true sha. Returns the file too.
    async fn register_in(
        pool: &SqlitePool,
        root: &Path,
        project: Option<&str>,
        name: &str,
        text: &str,
    ) -> (Spec, PathBuf) {
        let specs = root.join(".ai").join("specs");
        fs::create_dir_all(&specs).unwrap();
        let file = specs.join(name);
        fs::write(&file, text).unwrap();
        let spec = upsert_by_path(
            pool,
            project,
            &format!(".ai/specs/{name}"),
            "Title",
            &content_sha(text.as_bytes()),
        )
        .await
        .unwrap();
        (spec, file)
    }

    async fn register(pool: &SqlitePool, root: &Path, name: &str, text: &str) -> (Spec, PathBuf) {
        register_in(pool, root, PROJECT, name, text).await
    }

    async fn stored(pool: &SqlitePool, id: &str) -> Spec {
        get_spec(pool, id).await.unwrap().expect("spec row")
    }

    async fn seed_criterion(pool: &SqlitePool, spec: &Spec, text: &str) {
        let item = NewCriterion {
            text: text.to_owned(),
            kind: CriterionKind::Behavior,
            verify_hint: None,
            status: CriterionStatus::Open,
        };
        replace_criteria(pool, &spec.id, &spec.content_sha, &[item])
            .await
            .unwrap();
    }

    const SMALL_SPEC: &str = "# Alpha\n\n## Goal\nThe marker-zebra behaviour must hold.\n";

    #[tokio::test]
    async fn stores_criteria_for_valid_output() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = register(&pool, dir.path(), "alpha.md", SMALL_SPEC).await;
        let runner = FakeRunner::answering(&[spec_answer(&["first", "second"])]);

        let outcome = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            ExtractOutcome::Stored {
                kind: SpecKind::Spec,
                criteria: 2
            }
        );
        let row = stored(&pool, &spec.id).await;
        assert_eq!(row.status, SpecStatus::Extracted);
        assert_eq!(row.kind, SpecKind::Spec);
        let criteria = list_criteria(&pool, &spec.id).await.unwrap();
        let ordinals: Vec<i64> = criteria.iter().map(|c| c.ordinal).collect();
        let texts: Vec<&str> = criteria.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(ordinals, vec![1, 2]);
        assert_eq!(texts, vec!["first", "second"]);
        let sha = content_sha(SMALL_SPEC.as_bytes());
        assert!(criteria.iter().all(|c| c.spec_sha == sha));
        assert!(criteria.iter().all(|c| c.status == CriterionStatus::Open));
        let prompts = runner.prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains(SMALL_SPEC));
    }

    #[tokio::test]
    async fn stores_deps_and_questions_of_the_extraction() {
        // Both project shapes: a NULL project must still find its own registry paths.
        for project in [PROJECT, None] {
            let pool = crate::testdb::fresh_pool().await;
            let dir = tempfile::tempdir().unwrap();
            let target_path = ".ai/specs/target.md";
            let ghost_path = ".ai/specs/ghost.md";
            let elsewhere_path = ".ai/specs/elsewhere.md";
            register_in(&pool, dir.path(), project, "target.md", "# Target\n").await;
            // Registered, but in another project: as unknown here as the ghost.
            upsert_by_path(&pool, Some("proj-2"), elsewhere_path, "Elsewhere", "sha-e")
                .await
                .unwrap();
            let (spec, _) = register_in(&pool, dir.path(), project, "alpha.md", SMALL_SPEC).await;
            let answer = fenced(json!({
                "kind": "spec",
                "criteria": [criterion("first")],
                "depends_on": [target_path, ghost_path, elsewhere_path],
                "questions": ["Which port?"],
            }));
            let runner = FakeRunner::answering(&[answer]);

            let outcome = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
                .await
                .unwrap();

            assert_eq!(
                outcome,
                ExtractOutcome::Stored {
                    kind: SpecKind::Spec,
                    criteria: 1
                }
            );
            assert_eq!(
                list_deps(&pool, &spec.id).await.unwrap(),
                vec![Dep {
                    spec_id: spec.id.clone(),
                    depends_on: spec_id(project, target_path),
                    source: DepSource::Extracted,
                }]
            );
            let questions: Vec<String> = list_questions(&pool, &spec.id)
                .await
                .unwrap()
                .into_iter()
                .map(|question| question.question)
                .collect();
            assert_eq!(questions.len(), 3, "{questions:?}");
            assert!(
                questions.iter().any(|q| q == "Which port?"),
                "{questions:?}"
            );
            assert!(
                questions.iter().any(|q| q.contains(ghost_path)),
                "{questions:?}"
            );
            assert!(
                questions.iter().any(|q| q.contains(elsewhere_path)),
                "{questions:?}"
            );
        }
    }

    #[tokio::test]
    async fn retries_once_with_the_error_then_needs_owner() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = register(&pool, dir.path(), "alpha.md", SMALL_SPEC).await;
        seed_criterion(&pool, &spec, "from an earlier extraction").await;
        let criteria_before = list_criteria(&pool, &spec.id).await.unwrap();
        let runner = FakeRunner::answering(&[NOT_JSON.to_owned(), NO_CRITERIA.to_owned()]);

        let outcome = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap();

        let first_error = ParseError::NoJsonBlock.to_string();
        let second_error = ParseError::NoCriteria.to_string();
        let ExtractOutcome::NeedsOwner { error } = outcome else {
            panic!("expected NeedsOwner, got {outcome:?}");
        };
        assert!(error.contains(&second_error), "{error}");

        let prompts = runner.prompts();
        assert_eq!(prompts.len(), 2);
        assert!(!prompts[0].contains(&first_error));
        assert!(prompts[1].contains(&first_error));

        let row = stored(&pool, &spec.id).await;
        assert_eq!(row.status, SpecStatus::NeedsOwner);
        let reason = row.status_reason.expect("a status reason");
        assert!(reason.contains(&error), "{reason}");
        let questions = list_questions(&pool, &spec.id).await.unwrap();
        assert!(
            questions.iter().any(|q| q.status == "open"),
            "{questions:?}"
        );
        assert_eq!(
            list_criteria(&pool, &spec.id).await.unwrap(),
            criteria_before
        );
    }

    #[tokio::test]
    async fn retry_that_succeeds_stores_normally() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = register(&pool, dir.path(), "alpha.md", SMALL_SPEC).await;
        let runner = FakeRunner::answering(&[NOT_JSON.to_owned(), spec_answer(&["first"])]);

        let outcome = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            ExtractOutcome::Stored {
                kind: SpecKind::Spec,
                criteria: 1
            }
        );
        assert_eq!(runner.prompts().len(), 2);
        assert!(list_questions(&pool, &spec.id).await.unwrap().is_empty());
        assert_eq!(stored(&pool, &spec.id).await.status, SpecStatus::Extracted);
    }

    /// A review or a note stays registered, with its kind and no criteria: the root's decision is
    /// that these kinds are out of the finish backlog, not out of the registry.
    async fn assert_kind_is_registered_without_criteria(
        file_name: &str,
        raw_kind: &str,
        kind: SpecKind,
    ) {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) =
            register(&pool, dir.path(), file_name, "# Findings\nNothing to do.\n").await;
        let runner = FakeRunner::answering(&[empty_answer(raw_kind)]);

        let outcome = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap();

        assert_eq!(outcome, ExtractOutcome::Stored { kind, criteria: 0 });
        let row = stored(&pool, &spec.id).await;
        assert_eq!(row.kind, kind);
        assert_ne!(row.status, SpecStatus::NeedsOwner);
        assert!(list_criteria(&pool, &spec.id).await.unwrap().is_empty());
        assert_eq!(runner.prompts().len(), 1);
    }

    #[tokio::test]
    async fn review_kind_file_is_not_a_spec() {
        assert_kind_is_registered_without_criteria(
            "2026-01-01-alpha-REVIEW.md",
            "review",
            SpecKind::Review,
        )
        .await;
    }

    #[tokio::test]
    async fn note_kind_is_registered_without_criteria() {
        assert_kind_is_registered_without_criteria("2026-01-01-note.md", "note", SpecKind::Note)
            .await;
    }

    #[tokio::test]
    async fn ambiguous_criterion_is_stored_as_doubt() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = register(&pool, dir.path(), "alpha.md", SMALL_SPEC).await;
        let answer = fenced(json!({
            "kind": "spec",
            "criteria": [
                {"text": "vague", "kind": "behavior", "verify_hint": null, "ambiguous": true},
                criterion("clear"),
            ],
            "depends_on": [],
            "questions": [],
        }));
        let runner = FakeRunner::answering(&[answer]);

        extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap();

        let criteria = list_criteria(&pool, &spec.id).await.unwrap();
        let statuses: Vec<CriterionStatus> = criteria.iter().map(|c| c.status).collect();
        assert_eq!(
            statuses,
            vec![CriterionStatus::Doubt, CriterionStatus::Open]
        );
    }

    #[tokio::test]
    async fn large_spec_runs_one_prompt_per_section() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let filler = "x".repeat(30_000);
        let text = format!("## First\n{filler}\n## Second\n{filler}\n## Third\n{filler}\n");
        assert!(text.chars().count() > MAX_SPEC_CHARS);
        let sections = split_sections(&text);
        assert_eq!(sections.len(), 3);
        let (spec, _) = register(&pool, dir.path(), "large.md", &text).await;
        let runner = FakeRunner::answering(&[
            spec_answer(&["first a", "first b"]),
            spec_answer(&["second a"]),
            spec_answer(&["third a"]),
        ]);

        let outcome = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            ExtractOutcome::Stored {
                kind: SpecKind::Spec,
                criteria: 4
            }
        );
        let prompts = runner.prompts();
        assert_eq!(prompts.len(), sections.len());
        for (prompt, section) in prompts.iter().zip(&sections) {
            assert!(prompt.contains(section.as_str()));
        }
        let criteria = list_criteria(&pool, &spec.id).await.unwrap();
        let ordinals: Vec<i64> = criteria.iter().map(|c| c.ordinal).collect();
        let texts: Vec<&str> = criteria.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(ordinals, vec![1, 2, 3, 4]);
        assert_eq!(texts, vec!["first a", "first b", "second a", "third a"]);
    }

    #[tokio::test]
    async fn changed_file_is_refused_without_a_run() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, file) = register(&pool, dir.path(), "alpha.md", SMALL_SPEC).await;
        seed_criterion(&pool, &spec, "from an earlier extraction").await;
        let criteria_before = list_criteria(&pool, &spec.id).await.unwrap();
        fs::write(&file, "# Alpha\nRewritten after registration.\n").unwrap();
        let runner = FakeRunner::new(Vec::new());

        let error = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap_err();

        match error {
            ExtractError::SpecChanged { spec_id: changed } => assert_eq!(changed, spec.id),
            other => panic!("expected SpecChanged, got {other:?}"),
        }
        assert!(runner.prompts().is_empty());
        assert_eq!(
            list_criteria(&pool, &spec.id).await.unwrap(),
            criteria_before
        );
    }

    #[tokio::test]
    async fn runner_failure_is_an_error_and_not_needs_owner() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, _) = register(&pool, dir.path(), "alpha.md", SMALL_SPEC).await;
        let runner = FakeRunner::new(vec![Err("the agent crashed".to_owned())]);

        let error = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap_err();

        assert!(matches!(error, ExtractError::Runner(_)), "{error:?}");
        assert_eq!(runner.prompts().len(), 1);
        let status = stored(&pool, &spec.id).await.status;
        assert_ne!(status, SpecStatus::NeedsOwner);
        assert_ne!(status, SpecStatus::Extracting);
    }

    // The owner's rule: a spec file is never altered, by anything in the backlog.
    #[tokio::test]
    async fn extract_never_alters_the_spec_file() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let (spec, file) = register(&pool, dir.path(), "alpha.md", SMALL_SPEC).await;
        let original = fs::metadata(&file).unwrap().permissions();
        let mut read_only = original.clone();
        read_only.set_readonly(true);
        fs::set_permissions(&file, read_only).unwrap();
        let modified_before = fs::metadata(&file).unwrap().modified().unwrap();
        let runner = FakeRunner::answering(&[spec_answer(&["first"])]);

        let outcome = extract(&pool, &roots_at(dir.path()), &spec.id, &runner)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            ExtractOutcome::Stored {
                kind: SpecKind::Spec,
                criteria: 1
            }
        );
        assert_eq!(fs::read(&file).unwrap(), SMALL_SPEC.as_bytes());
        assert_eq!(
            fs::metadata(&file).unwrap().modified().unwrap(),
            modified_before
        );

        fs::set_permissions(&file, original).unwrap();
    }

    #[tokio::test]
    async fn unknown_spec_is_not_found() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let runner = FakeRunner::new(Vec::new());

        let error = extract(&pool, &roots_at(dir.path()), "no-such-spec", &runner)
            .await
            .unwrap_err();

        assert!(matches!(error, ExtractError::NotFound(_)), "{error:?}");
        assert!(runner.prompts().is_empty());
    }
}
