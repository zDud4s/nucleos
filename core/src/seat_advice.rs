//! The llm-router's advice for the agents the daemon launches OUTSIDE `runs::spawn_run`: a team
//! seat, a council seat, and a director's recruit.
//!
//! `route_advice::resolve` is the runs surface; these three are the other places the daemon already
//! picks an agent's model (plan decision 4), and each has its own switch in `.ai/router.yaml`
//! (`team`, `council`, `recruit`). The rules are the same: the daemon decides and the router
//! suggests, `off` is today's launch byte for byte with no call made, and any failure — router down,
//! slow, 400, 422, an answer outside what was sent — launches exactly what would have launched.
//!
//! What differs per surface is what is SENT:
//! - a team seat sends its own runner and that runner's allowed globs, so the router picks a model
//!   and an effort inside the provider the seat already runs on;
//! - a council seat sends its `model_ref` as the only glob, so the router can advise an effort and
//!   nothing else — a council's value is that its seats are DIFFERENT models, and a router free to
//!   pick would pick the same one for all of them;
//! - a recruit sends the proposed engine's runner (or every runner the router knows, for the
//!   `suggest_model` tool), with `speciality`, `why` and the candidate's prompt as the task.
//!
//! Local seats never come here: their launch paths (`team::spawn_agent`'s `engine == "local"`
//! branch, `council::run_local_seat`) never call into this module.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::route_advice::{
    Mode, Outcome, RouteQuery, Router, RunnerKind, Surface, clamp_effort, effort_ceiling,
    glob_match, outcome_of_run_end, report_detached, speed_of,
};
use crate::router_client::{RouteAdvice, RouteRequest};
use crate::runner::RunRequest;

/// What a seat is told about itself: the head of its prompt, nothing else.
fn seat_query(prompt: &str) -> RouteQuery {
    RouteQuery {
        task: prompt.to_owned(),
        stage: None,
        item: None,
        resume: false,
    }
}

/// A team seat's request: the seat's runner (the primary — a seat never changes CLI in this phase,
/// plan decision 7) and that runner's allowed globs.
pub fn team_request(router: &Router, prompt: &str) -> RouteRequest {
    router.build_request(&seat_query(prompt), &[router.primary.kind])
}

/// A council seat's request: the seat's runner and its `model_ref` as the ONLY glob, so the advice
/// can move the effort and never the model.
pub fn council_request(router: &Router, prompt: &str, model_ref: &str) -> RouteRequest {
    RouteRequest {
        models: vec![model_ref.to_owned()],
        ..router.build_request(&seat_query(prompt), &[router.primary.kind])
    }
}

/// Asks, and keeps an answer only when it stayed inside what was sent.
async fn ask(
    router: &Router,
    request: &RouteRequest,
    run_id: Option<i64>,
    check: impl Fn(&RouteAdvice) -> Result<(), String>,
) -> Option<RouteAdvice> {
    match router.client.route(request).await {
        Ok(advice) => match check(&advice) {
            Ok(()) => Some(advice),
            Err(why) => {
                tracing::warn!(?run_id, %why, "llm-router answered a seat outside what was sent; ignoring it");
                None
            }
        },
        Err(error) => {
            tracing::warn!(?run_id, %error, "no advice from the llm-router for a seat; launching as configured");
            None
        }
    }
}

/// A council answer must stay on the seat's runner and on its `model_ref`. Not `check_answer`,
/// because that also holds the model to the runner's catalogue, and a council seat's `model_ref`
/// is the council's choice, not the catalogue's — it would refuse every seat outside it.
fn check_council(advice: &RouteAdvice, sent: &RouteRequest, model_ref: &str) -> Result<(), String> {
    if !sent.runners.iter().any(|runner| runner == &advice.runner) {
        return Err(format!(
            "runner {} is not among those sent {:?}",
            advice.runner, sent.runners
        ));
    }
    let names = std::iter::once(advice.model.as_str()).chain(advice.tier.as_deref());
    if names.clone().all(|name| !glob_match(model_ref, name)) {
        return Err(format!(
            "model {} is not the seat's {model_ref}",
            advice.model
        ));
    }
    Ok(())
}

