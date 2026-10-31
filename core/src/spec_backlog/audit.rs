//! Independent review and verification of extracted specs.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

use sqlx::SqlitePool;

use super::model::{Criterion, CriterionStatus, Spec, SpecKind, SpecStatus};
use super::store::{self, StoreError};
use super::verdict::{done, ReviewFacts, ReviewerRun, Verdict, VerifyState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionReview {
    pub ordinal: i64,
    pub status: CriterionStatus,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerAnswer {
    pub run: ReviewerRun,
    pub reviews: Vec<CriterionReview>,
}

#[async_trait::async_trait]
pub trait ReviewerRunner: Send + Sync {
    async fn review(&self, spec: &Spec, criteria: &[Criterion]) -> Result<ReviewerAnswer, String>;
}

#[async_trait::async_trait]
pub trait VerifyRunner: Send + Sync {
    async fn verify(&self, spec: &Spec, criteria: &[Criterion]) -> VerifyState;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecLineage {
    pub run_ids: Vec<String>,
    pub lineage_roots: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AuditReport {
    pub reviewer_run_id: String,
    pub verify: VerifyState,
    pub verdict: Verdict,
    pub status: SpecStatus,
}

#[derive(Debug)]
pub enum AuditError {
    Store(StoreError),
    NotFound(String),
    NotASpec(SpecKind),
    NoCriteria,
    StaleCriteria,
    Reviewer(String),
    ReviewerNotNew { run_id: String },
}

impl From<StoreError> for AuditError { fn from(error: StoreError) -> Self { Self::Store(error) } }

pub(super) fn safe_relative_path(worktree: &Path, path: &str) -> Option<std::path::PathBuf> {
    let relative = Path::new(path);
    if relative.is_absolute() || path.is_empty() || relative.components().any(|component| !matches!(component, Component::Normal(_))) {
        return None;
    }
    Some(worktree.join(relative))
}

pub fn evidence_exists(worktree: &Path, evidence: &str) -> bool {
    let Some((path, target)) = evidence.rsplit_once(':').or_else(|| evidence.rsplit_once('#')) else { return false; };
    let Some(file) = safe_relative_path(worktree, path) else { return false; };
    let Ok(contents) = fs::read_to_string(file) else { return false; };
    if evidence.contains(':') {
        return target.parse::<usize>().ok().is_some_and(|line| line > 0 && contents.lines().nth(line - 1).is_some());
    }
    if target.is_empty() { return false; }
    contents.lines().any(|line| {
        let line = line.trim_start();
        let name = line.strip_prefix("fn ").or_else(|| line.strip_prefix("pub fn "));
        name.is_some_and(|name| name.strip_prefix(target).is_some_and(|suffix| {
            suffix.starts_with('(') || suffix.starts_with('<') || suffix.starts_with(' ')
        }))
    })
}

pub async fn audit(
    pool: &SqlitePool, spec_id: &str, worktree: &Path, lineage: &SpecLineage,
    reviewer: &dyn ReviewerRunner, verify: &dyn VerifyRunner,
) -> Result<AuditReport, AuditError> {
    let spec = store::get_spec(pool, spec_id).await?.ok_or_else(|| AuditError::NotFound(spec_id.to_owned()))?;
    if matches!(spec.kind, SpecKind::Review | SpecKind::Note) { return Err(AuditError::NotASpec(spec.kind)); }
    let criteria = store::list_criteria(pool, spec_id).await?;
    if criteria.is_empty() { return Err(AuditError::NoCriteria); }
    if criteria.iter().any(|criterion| criterion.status == CriterionStatus::Stale || criterion.spec_sha != spec.content_sha) {
        return Err(AuditError::StaleCriteria);
    }
    let answer = reviewer.review(&spec, &criteria).await.map_err(AuditError::Reviewer)?;
    let mut forbidden: HashSet<&str> = lineage.run_ids.iter().map(String::as_str).collect();
    forbidden.extend(lineage.lineage_roots.iter().map(String::as_str));
    forbidden.extend(criteria.iter().filter_map(|criterion| criterion.reviewer_run_id.as_deref()));
    if forbidden.contains(answer.run.run_id.as_str()) || answer.run.lineage_root_id.as_deref().is_some_and(|root| forbidden.contains(root)) {
        return Err(AuditError::ReviewerNotNew { run_id: answer.run.run_id });
    }
    for criterion in &criteria {
        let review = answer.reviews.iter().find(|review| review.ordinal == criterion.ordinal);
        let (status, evidence) = match review {
            None => (CriterionStatus::Doubt, None),
            Some(review) if review.evidence.as_deref().is_some_and(|evidence| !evidence_exists(worktree, evidence)) => (CriterionStatus::Doubt, review.evidence.as_deref()),
            Some(review) if review.status == CriterionStatus::Met && review.evidence.as_deref().is_some_and(|evidence| evidence_exists(worktree, evidence)) => (CriterionStatus::Met, review.evidence.as_deref()),
            Some(review) if review.status == CriterionStatus::Unmet => (CriterionStatus::Unmet, review.evidence.as_deref()),
            Some(review) => (CriterionStatus::Doubt, review.evidence.as_deref()),
        };
        store::set_criterion_review(pool, spec_id, criterion.ordinal, status, evidence, &answer.run.run_id).await?;
        if status == CriterionStatus::Doubt {
            store::add_question(pool, spec_id, &format!("Owner decision needed for criterion: {}", criterion.text)).await?;
        }
    }
    let reviewed = store::list_criteria(pool, spec_id).await?;
    let verify_state = verify.verify(&spec, &reviewed).await;
    let existing_evidence = reviewed.iter().filter_map(|criterion| criterion.evidence.as_deref())
        .filter(|evidence| evidence_exists(worktree, evidence)).map(str::to_owned).collect();
    let facts = ReviewFacts {
        tree_run_ids: lineage.run_ids.clone(), tree_lineage_roots: lineage.lineage_roots.clone(),
        reviewers: vec![answer.run.clone()], existing_evidence,
    };
    let verdict = done(&spec, &reviewed, verify_state, &facts);
    let status = if verdict == Verdict::Done { SpecStatus::Done } else { SpecStatus::Audited };
    let reason = (status != SpecStatus::Done).then(|| format!("{verdict:?}"));
    store::set_audit_result(pool, spec_id, status, reason.as_deref(), &spec.content_sha).await?;
    Ok(AuditReport { reviewer_run_id: answer.run.run_id, verify: verify_state, verdict, status })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_backlog::model::{
        Criterion, CriterionKind, CriterionStatus, NewCriterion, Question, Spec, SpecKind,
        SpecStatus,
    };
    use crate::spec_backlog::store;
    use crate::spec_backlog::verdict::{NotDoneReason, ReviewerRun, Verdict, VerifyState};
    use sqlx::SqlitePool;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::SystemTime;

    const SPEC_PATH: &str = ".ai/specs/2026-01-01-alpha.md";
    const SHA: &str = "sha-1";
    const NEW_SHA: &str = "sha-2";
    const LIB_RS: &str = "pub fn first() {}\n\n#[test]\nfn some_test() {}\n";
    const GOOD: &str = "src/lib.rs:1";

    struct FakeReviewer {
        answer: Result<ReviewerAnswer, String>,
        calls: AtomicUsize,
    }

    impl FakeReviewer {
        fn answering(run_id: &str, root: Option<&str>, reviews: Vec<CriterionReview>) -> Self {
            let run = ReviewerRun {
                run_id: run_id.to_owned(),
                lineage_root_id: root.map(str::to_owned),
            };
            Self {
                answer: Ok(ReviewerAnswer { run, reviews }),
                calls: AtomicUsize::new(0),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                answer: Err(message.to_owned()),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ReviewerRunner for FakeReviewer {
        async fn review(
            &self,
            _spec: &Spec,
            _criteria: &[Criterion],
        ) -> Result<ReviewerAnswer, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    struct FakeVerify {
        state: VerifyState,
        calls: AtomicUsize,
    }

    impl FakeVerify {
        fn new(state: VerifyState) -> Self {
            Self {
                state,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl VerifyRunner for FakeVerify {
        async fn verify(&self, _spec: &Spec, _criteria: &[Criterion]) -> VerifyState {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.state
        }
    }

    fn review(ordinal: i64, status: CriterionStatus, evidence: Option<&str>) -> CriterionReview {
        CriterionReview {
            ordinal,
            status,
            evidence: evidence.map(str::to_owned),
        }
    }

    fn met(ordinal: i64, evidence: &str) -> CriterionReview {
        review(ordinal, CriterionStatus::Met, Some(evidence))
    }

    fn populate(root: &Path) {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), LIB_RS).unwrap();
        fs::create_dir_all(root.join(".ai/specs")).unwrap();
        fs::write(root.join(SPEC_PATH), "# Alpha\n").unwrap();
    }

    fn worktree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        populate(dir.path());
        dir
    }

    /// Every path under `root` with its bytes (none for a directory) and mtime.
    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
        let mut seen = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let meta = fs::metadata(&path).unwrap();
            let modified = meta.modified().unwrap();
            let bytes = if meta.is_dir() {
                for entry in fs::read_dir(&path).unwrap() {
                    pending.push(entry.unwrap().path());
                }
                Vec::new()
            } else {
                fs::read(&path).unwrap()
            };
            seen.insert(path, (bytes, modified));
        }
        seen
    }

    async fn exec(pool: &SqlitePool, sql: &'static str, binds: &[&str]) {
        let mut query = sqlx::query(sql);
        for value in binds {
            query = query.bind((*value).to_owned());
        }
        query.execute(pool).await.unwrap();
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

    struct Fixture {
        pool: SqlitePool,
        worktree: tempfile::TempDir,
        id: String,
    }

    impl Fixture {
        /// A registered spec of kind `spec`, status `extracted`, with one open criterion per text.
        async fn new(texts: &[&str]) -> Self {
            let pool = crate::testdb::fresh_pool().await;
            let spec = store::upsert_by_path(&pool, None, SPEC_PATH, "Title", SHA)
                .await
                .unwrap();
            store::replace_criteria(&pool, &spec.id, SHA, &open_items(texts))
                .await
                .unwrap();
            exec(
                &pool,
                "UPDATE specs SET kind = 'spec', status = 'extracted' WHERE id = ?",
                &[spec.id.as_str()],
            )
            .await;
            Self {
                pool,
                worktree: worktree(),
                id: spec.id,
            }
        }

        async fn run(
            &self,
            reviewer: &FakeReviewer,
            verify: &FakeVerify,
        ) -> Result<AuditReport, AuditError> {
            self.run_in(&SpecLineage::default(), reviewer, verify).await
        }

        async fn run_in(
            &self,
            lineage: &SpecLineage,
            reviewer: &FakeReviewer,
            verify: &FakeVerify,
        ) -> Result<AuditReport, AuditError> {
            audit(
                &self.pool,
                &self.id,
                self.worktree.path(),
                lineage,
                reviewer,
                verify,
            )
            .await
        }

        async fn spec(&self) -> Spec {
            store::get_spec(&self.pool, &self.id)
                .await
                .unwrap()
                .unwrap()
        }

        async fn criteria(&self) -> Vec<Criterion> {
            store::list_criteria(&self.pool, &self.id).await.unwrap()
        }

        async fn questions(&self) -> Vec<Question> {
            store::list_questions(&self.pool, &self.id).await.unwrap()
        }

        /// All the audit may write, to compare before and after a refusal.
        async fn state(&self) -> (Spec, Vec<Criterion>, Vec<Question>) {
            (
                self.spec().await,
                self.criteria().await,
                self.questions().await,
            )
        }

        async fn has_open_question_about(&self, text: &str) -> bool {
            self.questions()
                .await
                .iter()
                .any(|q| q.status == "open" && q.question.contains(text))
        }
    }

    fn reasons(verdict: &Verdict) -> &[NotDoneReason] {
        match verdict {
            Verdict::NotDone(reasons) => reasons,
            Verdict::Done => panic!("expected NotDone, got Done"),
        }
    }

    fn has_reason_text(spec: &Spec) -> bool {
        spec.status_reason
            .as_deref()
            .is_some_and(|reason| !reason.trim().is_empty())
    }

    #[tokio::test]
    async fn marks_criterion_met_only_with_evidence() {
        let fx = Fixture::new(&["the widget renders"]).await;
        let reviewer = FakeReviewer::answering("review-1", None, vec![met(1, GOOD)]);
        let verify = FakeVerify::new(VerifyState::Green);

        fx.run(&reviewer, &verify).await.unwrap();

        let criteria = fx.criteria().await;
        assert_eq!(criteria.len(), 1);
        assert_eq!(criteria[0].status, CriterionStatus::Met);
        assert_eq!(criteria[0].evidence.as_deref(), Some(GOOD));
        assert_eq!(criteria[0].reviewer_run_id.as_deref(), Some("review-1"));
        assert!(criteria[0].checked_at.is_some());
        assert!(fx.questions().await.is_empty());
    }

    #[tokio::test]
    async fn criterion_without_evidence_becomes_doubt() {
        let text = "the widget renders";
        let fx = Fixture::new(&[text]).await;
        let reviewer = FakeReviewer::answering(
            "review-1",
            None,
            vec![review(1, CriterionStatus::Met, None)],
        );
        let verify = FakeVerify::new(VerifyState::Green);

        let report = fx.run(&reviewer, &verify).await.unwrap();

        let criteria = fx.criteria().await;
        assert_eq!(criteria[0].status, CriterionStatus::Doubt);
        assert_eq!(criteria[0].evidence, None);
        assert_eq!(report.status, SpecStatus::Audited);
        assert!(reasons(&report.verdict).contains(&NotDoneReason::Unresolved {
            ordinal: 1,
            status: CriterionStatus::Doubt,
        }));
        let spec = fx.spec().await;
        assert_eq!(spec.status, SpecStatus::Audited);
        assert!(has_reason_text(&spec), "{:?}", spec.status_reason);
        assert_eq!(spec.last_audited_sha.as_deref(), Some(SHA));
        assert!(fx.has_open_question_about(text).await);
    }

    #[tokio::test]
    async fn citation_of_a_missing_file_or_line_becomes_doubt() {
        let (first, second) = ("the widget renders", "the widget resizes");
        let fx = Fixture::new(&[first, second]).await;
        let (absent_file, absent_line) = ("src/absent.rs:1", "src/lib.rs:9999");
        let reviewer = FakeReviewer::answering(
            "review-1",
            None,
            vec![met(1, absent_file), met(2, absent_line)],
        );
        let verify = FakeVerify::new(VerifyState::Green);

        let report = fx.run(&reviewer, &verify).await.unwrap();

        let criteria = fx.criteria().await;
        assert_eq!(criteria[0].status, CriterionStatus::Doubt);
        assert_eq!(criteria[0].evidence.as_deref(), Some(absent_file));
        assert_eq!(criteria[1].status, CriterionStatus::Doubt);
        assert_eq!(criteria[1].evidence.as_deref(), Some(absent_line));
        assert!(matches!(report.verdict, Verdict::NotDone(_)));
        assert_ne!(report.status, SpecStatus::Done);
        assert_ne!(fx.spec().await.status, SpecStatus::Done);
        assert!(fx.has_open_question_about(first).await);
        assert!(fx.has_open_question_about(second).await);
    }

    #[tokio::test]
    async fn criterion_left_unreviewed_becomes_doubt() {
        let texts = [
            "reviewed and met",
            "never reviewed",
            "reviewed and unmet",
            "reviewed as open",
            "reviewed as stale",
        ];
        let fx = Fixture::new(&texts).await;
        let reviewer = FakeReviewer::answering(
            "review-1",
            None,
            vec![
                met(1, GOOD),
                review(3, CriterionStatus::Unmet, None),
                review(4, CriterionStatus::Open, None),
                review(5, CriterionStatus::Stale, None),
                met(99, GOOD),
            ],
        );
        let verify = FakeVerify::new(VerifyState::Green);

        fx.run(&reviewer, &verify).await.unwrap();

        let criteria = fx.criteria().await;
        let stored: Vec<(i64, CriterionStatus)> =
            criteria.iter().map(|c| (c.ordinal, c.status)).collect();
        assert_eq!(
            stored,
            vec![
                (1, CriterionStatus::Met),
                (2, CriterionStatus::Doubt),
                (3, CriterionStatus::Unmet),
                (4, CriterionStatus::Doubt),
                (5, CriterionStatus::Doubt),
            ],
            "ordinal 99 must create no row"
        );
        for criterion in &criteria {
            assert_eq!(criterion.reviewer_run_id.as_deref(), Some("review-1"));
            assert!(criterion.checked_at.is_some());
        }
        // One question per doubt: an unmet criterion asks the owner nothing.
        assert_eq!(fx.questions().await.len(), 3);
        for text in [texts[1], texts[3], texts[4]] {
            assert!(fx.has_open_question_about(text).await, "{text}");
        }
    }

    #[tokio::test]
    async fn reviewer_run_must_be_new() {
        let fx = Fixture::new(&["the widget renders", "the widget resizes"]).await;
        let lineage = SpecLineage {
            run_ids: vec!["tree-run".to_owned()],
            lineage_roots: vec!["tree-root".to_owned()],
        };
        let reviews = || vec![met(1, GOOD), met(2, GOOD)];

        let refused = |run_id: &'static str, root: Option<&'static str>| {
            (
                run_id,
                root,
                FakeReviewer::answering(run_id, root, reviews()),
            )
        };
        let in_lineage = [
            refused("tree-run", None),
            refused("tree-root", None),
            refused("fresh", Some("tree-run")),
            refused("fresh", Some("tree-root")),
        ];
        for (run_id, root, reviewer) in &in_lineage {
            let before = fx.state().await;
            let verify = FakeVerify::new(VerifyState::Green);
            let result = fx.run_in(&lineage, reviewer, &verify).await;
            let case = format!("{run_id} / {root:?}");
            assert!(
                matches!(result, Err(AuditError::ReviewerNotNew { .. })),
                "{case}: {result:?}"
            );
            assert_eq!(verify.calls(), 0, "{case}");
            assert_eq!(reviewer.calls(), 1, "{case}");
            assert_eq!(fx.state().await, before, "{case}");
        }

        // A run that already reviewed this spec, in another audit, is not new either.
        exec(
            &fx.pool,
            "UPDATE spec_criteria SET reviewer_run_id = 'old-review' \
             WHERE spec_id = ? AND ordinal = 2",
            &[fx.id.as_str()],
        )
        .await;
        let already_reviewed = [
            refused("old-review", None),
            refused("fresh", Some("old-review")),
        ];
        for (run_id, root, reviewer) in &already_reviewed {
            let before = fx.state().await;
            let verify = FakeVerify::new(VerifyState::Green);
            let result = fx.run_in(&lineage, reviewer, &verify).await;
            let case = format!("{run_id} / {root:?}");
            assert!(
                matches!(result, Err(AuditError::ReviewerNotNew { .. })),
                "{case}: {result:?}"
            );
            assert_eq!(verify.calls(), 0, "{case}");
            assert_eq!(fx.state().await, before, "{case}");
        }

        let reviewer = FakeReviewer::answering("fresh", Some("unrelated-root"), reviews());
        let verify = FakeVerify::new(VerifyState::Green);
        let report = fx.run_in(&lineage, &reviewer, &verify).await.unwrap();
        assert_eq!(report.reviewer_run_id, "fresh");
        assert_eq!(report.verdict, Verdict::Done);
        assert_eq!(verify.calls(), 1);
        for criterion in fx.criteria().await {
            assert_eq!(criterion.reviewer_run_id.as_deref(), Some("fresh"));
        }
    }

    #[tokio::test]
    async fn verify_red_keeps_spec_not_done() {
        for state in [VerifyState::Red, VerifyState::Unknown] {
            let fx = Fixture::new(&["the widget renders"]).await;
            let reviewer = FakeReviewer::answering("review-1", None, vec![met(1, GOOD)]);
            let verify = FakeVerify::new(state);

            let report = fx.run(&reviewer, &verify).await.unwrap();

            assert_eq!(report.verify, state);
            assert!(
                reasons(&report.verdict).contains(&NotDoneReason::VerifyNotGreen),
                "{state:?}"
            );
            assert_eq!(report.status, SpecStatus::Audited, "{state:?}");
            let spec = fx.spec().await;
            assert_eq!(spec.status, SpecStatus::Audited, "{state:?}");
            assert!(has_reason_text(&spec), "{state:?}");
            assert_eq!(spec.last_audited_sha.as_deref(), Some(SHA), "{state:?}");
            assert_eq!(fx.criteria().await[0].status, CriterionStatus::Met);
        }
    }

    #[tokio::test]
    async fn all_met_and_green_marks_spec_done() {
        let fx = Fixture::new(&["the widget renders", "the widget resizes"]).await;
        let lineage = SpecLineage {
            run_ids: vec!["tree-run".to_owned()],
            lineage_roots: vec!["tree-root".to_owned()],
        };
        let reviewer = FakeReviewer::answering(
            "review-1",
            Some("review-root"),
            vec![met(1, GOOD), met(2, "src/lib.rs#some_test")],
        );
        let verify = FakeVerify::new(VerifyState::Green);

        let report = fx.run_in(&lineage, &reviewer, &verify).await.unwrap();

        assert_eq!(report.reviewer_run_id, "review-1");
        assert_eq!(report.verify, VerifyState::Green);
        assert_eq!(report.verdict, Verdict::Done);
        assert_eq!(report.status, SpecStatus::Done);
        assert_eq!(reviewer.calls(), 1);
        assert_eq!(verify.calls(), 1);
        let spec = fx.spec().await;
        assert_eq!(spec.status, SpecStatus::Done);
        assert_eq!(
            spec.last_audited_sha.as_deref(),
            Some(spec.content_sha.as_str())
        );
        assert!(fx.questions().await.is_empty());
    }

    #[tokio::test]
    async fn doubt_is_never_done() {
        let doubted = "the widget resizes";
        let fx = Fixture::new(&["the widget renders", doubted, "the widget closes"]).await;
        // The doubt carries a citation that exists: a reviewer's doubt still stands.
        let reviewer = FakeReviewer::answering(
            "review-1",
            None,
            vec![
                met(1, GOOD),
                review(2, CriterionStatus::Doubt, Some(GOOD)),
                met(3, GOOD),
            ],
        );
        let verify = FakeVerify::new(VerifyState::Green);

        let report = fx.run(&reviewer, &verify).await.unwrap();

        assert_eq!(fx.criteria().await[1].status, CriterionStatus::Doubt);
        assert!(reasons(&report.verdict).contains(&NotDoneReason::Unresolved {
            ordinal: 2,
            status: CriterionStatus::Doubt,
        }));
        assert_ne!(report.status, SpecStatus::Done);
        assert_ne!(fx.spec().await.status, SpecStatus::Done);
        assert_eq!(fx.questions().await.len(), 1);
        assert!(fx.has_open_question_about(doubted).await);
    }

    #[tokio::test]
    async fn audit_writes_nothing_to_the_checkout() {
        let fx = Fixture::new(&["the widget renders"]).await;
        let root = fx.worktree.path();
        let before = snapshot_tree(root);
        assert!(before.contains_key(&root.join(SPEC_PATH)));
        assert!(before.contains_key(&root.join("src/lib.rs")));
        let reviewer = FakeReviewer::answering("review-1", None, vec![met(1, GOOD)]);
        let verify = FakeVerify::new(VerifyState::Green);

        let report = fx.run(&reviewer, &verify).await.unwrap();

        assert_eq!(report.status, SpecStatus::Done);
        assert_eq!(snapshot_tree(root), before);
        assert_eq!(fx.spec().await.tree_id, None);

        // The source half: production code cannot open a file for writing nor start a tree.
        let production = include_str!("audit.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "OpenOptions",
            "File::create",
            "fs::write",
            "create_dir",
            "remove_file",
            "tree_job::",
            "create_run",
        ] {
            assert!(!production.contains(forbidden), "{forbidden}");
        }
        assert!(production.contains("evidence_exists"));
    }

    #[tokio::test]
    async fn review_and_note_kinds_are_refused() {
        for kind in [SpecKind::Review, SpecKind::Note] {
            let fx = Fixture::new(&["the widget renders"]).await;
            exec(
                &fx.pool,
                "UPDATE specs SET kind = ? WHERE id = ?",
                &[kind.as_str(), fx.id.as_str()],
            )
            .await;
            let before = fx.state().await;
            let reviewer = FakeReviewer::answering("review-1", None, vec![met(1, GOOD)]);
            let verify = FakeVerify::new(VerifyState::Green);

            let result = fx.run(&reviewer, &verify).await;

            assert!(
                matches!(result, Err(AuditError::NotASpec(refused)) if refused == kind),
                "{kind:?}: {result:?}"
            );
            assert_eq!(reviewer.calls(), 0, "{kind:?}");
            assert_eq!(verify.calls(), 0, "{kind:?}");
            assert_eq!(fx.state().await, before, "{kind:?}");
        }
    }

    #[tokio::test]
    async fn no_criteria_is_refused_without_a_run() {
        let fx = Fixture::new(&[]).await;
        let before = fx.state().await;
        let reviewer = FakeReviewer::answering("review-1", None, Vec::new());
        let verify = FakeVerify::new(VerifyState::Green);

        let result = fx.run(&reviewer, &verify).await;

        assert!(matches!(result, Err(AuditError::NoCriteria)), "{result:?}");
        assert_eq!(reviewer.calls(), 0);
        assert_eq!(verify.calls(), 0);
        assert_eq!(fx.state().await, before);
    }

    #[tokio::test]
    async fn stale_criteria_are_refused_without_a_run() {
        let texts = ["the widget renders", "the widget resizes"];
        let fx = Fixture::new(&texts).await;
        store::upsert_by_path(&fx.pool, None, SPEC_PATH, "Title", NEW_SHA)
            .await
            .unwrap();

        async fn assert_refused(fx: &Fixture, case: &str) {
            let before = fx.state().await;
            let reviewer =
                FakeReviewer::answering("review-1", None, vec![met(1, GOOD), met(2, GOOD)]);
            let verify = FakeVerify::new(VerifyState::Green);
            let result = fx.run(&reviewer, &verify).await;
            assert!(
                matches!(result, Err(AuditError::StaleCriteria)),
                "{case}: {result:?}"
            );
            assert_eq!(reviewer.calls(), 0, "{case}");
            assert_eq!(verify.calls(), 0, "{case}");
            assert_eq!(fx.state().await, before, "{case}");
        }

        assert_refused(&fx, "marked stale by the upsert").await;

        // An old `spec_sha` alone is enough, whatever the status says.
        exec(
            &fx.pool,
            "UPDATE spec_criteria SET status = 'open' WHERE spec_id = ?",
            &[fx.id.as_str()],
        )
        .await;
        assert_refused(&fx, "old spec_sha, status open").await;

        // And one stale criterion among current ones.
        store::replace_criteria(&fx.pool, &fx.id, NEW_SHA, &open_items(&texts))
            .await
            .unwrap();
        exec(
            &fx.pool,
            "UPDATE spec_criteria SET status = 'stale' WHERE spec_id = ? AND ordinal = 2",
            &[fx.id.as_str()],
        )
        .await;
        assert_refused(&fx, "current spec_sha, one status stale").await;
    }

    #[tokio::test]
    async fn reviewer_failure_changes_nothing() {
        let fx = Fixture::new(&["the widget renders"]).await;
        let before = fx.state().await;
        let reviewer = FakeReviewer::failing("the run crashed");
        let verify = FakeVerify::new(VerifyState::Green);

        let result = fx.run(&reviewer, &verify).await;

        assert!(matches!(result, Err(AuditError::Reviewer(_))), "{result:?}");
        assert_eq!(reviewer.calls(), 1);
        assert_eq!(verify.calls(), 0);
        assert_eq!(fx.state().await, before);
    }

    #[tokio::test]
    async fn unknown_spec_is_not_found() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = worktree();
        let reviewer = FakeReviewer::answering("review-1", None, Vec::new());
        let verify = FakeVerify::new(VerifyState::Green);

        let result = audit(
            &pool,
            "no-such-spec",
            dir.path(),
            &SpecLineage::default(),
            &reviewer,
            &verify,
        )
        .await;

        assert!(matches!(result, Err(AuditError::NotFound(_))), "{result:?}");
        assert_eq!(reviewer.calls(), 0);
        assert_eq!(verify.calls(), 0);
    }

    #[test]
    fn evidence_exists_checks_file_and_line() {
        let dir = worktree();
        let root = dir.path();
        let last = LIB_RS.lines().count();

        for good in [
            "src/lib.rs:1".to_owned(),
            format!("src/lib.rs:{last}"),
            "src/lib.rs#some_test".to_owned(),
        ] {
            assert!(evidence_exists(root, &good), "{good:?}");
        }
        for bad in [
            String::new(),
            "   ".to_owned(),
            "src/lib.rs".to_owned(),
            "src/lib.rs:0".to_owned(),
            format!("src/lib.rs:{}", last + 1),
            "src/lib.rs:9999".to_owned(),
            "src/lib.rs:abc".to_owned(),
            "src/absent.rs:1".to_owned(),
            "src/lib.rs#absent_fn".to_owned(),
            "src/lib.rs#".to_owned(),
            "some_test".to_owned(),
            "src:1".to_owned(),
        ] {
            assert!(!evidence_exists(root, &bad), "{bad:?}");
        }
    }

    #[test]
    fn evidence_cannot_escape_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.rs");
        fs::write(&outside, "fn outside_fn() {}\n").unwrap();
        let root = dir.path().join("worktree");
        populate(&root);
        let absolute_outside = outside.display();
        let absolute_inside = root.join("src/lib.rs");
        let absolute_inside = absolute_inside.display();

        for bad in [
            "../outside.rs:1".to_owned(),
            "../outside.rs#outside_fn".to_owned(),
            "src/../src/lib.rs:1".to_owned(),
            format!("{absolute_outside}:1"),
            format!("{absolute_outside}#outside_fn"),
            format!("{absolute_inside}:1"),
        ] {
            assert!(!evidence_exists(&root, &bad), "{bad:?}");
        }
        // The worktree itself still resolves, so the refusals above are about the path.
        assert!(evidence_exists(&root, GOOD));
    }
}
