//! The daemon's decision for one subagent spawn: which model the Agent tool call should carry.
//!
//! The PreToolUse hook asks (`POST /hooks/agent-model`) and this module answers. The rules, in order:
//! the surface is off ⇒ no call and no row; a `model` the call already names is never touched; a
//! `wf-*` cockpit is left alone and never reaches the router; otherwise the router suggests a model
//! and, when it has no usable answer inside its time ceiling, the fixed ladder decides. A recent
//! failure of the same task lifts the choice one rung, and nothing here ever lowers an answer.
//! `shadow` records what would have been chosen and rewrites nothing; `apply` returns the alias.
//!
//! The hook must never be held up: every failure inside answers `failopen` and the spawn goes
//! through untouched.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::agent_route::{
    Rung, SOURCE_EXPLICIT, SOURCE_FAILOPEN, SOURCE_LADDER, SOURCE_ROUTER, SOURCE_SKIPPED, classify,
    escalate, head_chars, rung_of_model_id,
};
use super::agent_spawns::{NewSpawn, recent_failures, record_spawn};
use super::route_advice::{Mode, Router, Surface, TASK_CHARS};
use crate::router_client::RouteRequest;

/// How far back a failed spawn of the same task still counts toward escalation.
const FAILURE_WINDOW_SECS: i64 = 1800;

/// The body the hook posts: what the Agent tool call carries, as far as it matters here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentModelRequest {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub subagent_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub prompt: String,
    /// The `model` the call already names, if any. Empty counts as absent.
    #[serde(default)]
    pub model: Option<String>,
}

/// What the hook is told. `model` is the alias (`haiku|sonnet|opus`) to write into the call, or
/// `None` for "leave it as it is".
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentModelAnswer {
    pub model: Option<String>,
    pub source: String,
    pub mode: String,
}

impl AgentModelAnswer {
    fn leave(source: &str, mode: &str) -> Self {
        Self {
            model: None,
            source: source.to_owned(),
            mode: mode.to_owned(),
        }
    }

    /// Nothing decided and nothing recorded: the surface is off.
    pub fn off() -> Self {
        Self::leave("off", "off")
    }

    /// The answer for an internal error: the spawn goes through untouched.
    pub fn failopen() -> Self {
        Self::leave(SOURCE_FAILOPEN, "off")
    }
}

/// A router answer the ladder can use: its family, and the ids to record.
struct Advice {
    rung: Rung,
    decision_id: String,
}

/// Asks the router for a model inside the Claude families. `None` on any failure, a slow answer,
/// or a model the Agent tool would not accept.
async fn ask_router(router: &Router, req: &AgentModelRequest, failed: &[Rung]) -> Option<Advice> {
    let task = head_chars(&format!("{}\n{}", req.description, req.prompt), TASK_CHARS).to_owned();
    if task.trim().is_empty() {
        return None;
    }
    let request = RouteRequest {
        task,
        runners: vec!["claude".to_owned()],
        models: vec!["claude-*".to_owned()],
        failed: failed.iter().map(|r| r.as_alias().to_owned()).collect(),
        ..RouteRequest::default()
    };
    let ceiling = Duration::from_millis(router.config.timeout_ms);
    let advice = match tokio::time::timeout(ceiling, router.client.route(&request)).await {
        Ok(Ok(advice)) => advice,
        Ok(Err(error)) => {
            tracing::debug!("agent model: router gave no answer: {error}");
            return None;
        }
        Err(_) => {
            tracing::debug!("agent model: router did not answer within {ceiling:?}");
            return None;
        }
    };
    let rung = rung_of_model_id(&advice.model)?;
    Some(Advice {
        rung,
        decision_id: advice.decision_id,
    })
}

/// Decides the model for one spawn and records the decision. Never fails: an internal error
/// answers `failopen`.
pub async fn decide(
    pool: &SqlitePool,
    router: Option<Arc<Router>>,
    req: &AgentModelRequest,
) -> AgentModelAnswer {
    let Some(router) = router else {
        return AgentModelAnswer::off();
    };
    let mode = router.mode_for(Surface::Agents);
    if mode == Mode::Off {
        return AgentModelAnswer::off();
    }
    let mode_label = mode.as_str();
    let mut spawn = NewSpawn {
        session_id: req.session_id.clone(),
        subagent_type: req.subagent_type.clone(),
        description: req.description.clone(),
        model: None,
        source: SOURCE_FAILOPEN,
        mode: mode_label,
        advised_model: None,
        route_decision_id: None,
        failed: Vec::new(),
    };

    if let Some(explicit) = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        spawn.model = Some(explicit.to_owned());
        spawn.source = SOURCE_EXPLICIT;
        return finish(pool, spawn, None).await;
    }
    if req.subagent_type.starts_with("wf-") {
        spawn.source = SOURCE_SKIPPED;
        return finish(pool, spawn, None).await;
    }

    let failed =
        recent_failures(pool, &req.session_id, &req.description, FAILURE_WINDOW_SECS).await;
    spawn.failed = failed.iter().map(|r| r.as_alias().to_owned()).collect();
    let (rung, source) = match ask_router(&router, req, &failed).await {
        Some(advice) => {
            spawn.route_decision_id = Some(advice.decision_id);
            // Never lowered: `escalate` only lifts an answer at or below a rung that already failed.
            (escalate(advice.rung, &failed), SOURCE_ROUTER)
        }
        None => (
            escalate(classify(&req.subagent_type, &req.description), &failed),
            SOURCE_LADDER,
        ),
    };
    spawn.source = source;
    spawn.advised_model = Some(rung.as_alias().to_owned());
    let rewrite = (mode == Mode::Apply).then(|| rung.as_alias().to_owned());
    spawn.model = rewrite.clone();
    finish(pool, spawn, rewrite).await
}

