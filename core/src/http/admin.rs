//! API tokens, backups, sidecars and this machine's settings (`/api-tokens`, `/backup*`, `/sidecars`, `/config/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/machine", get(get_machine).put(put_machine))
        .route("/sidecars", get(get_sidecars))
        // The update handshake. In no `auth.rs` table on purpose: Control and Admin only, like
        // `/daemon/shutdown`. A run's key must not read the readiness or begin a drain.
        .route("/system/update-readiness", get(get_update_readiness))
        .route("/system/shutdown", post(post_system_shutdown))
        // Restarts the daemon once running work has settled. Control and Admin only, by the `system` segment, like `/system/shutdown`.
        .route("/system/restart", post(super::restart::post_system_restart))
        // Control token only, checked inside the handler (`require_control`) and not only by the
        // `system` segment: a later device level must not open a route that takes a password.
        .route(
            "/system/autostart",
            get(get_system_autostart).put(put_system_autostart),
        )
        .route("/sidecars/{name}/restart", post(post_sidecar_restart))
        .route("/config/email", get(get_email_config))
        // In no `auth.rs` table on purpose: Control and Admin only, like `/settings`.
        .route(
            "/config/distiller",
            get(crate::distill::model::get_distiller_config)
                .post(crate::distill::model::post_distiller_config),
        )
        // In no `auth.rs` table on purpose, like `/config/distiller`.
        .route(
            "/config/distiller-sources",
            get(crate::distill::sources::get_sources).post(crate::distill::sources::post_sources),
        )
        .route("/distill/chats/ingest", post(post_ingest_chats))
        .route(
            "/config/capture",
            get(get_capture_config).post(post_capture_config),
        )
        .route(
            "/config/embedding",
            get(crate::embed::model::get_embedding_config)
                .post(crate::embed::model::post_embedding_config),
        )
        .route("/config/secrets", get(get_machine_secrets))
        .route(
            "/config/secrets/{key}",
            axum::routing::put(put_machine_secret).delete(delete_machine_secret),
        )
        .route("/backup", post(post_backup))
        .route("/backups", get(get_backups))
        // In no `auth.rs` table on purpose, like `/settings`: it names the data directory's
        // layout and sizes, which a read-only key has no use for.
        .route("/system/storage", get(get_system_storage))
        // Same reasoning, and the same default: it rewrites the database file on the next start.
        .route("/system/storage/compact", post(post_system_storage_compact))
        .route("/backups/{name}/restore", post(post_backup_restore))
        .route("/api-tokens", get(list_api_tokens).post(create_api_token))
        .route(
            "/api-tokens/{name}",
            axum::routing::delete(revoke_api_token),
        )
}

#[derive(Deserialize)]
pub(super) struct CreateApiTokenRequest {
    pub(super) name: String,
    pub(super) level: ApiTokenLevel,
}

#[derive(Serialize)]
pub(super) struct CreatedApiToken {
    pub(super) name: String,
    pub(super) level: ApiTokenLevel,
    pub(super) created_at: String,
    /// The complete bearer credential. It is returned only by creation, never by listing.
    pub(super) token: String,
}

#[derive(Serialize)]
pub(super) struct ApiTokenSummary {
    pub(super) name: String,
    pub(super) level: ApiTokenLevel,
    pub(super) created_at: String,
}

#[derive(sqlx::FromRow)]
pub(super) struct ApiTokenRow {
    pub(super) name: String,
    pub(super) access_level: String,
    pub(super) created_at: String,
}

