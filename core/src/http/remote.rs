//! Remote access (`/remote/*`): the pairing exchange, the paired device's own two routes, and the
//! owner's code and device management. Thin over `crate::remote`; who may reach which route is
//! decided in `auth::require_token`, not here.

use super::*;

use crate::auth::RemoteDevice;

/// The pairing body is a code and a name: a few dozen bytes. Anything bigger is not one.
const PAIR_BODY_LIMIT: usize = 4 * 1024;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        // The gateway's key without a device session, only (`auth::REMOTE_GATEWAY_ROUTES`).
        .route(
            "/remote/pair",
            post(post_pair).layer(DefaultBodyLimit::max(PAIR_BODY_LIMIT)),
        )
        // A paired device, any level. The handlers still refuse a request with no device behind it.
        .route("/remote/session", get(get_session))
        .route("/remote/logout", post(post_logout))
        // The owner (control token or an Admin key); in no scope table, and `remote` is on the deny
        // list for devices.
        .route(
            "/remote/pairing-codes",
            post(post_pairing_code).layer(DefaultBodyLimit::max(PAIR_BODY_LIMIT)),
        )
        .route("/remote/devices", get(list_devices))
        .route("/remote/devices/{id}", delete(delete_device))
}

fn storage_failure(error: impl std::fmt::Display) -> (StatusCode, Json<serde_json::Value>) {
    tracing::warn!("remote access storage failed: {error}");
    refusal(StatusCode::INTERNAL_SERVER_ERROR, "storage_failed")
}

#[derive(Deserialize)]
struct PairBody {
    code: String,
    /// Accepted for the gateway's sake and ignored: the device's name was fixed by the owner when
    /// the code was created, and a browser does not get to rename itself.
    #[serde(default)]
    #[allow(dead_code)]
    device_name: Option<String>,
}

/// `POST /remote/pair`: trades a pairing code for a device session. The secret is in this answer
/// and nowhere else; every refusal is the same 401.
async fn post_pair(State(state): State<AppState>, Json(body): Json<PairBody>) -> impl IntoResponse {
    let code = body.code.trim();
    if code.is_empty() {
        return refusal(StatusCode::UNAUTHORIZED, "invalid_code").into_response();
    }
    match crate::remote::exchange(&state.pool, code, chrono::Utc::now()).await {
        Ok(paired) => Json(serde_json::json!({
            "session": paired.session,
            "device_id": paired.device_id,
            "name": paired.name,
            "access_level": paired.access_level,
            "expires_at": paired.expires_at,
        }))
        .into_response(),
        Err(crate::remote::ExchangeError::Invalid) => {
            refusal(StatusCode::UNAUTHORIZED, "invalid_code").into_response()
        }
        Err(crate::remote::ExchangeError::Storage(error)) => storage_failure(error).into_response(),
    }
}

/// `GET /remote/session`: who the paired device is. Read from the row, so a device revoked between
/// the middleware and here is still refused.
async fn get_session(
    State(state): State<AppState>,
    device: Option<Extension<RemoteDevice>>,
) -> impl IntoResponse {
    let Some(Extension(device)) = device else {
        return refusal(StatusCode::FORBIDDEN, "not_a_remote_device").into_response();
    };
    let devices = match crate::remote::list_devices(&state.pool).await {
        Ok(devices) => devices,
        Err(error) => return storage_failure(error).into_response(),
    };
    let found = devices
        .into_iter()
        .find(|row| row.id == device.id && row.revoked_at.is_none());
    let Some(found) = found else {
        return refusal(StatusCode::UNAUTHORIZED, "session_invalid").into_response();
    };
    let Ok(created) = chrono::DateTime::parse_from_rfc3339(&found.created_at) else {
        return storage_failure("unreadable created_at").into_response();
    };
    let expires_at = (created.with_timezone(&chrono::Utc) + crate::remote::SESSION_ABSOLUTE)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    Json(serde_json::json!({
        "device_id": found.id,
        "name": found.name,
        "access_level": found.access_level,
        "expires_at": expires_at,
    }))
    .into_response()
}

/// `POST /remote/logout`: revokes the calling device.
async fn post_logout(
    State(state): State<AppState>,
    device: Option<Extension<RemoteDevice>>,
) -> impl IntoResponse {
    let Some(Extension(device)) = device else {
        return refusal(StatusCode::FORBIDDEN, "not_a_remote_device").into_response();
    };
    match crate::remote::revoke_device(&state.pool, device.id, chrono::Utc::now()).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => storage_failure(error).into_response(),
    }
}

#[derive(Deserialize)]
struct CodeBody {
    #[serde(default)]
    access_level: Option<ApiTokenLevel>,
    device_name: String,
}

