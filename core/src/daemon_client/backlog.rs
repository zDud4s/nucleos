//! Backlog routes beside the ones in `trees.rs`.
use super::*;

impl DaemonClient {
    /// `POST /backlog/finish/cancel`: `{project_root?, spec_ids?}`, answered with
    /// `{cancelled, already_launched}`.
    pub async fn spec_finish_cancel(&self, body: &Value) -> Result<Value, String> {
        let response = self
            .request(reqwest::Method::POST, "/backlog/finish/cancel")
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        json_or_refusal(response, "cancelling a spec finish").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;

    /// A daemon that records every `(path, body)` it gets and answers `answer` with `status`.
    async fn cancel_daemon(status: axum::http::StatusCode, answer: Value) -> (String, Seen) {
        let seen: Seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let app = axum::Router::new().fallback(
            move |uri: axum::http::Uri, axum::Json(body): axum::Json<Value>| {
                let recorded = recorded.clone();
                let answer = answer.clone();
                async move {
                    recorded.lock().unwrap().push((uri.to_string(), body));
                    (status, axum::Json(answer))
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), seen)
    }

    #[tokio::test]
    async fn spec_finish_cancel_posts_its_body_and_returns_the_daemon_json() {
        let answer = serde_json::json!({ "cancelled": ["s1"], "already_launched": ["s2"] });
        let (url, seen) = cancel_daemon(axum::http::StatusCode::OK, answer.clone()).await;
        let client = DaemonClient::new(url, "test-token".to_string());
        let body = serde_json::json!({ "project_root": "C:/p", "spec_ids": ["s1", "s2"] });

        let got = client
            .spec_finish_cancel(&body)
            .await
            .expect("the stub daemon answers 200");

        assert_eq!(got, answer);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen, [("/backlog/finish/cancel".to_owned(), body)]);
    }

    #[tokio::test]
    async fn spec_finish_cancel_surfaces_a_refusal() {
        let (url, _seen) = cancel_daemon(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({ "error": "name a project_root or spec_ids" }),
        )
        .await;
        let client = DaemonClient::new(url, "test-token".to_string());

        let refusal = client
            .spec_finish_cancel(&serde_json::json!({}))
            .await
            .expect_err("a 422 is not a result");

        assert!(refusal.contains("spec_ids"), "said {refusal:?}");
    }
}