/// Writes what a seat launched beside what was advised, in ONE update of its `runs` row — the same
/// columns `route_advice::resolve` writes for a run. A failed write only warns.
async fn record(
    pool: &SqlitePool,
    run_id: i64,
    mode: Mode,
    runner: RunnerKind,
    model: &str,
    effort: Option<&str>,
    advice: Option<&RouteAdvice>,
) {
    if let Err(error) = sqlx::query(
        "UPDATE runs SET model = ?, effort = ?, runner = ?, route_mode = ?, route_decision_id = ?,
                         advised_runner = ?, advised_model = ?, advised_effort = ?
          WHERE id = ?",
    )
    .bind(model)
    .bind(effort)
    .bind(runner.as_str())
    .bind(mode.as_str())
    .bind(advice.map(|advice| advice.decision_id.as_str()))
    .bind(advice.map(|advice| advice.runner.as_str()))
    .bind(advice.map(|advice| advice.model.as_str()))
    .bind(advice.and_then(|advice| advice.effort.as_deref()))
    .bind(run_id)
    .execute(pool)
    .await
    {
        tracing::warn!(run_id, %error, "could not record a seat's route decision");
    }
}

/// Routes a cloud team seat, rewriting `request.model`/`request.effort` only under `apply`.
///
/// Returns the router's `decision_id` when there was usable advice — what an outcome would later be
/// reported against. With no router or the `team` surface off it returns `None` having made no call
/// and written nothing.
///
/// The effort is held under the ceiling of the speed the seat is launched with (`request.env`,
/// written by `speed::Capacity::env`), exactly as a run's is.
pub async fn route_team_seat(
    pool: &SqlitePool,
    router: Option<Arc<Router>>,
    run_id: i64,
    request: &mut RunRequest,
) -> Option<String> {
    let router = router?;
    let mode = router.mode_for(Surface::Team);
    if mode == Mode::Off {
        return None;
    }
    let sent = team_request(&router, &request.prompt);
    let advice = ask(&router, &sent, Some(run_id), |advice| {
        router.check_answer(advice, &sent).and_then(|kind| {
            if kind == router.primary.kind {
                Ok(())
            } else {
                Err(format!("runner {} is not the seat's", kind.as_str()))
            }
        })
    })
    .await;
    if mode == Mode::Apply
        && let Some(advice) = &advice
    {
        let ceiling = effort_ceiling(speed_of(&request.env));
        request.model = Some(advice.model.clone());
        request.effort = advice
            .effort
            .as_deref()
            .map(|effort| clamp_effort(effort, ceiling));
    }
    let model = request
        .model
        .clone()
        .unwrap_or_else(|| router.primary.default_model.clone());
    record(
        pool,
        run_id,
        mode,
        router.primary.kind,
        &model,
        request.effort.as_deref(),
        advice.as_ref(),
    )
    .await;
    advice.map(|advice| advice.decision_id)
}

/// Routes a cloud council seat, whose `request.model` is its `model_ref`. Under `apply` only the
/// EFFORT changes, and it is not held under a speed ceiling: a council has no speed of its own (its
/// seats launch with `Capacity::solo()`, which is a default, not a choice anybody made).
pub async fn route_council_seat(
    pool: &SqlitePool,
    router: Option<Arc<Router>>,
    run_id: i64,
    request: &mut RunRequest,
) -> Option<String> {
    let router = router?;
    let mode = router.mode_for(Surface::Council);
    if mode == Mode::Off {
        return None;
    }
    // A cloud seat always names its model; one that did not has nothing to hold the advice to.
    let model_ref = request.model.clone()?;
    let sent = council_request(&router, &request.prompt, &model_ref);
    let advice = ask(&router, &sent, Some(run_id), |advice| {
        check_council(advice, &sent, &model_ref)
    })
    .await;
    if mode == Mode::Apply
        && let Some(effort) = advice.as_ref().and_then(|advice| advice.effort.clone())
    {
        request.effort = Some(effort);
    }
    record(
        pool,
        run_id,
        mode,
        router.primary.kind,
        &model_ref,
        request.effort.as_deref(),
        advice.as_ref(),
    )
    .await;
    advice.map(|advice| advice.decision_id)
}

