//! A write or rollback of the `devtime` document voids the pending shadow rows decided under the old
//! thresholds (`thresholds_hash`), and nothing else. The document's write is never in question.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::super::tests::{test_state, workflow_call};
use crate::config::{DevtimeActionsConfig, DevtimeConfig, VerifyConfig};
use crate::devtime::store::{ShadowRow, insert_decision, thresholds_hash};
use crate::project::rules::testing::Home;
use crate::settings::{self as store, Scope};
use crate::state::AppState;

const RESOLVED_AT: &str = "2026-10-05T12:01:00.000Z";

async fn fixture() -> (Home, AppState) {
    (Home::new(), test_state().await)
}

fn default_cfg() -> DevtimeConfig {
    DevtimeConfig::default()
}

/// The default document with `propose` tuned: only the `propose` hashes change.
fn tuned_cfg() -> DevtimeConfig {
    let mut cfg = DevtimeConfig::default();
    cfg.actions.propose_min_sessions = 5;
    cfg
}

fn body(cfg: &DevtimeConfig) -> Value {
    serde_json::to_value(cfg).unwrap()
}

/// Write `devtime` past the HTTP door (so no hook runs), as version `expected + 1`.
async fn seed_doc(state: &AppState, cfg: &DevtimeConfig, expected: i64) {
    store::put(
        &state.pool,
        &Scope::Machine,
        "devtime",
        &body(cfg),
        "test",
        Some(expected),
    )
    .await
    .expect("seed the devtime document");
}

/// One `devtime_shadow` row stamped with the hash `cfg` gives its pair, then moved to `outcome_state`.
async fn add_row(
    state: &AppState,
    action: &str,
    rule: &str,
    subject: &str,
    outcome_state: &str,
    cfg: &DevtimeActionsConfig,
) {
    let template = format!("{action}.{}.v1", rule.to_lowercase());
    let row = ShadowRow {
        action: action.into(),
        rule_id: rule.into(),
        project_id: "p1".into(),
        session_id: Some("s1".into()),
        subject_key: subject.into(),
        mode: "shadow".into(),
        decision: "fire".into(),
        template_id: template,
        rule_version: 1,
        action_version: 1,
        thresholds_hash: thresholds_hash(cfg, action, rule),
        rules_version: "rv".into(),
        decided_at: "2026-10-05T10:00:00.000Z".into(),
        window_end_at: "2026-10-05T10:30:00.000Z".into(),
        outcome_state: "pending".into(),
        ..ShadowRow::default()
    };
    assert!(insert_decision(&state.pool, &row).await.unwrap());
    if outcome_state != "pending" {
        sqlx::query(
            "UPDATE devtime_shadow SET outcome_state = ?, resolved_at = ? WHERE subject_key = ?",
        )
        .bind(outcome_state)
        .bind(RESOLVED_AT)
        .bind(subject)
        .execute(&state.pool)
        .await
        .unwrap();
    }
}

/// Five rows decided under the DEFAULT thresholds, plus one decided under the tuned ones:
/// `stale-f1` and `stale-f2` are the only ones a `propose` tuning makes stale.
async fn seed_rows(state: &AppState) {
    let old = default_cfg().actions;
    let new = tuned_cfg().actions;
    add_row(state, "propose", "F1", "stale-f1", "pending", &old).await;
    add_row(state, "propose", "F2", "stale-f2", "pending", &old).await;
    add_row(state, "inject", "D3", "other-action", "pending", &old).await;
    add_row(state, "propose", "F1", "expired-f1", "expired", &old).await;
    add_row(state, "propose", "F1", "resolved-f1", "resolved", &old).await;
    add_row(state, "propose", "F1", "fresh-f1", "pending", &new).await;
}