/// Records the spawn and builds the answer; a store that cannot record answers `failopen`.
async fn finish(pool: &SqlitePool, spawn: NewSpawn, rewrite: Option<String>) -> AgentModelAnswer {
    let answer = AgentModelAnswer {
        model: rewrite,
        source: spawn.source.to_owned(),
        mode: spawn.mode.to_owned(),
    };
    match record_spawn(pool, &spawn).await {
        Ok(_) => answer,
        Err(error) => {
            tracing::warn!("agent spawn not recorded: {error}");
            AgentModelAnswer::failopen()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::FakeCommandRunner;
    use crate::models::route_advice::{Available, RouterConfig, RunnerKind};
    use crate::router_client::test_support::{dead_address, slow_router, stub_router};

    fn router(url: &str, mode: Mode, timeout_ms: u64) -> Option<Arc<Router>> {
        let config = RouterConfig {
            mode: Mode::Shadow,
            url: url.to_owned(),
            timeout_ms,
            surfaces: [("agents", mode)].into_iter().collect(),
            ..RouterConfig::off()
        };
        let primary = Available {
            kind: RunnerKind::Claude,
            runner: Arc::new(FakeCommandRunner::default()),
            default_model: "claude-sonnet-5".into(),
            models: vec!["claude-*".into()],
        };
        Some(Arc::new(Router::new(config, primary, Vec::new())))
    }

    fn advice(model: &str) -> serde_json::Value {
        serde_json::json!({"decision_id": "d1", "runner": "claude", "model": model})
    }

    fn call(description: &str) -> AgentModelRequest {
        AgentModelRequest {
            session_id: "s1".into(),
            subagent_type: "worker".into(),
            description: description.into(),
            prompt: "do the thing".into(),
            model: None,
        }
    }

    async fn rows(pool: &SqlitePool) -> Vec<(Option<String>, String, String, Option<String>)> {
        sqlx::query_as("SELECT model, source, mode, advised_model FROM agent_spawns ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_explicit_model_is_kept_and_recorded_as_explicit() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, mut seen) = stub_router(200, advice("claude-haiku-5-5")).await;
        let mut req = call("fix the parser");
        req.model = Some("claude-opus-5-5".into());
        let answer = decide(&pool, router(&url, Mode::Apply, 2000), &req).await;
        assert_eq!(answer.model, None);
        assert_eq!(answer.source, "explicit");
        assert!(
            seen.try_recv().is_err(),
            "an explicit model is never routed"
        );
        let got = rows(&pool).await;
        assert_eq!(got[0].0.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(got[0].1, "explicit");

        req.model = Some("  ".into());
        let answer = decide(&pool, router(&url, Mode::Apply, 2000), &req).await;
        assert_eq!(answer.source, "router", "a blank model counts as absent");
    }

    #[tokio::test]
    async fn off_makes_no_call_and_writes_no_row() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, mut seen) = stub_router(200, advice("claude-sonnet-5")).await;
        let answer = decide(&pool, router(&url, Mode::Off, 2000), &call("fix it")).await;
        assert_eq!(answer.model, None);
        assert_eq!(answer.mode, "off");
        assert!(seen.try_recv().is_err());
        assert!(rows(&pool).await.is_empty());
        let none = decide(&pool, None, &call("fix it")).await;
        assert_eq!(none.mode, "off");
        assert!(rows(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn a_wf_cockpit_is_never_routed() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, mut seen) = stub_router(200, advice("claude-haiku-5-5")).await;
        let mut req = call("execute the packet");
        req.subagent_type = "wf-executor".into();
        let answer = decide(&pool, router(&url, Mode::Apply, 2000), &req).await;
        assert_eq!(answer.model, None);
        assert_eq!(answer.source, "skipped");
        assert_eq!(answer.mode, "apply");
        assert!(seen.try_recv().is_err(), "the router is not consulted");
        let got = rows(&pool).await;
        assert_eq!((got[0].1.as_str(), got[0].2.as_str()), ("skipped", "apply"));
    }

    #[tokio::test]
    async fn shadow_records_the_choice_and_rewrites_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, _seen) = stub_router(200, advice("claude-sonnet-5")).await;
        let answer = decide(&pool, router(&url, Mode::Shadow, 2000), &call("fix it")).await;
        assert_eq!(answer.model, None);
        assert_eq!(answer.source, "router");
        assert_eq!(answer.mode, "shadow");
        let got = rows(&pool).await;
        assert_eq!(got[0].0, None);
        assert_eq!(got[0].3.as_deref(), Some("sonnet"));
        assert_eq!(got[0].1, "router");
    }

    #[tokio::test]
    async fn apply_returns_the_router_family_as_an_alias() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, _seen) = stub_router(200, advice("claude-sonnet-5")).await;
        let answer = decide(&pool, router(&url, Mode::Apply, 2000), &call("find x")).await;
        assert_eq!(answer.model.as_deref(), Some("sonnet"));
        assert_eq!(rows(&pool).await[0].0.as_deref(), Some("sonnet"));
    }

    #[tokio::test]
    async fn a_router_answer_outside_the_claude_families_falls_to_the_ladder() {
        for model in ["gpt-5", ""] {
            let pool = crate::testdb::fresh_pool().await;
            let (url, _seen) = stub_router(200, advice(model)).await;
            let answer = decide(&pool, router(&url, Mode::Apply, 2000), &call("find x")).await;
            assert_eq!(answer.source, "ladder", "{model:?}");
            assert_eq!(answer.model.as_deref(), Some("haiku"));
        }
    }

    #[tokio::test]
    async fn a_router_failure_or_timeout_falls_to_the_ladder_and_never_blocks() {
        let pool = crate::testdb::fresh_pool().await;
        let slow = slow_router(Duration::from_secs(5), advice("claude-opus-5-5")).await;
        let started = std::time::Instant::now();
        let answer = decide(&pool, router(&slow, Mode::Apply, 150), &call("find x")).await;
        assert_eq!(answer.source, "ladder");
        assert!(started.elapsed() < Duration::from_millis(1500));
        let dead = dead_address().await;
        let answer = decide(&pool, router(&dead, Mode::Apply, 150), &call("find x")).await;
        assert_eq!(answer.source, "ladder");
        let (bad, _seen) = stub_router(500, serde_json::json!({})).await;
        let answer = decide(&pool, router(&bad, Mode::Apply, 150), &call("find x")).await;
        assert_eq!(answer.source, "ladder");
    }

    #[tokio::test]
    async fn the_ladder_never_lowers_a_router_answer() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, _seen) = stub_router(200, advice("claude-opus-5-5")).await;
        let answer = decide(&pool, router(&url, Mode::Apply, 2000), &call("find x")).await;
        assert_eq!(answer.model.as_deref(), Some("opus"));
        assert_eq!(answer.source, "router");
    }

    #[tokio::test]
    async fn a_recent_failure_escalates_one_rung_only_and_caps_at_opus() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, _seen) = stub_router(200, advice("claude-sonnet-5")).await;
        let apply = || router(&url, Mode::Apply, 2000);
        let first = decide(&pool, apply(), &call("fix the parser")).await;
        assert_eq!(first.model.as_deref(), Some("sonnet"));
        sqlx::query("UPDATE agent_spawns SET outcome = 'error', ended_at = 1")
            .execute(&pool)
            .await
            .unwrap();
        let second = decide(&pool, apply(), &call("fix the parser")).await;
        assert_eq!(second.model.as_deref(), Some("opus"), "sonnet failed");
        let third = decide(&pool, apply(), &call("fix the parser")).await;
        assert_eq!(third.model.as_deref(), Some("opus"), "capped at opus");
    }

    #[tokio::test]
    async fn a_long_multibyte_prompt_is_cut_without_panicking() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, mut seen) = stub_router(200, advice("claude-sonnet-5")).await;
        let mut req = call("fix it");
        req.prompt = "é".repeat(9000);
        decide(&pool, router(&url, Mode::Shadow, 2000), &req).await;
        let sent = seen.recv().await.unwrap();
        assert_eq!(sent["task"].as_str().unwrap().chars().count(), TASK_CHARS);
        assert_eq!(sent["runners"], serde_json::json!(["claude"]));
        assert_eq!(sent["models"], serde_json::json!(["claude-*"]));
    }

    #[tokio::test]
    async fn a_store_that_cannot_record_answers_failopen() {
        let pool = crate::testdb::fresh_pool().await;
        let (url, _seen) = stub_router(200, advice("claude-sonnet-5")).await;
        pool.close().await;
        let answer = decide(&pool, router(&url, Mode::Apply, 2000), &call("fix it")).await;
        assert_eq!(answer, AgentModelAnswer::failopen());
    }
}