pub(super) fn valid_api_token_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) async fn create_api_token(
    State(state): State<AppState>,
    Json(body): Json<CreateApiTokenRequest>,
) -> Result<(StatusCode, Json<CreatedApiToken>), StatusCode> {
    if !valid_api_token_name(&body.name) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let created_at = chrono::Utc::now().to_rfc3339();
    let (token, secret) = mint_api_token(&body.name);
    let result = sqlx::query(
        "INSERT INTO api_tokens (name, token, access_level, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&body.name)
    .bind(secret)
    .bind(body.level.as_str())
    .bind(&created_at)
    .execute(&state.pool)
    .await;

    if let Err(error) = result {
        if error
            .as_database_error()
            .is_some_and(|database| database.is_unique_violation())
        {
            return Err(StatusCode::CONFLICT);
        }
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok((
        StatusCode::CREATED,
        Json(CreatedApiToken {
            name: body.name,
            level: body.level,
            created_at,
            token,
        }),
    ))
}

pub(super) async fn list_api_tokens(
    State(state): State<AppState>,
) -> Result<Json<Vec<ApiTokenSummary>>, StatusCode> {
    let rows = sqlx::query_as::<_, ApiTokenRow>(
        "SELECT name, access_level, created_at FROM api_tokens ORDER BY created_at, name",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    rows.into_iter()
        .map(|row| {
            Ok(ApiTokenSummary {
                name: row.name,
                level: ApiTokenLevel::from_str(&row.access_level)
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?,
                created_at: row.created_at,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

pub(super) async fn revoke_api_token(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let result = sqlx::query("DELETE FROM api_tokens WHERE name = ?")
        .bind(name)
        .execute(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

pub(super) async fn post_backup(
    State(state): State<AppState>,
) -> Result<Json<backup::BackupInfo>, StatusCode> {
    backup::take_backup(&state.pool, backup::DEFAULT_RETENTION)
        .await
        .map(Json)
        .map_err(|error| backup_status(&error))
}

pub(super) async fn get_backups(
    State(state): State<AppState>,
) -> Result<Json<Vec<backup::BackupInfo>>, StatusCode> {
    backup::list_backups(&state.pool)
        .await
        .map(Json)
        .map_err(|error| backup_status(&error))
}

/// How long one folder may take to measure before the panel shows "unknown" instead of waiting.
/// A files folder holding years of attachments is a tree of tens of thousands of entries, and a
/// slow disk must not hold a poll that the shell repeats every few seconds.
const FOLDER_SIZE_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Serialize)]
pub(super) struct StorageReport {
    #[serde(flatten)]
    database: crate::storage::DatabaseStats,
    /// `null` for the three folders when measuring ran past [`FOLDER_SIZE_BUDGET`] (or the folder
    /// is not configured), so "unknown" never reads as "empty".
    backups_bytes: Option<u64>,
    files_bytes: Option<u64>,
    logs_bytes: Option<u64>,
    /// `nucleos.db.*` copies lying beside the database, that nothing prunes.
    loose_bytes: u64,
    /// When the newest automatic backup was taken, from its file name.
    auto_backup_last: Option<String>,
    /// A compaction is scheduled for the next start (`POST /system/storage/compact`).
    compaction_pending: bool,
}

#[derive(Serialize)]
pub(super) struct StagedCompaction {
    applies: &'static str,
}

/// Schedules a `VACUUM` for the next daemon start, before the pool opens. Never runs one here: a
/// compaction holds the file for as long as it takes, which no request should wait on. 409 while a
/// restore is pending.
pub(super) async fn post_system_storage_compact(
    State(state): State<AppState>,
) -> Result<Json<StagedCompaction>, StatusCode> {
    backup::stage_compaction(&state.pool)
        .await
        .map(|()| {
            Json(StagedCompaction {
                applies: "on next daemon start",
            })
        })
        .map_err(|error| backup_status(&error))
}

pub(super) async fn get_system_storage(
    State(state): State<AppState>,
) -> Result<Json<StorageReport>, StatusCode> {
    let database = crate::storage::database_stats(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "storage: could not read the database page counts");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let db_path = crate::storage::database_file(&state.pool)
        .await
        .ok()
        .flatten();
    let compaction_pending = db_path.as_deref().is_some_and(backup::compaction_pending);
    let data_dir = db_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let (backups_bytes, files_bytes, logs_bytes) = tokio::join!(
        folder_size(data_dir.as_ref().map(|dir| dir.join("backups"))),
        folder_size(state.files_root.clone()),
        folder_size(data_dir.as_ref().map(|dir| dir.join("logs"))),
    );
    let loose_bytes = match db_path {
        Some(path) => tokio::task::spawn_blocking(move || crate::storage::loose_files_bytes(&path))
            .await
            .unwrap_or(0),
        None => 0,
    };
    let auto_backup_last = backup::newest_auto_backup(&state.pool)
        .await
        .ok()
        .flatten()
        .map(|taken| taken.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    Ok(Json(StorageReport {
        database,
        backups_bytes,
        files_bytes,
        logs_bytes,
        loose_bytes,
        auto_backup_last,
        compaction_pending,
    }))
}

async fn folder_size(path: Option<std::path::PathBuf>) -> Option<u64> {
    let path = path?;
    let measuring = tokio::task::spawn_blocking(move || crate::storage::directory_size(&path));
    tokio::time::timeout(FOLDER_SIZE_BUDGET, measuring)
        .await
        .ok()?
        .ok()
}

pub(super) async fn post_backup_restore(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<backup::StagedRestore>, StatusCode> {
    if !plain_filename(&name) {
        return Err(StatusCode::BAD_REQUEST);
    }

    backup::stage_restore(&state.pool, &name)
        .await
        .map(Json)
        .map_err(|error| backup_status(&error))
}

pub(super) fn plain_filename(name: &str) -> bool {
    if name.is_empty() || name.contains("..") || name.contains('/') || name.contains('\\') {
        return false;
    }

    let mut components = std::path::Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

pub(super) fn backup_status(error: &backup::BackupError) -> StatusCode {
    match error {
        backup::BackupError::InvalidName | backup::BackupError::InvalidRetention => {
            StatusCode::BAD_REQUEST
        }
        backup::BackupError::NotFound => StatusCode::NOT_FOUND,
        backup::BackupError::ExistingTarget(_) | backup::BackupError::PendingRestoreExists => {
            StatusCode::CONFLICT
        }
        backup::BackupError::Verification(_) => StatusCode::UNPROCESSABLE_ENTITY,
        backup::BackupError::Database(_)
        | backup::BackupError::Io(_)
        | backup::BackupError::CompactionUnverified { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Every supervised sidecar and what has happened to it.
///
/// A sidecar that keeps failing to start was previously invisible: `sidecar.rs` restarts it with a
/// backoff and writes one warning per attempt to a log nobody reads while using the app. For the
/// email poller in particular that meant the Mail tab looked like a quiet mailbox — which is what an
/// empty inbox looks like too.
pub(super) async fn get_sidecars() -> Json<Vec<crate::sidecar::SidecarState>> {
    Json(crate::sidecar::states())
}

/// Ask a sidecar's supervisor to stop waiting and start it now.
///
/// **The kill switch covers this**, for [`delete_project_folder`]'s reason and with its wording:
/// everything else the switch stops is autonomous and this is a person pressing a button, but the
/// switch means *nothing that changes anything happens right now*, and spawning an operating-system
/// process is a change. What it does NOT do is stop the supervisor's own retry loop, which the switch
/// has never governed — so this refuses the button and says so, and the shell says the rest rather
/// than implying the machine is frozen. Project brakes are not consulted: a sidecar belongs to no
/// project, and `scoped_kill_engaged` wants a project id this route does not have.
///
/// `{name}` is the SUPERVISOR's key (`browser`), which is what `GET /sidecars` calls a sidecar. The
/// health readout's row name (`browser_sidecar`) is the shell's to translate, and a wrong translation
/// arrives here as `not_supervised` — a sentence in the row — rather than as silence.
pub(super) async fn post_sidecar_restart(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let halted = crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "refusal": "internal" })),
            )
        })?;
    if halted {
        return Err((
            StatusCode::LOCKED,
            Json(serde_json::json!({ "refusal": "kill_switch" })),
        ));
    }

    match crate::sidecar::ask_to_restart(&name) {
        crate::sidecar::RestartOutcome::Asked => {
            tracing::info!(sidecar = %name, "a restart was asked for");
            // 202 and not 200: what this promises is that the supervisor was told, not that the
            // sidecar is up. The spawn happens in the supervisor's own task, and the readout is
            // where the answer to "did it work" lives.
            Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "name": name, "asked": true })),
            ))
        }
        crate::sidecar::RestartOutcome::AlreadyRunning => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "refusal": "running" })),
        )),
        crate::sidecar::RestartOutcome::NotSupervised => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "refusal": "not_supervised" })),
        )),
    }
}