/// `(outcome_state, resolved_at)` of the row with this subject.
async fn row_state(state: &AppState, subject: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT outcome_state, resolved_at FROM devtime_shadow WHERE subject_key = ?")
        .bind(subject)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

async fn assert_only_stale_pending_voided(state: &AppState) {
    for subject in ["stale-f1", "stale-f2"] {
        let (outcome, resolved_at) = row_state(state, subject).await;
        assert_eq!(
            outcome, "void",
            "{subject} was decided under old thresholds"
        );
        assert!(
            resolved_at.is_some(),
            "{subject}: a voided row is stamped like `void_pending`"
        );
    }
    assert_eq!(
        row_state(state, "other-action").await,
        ("pending".into(), None),
        "inject hashes did not change"
    );
    assert_eq!(
        row_state(state, "fresh-f1").await,
        ("pending".into(), None),
        "decided under the new thresholds"
    );
    assert_eq!(
        row_state(state, "expired-f1").await,
        ("expired".into(), Some(RESOLVED_AT.into()))
    );
    assert_eq!(
        row_state(state, "resolved-f1").await,
        ("resolved".into(), Some(RESOLVED_AT.into()))
    );
}

async fn assert_nothing_voided(state: &AppState) {
    let voided: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM devtime_shadow WHERE outcome_state = 'void'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(voided, 0, "no row may be voided");
    for subject in ["stale-f1", "stale-f2", "other-action", "fresh-f1"] {
        assert_eq!(row_state(state, subject).await.0, "pending", "{subject}");
    }
}

async fn put_devtime(state: &AppState, cfg: &DevtimeConfig, version: i64) -> (StatusCode, Value) {
    let request = json!({ "body": body(cfg), "version": version });
    workflow_call(
        state.clone(),
        "PUT",
        "/settings/machine/devtime",
        Some(request),
    )
    .await
}

/// WARN events of this thread, as `field=value` text, until the guard drops. `tracing-subscriber` is not
/// a dependency of the daemon crate, so this is a minimal `Subscriber`.
#[derive(Clone, Default)]
struct Warnings(Arc<Mutex<Vec<String>>>);

struct FieldText(String);

impl tracing::field::Visit for FieldText {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Warnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            let mut text = FieldText(String::new());
            event.record(&mut text);
            self.0.lock().unwrap().push(text.0);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

impl Warnings {
    fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let warnings = Warnings::default();
        let guard = tracing::subscriber::set_default(warnings.clone());
        (warnings, guard)
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn a_put_that_changes_thresholds_voids_the_stale_pending_rows_only() {
    let (_home, state) = fixture().await;
    seed_doc(&state, &default_cfg(), 0).await;
    seed_rows(&state).await;

    let (status, answer) = put_devtime(&state, &tuned_cfg(), 1).await;

    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["version"], 2);
    assert_only_stale_pending_voided(&state).await;
}

#[tokio::test]
async fn a_rollback_that_changes_thresholds_voids_the_stale_pending_rows_only() {
    let (_home, state) = fixture().await;
    seed_doc(&state, &tuned_cfg(), 0).await;
    seed_doc(&state, &default_cfg(), 1).await;
    seed_rows(&state).await;

    // Version 1 is the tuned document: the rows decided under the default thresholds go stale.
    let (status, answer) = workflow_call(
        state.clone(),
        "POST",
        "/settings/machine/devtime/rollback/1",
        Some(json!({ "version": 2 })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["version"], 3);
    assert_only_stale_pending_voided(&state).await;
}

#[tokio::test]
async fn a_put_on_another_document_does_not_void() {
    let (_home, state) = fixture().await;
    // The stored document is already tuned, so the default-hash rows ARE stale against it: only a sweep
    // triggered by a `devtime` write may touch them.
    seed_doc(&state, &tuned_cfg(), 0).await;
    seed_rows(&state).await;

    let request = json!({ "body": { "capacity": 7 }, "version": 0 });
    let (status, answer) = workflow_call(
        state.clone(),
        "PUT",
        "/settings/machine/verify",
        Some(request),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(store::get::<VerifyConfig>(&Scope::Machine).capacity, 7);
    assert_nothing_voided(&state).await;
}

#[tokio::test]
async fn a_put_that_leaves_the_thresholds_alone_voids_nothing() {
    let (_home, state) = fixture().await;
    seed_doc(&state, &default_cfg(), 0).await;
    // Rows stamped with the hashes the current document gives: nothing is stale.
    let current = default_cfg().actions;
    add_row(&state, "propose", "F1", "stale-f1", "pending", &current).await;
    add_row(&state, "propose", "F2", "stale-f2", "pending", &current).await;
    add_row(&state, "inject", "D3", "other-action", "pending", &current).await;
    add_row(&state, "propose", "F1", "fresh-f1", "pending", &current).await;

    // Fields `thresholds_hash` does not read: the cadence and the human-mark count.
    let mut cfg = default_cfg();
    cfg.cycle_seconds = 120;
    cfg.actions.human_min_marks = 9;
    let (status, answer) = put_devtime(&state, &cfg, 1).await;

    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["version"], 2);
    assert_nothing_voided(&state).await;
}

#[tokio::test]
async fn a_failed_voiding_neither_undoes_the_write_nor_fails_the_put() {
    let (_home, state) = fixture().await;
    seed_doc(&state, &default_cfg(), 0).await;
    seed_rows(&state).await;
    // The sweep cannot run: its table is gone.
    sqlx::query("DROP TABLE devtime_shadow")
        .execute(&state.pool)
        .await
        .unwrap();
    let (warnings, _guard) = Warnings::capture();

    let (status, answer) = put_devtime(&state, &tuned_cfg(), 1).await;

    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["version"], 2);
    assert_eq!(
        store::get::<DevtimeConfig>(&Scope::Machine)
            .actions
            .propose_min_sessions,
        5,
        "the document stays written"
    );
    let rows = store::history(&state.pool, &Scope::Machine, "devtime")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "both versions are in the history");
    let lines = warnings.lines();
    assert!(
        lines.iter().any(|line| line.contains("devtime")),
        "the failure is logged as a warning that names devtime, like the neighbouring hooks: {lines:?}"
    );
}