/// `POST /remote/pairing-codes`: the owner mints a code, shown once, at `access_level` (read-only
/// when omitted; Admin only on an explicit request). 409 while remote access is
/// off, because a code nobody can exchange would only mislead.
async fn post_pairing_code(
    State(state): State<AppState>,
    Json(body): Json<CodeBody>,
) -> impl IntoResponse {
    if !crate::remote::valid_device_name(&body.device_name) {
        return refusal(StatusCode::BAD_REQUEST, "invalid_device_name").into_response();
    }
    match crate::remote::is_armed(&state.pool).await {
        Ok(true) => {}
        Ok(false) => return refusal(StatusCode::CONFLICT, "remote_disabled").into_response(),
        Err(error) => return storage_failure(error).into_response(),
    }
    // Least privilege: a code minted without an explicit level pairs a read-only device. Admin has
    // to be asked for by name.
    let level = body.access_level.unwrap_or(ApiTokenLevel::ReadOnly);
    match crate::remote::create_pairing_code(
        &state.pool,
        level,
        &body.device_name,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(created) => (StatusCode::CREATED, Json(created)).into_response(),
        Err(error) => storage_failure(error).into_response(),
    }
}

/// `GET /remote/devices`: every device, never a hash or a secret (`DeviceInfo` has neither).
async fn list_devices(State(state): State<AppState>) -> impl IntoResponse {
    match crate::remote::list_devices(&state.pool).await {
        Ok(devices) => Json(devices).into_response(),
        Err(error) => storage_failure(error).into_response(),
    }
}

