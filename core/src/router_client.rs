//! Typed HTTP client for the local llm-router's route-only API (`jev-model-router`).
//!
//! Transport only, for the reason `quota_client.rs` is: `route_advice.rs` owns what the daemon asks
//! and what it does with the answer, and never learns that the router speaks HTTP. The router never
//! runs anything for the daemon — this client knows `POST /v1/route` and `GET /v1/route/targets`,
//! and nothing of the router's proxy mode.
//!
//! The request is the router's schema and NOTHING else. `route_api.py` refuses any unknown top-level
//! field with a 400, so a field added here "for later" would turn every routed run into a fallback
//! without a single test on this side noticing. Every optional or empty field is skipped rather than
//! sent as `null` or `[]`, because an empty `runners` means "any runner" to the router — the
//! opposite of what an empty list means here.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// One `POST /v1/route` body. The fields are exactly the router's `_KNOWN` set, spelled as it spells
/// them.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct RouteRequest {
    /// The work to route. Required and non-empty; `route_advice` sends the head of the prompt.
    pub task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_output: Option<String>,
    /// `model[@effort]` of every attempt that already failed this item.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    /// The runners the daemon can launch this task on. Never sent empty: to the router, empty is
    /// "any runner".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub runners: Vec<String>,
    /// Free-form context. Unused by the runs surface; the one place extra context is allowed to go.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packet: Option<serde_json::Map<String, serde_json::Value>>,
    /// Globs over tier names and model ids: the shortlist the router may choose from.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// The router's answer, reduced to what the daemon reads. Everything else it sends is ignored.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RouteAdvice {
    /// What an outcome is later reported against.
    pub decision_id: String,
    /// The CLI the tier runs on: `claude`, `codex`, or something this daemon cannot launch.
    pub runner: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
    /// The router's tier name. Read because the request's `models` globs match tier names as well
    /// as model ids, so checking an answer against them needs both.
    #[serde(default)]
    pub tier: Option<String>,
    /// The router's own price for the choice. Read for the director's `suggest_model`; a comparison
    /// between tiers, never a budget.
    #[serde(default)]
    pub estimated_cost_usd: Option<f64>,
    /// Why the router chose it, in its words.
    #[serde(default)]
    pub rule: Option<String>,
}

/// One tier a caller could be told to run, from `GET /v1/route/targets`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RouteTarget {
    pub tier: String,
    pub runner: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Targets {
    targets: Vec<RouteTarget>,
}

/// Why no advice came back. Three variants because the router means three different things, and
/// the warning a person reads should say which one it was — though every one of them leads to the
/// same place: the run launches exactly as it would have without a router.
#[derive(Debug)]
pub enum RouterError {
    /// 400: the router refused the request as malformed. A daemon bug, never a router state.
    Invalid(String),
    /// 422: the request's filters left no tier. Honest, and expected while catalogues disagree.
    NoEligibleTier(String),
    /// Not listening, too slow, any other status, or an answer that would not parse.
    Unavailable(String),
}

impl std::fmt::Display for RouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "llm-router refused the request: {why}"),
            Self::NoEligibleTier(why) => write!(f, "llm-router had no eligible tier: {why}"),
            Self::Unavailable(why) => write!(f, "llm-router unavailable: {why}"),
        }
    }
}

#[derive(Clone)]
pub struct RouterClient {
    http: reqwest::Client,
    base: String,
}

/// No credential travels on this path, but the struct still describes itself by hand so a future
/// field cannot reach a log through a derive.
impl std::fmt::Debug for RouterClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouterClient")
            .field("base", &self.base)
            .finish()
    }
}