/// The email pillar's settings, minus everything secret.
///
/// No password: it comes from Credential Manager, is handed to the sidecar process, and does not
/// pass through here. The host and account are named because "which mailbox is this" is the question
/// the rest of the panel's numbers are about.
#[derive(serde::Serialize)]
pub(super) struct EmailConfigView {
    pub(super) enabled: bool,
    /// True only once the hook barrier has been PROVEN at startup. Enabled but unarmed is a real
    /// state — the pillar owns retention either way — and it is why triage can be stopped while
    /// mail keeps arriving.
    pub(super) armed: bool,
    pub(super) host: String,
    pub(super) username: String,
    pub(super) mailbox: String,
    pub(super) sent_mailbox: Option<String>,
    pub(super) poll_interval_secs: u64,
    pub(super) notify_classes: Vec<String>,
    pub(super) digest_hour_utc: u8,
    pub(super) retain_bodies_days: u8,
    /// Why local triage is unavailable when a local model was configured but could not be trusted.
    pub(super) local_triage_disabled: Option<String>,
}

/// The credentials this machine holds, by whether they are set — never by what they are.
///
/// The listing carries no value and there is no route that serves one, which is enforced a layer
/// down: [`crate::secrets::SecretStore`] has no method that returns a secret, so a future handler
/// cannot serve one by accident. Presence is the only question a settings page has.
pub(super) async fn get_machine_secrets(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let rows = crate::machine_config::SECRETS
        .iter()
        .map(|secret| {
            // A store that cannot answer is reported as unknown rather than as absent. "Not set"
            // is a fact somebody would act on by pasting a credential they have already pasted.
            let present = state.secrets.present(secret.key).ok();
            serde_json::json!({
                "key": secret.key,
                "area": secret.area,
                "what": secret.what,
                "present": present,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(serde_json::json!({ "secrets": rows })))
}

/// A credential on its way in. Deliberately derives no `Debug`.
///
/// `Debug` is how a secret ends up in a log: one `tracing::warn!(?body, …)` written in a hurry by
/// somebody debugging an unrelated failure, and the credential is on disk in the daemon's rotating
/// log file. Not deriving it makes that line refuse to compile.
#[derive(Deserialize)]
pub(super) struct SecretRequest {
    pub(super) value: String,
}

/// Stores one credential, or refuses.
///
/// **Why the app may write these at all**, given that `--set-*` reads from stdin precisely to keep
/// a token off a command line: that reasoning is about ARGV. A Windows command line is readable by
/// any process running as the same user and is recorded verbatim in PSReadLine's history, so a
/// token passed as an argument lands on disk in cleartext at the moment somebody was securely
/// storing it. None of that is true of a request body over loopback to the one process that
/// already holds every one of these credentials and hands them to the sidecars it starts.
///
/// Admin, by appearing in no table in `auth.rs`, on the same default-deny footing as everything
/// else under `/config`. The kill switch is consulted for the same reason the settings write
/// consults it: the control token reaches here, and a credential is a capability.
pub(super) async fn put_machine_secret(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<SecretRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    if crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }

    // `daemon-token` takes this path: it is not in the table, so it is `not_ours` like any other
    // key nobody declared. That is deliberate rather than incidental — see `SECRETS`.
    let Some(secret) = crate::machine_config::secret_for(&key) else {
        return Err(refusal(StatusCode::FORBIDDEN, "not_ours"));
    };

    // An empty value is refused rather than stored. Storing one would leave a credential that
    // exists and does not work, which reads as "configured" everywhere it is asked about — the
    // worst of the three available states. Forgetting it is what DELETE is for.
    if body.value.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "invalid",
                "detail": "an empty value is not a credential; use DELETE to forget one",
            })),
        ));
    }

    state
        .secrets
        .store(secret.key, &body.value)
        .map_err(|error| {
            // `error` and never the value.
            tracing::warn!(%error, key = %secret.key, "storing a credential failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;

    // The key, never the value, and no length either — a length is a hint about a secret.
    if let Err(error) = feed::append(
        &state.pool,
        None,
        "secret_stored",
        &format!("the {} credential was set from the app", secret.key),
        None,
        // A machine setting or a credential: nothing a run, a job or a council owns.
        None,
    )
    .await
    {
        tracing::error!(%error, key = %secret.key, "the credential was stored but the feed line was lost");
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Forgets one credential.
///
/// Idempotent, because [`crate::secrets::delete_secret`] already treats a missing entry as done:
/// a page that had to distinguish "forgotten" from "was not there" would be asking a question
/// whose two answers call for the same next step.
pub(super) async fn delete_machine_secret(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    if crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }

    let Some(secret) = crate::machine_config::secret_for(&key) else {
        return Err(refusal(StatusCode::FORBIDDEN, "not_ours"));
    };

    state.secrets.forget(secret.key).map_err(|error| {
        tracing::warn!(%error, key = %secret.key, "forgetting a credential failed");
        refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    if let Err(error) = feed::append(
        &state.pool,
        None,
        "secret_forgotten",
        &format!("the {} credential was forgotten from the app", secret.key),
        None,
        // A machine setting or a credential: nothing a run, a job or a council owns.
        None,
    )
    .await
    {
        tracing::error!(%error, key = %secret.key, "the credential was forgotten but the feed line was lost");
    }

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn get_email_config(State(state): State<AppState>) -> Json<EmailConfigView> {
    Json(EmailConfigView {
        enabled: state.email.enabled,
        armed: state.email.armed.load(std::sync::atomic::Ordering::Relaxed),
        host: state.email.host.clone(),
        username: state.email.username.clone(),
        mailbox: state.email.mailbox.clone(),
        sent_mailbox: state.email.sent_mailbox.clone(),
        poll_interval_secs: state.email.poll_interval_secs,
        notify_classes: state.email.notify_classes.clone(),
        digest_hour_utc: state.email.digest_hour_utc,
        retain_bodies_days: state.email.retain_bodies_days,
        local_triage_disabled: state.local_triage_disabled.clone(),
    })
}

async fn capture_config_body(pool: &sqlx::SqlitePool) -> serde_json::Value {
    serde_json::json!({
        "answer_days": crate::capture::answer_days(pool).await,
        "digest_hour": crate::capture::digest_hour(pool).await,
    })
}

pub(super) async fn get_capture_config(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(capture_config_body(&state.pool).await)
}

#[derive(serde::Deserialize)]
pub(super) struct CaptureConfigRequest {
    pub(super) answer_days: Option<i64>,
    pub(super) digest_hour: Option<i64>,
}

pub(super) async fn post_capture_config(
    State(state): State<AppState>,
    Json(body): Json<CaptureConfigRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use crate::capture::ConfigError;
    match crate::capture::set_config(
        &state.pool,
        body.answer_days,
        body.digest_hour,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(()) => Ok(Json(capture_config_body(&state.pool).await)),
        Err(ConfigError::OutOfRange) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "out_of_range" })),
        )),
        Err(ConfigError::Db(error)) => {
            tracing::warn!(%error, "setting the capture config failed");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

#[derive(serde::Deserialize, Default)]
pub(super) struct IngestChatsRequest {
    chat_ids: Option<Vec<String>>,
    project: Option<String>,
}

/// `POST /distill/chats/ingest`: the owner's explicit request to let the distiller read past chats
/// (P8). Seeds, never asks a model; answers `{seeded, skipped}`. `chat_ids` and `project` together
/// are ambiguous: 400.
pub(super) async fn post_ingest_chats(
    State(state): State<AppState>,
    Json(body): Json<IngestChatsRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use crate::distill::chats::{Selection, ingest};
    let selection = match (body.chat_ids, body.project) {
        (Some(_), Some(_)) => return Err(refusal(StatusCode::BAD_REQUEST, "chat_ids_or_project")),
        (Some(ids), None) => Selection::Chats(ids),
        (None, Some(project)) => Selection::Project(project),
        (None, None) => Selection::All,
    };
    match ingest(&state.pool, selection).await {
        Ok((seeded, skipped)) => Ok(Json(
            serde_json::json!({ "seeded": seeded, "skipped": skipped }),
        )),
        Err(error) => {
            tracing::warn!(%error, "ingesting past chats failed");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

#[derive(Deserialize)]
pub(super) struct UpdateReadinessQuery {
    /// The run asking, left out of `waiting_on` with its chat (see `shutdown::blocking_runs`).
    pub(super) requester_run: Option<i64>,
}

/// `GET /system/update-readiness`: the version, how many runs are live, and whether the daemon is
/// already draining. What the updater asks before it stops anything.
///
/// `waiting_on` leaves out runs parked on a question, and the requester's own chat when
/// `requester_run` is given.
async fn get_update_readiness(
    State(state): State<AppState>,
    Query(query): Query<UpdateReadinessQuery>,
) -> Result<Json<crate::shutdown::Readiness>, StatusCode> {
    crate::shutdown::readiness_for(
        &state.pool,
        query.requester_run,
        &crate::hooks::run_is_parked,
    )
    .await
    .map(Json)
    .map_err(|error| {
        tracing::warn!(%error, "could not count the live runs for the update readiness");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Only the control token: not an API key of any level, not a run, not a paired device.
fn require_control(scope: &crate::auth::Scope) -> Result<(), StatusCode> {
    if *scope == crate::auth::Scope::Control {
        Ok(())
    } else {
        tracing::warn!(
            ?scope,
            "refused a caller other than the control token on a control-only machine route"
        );
        Err(StatusCode::FORBIDDEN)
    }
}

#[derive(Serialize)]
struct MachineView {
    name: String,
    role: crate::config::MachineRole,
}

impl From<crate::config::MachineConfig> for MachineView {
    fn from(machine: crate::config::MachineConfig) -> Self {
        Self {
            name: machine.name(),
            role: machine.role,
        }
    }
}

#[derive(Deserialize)]
struct PutMachine {
    name: Option<String>,
    role: Option<crate::config::MachineRole>,
}

#[derive(Serialize)]
struct PutMachineView {
    #[serde(flatten)]
    machine: MachineView,
    restart_required: bool,
}

/// `GET /machine`: the local machine's display name and role.
async fn get_machine(
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
) -> Result<Json<MachineView>, StatusCode> {
    require_control(&scope)?;
    Ok(Json(
        (*crate::settings::get::<crate::config::MachineConfig>(&crate::settings::Scope::Machine))
            .clone()
            .into(),
    ))
}

/// `PUT /machine { name?, role? }`: update the local identity or role.
///
/// Sidecar and scheduler supervisors are selected once during startup, so changing the role is
/// durable immediately but takes effect after the daemon restarts.
async fn put_machine(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    Json(update): Json<PutMachine>,
) -> Result<Json<PutMachineView>, StatusCode> {
    require_control(&scope)?;
    let name = update.name;
    let role = update.role;
    let role_changed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let changed = std::sync::Arc::clone(&role_changed);
    crate::settings::update::<crate::config::MachineConfig, _>(
        &state.pool,
        &crate::settings::Scope::Machine,
        "control",
        move |machine| {
            if let Some(name) = name {
                machine.name = (!name.trim().is_empty()).then_some(name);
            }
            if let Some(role) = role {
                changed.store(machine.role != role, std::sync::atomic::Ordering::Relaxed);
                machine.role = role;
            }
        },
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "could not store the machine role");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let machine =
        (*crate::settings::get::<crate::config::MachineConfig>(&crate::settings::Scope::Machine))
            .clone()
            .into();
    Ok(Json(PutMachineView {
        machine,
        restart_required: role_changed.load(std::sync::atomic::Ordering::Relaxed),
    }))
}

#[derive(Serialize)]
struct AutostartView {
    mode: crate::autostart::Mode,
    #[serde(flatten)]
    capability: crate::autostart::Capability,
}

async fn autostart_view() -> Result<Json<AutostartView>, StatusCode> {
    tokio::task::spawn_blocking(|| AutostartView {
        mode: crate::autostart::current_mode(),
        capability: crate::autostart::capability(),
    })
    .await
    .map(Json)
    .map_err(|error| {
        tracing::warn!(%error, "could not read the autostart mode");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// `GET /system/autostart`: when the daemon starts on its own, and what this platform can do.
async fn get_system_autostart(
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
) -> Result<Json<AutostartView>, StatusCode> {
    require_control(&scope)?;
    autostart_view().await
}

/// No `Debug`: the password must not be printable by accident.
#[derive(Deserialize)]
struct PutAutostartBody {
    mode: crate::autostart::Mode,
    /// The account's password, needed once to start at boot on Windows. Passed to the system tool
    /// and dropped: never stored, never logged.
    password: Option<String>,
}

/// `PUT /system/autostart { mode, password? }`.
///
/// A refusal names what happened in `refusal`; a system tool's own complaint (Windows rejecting an
/// account with no usable password, for one) comes back in `detail`, already scrubbed of the
/// password.
async fn put_system_autostart(
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    Json(body): Json<PutAutostartBody>,
) -> Result<Json<AutostartView>, (StatusCode, Json<serde_json::Value>)> {
    use crate::autostart::SetModeError;
    require_control(&scope).map_err(|status| (status, Json(serde_json::json!({}))))?;
    let exe_path = std::env::current_exe().map_err(|error| {
        tracing::warn!(%error, "could not resolve the executable for the autostart entry");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "refusal": "internal" })),
        )
    })?;
    let PutAutostartBody { mode, password } = body;
    let outcome = tokio::task::spawn_blocking(move || {
        crate::autostart::set_mode(mode, password.as_deref(), &exe_path)
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "the autostart change did not finish");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "refusal": "internal" })),
        )
    })?;
    match outcome {
        Ok(()) => autostart_view()
            .await
            .map_err(|status| (status, Json(serde_json::json!({ "refusal": "internal" })))),
        Err(SetModeError::PasswordRequired) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "refusal": "password_required" })),
        )),
        Err(error @ SetModeError::Unsupported(_)) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "refusal": "unsupported", "detail": error.to_string() })),
        )),
        Err(error @ SetModeError::Failed(_)) => {
            tracing::warn!(%error, "the system refused the autostart change");
            Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(
                    serde_json::json!({ "refusal": "system_refused", "detail": error.to_string() }),
                ),
            ))
        }
    }
}

