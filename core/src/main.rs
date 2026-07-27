mod assistant;
mod auth;
mod autopilot;
mod autostart;
mod budget;
mod classifier;
mod config;
mod daemon_client;
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

    let state = AppState {
        token: Token(token_value),
        pool,
        runner: Arc::new(runner::ClaudeCliRunner {
            model: models_config.claude_model.clone(),
        }),
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
    axum::serve(listener, app).await.unwrap();
}
