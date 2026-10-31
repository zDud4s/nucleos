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
        // The setup wizard. Control token only, checked in each handler (`require_control`) as well
        // as by the device deny list, so a later "owner device" level does not open it.
        .route(
            "/remote/setup",
            get(get_setup)
                .post(post_setup)
                .delete(delete_setup)
                .layer(DefaultBodyLimit::max(PAIR_BODY_LIMIT)),
        )
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

/// Only the control token: not an API key of any level, not a run, not a paired device. The same
/// lock as `/system/autostart` (`admin.rs`), so a device level added later cannot reach this route.
fn require_control(scope: &Scope) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if *scope == Scope::Control {
        Ok(())
    } else {
        tracing::warn!(
            ?scope,
            "refused a caller other than the control token on /remote/setup"
        );
        Err(refusal(StatusCode::FORBIDDEN, "control_only"))
    }
}

/// `GET /remote/setup` and what the other two verbs answer with.
#[derive(Serialize)]
struct SetupView {
    enabled: bool,
    public_origin: String,
    tailscale: crate::remote::setup::TailscaleView,
    /// Tailscale publishes the gateway on 443.
    serving: bool,
    /// Port 443 is open to the public internet (Funnel). Setup refuses to turn on while it is.
    funnel: bool,
    /// Port 443 already serves something that is not the gateway; setup leaves it alone.
    port_taken: bool,
}

async fn setup_view(root: &std::path::Path) -> SetupView {
    use crate::remote::setup::{self, ServePort};
    let config = crate::remote::RemoteConfig::load(&root.join(crate::remote::REMOTE_FILE));
    let cli = setup::locate_cli();
    let tailscale = setup::probe(cli.as_deref()).await;
    let reading = if tailscale.installed {
        setup::serve_reading(cli.as_deref()).await
    } else {
        None
    };
    SetupView {
        enabled: config.enabled && config.validate().is_ok(),
        public_origin: config.public_origin,
        tailscale,
        serving: reading.is_some_and(|r| r.port == ServePort::Ours),
        funnel: reading.is_some_and(|r| r.funnel),
        port_taken: reading.is_some_and(|r| r.port == ServePort::Foreign),
    }
}

fn machine_root(state: &AppState) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    state
        .machine_config_root
        .clone()
        .ok_or_else(|| refusal(StatusCode::CONFLICT, "no_machine_config"))
}

fn exe_dir() -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .ok_or_else(|| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
}

/// `GET /remote/setup`: whether remote access is on, and what Tailscale says about this machine.
async fn get_setup(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
) -> impl IntoResponse {
    if let Err(denied) = require_control(&scope) {
        return denied.into_response();
    }
    match machine_root(&state) {
        Ok(root) => Json(setup_view(&root).await).into_response(),
        Err(refused) => refused.into_response(),
    }
}

#[derive(Deserialize)]
struct SetupBody {
    /// The owner's own `https://` origin. Absent: use this machine's Tailscale name and publish the
    /// gateway there with `tailscale serve`.
    #[serde(default)]
    public_origin: Option<String>,
}

/// A refusal for a setup step that failed before or while changing something.
fn setup_error(error: crate::remote::setup::SetupError) -> (StatusCode, Json<serde_json::Value>) {
    use crate::remote::setup::SetupError;
    match error {
        SetupError::InvalidOrigin(why) => {
            super::common::refusal_with_detail(StatusCode::BAD_REQUEST, "invalid_origin", why)
        }
        SetupError::PageMissing => refusal(StatusCode::CONFLICT, "remote_page_missing"),
        SetupError::ConfigUnreadable(why) => {
            super::common::refusal_with_detail(StatusCode::CONFLICT, "config_unreadable", why)
        }
        other => storage_failure(other),
    }
}