impl RouterClient {
    /// `timeout` bounds the connect AND the whole call: a router that accepts and never answers must
    /// cost a run no more than one that is not listening at all.
    pub fn new(base_url: &str, timeout: Duration) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(timeout)
                .connect_timeout(timeout)
                .build()
                .unwrap_or_default(),
            base: base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// Ask which model, effort and runner should take `request`.
    pub async fn route(&self, request: &RouteRequest) -> Result<RouteAdvice, RouterError> {
        let response = self
            .http
            .post(format!("{}/v1/route", self.base))
            .json(request)
            .send()
            .await
            .map_err(|error| RouterError::Unavailable(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let why = format!("{}: {}", status.as_u16(), body.trim());
            return Err(match status.as_u16() {
                400 => RouterError::Invalid(why),
                422 => RouterError::NoEligibleTier(why),
                _ => RouterError::Unavailable(why),
            });
        }
        response
            .json::<RouteAdvice>()
            .await
            .map_err(|error| RouterError::Unavailable(error.to_string()))
    }

    /// Every tier the router could tell a caller to run.
    pub async fn targets(&self) -> Result<Vec<RouteTarget>, RouterError> {
        let response = self
            .http
            .get(format!("{}/v1/route/targets", self.base))
            .send()
            .await
            .map_err(|error| RouterError::Unavailable(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RouterError::Unavailable(format!(
                "{}: {}",
                status.as_u16(),
                body.trim()
            )));
        }
        response
            .json::<Targets>()
            .await
            .map(|targets| targets.targets)
            .map_err(|error| RouterError::Unavailable(error.to_string()))
    }
}

/// Stub routers for tests here and in `route_advice`: loopback, port 0, no live llm-router.
#[cfg(test)]
pub mod test_support {
    /// Serves `app` on a fresh loopback port and returns its base URL.
    pub async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    /// A router that answers every `/v1/route` with `status` and `body`, and hands each request body
    /// it received to the returned receiver.
    pub async fn stub_router(
        status: u16,
        body: serde_json::Value,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    ) {
        let (sent, received) = tokio::sync::mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/v1/route",
            axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
                let body = body.clone();
                let sent = sent.clone();
                async move {
                    let _ = sent.send(request);
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        axum::Json(body),
                    )
                }
            }),
        );
        (serve(app).await, received)
    }

    /// A router that accepts and answers only after `delay`.
    pub async fn slow_router(delay: std::time::Duration, body: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/v1/route",
            axum::routing::post(move || {
                let body = body.clone();
                async move {
                    tokio::time::sleep(delay).await;
                    axum::Json(body)
                }
            }),
        );
        serve(app).await
    }

    /// A loopback address nothing listens on: bound, read, and released.
    pub async fn dead_address() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::time::Instant;

    fn advice_json() -> serde_json::Value {
        serde_json::json!({
            "decision_id": "rt_1", "tier": "sonnet-high", "runner": "claude",
            "model": "claude-sonnet-5", "effort": "high", "success": 0.8,
            "estimated_cost_usd": 0.12, "cost_basis": "task", "rule": "cheapest that passes",
            "router": "jev", "unknown_failed": []
        })
    }

    /// Only the router's own field names may go out, and nothing empty: an empty `runners` would be
    /// read as "any runner", and an unknown field is a 400 that turns every run into a fallback.
    #[test]
    fn the_request_carries_only_schema_fields_and_skips_what_is_empty() {
        let bare = serde_json::to_value(RouteRequest {
            task: "do it".into(),
            ..RouteRequest::default()
        })
        .unwrap();
        assert_eq!(bare, serde_json::json!({"task": "do it"}));

        let full = serde_json::to_value(RouteRequest {
            task: "t".into(),
            stage: Some("implement".into()),
            files: vec!["a.rs".into()],
            attempt: Some(2),
            gate_output: Some("red".into()),
            failed: vec!["m@high".into()],
            runners: vec!["claude".into()],
            packet: Some(serde_json::Map::new()),
            models: vec!["claude-*".into()],
            exclude: vec!["x".into()],
        })
        .unwrap();
        let known = [
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
        let object = full.as_object().unwrap();
        assert_eq!(object.len(), known.len());
        for key in object.keys() {
            assert!(known.contains(&key.as_str()), "{key} is not in the schema");
        }
    }

    #[tokio::test]
    async fn an_answer_is_read_and_the_extra_fields_ignored() {
        let (url, mut received) = stub_router(200, advice_json()).await;
        let client = RouterClient::new(&url, Duration::from_secs(2));

        let advice = client
            .route(&RouteRequest {
                task: "t".into(),
                ..RouteRequest::default()
            })
            .await
            .expect("a 200 with the schema's fields is advice");

        assert_eq!(advice.decision_id, "rt_1");
        assert_eq!(advice.runner, "claude");
        assert_eq!(advice.model, "claude-sonnet-5");
        assert_eq!(advice.effort.as_deref(), Some("high"));
        assert_eq!(advice.tier.as_deref(), Some("sonnet-high"));
        assert_eq!(
            received.recv().await.unwrap(),
            serde_json::json!({"task": "t"})
        );
    }

    #[tokio::test]
    async fn a_router_that_is_not_listening_is_an_error_not_a_hang() {
        let client = RouterClient::new(&dead_address().await, Duration::from_millis(500));
        let started = Instant::now();

        let result = client
            .route(&RouteRequest {
                task: "t".into(),
                ..RouteRequest::default()
            })
            .await;

        assert!(
            matches!(result, Err(RouterError::Unavailable(_))),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_router_slower_than_the_timeout_is_unavailable_within_it() {
        let url = slow_router(Duration::from_secs(5), advice_json()).await;
        let client = RouterClient::new(&url, Duration::from_millis(150));
        let started = Instant::now();

        let result = client
            .route(&RouteRequest {
                task: "t".into(),
                ..RouteRequest::default()
            })
            .await;

        assert!(
            matches!(result, Err(RouterError::Unavailable(_))),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_400_and_a_422_are_told_apart() {
        let request = RouteRequest {
            task: "t".into(),
            ..RouteRequest::default()
        };
        let (bad, _rx) = stub_router(400, serde_json::json!({"error": "unknown fields"})).await;
        let refused = RouterClient::new(&bad, Duration::from_secs(2))
            .route(&request)
            .await;
        assert!(
            matches!(refused, Err(RouterError::Invalid(_))),
            "{refused:?}"
        );

        let (none, _rx) = stub_router(422, serde_json::json!({"error": "no tier"})).await;
        let empty = RouterClient::new(&none, Duration::from_secs(2))
            .route(&request)
            .await;
        assert!(
            matches!(empty, Err(RouterError::NoEligibleTier(_))),
            "{empty:?}"
        );
    }

    #[tokio::test]
    async fn the_targets_are_listed() {
        let app = axum::Router::new().route(
            "/v1/route/targets",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({"targets": [
                    {"tier": "t1", "runner": "claude", "model": "claude-sonnet-5",
                     "effort": null, "executable": "claude"}
                ]}))
            }),
        );
        let url = serve(app).await;

        let targets = RouterClient::new(&url, Duration::from_secs(2))
            .targets()
            .await
            .expect("a listing");

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].model, "claude-sonnet-5");
        assert_eq!(targets[0].effort, None);
    }
}