/// What a director's recruit is described by, as the task the router reads.
pub fn recruit_task(speciality: &str, why: &str, prompt: &str) -> String {
    [speciality, why, prompt]
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Advice for a candidate: `runner` when the recruit names one, else every runner the router knows.
/// `Err` carries why there is none, in a sentence a director can read.
pub async fn suggest(
    router: Option<Arc<Router>>,
    runner: Option<RunnerKind>,
    task: &str,
) -> Result<RouteAdvice, String> {
    let Some(router) = router else {
        return Err("model advice is off on this machine".to_owned());
    };
    if router.mode_for(Surface::Recruit) == Mode::Off {
        return Err("model advice is off for recruits".to_owned());
    }
    let runners: Vec<RunnerKind> = match runner {
        Some(kind) => vec![kind],
        None => std::iter::once(router.primary.kind)
            .chain(router.alternates.iter().map(|available| available.kind))
            .collect(),
    };
    let sent = router.build_request(&seat_query(task), &runners);
    ask(&router, &sent, None, |advice| {
        router.check_answer(advice, &sent).map(|_| ())
    })
    .await
    .ok_or_else(|| "the model adviser gave no usable answer".to_owned())
}

/// The `suggest_model` tool's answer: the advice's four fields, or a plain "no suggestion".
pub async fn suggest_model(
    router: Option<Arc<Router>>,
    speciality: &str,
    why: &str,
    prompt: &str,
) -> serde_json::Value {
    match suggest(router, None, &recruit_task(speciality, why, prompt)).await {
        Ok(advice) => serde_json::json!({
            "model": advice.model,
            "effort": advice.effort,
            "estimated_cost_usd": advice.estimated_cost_usd,
            "rule": advice.rule,
        }),
        Err(why) => serde_json::Value::String(format!("no suggestion: {why}")),
    }
}

/// The model a recruit that named none defaults to, when the `recruit` surface is `apply` and the
/// engine is a cloud runner. `None` — so the caller falls back to the director's own model — in
/// every other case, including any router failure.
pub async fn recruit_default(
    router: Option<Arc<Router>>,
    engine: &str,
    speciality: &str,
    why: &str,
    prompt: &str,
) -> Option<String> {
    let router = router?;
    if router.mode_for(Surface::Recruit) != Mode::Apply {
        return None;
    }
    let kind = RunnerKind::parse(engine)?;
    suggest(
        Some(router),
        Some(kind),
        &recruit_task(speciality, why, prompt),
    )
    .await
    .ok()
    .map(|advice| advice.model)
}

// ---- outcomes ----
//
// What became of a routed seat, told back to the router with P2's fire-and-forget report. A seat
// reports only when its launch holds a decision id, once, against that one decision: every team
// item and every council stage opens its own run, and so its own decision. Nothing here awaits the
// network — a router that is down never costs a seat, a round or a council a second.

/// The router's word for how a team item's run ended. `completed` is the item filed; `failed` is
/// the specialist's own failure unless the run ended on a 429 or a transient API error, or never
/// launched at all (no stdout), none of which says anything about the model; `timed_out` is the wall clock, not the work. A cancelled or interrupted
/// run was stopped from outside and is not reported.
pub fn team_item_outcome(run_status: &str, stdout: &str) -> Option<Outcome> {
    match run_status {
        "completed" => Some(Outcome::Pass),
        // A run that wrote nothing never launched (a spawn failure lands its reason in stderr):
        // infrastructure, as `runs.rs` reports a run's own launch failure, never the model's fail.
        "failed" if stdout.trim().is_empty() => Some(Outcome::Error),
        "failed" => Some(outcome_of_run_end(stdout).unwrap_or(Outcome::Fail)),
        "timed_out" => Some(Outcome::Error),
        _ => None,
    }
}

/// The router's word for a council seat's `SeatOutcome` status. A council has no verdict on an
/// answer's quality, so a seat that did not answer is `error` (or `rate_limited` on a 429), never
/// `fail`; a cancelled seat — and a pending or skipped one — is not reported.
pub fn council_seat_outcome(status: &str, stdout: &str) -> Option<Outcome> {
    match status {
        crate::council::SEAT_OK => Some(Outcome::Pass),
        crate::council::SEAT_ERROR if crate::runner::failed_on_rate_limit(stdout) => {
            Some(Outcome::RateLimited)
        }
        crate::council::SEAT_ERROR | crate::council::SEAT_TIMEOUT => Some(Outcome::Error),
        _ => None,
    }
}

/// Reports a landed team item against the decision on its run's row. One DB read and a spawn; an
/// unreadable row only warns, and a row with no decision reports nothing.
pub async fn report_team_item(pool: &SqlitePool, router: Option<Arc<Router>>, run_id: i64) {
    let Some(router) = router else {
        return;
    };
    let row: Option<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT status, stdout, route_decision_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(run_id, %error, "could not read a team item's route decision");
                None
            });
    let Some((status, stdout, Some(decision_id))) = row else {
        return;
    };
    if decision_id.is_empty() {
        return;
    }
    if let Some(outcome) = team_item_outcome(&status, stdout.as_deref().unwrap_or_default()) {
        report_detached(router, decision_id, outcome);
    }
}

