use axum::{
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::Response,
};
use rand::RngExt;
use subtle::ConstantTimeEq;

use crate::state::AppState;

#[derive(Clone)]
pub struct Token(pub String);

pub fn generate_token() -> String {
    rand::rng()
        .sample_iter(&rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// What a presented bearer token is allowed to reach.
///
/// The daemon had exactly one key, and `runs::run_env` handed it to every spawned CLI. `worktree`
/// and `shadow` runs launch with `ToolPolicy::Unrestricted`, so they own a Bash tool, and the
/// classifier calls `echo $NUCLEOS_DAEMON_TOKEN` a `read-local` action — allowed even in shadow
/// mode, whose entire premise is watching a run work without letting it act. That run could print
/// the key that approves its own proposals and disengages the kill switch, and every decision the
/// hook made after that was advisory. A gate is only a gate while what is behind it cannot be
/// reached another way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The human's key: the shell reads it from Credential Manager, the sidecars are launched with
    /// it, and orchestrator turns carry it because they act for the user. Every route.
    ///
    /// An orchestrator turn is the one agent that holds this, and it can because
    /// `ToolPolicy::McpOnly` leaves it no Bash, no Read and no Write — it has no way to look at its
    /// own environment. That is the property that makes it safe, not the mode's name.
    Control,
    /// One autonomous run's key. Minted when the run is created, dead the moment the run stops
    /// running, and good for exactly one route: asking the safety gate about its own tool call.
    ///
    /// Nothing is lost by keeping it that narrow — only orchestrator turns are given an
    /// `--mcp-config`, so a `worktree`, `shadow` or triage run has no daemon tool to call in the
    /// first place. Everything such a run wants to do goes through the gate, which is the point.
    Run(i64),
}

/// The only route a run token opens, and the reason a run token exists.
const HOOK_ROUTE: &str = "/hooks/pretooluse-decision";

/// PURE: whether `scope` may perform `method` on `path`.
///
/// One table rather than a capability declared beside each route: this is a safety boundary, and a
/// boundary you have to reconstruct by reading forty route definitions is one nobody audits. A new
/// route is unreachable by a run token until someone adds it here on purpose.
fn permits(scope: &Scope, method: &Method, path: &str) -> bool {
    match scope {
        Scope::Control => true,
        Scope::Run(_) => method == Method::POST && path == HOOK_ROUTE,
    }
}

/// A run's key and the secret to store for it: `<run_id>.<secret>`.
///
/// The id travels in the token so the lookup is by primary key rather than by the secret itself,
/// which keeps the comparison in Rust — and constant-time — instead of in SQLite's `=`.
pub fn mint_run_token(id: i64) -> (String, String) {
    let secret = generate_token();
    (format!("{id}.{secret}"), secret)
}

/// Resolves a presented bearer to a scope, or `None` if it authenticates nothing.
async fn resolve(state: &AppState, presented: &str) -> Option<Scope> {
    // Constant-time compare: `==` on the token short-circuits at the first differing byte, timing
    // which leaks the secret's content one byte at a time. `ct_eq` only short-circuits on a length
    // mismatch, and the length is not the secret.
    if bool::from(presented.as_bytes().ct_eq(state.token.0.as_bytes())) {
        return Some(Scope::Control);
    }

    let (id, secret) = presented.split_once('.')?;
    let id: i64 = id.parse().ok()?;

    let (stored, status) = sqlx::query_as::<_, (Option<String>, String)>(
        "SELECT token, status FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .ok()??;

    // A run token dies with its run. Otherwise a finished run's environment — still sitting in a
    // log, a crash dump, or a child process that outlived the CLI — would stay a working key long
    // after the run it belonged to stopped being governed by anything.
    if status != "running" {
        return None;
    }
    // `None` is every row written before migration 0022 and every orchestrator turn: no stored
    // secret, so nothing to match, so the token authenticates nothing.
    bool::from(secret.as_bytes().ct_eq(stored?.as_bytes())).then_some(Scope::Run(id))
}

pub async fn require_token(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?
        .to_owned();

    let scope = resolve(&state, &presented)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // 403 rather than 401: the caller authenticated, it is simply not allowed here. Warned rather
    // than silently refused, because a run reaching for a control route is the exact signature of
    // the thing this scope exists to stop, and it should be visible when it happens.
    if !permits(&scope, req.method(), req.uri().path()) {
        tracing::warn!(
            ?scope,
            method = %req.method(),
            path = %req.uri().path(),
            "refused a request outside the caller's scope"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    // Handlers that care WHICH run is calling read this rather than believing the request body;
    // `hooks::pretooluse_decision` is the one that does.
    req.extensions_mut().insert(scope);
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeCommandRunner;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn test_state(token: &str) -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        // Run tokens are resolved against the `runs` table, so this can no longer be a bare pool.
        sqlx::migrate!().run(&pool).await.unwrap();
        AppState {
            token: Token(token.to_string()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    fn protected_router(state: AppState) -> Router {
        Router::new()
            .route("/secret", get(|| async { "top secret" }))
            // A stand-in for the real gate route: these tests are about who may reach it, and the
            // path is what `permits` matches on.
            .route(HOOK_ROUTE, axum::routing::post(|| async { "decided" }))
            .route(
                "/proposals/{id}/approve",
                axum::routing::post(|| async { "" }),
            )
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_token,
            ))
            .with_state(state)
    }

    /// A run in the state a real one is in when its CLI calls the gate: `running`, with its minted
    /// secret stored. Returns what the CLI would find in `NUCLEOS_DAEMON_TOKEN`.
    async fn running_run_with_token(state: &AppState) -> (i64, String) {
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'worktree', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let (token, secret) = mint_run_token(id);
        sqlx::query("UPDATE runs SET token = ? WHERE id = ?")
            .bind(&secret)
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        (id, token)
    }

    async fn status_of(app: &Router, method: &str, uri: &str, bearer: &str) -> StatusCode {
        app.clone()
            .oneshot(
                HttpRequest::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn rejects_missing_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_wrong_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_token_prefix() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer expected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_malformed_scheme() {
        for header in ["expected-token", "bearer expected-token", "Bearer"] {
            let app = protected_router(test_state("expected-token").await);
            let response = app
                .oneshot(
                    HttpRequest::builder()
                        .uri("/secret")
                        .header("Authorization", header)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "header {header:?} should not authenticate"
            );
        }
    }

    #[tokio::test]
    async fn accepts_correct_token() {
        let app = protected_router(test_state("expected-token").await);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/secret")
                    .header("Authorization", "Bearer expected-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The whole point of the scope. A `worktree` or `shadow` run has a Bash tool and the classifier
    /// permits `echo $NUCLEOS_DAEMON_TOKEN`, so whatever is in its environment must be assumed
    /// published. What it opens is one route.
    #[tokio::test]
    async fn a_run_token_opens_the_gate_route_and_nothing_else() {
        let state = test_state("control-token").await;
        let (_, run_token) = running_run_with_token(&state).await;
        let app = protected_router(state);

        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &run_token).await,
            StatusCode::OK,
            "a run must still be able to ask the gate about its own tool call"
        );
        // 403, not 401: it authenticated. It is simply not allowed to approve anything.
        assert_eq!(
            status_of(&app, "POST", "/proposals/7/approve", &run_token).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_of(&app, "GET", "/secret", &run_token).await,
            StatusCode::FORBIDDEN
        );
    }

    /// A run's environment outlives the run — in a log, a crash dump, a child process that survived
    /// the CLI. The key must not.
    #[tokio::test]
    async fn a_run_token_stops_working_when_its_run_stops_running() {
        let state = test_state("control-token").await;
        let (id, run_token) = running_run_with_token(&state).await;
        let app = protected_router(state.clone());

        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &run_token).await,
            StatusCode::OK
        );

        for status in [
            "completed",
            "failed",
            "timed_out",
            "cancelled",
            "interrupted",
        ] {
            sqlx::query("UPDATE runs SET status = ? WHERE id = ?")
                .bind(status)
                .bind(id)
                .execute(&state.pool)
                .await
                .unwrap();
            assert_eq!(
                status_of(&app, "POST", HOOK_ROUTE, &run_token).await,
                StatusCode::UNAUTHORIZED,
                "a {status} run's token must not still open the gate"
            );
        }
    }

    /// Every `runs` row written before migration 0022 has a NULL token, and so does every
    /// orchestrator turn. `NULL` must not be something a caller can match by guessing the shape.
    #[tokio::test]
    async fn a_run_with_no_stored_secret_authenticates_nothing() {
        let state = test_state("control-token").await;
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'worktree', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let app = protected_router(state);

        for guess in [format!("{id}."), format!("{id}.null"), format!("{id}.{id}")] {
            assert_eq!(
                status_of(&app, "POST", HOOK_ROUTE, &guess).await,
                StatusCode::UNAUTHORIZED,
                "{guess:?} must not authenticate"
            );
        }
    }

    #[tokio::test]
    async fn a_token_naming_a_run_that_does_not_exist_authenticates_nothing() {
        let state = test_state("control-token").await;
        let app = protected_router(state);

        for bogus in ["999.secret", "abc.secret", "-1.secret", ".", "0.0"] {
            assert_eq!(
                status_of(&app, "POST", HOOK_ROUTE, bogus).await,
                StatusCode::UNAUTHORIZED,
                "{bogus:?} must not authenticate"
            );
        }
    }

    /// One run's key must not open another run's, even though both are live and both are runs.
    #[tokio::test]
    async fn one_runs_token_does_not_become_anothers() {
        let state = test_state("control-token").await;
        let (first_id, first_token) = running_run_with_token(&state).await;
        let (second_id, _) = running_run_with_token(&state).await;
        assert_ne!(first_id, second_id);

        let forged = format!("{second_id}.{}", first_token.split_once('.').unwrap().1);
        let app = protected_router(state);
        assert_eq!(
            status_of(&app, "POST", HOOK_ROUTE, &forged).await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// The shell and the sidecars are unaffected — this narrows what a *run* holds, not what the
    /// daemon's own key opens.
    #[tokio::test]
    async fn the_control_token_still_reaches_everything() {
        let state = test_state("control-token").await;
        let app = protected_router(state);

        for (method, uri) in [
            ("GET", "/secret"),
            ("POST", HOOK_ROUTE),
            ("POST", "/proposals/7/approve"),
        ] {
            assert_eq!(
                status_of(&app, method, uri, "control-token").await,
                StatusCode::OK,
                "{method} {uri}"
            );
        }
    }
}