/// A refusal for a Tailscale Serve step. `serve_needs_approval` carries the link in `detail`.
fn serve_refusal(error: crate::remote::setup::ServeError) -> (StatusCode, Json<serde_json::Value>) {
    use crate::remote::setup::ServeError;
    match error {
        ServeError::Taken => refusal(StatusCode::CONFLICT, "serve_port_taken"),
        ServeError::Funnel => refusal(StatusCode::CONFLICT, "funnel_enabled"),
        ServeError::Failed(failure) => match failure.approval_url {
            Some(url) => super::common::refusal_with_detail(
                StatusCode::CONFLICT,
                "serve_needs_approval",
                url,
            ),
            None => super::common::refusal_with_detail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "serve_failed",
                failure.why,
            ),
        },
    }
}

/// `POST /remote/setup { public_origin? }`: writes `remote.yaml`, starts the gateway and, with no
/// origin given, publishes it on the tailnet (never with Funnel).
async fn post_setup(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(body): Json<SetupBody>,
) -> impl IntoResponse {
    use crate::remote::setup;
    if let Err(denied) = require_control(&scope) {
        return denied.into_response();
    }
    let (root, exe_dir) = match (machine_root(&state), exe_dir()) {
        (Ok(root), Ok(dir)) => (root, dir),
        (Err(refused), _) | (_, Err(refused)) => return refused.into_response(),
    };
    turn_on(
        &state,
        &root,
        &exe_dir,
        body.public_origin.as_deref(),
        setup::gateway_allowed(),
        setup::locate_cli(),
    )
    .await
}

/// The turn-on step with its surroundings passed in (`primary`: this daemon may run the gateway;
/// `cli`: where Tailscale is), so a test needs no real Tailscale and no real gateway.
async fn turn_on(
    state: &AppState,
    root: &std::path::Path,
    exe_dir: &std::path::Path,
    own_origin: Option<&str>,
    primary: bool,
    cli: Option<PathBuf>,
) -> axum::response::Response {
    use crate::remote::setup::{self, TailscaleCall};
    // Before anything is published or written: a daemon that may not run the gateway would answer
    // "on" with nothing behind it.
    if !primary {
        return refusal(StatusCode::CONFLICT, "not_primary").into_response();
    }
    // One turn-on or turn-off at a time: they share the file, the key and the Tailscale port.
    let _one_at_a_time = setup::lock_setup().await;

    let own_origin = own_origin
        .map(str::trim)
        .filter(|origin| !origin.is_empty());
    let via_tailscale = own_origin.is_none();
    let origin = match own_origin {
        Some(origin) => origin.to_string(),
        None => {
            let tailscale = setup::probe(cli.as_deref()).await;
            match (tailscale.dns_name, tailscale.signed_in, tailscale.https_ok) {
                (Some(name), true, true) => setup::origin_from_dns(&name),
                _ => return refusal(StatusCode::CONFLICT, "tailscale_not_ready").into_response(),
            }
        }
    };

    // Everything that can be refused is refused before anything is published or written.
    let config = match setup::plan(root, &origin, exe_dir) {
        Ok(config) => config,
        Err(error) => return setup_error(error).into_response(),
    };
    let mut published_here = false;
    if via_tailscale {
        let Some(cli) = cli.as_deref() else {
            return refusal(StatusCode::CONFLICT, "tailscale_not_ready").into_response();
        };
        match setup::serve_on(cli).await {
            Ok(turned_on) => published_here = turned_on,
            Err(error) => return serve_refusal(error).into_response(),
        }
    }
    if let Err(error) = setup::commit(
        &state.pool,
        root,
        &config,
        exe_dir,
        &crate::daemon_client::daemon_url(),
        |env| {
            setup::launch_gateway(env);
        },
    )
    .await
    {
        // Serve was published by this call and nothing is behind it now: take it back, but never a
        // publication that was already there.
        if published_here {
            if let Some(cli) = cli.as_deref() {
                if let Err(failure) = setup::run(cli, TailscaleCall::ServeOff).await {
                    tracing::warn!(why = %failure.why, "could not roll back publishing the gateway on Tailscale");
                }
            }
        }
        return setup_error(error).into_response();
    }
    Json(setup_view(root).await).into_response()
}

/// `DELETE /remote/setup`: switches remote access off, stops the gateway and stops publishing it.
async fn delete_setup(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
) -> impl IntoResponse {
    use crate::remote::setup;
    if let Err(denied) = require_control(&scope) {
        return denied.into_response();
    }
    let root = match machine_root(&state) {
        Ok(root) => root,
        Err(refused) => return refused.into_response(),
    };
    turn_off(&state, &root, setup::gateway_allowed(), setup::locate_cli()).await
}