/// `DELETE /remote/devices/{id}`: 204, also for a device already revoked; 404 for an unknown id.
async fn delete_device(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    match crate::remote::revoke_device(&state.pool, id, chrono::Utc::now()).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => refusal(StatusCode::NOT_FOUND, "unknown_device").into_response(),
        Err(error) => storage_failure(error).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{DEVICE_SESSION_HEADER, Service, Token, mint_service_token};
    use crate::execution::runner::FakeCommandRunner;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use std::sync::Arc;
    use tower::ServiceExt;

    const CONTROL: &str = "control-token";

    async fn test_state() -> AppState {
        let pool = crate::testdb::fresh_pool().await;
        AppState {
            startup_config: Default::default(),
            token: Token(CONTROL.to_string()),
            pool,
            channels: Default::default(),
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            utility_model: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            fleet: Default::default(),
            turns: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// This file's routes behind the real token check, as `build_router` mounts them.
    fn router(state: AppState) -> Router {
        routes()
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::auth::require_token,
            ))
            .with_state(state)
    }

    async fn send(
        app: &Router,
        method: &str,
        uri: &str,
        bearer: &str,
        session: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, String) {
        let mut request = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {bearer}"));
        if let Some(session) = session {
            request = request.header(DEVICE_SESSION_HEADER, session);
        }
        let body = match body {
            Some(value) => {
                request = request.header("Content-Type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn json(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap_or(serde_json::Value::Null)
    }

    /// Pairs an admin device through the domain module and returns its session secret and id.
    async fn pair(state: &AppState) -> (String, i64) {
        let created = crate::remote::create_pairing_code(
            &state.pool,
            ApiTokenLevel::Admin,
            "test phone",
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        match crate::remote::exchange(&state.pool, &created.code, chrono::Utc::now()).await {
            Ok(paired) => (paired.session, paired.device_id),
            Err(_) => panic!("a fresh code must exchange"),
        }
    }

    #[tokio::test]
    async fn owner_creates_a_code_shown_once_defaulting_to_read_only() {
        let state = test_state().await;
        mint_service_token(&state.pool, Service::Remote)
            .await
            .unwrap();
        let pool = state.pool.clone();
        let app = router(state);

        let (status, text) = send(
            &app,
            "POST",
            "/remote/pairing-codes",
            CONTROL,
            None,
            Some(serde_json::json!({ "device_name": "Phone" })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let created = json(&text);
        assert_eq!(created["access_level"], "read-only");
        assert_eq!(created["device_name"], "Phone");
        let code = created["code"].as_str().unwrap();
        assert_eq!(code.len(), 11, "{code}");
        assert_eq!(code.as_bytes()[5], b'-');
        assert!(created["expires_at"].is_string());

        // Only the hash is stored: the code itself is nowhere in the table.
        let stored: Vec<(String,)> = sqlx::query_as("SELECT code_hash FROM pairing_codes")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        let raw = crate::remote::normalize_code(code);
        assert_ne!(stored[0].0, raw);
        assert_eq!(stored[0].0, crate::remote::hash_secret(&raw));

        // A name that is empty or too long is refused before anything is stored.
        for name in [String::new(), "x".repeat(65)] {
            let (status, text) = send(
                &app,
                "POST",
                "/remote/pairing-codes",
                CONTROL,
                None,
                Some(serde_json::json!({ "device_name": name })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(json(&text)["refusal"], "invalid_device_name");
        }
        let (_, text) = send(
            &app,
            "POST",
            "/remote/pairing-codes",
            CONTROL,
            None,
            Some(serde_json::json!({ "device_name": "Tablet", "access_level": "read-only" })),
        )
        .await;
        assert_eq!(json(&text)["access_level"], "read-only");
    }

    #[tokio::test]
    async fn an_explicit_admin_level_still_mints_an_admin_code() {
        let state = test_state().await;
        mint_service_token(&state.pool, Service::Remote)
            .await
            .unwrap();
        let app = router(state);

        let (status, text) = send(
            &app,
            "POST",
            "/remote/pairing-codes",
            CONTROL,
            None,
            Some(serde_json::json!({ "device_name": "Laptop", "access_level": "admin" })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        assert_eq!(json(&text)["access_level"], "admin");
    }

    #[tokio::test]
    async fn device_list_never_carries_a_hash_or_secret() {
        let state = test_state().await;
        mint_service_token(&state.pool, Service::Remote)
            .await
            .unwrap();
        let (session, id) = pair(&state).await;
        let app = router(state);

        let (status, text) = send(&app, "GET", "/remote/devices", CONTROL, None, None).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let list = json(&text);
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], id);
        assert_eq!(rows[0]["name"], "test phone");
        assert_eq!(rows[0]["access_level"], "admin");
        let mut keys: Vec<&str> = rows[0]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "access_level",
                "created_at",
                "id",
                "last_seen_at",
                "name",
                "revoked_at"
            ]
        );
        assert!(!text.contains(&session));
        assert!(!text.contains(&crate::remote::hash_secret(&session)));
        assert!(!text.contains("session_hash"));
    }

    #[tokio::test]
    async fn revoking_a_device_answers_204_and_unknown_404() {
        let state = test_state().await;
        let key = mint_service_token(&state.pool, Service::Remote)
            .await
            .unwrap();
        let (session, id) = pair(&state).await;
        let app = router(state);

        let (status, _) = send(&app, "GET", "/remote/session", &key, Some(&session), None).await;
        assert_eq!(status, StatusCode::OK);

        let uri = format!("/remote/devices/{id}");
        let (status, _) = send(&app, "DELETE", &uri, CONTROL, None, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        // Idempotent for a device that is already revoked.
        let (status, _) = send(&app, "DELETE", &uri, CONTROL, None, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = send(
            &app,
            "DELETE",
            "/remote/devices/987654",
            CONTROL,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The very next request of the revoked device is refused.
        let (status, text) = send(&app, "GET", "/remote/session", &key, Some(&session), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json(&text)["refusal"], "session_invalid");
    }

    #[tokio::test]
    async fn pair_returns_the_session_once_and_401_on_a_bad_code() {
        let state = test_state().await;
        let key = mint_service_token(&state.pool, Service::Remote)
            .await
            .unwrap();
        let created = crate::remote::create_pairing_code(
            &state.pool,
            ApiTokenLevel::RunCreating,
            "Laptop",
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let app = router(state);

        let (status, text) = send(
            &app,
            "POST",
            "/remote/pair",
            &key,
            None,
            Some(serde_json::json!({ "code": created.code, "device_name": "ignored" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let paired = json(&text);
        let session = paired["session"].as_str().unwrap().to_owned();
        assert!(session.starts_with("nos_device_"), "{session}");
        assert_eq!(paired["name"], "Laptop");
        assert_eq!(paired["access_level"], "run-creating");
        assert!(paired["device_id"].is_i64());
        assert!(paired["expires_at"].is_string());

        // The secret works as a session, and the same code does not pair twice.
        let (status, text) = send(&app, "GET", "/remote/session", &key, Some(&session), None).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(json(&text)["name"], "Laptop");
        assert!(!text.contains(&session));
        let (status, text) = send(
            &app,
            "POST",
            "/remote/pair",
            &key,
            None,
            Some(serde_json::json!({ "code": created.code })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json(&text)["refusal"], "invalid_code");

        for code in ["", "NOPE0-NOPE0"] {
            let (status, text) = send(
                &app,
                "POST",
                "/remote/pair",
                &key,
                None,
                Some(serde_json::json!({ "code": code })),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{code}");
            assert_eq!(json(&text)["refusal"], "invalid_code");
            assert!(!text.contains("nos_device_"));
        }
    }

    #[tokio::test]
    async fn owner_routes_answer_409_when_remote_is_disarmed() {
        // No `remote` row in `service_tokens`: remote access is off.
        let state = test_state().await;
        let app = router(state);

        let (status, text) = send(
            &app,
            "POST",
            "/remote/pairing-codes",
            CONTROL,
            None,
            Some(serde_json::json!({ "device_name": "Phone" })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert_eq!(json(&text)["refusal"], "remote_disabled");

        // Nothing was stored, and the owner can still see and revoke what exists.
        let (status, text) = send(&app, "GET", "/remote/devices", CONTROL, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json(&text), serde_json::json!([]));
    }
}
