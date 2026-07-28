mod assistant;
mod auth;
mod autopilot;
mod autostart;
mod budget;
mod classifier;
mod config;
mod daemon_client;
mod email;
mod feed;
mod hooks;
mod http;
mod inspect;
mod logging;
mod mcp_tools;
mod proposals;
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

    if let Some(pos) = std::env::args().position(|a| a == "--set-telegram-token") {
        match std::env::args().nth(pos + 1) {
            Some(value) => match secrets::store_secret(TELEGRAM_TOKEN_KEY, &value) {
                Ok(()) => println!("telegram bot token stored in Credential Manager"),
                Err(e) => {
                    eprintln!("failed to store telegram token: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("usage: nucleos-core --set-telegram-token <BOT_TOKEN>");
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

    let email_config = config::load_email_config(std::path::Path::new(".ai/email.yaml"));
    // Built whether or not the pillar is enabled: it is two small files, and having it always in a
    // known state means enabling email later is a config edit rather than a fresh directory.
    let triage_sandbox = dirs.data_local_dir().join("triage-sandbox");
    if let Err(error) = triage::ensure_sandbox(&triage_sandbox) {
        tracing::warn!(%error, "could not build the triage sandbox — the email pillar will stay off");
    }

    let state = AppState {
        token: Token(token_value),
        pool,
        runner: Arc::new(runner::ClaudeCliRunner {
            model: models_config.claude_model.clone(),
        }),
        email: Arc::new(state::EmailRuntime::from_config(
            &email_config,
            triage_sandbox,
        )),
        run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
            // Only reached when the barrier is proven. The loop is its own task beside the
            // scheduler, which is the precedent that takes `AppState`.
            triage::run_triage_loop(state).await;
        });
    } else {
        tracing::info!("email pillar disabled (.ai/email.yaml: enabled: false)");
    }

    axum::serve(listener, app).await.unwrap();
}
