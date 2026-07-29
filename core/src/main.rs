mod assistant;
mod auth;
mod autopilot;
mod autostart;
mod backup;
mod budget;
mod classifier;
mod config;
mod daemon_client;
mod email;
mod feed;
mod gate;
mod health;
mod hooks;
mod http;
mod inspect;
mod logging;
mod mailfiles;
mod mcp_tools;
mod presets;
mod proposals;
mod redact;
mod repo_trigger;
mod runner;
mod runs;
mod scheduler;
mod secrets;
mod shadow;
mod sidecar;
mod state;
mod storage;
mod triage;
mod wip;
mod worktree;

use auth::Token;
use state::AppState;
use std::sync::Arc;

const TOKEN_KEY: &str = "daemon-token";
const TELEGRAM_TOKEN_KEY: &str = "telegram-token";
/// The mailbox password (spec §3.4). An app password, in Credential Manager rather than in
/// `.ai/email.yaml`, so the one secret the pillar needs never sits in a file next to the config.
const EMAIL_PASSWORD_KEY: &str = "email-imap-password";

/// Reads a secret from stdin rather than from `argv`.
///
/// A Windows command line is readable by any process running as the same user
/// (`Get-CimInstance Win32_Process | select CommandLine`) and is recorded verbatim in PSReadLine's
/// plaintext history file. Passing an IMAP app password or a bot token as an argument therefore put
/// it on disk in cleartext at the exact moment the operator was securely storing it, which is the
/// one thing this path exists to avoid.
fn read_secret_from_stdin(prompt: &str) -> Option<String> {
    use std::io::BufRead;

    eprintln!("{prompt}");
    let mut value = String::new();
    if std::io::stdin().lock().read_line(&mut value).is_err() {
        return None;
    }
    // The line terminator only. A secret may legitimately end in a space, and silently eating one
    // would store a credential that differs from what was pasted — a failure that surfaces much
    // later as an authentication error nobody connects back to this prompt.
    let value = value
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_owned();
    if value.is_empty() { None } else { Some(value) }
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|a| a == "--print-token") {
        match secrets::load_secret(TOKEN_KEY) {
            Ok(Some(t)) => println!("{t}"),
            Ok(None) => {
                eprintln!("no daemon token stored yet — start the daemon once to generate one");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("failed to read token from Credential Manager: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|a| a == "--set-telegram-token") {
        match read_secret_from_stdin("paste the bot token, then press Enter:") {
            Some(value) => match secrets::store_secret(TELEGRAM_TOKEN_KEY, &value) {
                Ok(()) => println!("telegram bot token stored in Credential Manager"),
                Err(e) => {
                    eprintln!("failed to store telegram token: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("no bot token was read from stdin");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|a| a == "--set-email-password") {
        match read_secret_from_stdin("paste the app password, then press Enter:") {
            Some(value) => match secrets::store_secret(EMAIL_PASSWORD_KEY, &value) {
                Ok(()) => println!("email password stored in Credential Manager"),
                Err(e) => {
                    eprintln!("failed to store email password: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("no app password was read from stdin");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|a| a == "--mcp-tools") {
        if let Err(e) = mcp_tools::run_stdio().await {
            eprintln!("mcp-tools failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    let dirs = directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
        .expect("could not resolve local app data directory");

    let log_dir = dirs.data_local_dir().join("logs");
    let _log_guard = logging::init(&log_dir);

    match std::env::current_exe() {
        Ok(exe_path) => {
            if let Err(e) = autostart::ensure_registered(&exe_path) {
                tracing::warn!("failed to self-register Windows autostart task: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("failed to resolve current exe path for autostart registration: {e}")
        }
    }

    let db_path = dirs.data_local_dir().join("nucleos.db");
    match backup::apply_pending_restore(&db_path).await {
        Ok(Some(applied)) => tracing::warn!(
            "applied pending database restore from {}; safety backup at {}",
            applied.restored_from,
            applied.safety_backup.display()
        ),
        Ok(None) => tracing::info!("no pending database restore to apply"),
        Err(e) => tracing::error!(
            "failed to apply pending database restore; continuing startup and retrying next start: {e}"
        ),
    }
    let pool = storage::open(&db_path)
        .await
        .expect("failed to open local database");
    tracing::info!("nucleos-core database ready at {}", db_path.display());

    let interrupted = runs::reconcile_orphaned_runs(&pool)
        .await
        .expect("failed to reconcile orphaned runs on startup");
    if interrupted > 0 {
        tracing::warn!(
            "reconciled {interrupted} run(s) left 'running' by a previous crash -> 'interrupted'"
        );
    }

    let stranded = runs::reconcile_stranded_approvals(&pool)
        .await
        .expect("failed to reconcile stranded approval pauses on startup");
    if stranded > 0 {
        tracing::warn!(
            "reconciled {stranded} run(s) left 'awaiting_approval' with no pending proposal -> 'interrupted'"
        );
    }

    // After the run reconciliations above, so nothing from a previous life still counts as live.
    match worktree::reconcile_orphaned_worktrees(
        &pool,
        worktree::ORPHAN_MIN_AGE,
        worktree::GC_BACKOFF,
    )
    .await
    {
        Ok(collected) if collected > 0 => {
            tracing::warn!("collected {collected} orphaned worktree director(ies) on startup");
        }
        Ok(_) => {}
        // Hygiene, not a prerequisite: a daemon that cannot tidy up must still start.
        Err(error) => tracing::warn!(%error, "orphaned-worktree reconciliation failed"),
    }

    let token_value =
        match secrets::load_secret(TOKEN_KEY).expect("failed to read Credential Manager") {
            Some(existing) => existing,
            None => {
                let fresh = auth::generate_token();
                secrets::store_secret(TOKEN_KEY, &fresh).expect("failed to persist daemon token");
                fresh
            }
        };
    tracing::info!("nucleos-core token loaded from Credential Manager");

    let models_config_path = std::path::PathBuf::from(".ai/nucleos-models.yaml");
    let models_config = config::load_models_config(&models_config_path).unwrap_or_else(|e| {
        tracing::warn!("failed to parse .ai/nucleos-models.yaml ({e}), using defaults");
        config::ModelsConfig::default()
    });
    let (triage_runner, local_triage_disabled): (
        Option<Arc<dyn runner::CommandRunner>>,
        Option<String>,
    ) = if let Some(model) = models_config.local_triage_model.clone() {
        let local_runner =
            runner::OllamaRunner::new(runner::OLLAMA_BASE_URL.to_string(), model.clone());
        let probe = match reqwest::Client::new()
            .post(format!("{}/api/show", runner::OLLAMA_BASE_URL))
            .json(&serde_json::json!({ "model": &model }))
            .send()
            .await
        {
            Ok(response) => match response.text().await {
                Ok(body) => runner::interpret_context_probe(&body, triage::LOCAL_NUM_CTX),
                Err(error) => Err(runner::ModelError::UnparseableResponse(format!(
                    "could not read the local model probe response: {error}"
                ))),
            },
            Err(error) => Err(runner::ModelError::UnparseableResponse(format!(
                "could not reach the loopback Ollama endpoint: {error}"
            ))),
        };
        match runner::local_triage_decision(probe) {
            runner::LocalTriageDecision::Enabled => {
                tracing::info!(%model, "local triage model enabled");
                (Some(Arc::new(local_runner)), None)
            }
            runner::LocalTriageDecision::Disabled(reason) => {
                // Local triage is optional at startup: a bad probe must not take down unrelated
                // daemon services, just as a failed autostart registration does not.
                tracing::warn!(%model, %reason, "local triage model disabled");
                (None, Some(reason))
            }
        }
    } else {
        (None, None)
    };

    // Relative to the working directory, so it matters where the daemon was launched from — which
    // is exactly why the "off" message below has to name the path it looked at.
    let email_config_path = std::path::Path::new(".ai/email.yaml");
    let email_config_found = email_config_path.exists();
    let email_config = config::load_email_config(email_config_path);
    // Built whether or not the pillar is enabled: it is two small files, and having it always in a
    // known state means enabling email later is a config edit rather than a fresh directory.
    let triage_sandbox = dirs.data_local_dir().join("triage-sandbox");
    if let Err(error) = triage::ensure_sandbox(&triage_sandbox) {
        tracing::warn!(%error, "could not build the triage sandbox — the email pillar will stay off");
    }

    // The folder a person arranges their mail into. Created whether or not the pillar is enabled,
    // for the same reason as the sandbox: a directory that always exists is one less thing to go
    // wrong the day email is switched on. An empty path means every route under it refuses, which
    // is the right answer when the directory could not be made.
    let mail_files_root = match mailfiles::ensure_root(dirs.data_local_dir()) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(%error, "could not create the mail folder — organising mail will be unavailable");
            std::path::PathBuf::new()
        }
    };

    let state = AppState {
        token: Token(token_value),
        pool,
        runner: Arc::new(runner::ClaudeCliRunner {
            model: models_config.claude_model.clone(),
        }),
        triage_runner,
        local_triage_disabled,
        email: Arc::new(state::EmailRuntime::from_config(
            &email_config,
            triage_sandbox,
            mail_files_root,
        )),
        run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        progress_timeout: state::DEFAULT_PROGRESS_TIMEOUT,
        run_timeout: state::DEFAULT_RUN_TIMEOUT,
    };

    let app = http::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8791")
        .await
        .unwrap();
    tracing::info!(
        "nucleos-core listening on {}",
        listener.local_addr().unwrap()
    );
    let sidecar_path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("echo-sidecar.exe");
    tokio::spawn(sidecar::supervise("echo".to_string(), sidecar_path, vec![]));
    match secrets::load_secret(TELEGRAM_TOKEN_KEY) {
        Ok(Some(bot_token)) => {
            let telegram_path = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join("telegram-sidecar.exe");
            let telegram_env =
                sidecar::telegram_env("http://127.0.0.1:8791", &state.token.0, &bot_token);
            tokio::spawn(sidecar::supervise(
                "telegram".to_string(),
                telegram_path,
                telegram_env,
            ));
            tracing::info!("telegram sidecar supervised");
        }
        Ok(None) => {
            tracing::info!(
                "no telegram-token stored — telegram sidecar not started (set with --set-telegram-token)"
            );
        }
        Err(e) => {
            tracing::warn!("failed to read telegram-token from Credential Manager: {e}");
        }
    }
    tokio::spawn(scheduler::run_scheduler(state.clone()));
    tokio::spawn(repo_trigger::run_repo_poller(state.clone()));
    tokio::spawn(worktree::run_gc(state.pool.clone()));

    // Spawned whether or not the pillar is enabled: the loop also owns retention, and bodies
    // already stored do not stop needing to expire because polling was switched off.
    tokio::spawn(triage::run_triage_loop(state.clone()));

    // The email pillar starts only after its hook barrier has been PROVEN, and the proof can only
    // be attempted once this listener is serving — the hook reaches the daemon over HTTP, and a
    // daemon that is not up yet produces `ask_daemon.py`'s fail-closed `block`, which looks like
    // success and proves nothing (spec §5.5). Hence a task that waits for `axum::serve` below
    // rather than a check inline here.
    if state.email.enabled {
        let state = state.clone();
        tokio::spawn(async move {
            match triage::verify_hook_barrier(
                &state.pool,
                &state.email.sandbox,
                "http://127.0.0.1:8791",
                &state.token.0,
            )
            .await
            {
                Ok(()) => {
                    state
                        .email
                        .armed
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!("email triage barrier verified — the pillar is armed");
                }
                // Off rather than unprotected. The pillar's whole premise is that untrusted content
                // never meets a tool, and an unproven barrier is not a barrier.
                Err(error) => {
                    tracing::error!(
                        %error,
                        "email triage barrier could not be verified — the pillar stays OFF"
                    );
                    return;
                }
            }
            // Supervision needs all three: enabled, a password, and a PROVEN barrier. Any one
            // missing leaves the sidecar unstarted and the mailbox untouched — the pillar is off
            // rather than half-on.
            match secrets::load_secret(EMAIL_PASSWORD_KEY) {
                Ok(Some(password)) => {
                    // Its own key, minted before the spawn. A failure here leaves the sidecar
                    // unstarted rather than started with the control token: this process parses
                    // MIME written by strangers, and the fallback that hands it everything is the
                    // arrangement being removed.
                    match auth::mint_service_token(&state.pool, auth::Service::Email).await {
                        Ok(token) => {
                            let path = std::env::current_exe()
                                .unwrap()
                                .parent()
                                .unwrap()
                                .join("email-sidecar.exe");
                            let env = sidecar::email_env(
                                "http://127.0.0.1:8791",
                                &token,
                                &email_config,
                                &password,
                            );
                            tokio::spawn(sidecar::supervise("email".to_string(), path, env));
                            tracing::info!("email sidecar supervised");
                        }
                        Err(error) => tracing::error!(
                            %error,
                            "could not mint the email sidecar's token — the sidecar will not start"
                        ),
                    }
                }
                Ok(None) => tracing::warn!(
                    "no email-imap-password stored — the email sidecar will not start (set with --set-email-password)"
                ),
                Err(error) => {
                    tracing::warn!(%error, "could not read the email password from Credential Manager")
                }
            }
        });
    } else if email_config_found {
        tracing::info!("email pillar disabled (.ai/email.yaml says enabled: false)");
    } else {
        // Not the same thing, and saying so cost a diagnosis: a daemon started from the wrong
        // directory reported the user's config as switched off while that file sat there reading
        // `enabled: true`. Absolute, because the whole point is which directory was searched.
        tracing::info!(
            path = %std::path::absolute(email_config_path)
                .unwrap_or_else(|_| email_config_path.to_path_buf())
                .display(),
            "email pillar off — no config file here (the path is relative to the working directory)"
        );
    }

    axum::serve(listener, app).await.unwrap();
}
