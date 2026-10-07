//! §spec pilar-de-web
//!
//! Typed HTTP client for the web sidecar's loopback API.
//!
//! It exists for the same reason `daemon_client.rs` does: transport belongs in a module of its own.
//! `web.rs` owns the domain and must not know that the sidecar speaks HTTP, and the callers are not
//! all handlers — the pillars reach the web too (spec §2), so putting the call in `http.rs` would
//! leave `email.rs` with nowhere to go.
//!
//! This client talks ONLY to loopback, and it is the núcleo's whole relationship with the internet:
//! the daemon itself never opens a connection off this machine.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// One destination a search returned. No content — see spec §3.2 for why that separation is the
/// thing the trust decision depends on.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    pub provider: String,
    pub results: Vec<SearchResult>,
}

/// One page the sidecar fetched and extracted.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchedPage {
    pub requested_url: String,
    /// Where the bytes actually came from. `trust::decide` needs this AND `requested_url`, which is
    /// why the sidecar reports both rather than collapsing them.
    pub final_url: String,
    pub title: String,
    pub byline: String,
    pub markdown: String,
    pub status: String,
    pub bytes: i64,
}

/// What went wrong, in the shapes a caller has to tell apart.
#[derive(Debug)]
pub enum WebError {
    /// The sidecar is not running or not answering.
    Unreachable(String),
    /// The pillar is enabled but has no usable search provider.
    NotConfigured(String),
    /// The destination was refused by policy — loopback, a private address, a non-web scheme.
    /// Distinct from [`WebError::Failed`] because it must never be retried.
    Blocked(String),
    /// The fetch worked and there was nothing usable: too big, not HTML, no readable content.
    Unusable(String),
    /// Anything else.
    Failed(String),
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebError::Unreachable(why) => write!(f, "web sidecar unreachable: {why}"),
            WebError::NotConfigured(why) => write!(f, "web search not configured: {why}"),
            WebError::Blocked(why) => write!(f, "destination refused: {why}"),
            WebError::Unusable(why) => write!(f, "nothing readable: {why}"),
            WebError::Failed(why) => write!(f, "web request failed: {why}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// The ceiling on one sidecar call. Deliberately above the sidecar's own fetch timeout so a page
/// that times out over there returns an error rather than being cut off over here — two timeouts
/// racing produce a failure whose message names the wrong side.
const CALL_TIMEOUT: Duration = Duration::from_secs(150);

impl WebClient {
    pub fn new(addr: &str, token: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(CALL_TIMEOUT)
                .build()
                .unwrap_or_default(),
            base: format!("http://{addr}"),
            token,
        }
    }

    pub async fn search(&self, query: &str, limit: i64) -> Result<SearchResponse, WebError> {
        let response = self
            .post(
                "/search",
                &serde_json::json!({ "query": query, "limit": limit }),
            )
            .await?;
        let status = response.status();
        if status.is_success() {
            return response
                .json::<SearchResponse>()
                .await
                .map_err(|error| WebError::Failed(error.to_string()));
        }
        Err(self.classify(status, response).await)
    }

    pub async fn fetch(&self, url: &str) -> Result<FetchedPage, WebError> {
        // `render` is sent explicitly as false rather than omitted. The seam of spec §3.5 is a
        // field on the wire, and a caller that never names it would keep working if the default
        // ever flipped — which is precisely the change that must not happen quietly.
        let response = self
            .post(
                "/fetch",
                &serde_json::json!({ "url": url, "render": false }),
            )
            .await?;
        let status = response.status();
        if status.is_success() {
            return response
                .json::<FetchedPage>()
                .await
                .map_err(|error| WebError::Failed(error.to_string()));
        }
        Err(self.classify(status, response).await)
    }

    async fn post(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, WebError> {
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|error| WebError::Unreachable(error.to_string()))
    }

    /// Read the body, then classify. Split from [`classify`] so the mapping that decides whether a
    /// caller may retry is a pure function with a table of cases, not something only reachable
    /// through a live socket.
    async fn classify(&self, status: reqwest::StatusCode, response: reqwest::Response) -> WebError {
        let body = response.text().await.unwrap_or_default();
        classify(status.as_u16(), body.trim())
    }
}

/// Turn the sidecar's status code into the variant a caller can act on.
fn classify(status: u16, body: &str) -> WebError {
    let body = body.to_string();
    match status {
        403 => WebError::Blocked(body),
        422 => WebError::Unusable(body),
        501 => WebError::Failed(body),
        503 => WebError::NotConfigured(body),
        other => WebError::Failed(format!("{other}: {body}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_url_is_loopback_http() {
        let client = WebClient::new("127.0.0.1:8794", "t".into());
        assert_eq!(client.base, "http://127.0.0.1:8794");
    }

    /// A blocked destination must not look like a transient failure, because the difference decides
    /// whether a caller retries a URL that must never be tried again.
    #[test]
    fn a_refused_destination_is_distinguishable_from_a_broken_one() {
        assert!(matches!(
            classify(403, "blocked destination: 127.0.0.1 is loopback"),
            WebError::Blocked(_)
        ));
        assert!(matches!(
            classify(422, "no readable content"),
            WebError::Unusable(_)
        ));
        assert!(matches!(
            classify(503, "search provider is not configured"),
            WebError::NotConfigured(_)
        ));
        assert!(matches!(
            classify(502, "search failed"),
            WebError::Failed(_)
        ));
        assert!(matches!(classify(500, "boom"), WebError::Failed(_)));
    }

    /// 501 is the `render: true` seam of spec §3.5. It is a plain failure and NOT `Blocked`: a
    /// caller that treated "the browser is not built yet" as "this destination is refused" would
    /// record the wrong thing about a URL that is perfectly fine.
    #[test]
    fn the_unbuilt_browser_is_a_failure_and_not_a_refusal() {
        assert!(matches!(
            classify(501, "render is not implemented"),
            WebError::Failed(_)
        ));
    }

    /// An unknown status keeps its number in the message, because the one thing a reader needs from
    /// a status nobody anticipated is the status.
    #[test]
    fn an_unexpected_status_is_named_in_the_message() {
        assert!(classify(418, "teapot").to_string().contains("418"));
    }

    /// Every message a caller might surface names what happened without pretending to know more.
    #[test]
    fn errors_say_which_side_failed() {
        assert!(
            WebError::Unreachable("connection refused".into())
                .to_string()
                .contains("sidecar")
        );
        assert!(
            WebError::Blocked("loopback".into())
                .to_string()
                .contains("refused")
        );
    }
}