/// The turn-off step with its surroundings passed in, like `turn_on`.
async fn turn_off(
    state: &AppState,
    root: &std::path::Path,
    primary: bool,
    cli: Option<PathBuf>,
) -> axum::response::Response {
    use crate::remote::setup;
    // A daemon that does not run the gateway would disarm the key and unpublish the port of the one
    // that does, and leave that one's gateway running.
    if !primary {
        return refusal(StatusCode::CONFLICT, "not_primary").into_response();
    }
    let _one_at_a_time = setup::lock_setup().await;
    if let Err(error) = setup::disable(&state.pool, root, setup::stop_gateway).await {
        return setup_error(error).into_response();
    }
    // Best effort: the gateway is already off, so a Tailscale that will not answer changes nothing
    // about who can get in. Only a publication that points at the gateway is stopped.
    if let Some(cli) = cli {
        if let Err(failure) = setup::serve_off(&cli).await {
            tracing::warn!(why = %failure.why, "could not stop publishing the gateway on Tailscale");
        }
    }
    Json(setup_view(root).await).into_response()
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

    /// The route is refused to a device by the deny list AND by the handler itself; this checks both
    /// locks, the second being the one that stays shut if a later device level reaches `remote`.
    #[tokio::test]
    async fn a_remote_device_cannot_change_setup() {
        use crate::auth::ApiTokenLevel as Level;
        let state = test_state().await;
        for scope in [
            Scope::ApiToken(Level::Admin),
            Scope::ApiToken(Level::RunCreating),
            Scope::ApiToken(Level::ReadOnly),
            Scope::Run(7),
        ] {
            let read = get_setup(State(state.clone()), Extension(scope.clone()))
                .await
                .into_response();
            assert_eq!(read.status(), StatusCode::FORBIDDEN, "{scope:?}");
            let write = post_setup(
                State(state.clone()),
                Extension(scope.clone()),
                Json(SetupBody {
                    public_origin: Some("https://box.example.com".to_string()),
                }),
            )
            .await
            .into_response();
            assert_eq!(write.status(), StatusCode::FORBIDDEN, "{scope:?}");
            let off = delete_setup(State(state.clone()), Extension(scope.clone()))
                .await
                .into_response();
            assert_eq!(off.status(), StatusCode::FORBIDDEN, "{scope:?}");
        }
        assert!(require_control(&Scope::Control).is_ok());

        // Through the real token check, a paired admin device gets nowhere either.
        let key = mint_service_token(&state.pool, Service::Remote)
            .await
            .unwrap();
        let (session, _) = pair(&state).await;
        let app = router(state);
        for method in ["GET", "POST", "DELETE"] {
            let body = (method == "POST").then(|| serde_json::json!({}));
            // The session is valid, so a 401 would mean something else broke; the middleware's
            // deny list answers 403 with no body. The handler's own lock also answers 403 but
            // names itself `control_only`, so seeing that name means the deny list let it through.
            let (status, text) = send(
                &app,
                method,
                "/remote/setup",
                &key,
                Some(&session),
                body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {text}");
            assert!(
                !text.contains("control_only"),
                "{method}: reached the handler"
            );
            // The gateway's own key without a device is not an owner either.
            let (status, text) = send(&app, method, "/remote/setup", &key, None, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} (key only): {text}");
            assert!(!text.contains("control_only"), "{method} (key only)");
        }
        // And the deny list itself, level by level.
        for level in [Level::Admin, Level::RunCreating, Level::ReadOnly] {
            for method in [
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::DELETE,
            ] {
                assert!(
                    !crate::auth::remote_device_permits(level, &method, "/remote/setup"),
                    "{level:?} {method}"
                );
            }
        }
    }

    async fn body_of(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, json(&String::from_utf8_lossy(&bytes)))
    }

    fn page_in(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("dist-remote")).unwrap();
        std::fs::write(dir.join("dist-remote").join("index.html"), "<html></html>").unwrap();
    }

    #[tokio::test]
    async fn a_daemon_that_may_not_run_the_gateway_refuses_before_writing_anything() {
        let state = test_state().await;
        let tmp = tempfile::tempdir().unwrap();
        let (root, exe_dir) = (tmp.path().join("home"), tmp.path().join("app"));
        page_in(&exe_dir);

        let response = turn_on(
            &state,
            &root,
            &exe_dir,
            Some("https://box.example.com"),
            false,
            None,
        )
        .await;
        let (status, body) = body_of(response).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["refusal"], "not_primary");
        assert!(!root.join(crate::remote::REMOTE_FILE).exists());
        assert!(!crate::remote::is_armed(&state.pool).await.unwrap());
    }

    #[tokio::test]
    async fn a_daemon_that_may_not_run_the_gateway_cannot_turn_it_off() {
        let state = test_state().await;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join(crate::remote::REMOTE_FILE);
        std::fs::write(&file, "enabled: true\n").unwrap();

        let (status, body) = body_of(turn_off(&state, &root, false, None).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["refusal"], "not_primary");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "enabled: true\n");
    }

    #[tokio::test]
    async fn turning_on_with_no_tailscale_is_refused_and_writes_nothing() {
        let state = test_state().await;
        let tmp = tempfile::tempdir().unwrap();
        let (root, exe_dir) = (tmp.path().join("home"), tmp.path().join("app"));
        page_in(&exe_dir);

        let response = turn_on(&state, &root, &exe_dir, None, true, None).await;
        let (status, body) = body_of(response).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["refusal"], "tailscale_not_ready");
        assert!(!root.join(crate::remote::REMOTE_FILE).exists());
        assert!(!crate::remote::is_armed(&state.pool).await.unwrap());
    }

    #[tokio::test]
    async fn a_malformed_remote_yaml_is_refused_on_and_off_and_left_alone() {
        let state = test_state().await;
        let tmp = tempfile::tempdir().unwrap();
        let (root, exe_dir) = (tmp.path().join("home"), tmp.path().join("app"));
        page_in(&exe_dir);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join(crate::remote::REMOTE_FILE);
        let broken = "enabled: [oops\npublic_origin: \"https://mine.example.com\"\n";
        std::fs::write(&file, broken).unwrap();

        let response = turn_on(
            &state,
            &root,
            &exe_dir,
            Some("https://box.example.com"),
            true,
            None,
        )
        .await;
        let (status, body) = body_of(response).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["refusal"], "config_unreadable");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
        assert!(!crate::remote::is_armed(&state.pool).await.unwrap());

        let (status, body) = body_of(turn_off(&state, &root, true, None).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["refusal"], "config_unreadable");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
    }

    #[test]
    fn each_serve_failure_has_its_own_refusal() {
        use crate::remote::setup::{CliFailure, ServeError};
        let status_and_name = |error| {
            let (status, Json(body)) = serve_refusal(error);
            (status, body)
        };

        let (status, body) = status_and_name(ServeError::Taken);
        assert_eq!(
            (status, body["refusal"].as_str()),
            (StatusCode::CONFLICT, Some("serve_port_taken"))
        );

        let (status, body) = status_and_name(ServeError::Funnel);
        assert_eq!(
            (status, body["refusal"].as_str()),
            (StatusCode::CONFLICT, Some("funnel_enabled"))
        );

        // A link the person has to open: its own code, the link as the detail.
        let (status, body) = status_and_name(ServeError::Failed(CliFailure {
            why: "tailscale did not answer in time".to_string(),
            approval_url: Some("https://login.tailscale.com/f/serve?node=n1".to_string()),
        }));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["refusal"], "serve_needs_approval");
        assert_eq!(
            body["detail"],
            "https://login.tailscale.com/f/serve?node=n1"
        );

        // No link: the plain failure keeps what the CLI said.
        let (status, body) = status_and_name(ServeError::Failed(CliFailure {
            why: "boom".to_string(),
            approval_url: None,
        }));
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["refusal"], "serve_failed");
        assert_eq!(body["detail"], "boom");
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