#[cfg(test)]
mod autostart_tests {
    use super::*;
    use crate::auth::{ApiTokenLevel, Scope};

    /// A paired device is graded as an API key, and the route is also refused to its segment; this
    /// is the second lock, the one that stays shut if a later device level reaches `system`.
    #[tokio::test]
    async fn a_remote_device_cannot_change_autostart() {
        for scope in [
            Scope::ApiToken(ApiTokenLevel::Admin),
            Scope::ApiToken(ApiTokenLevel::RunCreating),
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::Run(7),
        ] {
            assert_eq!(
                require_control(&scope),
                Err(StatusCode::FORBIDDEN),
                "{scope:?}"
            );
            let read = get_system_autostart(axum::Extension(scope.clone())).await;
            assert_eq!(read.err(), Some(StatusCode::FORBIDDEN), "{scope:?}");
            let write = put_system_autostart(
                axum::Extension(scope.clone()),
                Json(PutAutostartBody {
                    mode: crate::autostart::Mode::AtBoot,
                    password: Some("irrelevant".to_owned()),
                }),
            )
            .await;
            assert_eq!(
                write.err().map(|e| e.0),
                Some(StatusCode::FORBIDDEN),
                "{scope:?}"
            );
        }
        assert_eq!(require_control(&Scope::Control), Ok(()));
    }
}