/// Reports a council seat's final status against the decision its launch was routed on.
pub fn report_council_seat(
    router: Option<Arc<Router>>,
    decision_id: Option<String>,
    status: &str,
    stdout: &str,
) {
    let (Some(router), Some(decision_id)) = (router, decision_id) else {
        return;
    };
    if decision_id.is_empty() {
        return;
    }
    if let Some(outcome) = council_seat_outcome(status, stdout) {
        report_detached(router, decision_id, outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_advice::{Available, RouterConfig};
    use crate::router_client::test_support::{
        dead_address, outcome_router, slow_router, stub_router,
    };

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn seed_run(pool: &SqlitePool) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'team', '2026-09-30T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn router(url: &str, surfaces: &[(&'static str, Mode)], timeout_ms: u64) -> Arc<Router> {
        let config = RouterConfig {
            mode: Mode::Shadow,
            url: url.to_owned(),
            timeout_ms,
            surfaces: surfaces.iter().copied().collect(),
            ..RouterConfig::off()
        };
        let primary = Available {
            kind: RunnerKind::Claude,
            runner: Arc::new(crate::runner::FakeCommandRunner::default()),
            default_model: "claude-sonnet-5".into(),
            models: vec!["claude-*".into()],
        };
        Arc::new(Router::new(config, primary, Vec::new()))
    }

    fn request(model: Option<&str>, speed: crate::speed::Speed) -> RunRequest {
        RunRequest {
            prompt: "draft the launch post".into(),
            env: crate::speed::Capacity::team(speed, 1).env().to_vec(),
            cwd: None,
            permission: crate::runner::Permission::Default,
            resume_session_id: None,
            mcp_config: None,
            mcp_box: None,
            tool_policy: crate::runner::ToolPolicy::McpOnly,
            progress_timeout: None,
            max_turns: None,
            session_id: None,
            fork_session: false,
            include_partial_messages: false,
            images: Vec::new(),
            steerable: true,
            classifier_governs_tools: false,
            ambient_mcp: false,
            model: model.map(str::to_owned),
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            messages: None,
            allowed_mcp_tools: None,
        }
    }

    fn answer(model: &str, effort: &str) -> serde_json::Value {
        serde_json::json!({
            "decision_id": "rt_seat",
            "runner": "claude",
            "model": model,
            "effort": effort,
            "tier": "opus",
            "estimated_cost_usd": 0.12,
            "rule": "hard-task",
        })
    }

    type Recorded = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    async fn recorded(pool: &SqlitePool, run_id: i64) -> Recorded {
        sqlx::query_as(
            "SELECT model, effort, runner, route_mode, route_decision_id, advised_model,
                    advised_effort
               FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// AC1: the team surface off — even with every other surface on — is today's launch, with no
    /// call made and nothing written.
    #[tokio::test]
    async fn a_team_seat_with_its_surface_off_is_launched_untouched_and_nothing_is_asked() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, mut sent) = stub_router(200, answer("claude-opus-5", "high")).await;
        let router = router(&url, &[("team", Mode::Off), ("council", Mode::Apply)], 2500);
        let mut launch = request(Some("claude-sonnet-5"), crate::speed::Speed::Normal);

        let decision = route_team_seat(&pool, Some(router), run_id, &mut launch).await;

        assert_eq!(decision, None);
        assert_eq!(launch.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(launch.effort, None);
        assert!(sent.try_recv().is_err(), "the router was asked");
        assert_eq!(recorded(&pool, run_id).await, Default::default());

        // And no router at all is the same.
        let mut launch = request(Some("claude-sonnet-5"), crate::speed::Speed::Normal);
        assert_eq!(
            route_team_seat(&pool, None, run_id, &mut launch).await,
            None
        );
        assert_eq!(launch.model.as_deref(), Some("claude-sonnet-5"));
    }

    /// AC4 for a seat: shadow launches the configured model and records the advice beside it. The
    /// request carries only the seat's runner and its globs.
    #[tokio::test]
    async fn a_team_seat_in_shadow_launches_as_configured_and_records_the_advice() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, mut sent) = stub_router(200, answer("claude-opus-5", "high")).await;
        let router = router(&url, &[("team", Mode::Shadow)], 2500);
        let mut launch = request(None, crate::speed::Speed::Normal);

        let decision = route_team_seat(&pool, Some(router), run_id, &mut launch).await;

        assert_eq!(decision.as_deref(), Some("rt_seat"));
        assert_eq!(launch.model, None);
        assert_eq!(launch.effort, None);
        let body = sent.try_recv().expect("the router was asked");
        assert_eq!(body["runners"], serde_json::json!(["claude"]));
        assert_eq!(body["models"], serde_json::json!(["claude-*"]));
        assert_eq!(body["task"], "draft the launch post");
        assert_eq!(
            recorded(&pool, run_id).await,
            (
                Some("claude-sonnet-5".into()),
                None,
                Some("claude".into()),
                Some("shadow".into()),
                Some("rt_seat".into()),
                Some("claude-opus-5".into()),
                Some("high".into()),
            )
        );
    }

    /// AC5 for a seat: apply launches the advice, its effort held under the run's speed ceiling.
    #[tokio::test]
    async fn a_team_seat_in_apply_launches_the_advice_under_the_speed_ceiling() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, _sent) = stub_router(200, answer("claude-opus-5", "max")).await;
        let router = router(&url, &[("team", Mode::Apply)], 2500);
        let mut launch = request(Some("claude-sonnet-5"), crate::speed::Speed::Fast);

        route_team_seat(&pool, Some(router), run_id, &mut launch).await;

        assert_eq!(launch.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(launch.effort.as_deref(), Some("low"));
        let row = recorded(&pool, run_id).await;
        assert_eq!(row.0.as_deref(), Some("claude-opus-5"));
        assert_eq!(row.1.as_deref(), Some("low"));
        assert_eq!(row.6.as_deref(), Some("max"), "the advice as it came");
    }

    /// AC2: down, slow, 422, or an answer outside the set — the seat launches as configured.
    #[tokio::test]
    async fn a_team_seat_launches_as_configured_whatever_goes_wrong_with_the_router() {
        let slow = slow_router(
            std::time::Duration::from_millis(1500),
            answer("claude-opus-5", "high"),
        )
        .await;
        let (unprocessable, _a) = stub_router(422, serde_json::json!({"detail": "no tier"})).await;
        let (outside, _b) = stub_router(200, answer("gpt-5.6-terra", "high")).await;
        for url in [dead_address().await, slow, unprocessable, outside] {
            let pool = pool().await;
            let run_id = seed_run(&pool).await;
            let router = router(&url, &[("team", Mode::Apply)], 200);
            let mut launch = request(Some("claude-sonnet-5"), crate::speed::Speed::Normal);
            let started = std::time::Instant::now();

            let decision = route_team_seat(&pool, Some(router), run_id, &mut launch).await;

            assert!(started.elapsed() < std::time::Duration::from_millis(1200));
            assert_eq!(decision, None, "{url}");
            assert_eq!(launch.model.as_deref(), Some("claude-sonnet-5"));
            assert_eq!(launch.effort, None);
            let row = recorded(&pool, run_id).await;
            assert_eq!(row.3.as_deref(), Some("apply"));
            assert_eq!(row.4, None, "no decision to report against");
        }
    }

    /// A council seat sends its `model_ref` as the only glob, and nothing outside the schema.
    #[tokio::test]
    async fn a_council_seat_asks_about_its_own_model_only_and_applies_the_effort() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, mut sent) = stub_router(200, answer("claude-opus-5", "high")).await;
        let router = router(&url, &[("council", Mode::Apply)], 2500);
        let mut launch = request(Some("claude-opus-5"), crate::speed::Speed::Normal);

        let decision = route_council_seat(&pool, Some(router), run_id, &mut launch).await;

        assert_eq!(decision.as_deref(), Some("rt_seat"));
        assert_eq!(launch.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(launch.effort.as_deref(), Some("high"));
        let body = sent.try_recv().expect("the router was asked");
        assert_eq!(body["models"], serde_json::json!(["claude-opus-5"]));
        assert_eq!(body["runners"], serde_json::json!(["claude"]));
        let allowed = [
            "task",
            "stage",
            "files",
            "attempt",
            "gate_output",
            "failed",
            "runners",
            "packet",
            "models",
            "exclude",
        ];
        for key in body.as_object().unwrap().keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "{key} is not in the schema"
            );
        }
    }

    /// A router that answered a different model for a council seat is ignored: the seat keeps its
    /// model AND its effort.
    #[tokio::test]
    async fn a_council_answer_naming_another_model_is_ignored() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, _sent) = stub_router(
            200,
            serde_json::json!({"decision_id": "d", "runner": "claude", "model": "claude-haiku-5", "effort": "low"}),
        )
        .await;
        let router = router(&url, &[("council", Mode::Apply)], 2500);
        let mut launch = request(Some("claude-opus-5"), crate::speed::Speed::Normal);

        assert_eq!(
            route_council_seat(&pool, Some(router), run_id, &mut launch).await,
            None
        );
        assert_eq!(launch.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(launch.effort, None);
    }

    #[tokio::test]
    async fn a_council_seat_with_its_surface_off_asks_nothing() {
        let pool = pool().await;
        let run_id = seed_run(&pool).await;
        let (url, mut sent) = stub_router(200, answer("claude-opus-5", "high")).await;
        let router = router(&url, &[("council", Mode::Off), ("team", Mode::Apply)], 2500);
        let mut launch = request(Some("claude-opus-5"), crate::speed::Speed::Normal);

        assert_eq!(
            route_council_seat(&pool, Some(router), run_id, &mut launch).await,
            None
        );
        assert_eq!(launch.effort, None);
        assert!(sent.try_recv().is_err());
        assert_eq!(recorded(&pool, run_id).await, Default::default());
    }

    #[tokio::test]
    async fn suggest_model_answers_the_four_fields_or_says_there_is_none() {
        let (url, mut sent) = stub_router(200, answer("claude-opus-5", "high")).await;
        let on = router(&url, &[("recruit", Mode::Shadow)], 2500);

        let answered = suggest_model(Some(on), "reads contracts", "nobody can", "").await;
        assert_eq!(
            answered,
            serde_json::json!({
                "model": "claude-opus-5",
                "effort": "high",
                "estimated_cost_usd": 0.12,
                "rule": "hard-task",
            })
        );
        let body = sent.try_recv().unwrap();
        assert_eq!(body["task"], "reads contracts\n\nnobody can");

        let off = router(&url, &[("recruit", Mode::Off)], 2500);
        let none = suggest_model(Some(off), "reads contracts", "nobody can", "").await;
        assert!(none.as_str().unwrap().starts_with("no suggestion"));
        assert!(sent.try_recv().is_err());
        let down = router(&dead_address().await, &[("recruit", Mode::Apply)], 200);
        let none = suggest_model(Some(down), "x", "y", "z").await;
        assert!(none.as_str().unwrap().starts_with("no suggestion"));
        assert!(suggest_model(None, "x", "y", "z").await.is_string());
    }

    #[tokio::test]
    async fn a_recruit_default_is_the_advice_only_under_apply_and_for_a_cloud_engine() {
        let (url, _sent) = stub_router(200, answer("claude-opus-5", "high")).await;
        let apply = router(&url, &[("recruit", Mode::Apply)], 2500);
        let shadow = router(&url, &[("recruit", Mode::Shadow)], 2500);

        assert_eq!(
            recruit_default(Some(Arc::clone(&apply)), "claude", "s", "w", "p").await,
            Some("claude-opus-5".to_owned())
        );
        assert_eq!(
            recruit_default(Some(apply), "local", "s", "w", "p").await,
            None
        );
        assert_eq!(
            recruit_default(Some(shadow), "claude", "s", "w", "p").await,
            None
        );
        assert_eq!(recruit_default(None, "claude", "s", "w", "p").await, None);
    }

    // ---- outcomes ----

    async fn seed_ended(
        pool: &SqlitePool,
        status: &str,
        stdout: &str,
        decision: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at, stdout, route_decision_id)
             VALUES ('x', ?, 'team', '2026-09-30T00:00:00Z', ?, ?)",
        )
        .bind(status)
        .bind(stdout)
        .bind(decision)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn reported(
        received: &mut tokio::sync::mpsc::UnboundedReceiver<(String, serde_json::Value)>,
    ) -> (String, serde_json::Value) {
        tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
            .await
            .expect("reported within 2s")
            .unwrap()
    }

    async fn nothing_reported(
        received: &mut tokio::sync::mpsc::UnboundedReceiver<(String, serde_json::Value)>,
    ) {
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), received.recv())
                .await
                .is_err(),
            "a report was sent"
        );
    }

    fn ended_on_api_error(status: &str) -> String {
        format!(
            "{{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\
             \"api_error_status\":{status},\"result\":\"API Error\"}}"
        )
    }

    #[test]
    fn a_team_items_run_status_maps_to_a_router_outcome() {
        assert_eq!(team_item_outcome("completed", ""), Some(Outcome::Pass));
        assert_eq!(team_item_outcome("failed", "red"), Some(Outcome::Fail));
        assert_eq!(
            team_item_outcome("failed", &ended_on_api_error("429")),
            Some(Outcome::RateLimited)
        );
        assert_eq!(
            team_item_outcome("failed", &ended_on_api_error("529")),
            Some(Outcome::Error)
        );
        assert_eq!(team_item_outcome("timed_out", ""), Some(Outcome::Error));
        // A seat that never launched wrote no stdout at all: infrastructure, as a run's own
        // launch failure is in `runs.rs`, and never the model's `fail`.
        assert_eq!(team_item_outcome("failed", ""), Some(Outcome::Error));
        assert_eq!(team_item_outcome("failed", " \n"), Some(Outcome::Error));
        assert_eq!(team_item_outcome("cancelled", ""), None);
        assert_eq!(team_item_outcome("interrupted", ""), None);
        assert_eq!(team_item_outcome("running", ""), None);
    }

    #[test]
    fn a_council_seats_status_maps_to_a_router_outcome() {
        assert_eq!(council_seat_outcome("ok", ""), Some(Outcome::Pass));
        assert_eq!(council_seat_outcome("error", "boom"), Some(Outcome::Error));
        assert_eq!(
            council_seat_outcome("error", &ended_on_api_error("429")),
            Some(Outcome::RateLimited)
        );
        assert_eq!(council_seat_outcome("timeout", ""), Some(Outcome::Error));
        assert_eq!(council_seat_outcome("cancelled", ""), None);
        assert_eq!(council_seat_outcome("pending", ""), None);
        assert_eq!(council_seat_outcome("skipped", ""), None);
    }

    #[tokio::test]
    async fn a_done_team_item_reports_pass_and_a_failed_one_reports_fail() {
        let pool = pool().await;
        let (url, mut received) = outcome_router(200).await;
        let router = router(&url, &[], 1000);

        let done = seed_ended(&pool, "completed", "the draft", Some("rt_done")).await;
        report_team_item(&pool, Some(Arc::clone(&router)), done).await;
        let (id, body) = reported(&mut received).await;
        assert_eq!(id, "rt_done");
        assert_eq!(body, serde_json::json!({"status": "pass"}));

        let failed = seed_ended(&pool, "failed", "red", Some("rt_failed")).await;
        report_team_item(&pool, Some(router), failed).await;
        let (id, body) = reported(&mut received).await;
        assert_eq!(id, "rt_failed");
        assert_eq!(body, serde_json::json!({"status": "fail"}));
    }

    /// A seat whose run never launched (failed, NULL stdout) reports `error`, and one that ended on
    /// a 429 reports `rate_limited`: neither says anything about the model.
    #[tokio::test]
    async fn a_team_item_that_never_launched_or_hit_a_429_is_not_reported_as_a_fail() {
        let pool = pool().await;
        let (url, mut received) = outcome_router(200).await;
        let router = router(&url, &[], 1000);

        let never = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at, stderr, route_decision_id)
             VALUES ('x', 'failed', 'team', '2026-09-30T00:00:00Z', 'spawn failed', 'rt_never')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        report_team_item(&pool, Some(Arc::clone(&router)), never).await;
        let (id, body) = reported(&mut received).await;
        assert_eq!(id, "rt_never");
        assert_eq!(body, serde_json::json!({"status": "error"}));

        let limited = seed_ended(&pool, "failed", &ended_on_api_error("429"), Some("rt_429")).await;
        report_team_item(&pool, Some(router), limited).await;
        let (id, body) = reported(&mut received).await;
        assert_eq!(id, "rt_429");
        assert_eq!(body, serde_json::json!({"status": "rate_limited"}));
    }

    /// No decision, no router, a cancelled run or a missing row: nothing is sent.
    #[tokio::test]
    async fn a_team_item_with_nothing_to_report_against_reports_nothing() {
        let pool = pool().await;
        let (url, mut received) = outcome_router(200).await;
        let router = router(&url, &[], 1000);

        let unrouted = seed_ended(&pool, "completed", "x", None).await;
        report_team_item(&pool, Some(Arc::clone(&router)), unrouted).await;
        let cancelled = seed_ended(&pool, "cancelled", "", Some("rt_c")).await;
        report_team_item(&pool, Some(Arc::clone(&router)), cancelled).await;
        report_team_item(&pool, Some(router), cancelled + 99).await;
        let routed = seed_ended(&pool, "completed", "x", Some("rt_off")).await;
        report_team_item(&pool, None, routed).await;

        nothing_reported(&mut received).await;
    }

    #[tokio::test]
    async fn an_ok_council_seat_reports_pass_and_one_without_a_decision_reports_nothing() {
        let (url, mut received) = outcome_router(200).await;
        let router = router(&url, &[], 1000);

        report_council_seat(
            Some(Arc::clone(&router)),
            Some("rt_seat".into()),
            "ok",
            "the answer",
        );
        let (id, body) = reported(&mut received).await;
        assert_eq!(id, "rt_seat");
        assert_eq!(body, serde_json::json!({"status": "pass"}));

        report_council_seat(Some(Arc::clone(&router)), None, "ok", "x");
        report_council_seat(Some(router), Some("rt_x".into()), "cancelled", "");
        report_council_seat(None, Some("rt_y".into()), "ok", "x");
        nothing_reported(&mut received).await;
    }
}