#[derive(Deserialize)]
struct SystemShutdownQuery {
    /// How long to wait for the live runs, in seconds. Absent means the default; capped.
    timeout_secs: Option<u64>,
    /// The run asking for the restart (`NUCLEOS_RUN_ID`). Its chat, not just the run, is left out
    /// of the wait: a kept process carries the first turn's id into later turns.
    requester_run: Option<i64>,
}

/// `POST /system/shutdown`: stop the daemon once no run is live, without ever interrupting one.
///
/// Answers 202 at once with the readiness it saw; the wait happens in a task. New work is refused
/// from this moment on (`shutdown.rs`). When the wait ends with no run live the daemon leaves by
/// the same graceful path as `/daemon/shutdown`, exit code 0, which on Windows is also what tells
/// the `--supervise` loop not to relaunch it; see `shutdown.rs`. When it times out the daemon
/// carries on and `GET /system/update-readiness` shows `draining: false` and the runs still live.
/// A second request while one is draining changes nothing.
async fn post_system_shutdown(
    State(state): State<AppState>,
    Query(query): Query<SystemShutdownQuery>,
) -> Result<(StatusCode, Json<crate::shutdown::Readiness>), StatusCode> {
    let timeout = query
        .timeout_secs
        .map(std::time::Duration::from_secs)
        .unwrap_or(crate::shutdown::DEFAULT_DRAIN_TIMEOUT)
        .min(crate::shutdown::MAX_DRAIN_TIMEOUT);
    let requester_run = query.requester_run;

    if crate::shutdown::begin_draining() {
        tracing::warn!(
            timeout_secs = timeout.as_secs(),
            "draining the daemon for an update"
        );
        let pool = state.pool.clone();
        tokio::spawn(async move {
            match crate::shutdown::drain_until_idle(
                &pool,
                requester_run,
                timeout,
                crate::shutdown::DRAIN_POLL,
                request_shutdown,
                &crate::hooks::run_is_parked,
            )
            .await
            {
                Ok(crate::shutdown::DrainOutcome::Stopped) => {
                    tracing::warn!("no run blocks the restart: stopping the daemon for the update");
                }
                Ok(crate::shutdown::DrainOutcome::TimedOut {
                    live_runs,
                    waiting_on,
                }) => {
                    tracing::warn!(
                        live_runs,
                        ?waiting_on,
                        "the update drain timed out; the daemon carries on"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "the update drain could not count the live runs; the daemon carries on"
                    );
                }
            }
        });
    }

    crate::shutdown::readiness_for(&state.pool, requester_run, &crate::hooks::run_is_parked)
        .await
        .map(|readiness| (StatusCode::ACCEPTED, Json(readiness)))
        .map_err(|error| {
            tracing::warn!(%error, "could not read the readiness after starting the drain");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}
