mod assistant;
mod attention;
mod auth;
mod autopilot;
mod autostart;
mod backup;
mod budget;
mod calendar;
mod chats;
mod classifier;
mod collision;
mod concurrency;
mod config;
mod contacts;
mod daemon_client;
mod email;
mod feed;
mod files;
mod gate;
mod git_exec;
mod handoff;
mod health;
mod hooks;
mod http;
mod inspect;
mod job;
mod local_agent;
mod logging;
mod mailsend;
mod mcp_tools;
mod notify;
mod pii_shadow;
mod presets;
mod priority;
mod proposals;
mod recurrence;
mod redact;
mod repo_trigger;
mod runner;
mod runs;
mod scheduler;
mod search;
mod secrets;
mod shadow;
mod sidecar;
mod state;
mod storage;
mod transcribe;
mod triage;
mod trust;
mod vcs;
mod voice;
mod web;
mod web_client;
mod webhook;
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
/// The web search provider's API key, in Credential Manager like every other secret — no key on
/// disk, and in particular not in `.ai/web.yaml`, which is a versioned file.
const WEB_SEARCH_KEY: &str = "web-search-api-key";

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

    // After the run reconciliations, and for a reason worth stating: they mark every run left
    // `running` as `interrupted`, so by now no job has a live node under it — which means "has no
    // live run" is true of every job, including the ones that died in the gap between two nodes.
    // Those are exactly the recoverable ones, so the pass below discriminates on HEAD instead.
    match job::reconcile_orphaned_jobs(&pool).await {
        Ok(retired) if retired > 0 => {
            tracing::warn!(
                "retired {retired} job(s) whose repository moved while the daemon was down -> 'interrupted'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "orphaned-job reconciliation failed"),
    }

    // After both owner reconciliations above, and that order is the whole correctness of this pass:
    // it frees a slot by asking whether its owner is still live, and before those two every dead
    // owner still reads live. Run earlier it would free nothing at all.
    match concurrency::reconcile_orphaned_slots(&pool).await {
        Ok(freed) if freed > 0 => {
            tracing::warn!("freed {freed} concurrency slot(s) left held by a previous crash");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "orphaned concurrency slot sweep failed"),
    }

    // Neither fatal like the run reconciliations above nor mere hygiene like the worktree sweep
    // below: louder than the sweep, quieter than the panics.
    //
    // A `vcs_requests` row left `running` holds its repository's only slot — the partial unique
    // index sees to that — so failing to clear it means no git operation for that project until
    // somebody notices. That is a jam, not untidiness, hence `error!`. But it is one pillar's queue:
    // refusing to boot mail, voice, calendar and runs over it would trade a stuck repository for a
    // stuck machine.
    match vcs::reconcile_interrupted(&pool).await {
        Ok(released) if released > 0 => {
            tracing::warn!(
                "reconciled {released} vcs request(s) left 'running' by a previous crash -> 'interrupted'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::error!(
            %error,
            "vcs request reconciliation failed — a repository may stay queue-locked, and nothing retries before the next startup"
        ),
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

    // The folder a person arranges their files in — uploads of their own, and the mail they filed.
    // Created whether or not the email pillar is enabled, for the same reason as the sandbox: a
    // directory that always exists is one less thing to go wrong the day email is switched on, and
    // this one is now reachable from its own tab with the pillar off. An empty path means every
    // route under it refuses, which is the right answer when the directory could not be made.
    let files_root = match files::ensure_root(dirs.data_local_dir()) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(%error, "could not create the files folder — the Files tab will be unavailable");
            std::path::PathBuf::new()
        }
    };

    // Relative to the working directory like the email pillar's, for the same reason: it matters where
    // the daemon was launched from, so the "off" path has to be discoverable rather than mysterious.
    let voice_config = config::load_voice_config(std::path::Path::new(".ai/voice.yaml"));
    let calendar_config = config::load_calendar_config(std::path::Path::new(".ai/calendar.yaml"));
    let web_config = config::load_web_config(std::path::Path::new(".ai/web.yaml"));
    // The web sidecar's own shared secret, minted per boot and never persisted.
    //
    // NOT the control token, and not for the reason the email sidecar has its own: this traffic
    // only ever flows daemon → sidecar, so the sidecar has nothing to authenticate itself FOR. It
    // needs a secret solely to refuse anything else on the machine that can open a socket. A
    // per-boot random value is therefore strictly better than a long-lived one — there is nothing
    // to leak and nothing to rotate.
    let web_sidecar_token = auth::generate_token();
    // Cleanup is armed SEPARATELY from transcription, and a failed probe costs only the tidying up.
    //
    // That asymmetry is deliberate. Local triage refuses to run at all when its probe fails, because
    // there the alternative is sending mail bodies off-machine. Here the fallback is a raw transcript
    // that never leaves the laptop, so the same failure should cost the polish and not the dictation —
    // exactly what `voice::clean_up` does at runtime, decided once here at startup.
    let voice_cleanup_model = match models_config.voice_cleanup_model.clone() {
        Some(model) if voice_config.armed() => {
            let probe = match reqwest::Client::new()
                .post(format!("{}/api/show", runner::OLLAMA_BASE_URL))
                .json(&serde_json::json!({ "model": &model }))
                .send()
                .await
            {
                Ok(response) => match response.text().await {
                    Ok(body) => runner::interpret_context_probe(&body, voice::CLEANUP_NUM_CTX),
                    Err(error) => Err(runner::ModelError::UnparseableResponse(format!(
                        "could not read the voice cleanup probe response: {error}"
                    ))),
                },
                Err(error) => Err(runner::ModelError::UnparseableResponse(format!(
                    "could not reach the loopback Ollama endpoint: {error}"
                ))),
            };
            // Reusing the triage-named decision function on purpose: it is a pure mapping from a
            // context-probe result to an operator-readable reason, and every message it produces talks
            // about "the local model" rather than about mail. A second copy would drift from this one.
            match runner::local_triage_decision(probe) {
                runner::LocalTriageDecision::Enabled => {
                    tracing::info!(%model, "voice cleanup model enabled");
                    Some(model)
                }
                runner::LocalTriageDecision::Disabled(reason) => {
                    tracing::warn!(%model, %reason, "voice cleanup disabled; transcripts will be delivered raw");
                    None
                }
            }
        }
        _ => None,
    };
    if voice_config.armed() {
        tracing::info!("voice pillar armed");
    }

    // Which agent CLI answers a run. Ship-dark like the local triage model above: an absent or
    // unrecognised name keeps the proven Claude path, so the second runner is reachable only once an
    // operator has asked for it by name. An unrecognised name warns rather than fails startup, for
    // the same reason a bad local-model probe does — a typo in one config key must not take down
    // every unrelated daemon service.
    let claude_runner = || runner::ClaudeCliRunner {
        model: models_config.claude_model.clone(),
        plan_model: models_config.plan_model.clone(),
        review_model: models_config.review_model.clone(),
    };
    let configured_runner = models_config.primary_runner.as_deref();
    let primary_runner: Arc<dyn runner::CommandRunner> = match configured_runner {
        Some("codex") => {
            tracing::info!(model = %models_config.codex_model, "codex CLI selected as the run runner");
            Arc::new(runner::CodexCliRunner {
                model: models_config.codex_model.clone(),
            })
        }
        Some(other) => {
            tracing::warn!(%other, "unknown primary_runner — keeping the Claude CLI");
            Arc::new(claude_runner())
        }
        None => Arc::new(claude_runner()),
    };

    // The email sidecar's own key, minted before `AppState` exists rather than beside the spawn.
    //
    // It moved here because two things now need it: the sidecar, which is handed it in its
    // environment, and `POST /email/send`, which must present the SIDECAR's key to the sidecar and
    // never the control token. Minting it twice would produce two keys and rotate the live one out
    // from under a running process (`INSERT OR REPLACE`), so it is minted once and read from
    // `state.email` by both. Gated on `enabled` so a daemon with the pillar off writes no key it
    // will never use, and a failure here is not fatal: it leaves the sidecar unstarted and the send
    // route answering 503, which is the same "off rather than half-on" posture as the rest.
    let email_sidecar_token = if email_config.enabled {
        match auth::mint_service_token(&pool, auth::Service::Email).await {
            Ok(token) => Some(token),
            Err(error) => {
                tracing::error!(
                    %error,
                    "could not mint the email sidecar's token — the sidecar will not start"
                );
                None
            }
        }
    } else {
        None
    };

    // The model that answers a chat turn asking to be answered on this machine.
    //
    // Probed exactly like local triage and voice cleanup, against this feature's own window: a turn
    // accumulates its tool schemas and every result on each round, so it needs more room than a
    // single triage prompt and the probe has to say so or Ollama silently truncates the middle of a
    // conversation.
    //
    // A failed probe falls back rather than disabling, which is the opposite of local triage and for
    // a reason worth stating: triage refuses because the alternative is mail bodies leaving the
    // machine, while this turn reads only the daemon's own state, so the fallback is what already
    // happens today. Warning and carrying on is right here and would be wrong there.
    let local_assistant = match models_config.local_assistant_model.clone() {
        Some(model) => {
            let probe = match reqwest::Client::new()
                .post(format!("{}/api/show", runner::OLLAMA_BASE_URL))
                .json(&serde_json::json!({ "model": &model }))
                .send()
                .await
            {
                Ok(response) => match response.text().await {
                    Ok(body) => runner::interpret_context_probe(&body, local_agent::TURN_NUM_CTX),
                    Err(error) => Err(runner::ModelError::UnparseableResponse(format!(
                        "could not read the local assistant probe response: {error}"
                    ))),
                },
                Err(error) => Err(runner::ModelError::UnparseableResponse(format!(
                    "could not reach the loopback Ollama endpoint: {error}"
                ))),
            };
            match runner::local_triage_decision(probe) {
                runner::LocalTriageDecision::Enabled => {
                    tracing::info!(%model, "local assistant enabled for chat turns that ask for it");
                    Some(Arc::new(local_agent::LocalAssistant::new(
                        Box::new(runner::OllamaChat::new(
                            runner::OLLAMA_BASE_URL.to_string(),
                            model,
                        )),
                        // Loopback to this same daemon, holding the control token it just loaded.
                        // The tools are the ones the MCP subprocess exposes, through the same
                        // handlers, so a local turn and a cloud turn cannot disagree about what a
                        // tool does — only about which ones they are offered.
                        Box::new(mcp_tools::LocalToolBox::new(
                            "http://127.0.0.1:8791".to_string(),
                            token_value.clone(),
                            pool.clone(),
                        )),
                    )))
                }
                runner::LocalTriageDecision::Disabled(reason) => {
                    tracing::warn!(%model, %reason, "local assistant disabled; chat turns stay on the CLI");
                    None
                }
            }
        }
        None => None,
    };

    let state = AppState {
        token: Token(token_value),
        pool,
        runner: primary_runner,
        triage_runner,
        local_triage_disabled,
        local_assistant,
        email: Arc::new(state::EmailRuntime::from_config(
            &email_config,
            triage_sandbox,
            files_root,
            email_sidecar_token,
        )),
        voice: Arc::new(voice::VoiceRuntime::from_config(
            &voice_config,
            voice_cleanup_model,
        )),
        calendar: Arc::new(calendar::CalendarRuntime::from_config(&calendar_config)),
        web: Arc::new(web::WebRuntime {
            enabled: web_config.enabled,
            trusted_hosts: web_config.trusted_hosts.clone(),
            retain_pages_days: web_config.retain_pages_days,
            client: web_client::WebClient::new(sidecar::WEB_ADDR, web_sidecar_token.clone()),
            // The local model that reads quarantined pages. It is the same one the voice pillar
            // probed at startup — one local model, one place it is pinned — but the consequence of
            // its absence is the opposite: voice degrades to a raw transcript, and this one
            // REFUSES. A typing aid may fail soft; a barrier may not.
            quarantine_model: models_config.voice_cleanup_model.clone(),
            ollama_base_url: runner::OLLAMA_BASE_URL.to_string(),
            http: reqwest::Client::new(),
        }),
        run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
    tokio::spawn(sidecar::supervise(
        sidecar::ECHO.to_string(),
        sidecar_path,
        vec![],
    ));

    // The web sidecar. Started only when the pillar is on: an unstarted one means `/web/*` answers
    // 502, which is the honest reading of "there is nothing to ask".
    if web_config.enabled {
        // The key is optional and its absence is not fatal — `/fetch` works without a search
        // provider, so an installation with no API key can still be handed a URL to read.
        let search_key = match secrets::load_secret(WEB_SEARCH_KEY) {
            Ok(Some(key)) => key,
            Ok(None) => {
                tracing::info!("no web search key stored; the sidecar will serve reads only");
                String::new()
            }
            Err(error) => {
                tracing::warn!(%error, "could not read the web search key; serving reads only");
                String::new()
            }
        };
        let path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("web-sidecar.exe");
        let env = sidecar::web_env(
            "http://127.0.0.1:8791",
            &web_sidecar_token,
            &web_config,
            &search_key,
        );
        tokio::spawn(sidecar::supervise(sidecar::WEB.to_string(), path, env));
        tracing::info!(provider = %web_config.provider, "web sidecar supervised");

        // Retention. Hourly rather than on a timer tied to reads: a cache that is never read again
        // must still empty, or "30 days" means "30 days after the last time anyone looked".
        let retention_state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                match web::prune(
                    &retention_state.pool,
                    retention_state.web.retain_pages_days,
                    chrono::Utc::now(),
                )
                .await
                {
                    Ok(0) => {}
                    Ok(pruned) => tracing::info!(pruned, "web: pages past the retention window"),
                    // Best-effort, like the activity feed: a failed sweep is a full disk later, not
                    // a reason to take the daemon down now.
                    Err(error) => tracing::warn!(%error, "web: retention sweep failed"),
                }
            }
        });
    }
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
                sidecar::TELEGRAM.to_string(),
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
    // Its own loop, not a step inside the scheduler's: a job pass can sit inside `run_gate` for the
    // whole gate timeout, and sharing a loop would stall every scheduled rule in the daemon behind
    // one project's test suite.
    tokio::spawn(job::run_job_loop(state.clone()));
    tokio::spawn(repo_trigger::run_repo_poller(state.clone()));
    tokio::spawn(worktree::run_gc(state.pool.clone()));
    // The worktree GC's counterpart inside the database. It collects the directories a finished run
    // leaves on disk; this expires the transcript it leaves in `runs`, the events it leaves in
    // `run_events`, and eventually its line in the feed. Nothing removed any of those, so all three
    // grew for as long as the daemon was ever used — and a transcript is stored twice and indexed a
    // third time, so they grew at three times the obvious rate.
    tokio::spawn(runs::run_retention_loop(state.clone()));
    tokio::spawn(vcs::run_queue_worker(
        state.pool.clone(),
        std::sync::Arc::new(git_exec::GitExecutor::default()),
    ));
    // Only when a local model is already configured, and reusing the triage one rather than adding
    // a key: this reads mail-derived text, which is the text that model was chosen for, and
    // `web.rs` sets the precedent of one local model pinned in one place serving more than one
    // reader. Without it the sweep simply never runs, and the observation table stays empty —
    // which is the correct behaviour for a measurement nobody asked for.
    if let Some(model) = models_config.local_triage_model.clone() {
        tokio::spawn(pii_shadow::run_sweep_loop(
            state.pool.clone(),
            runner::OLLAMA_BASE_URL.to_string(),
            model,
        ));
    }

    // Spawned whether or not the pillar is enabled: the loop also owns retention, and bodies
    // already stored do not stop needing to expire because polling was switched off.
    tokio::spawn(triage::run_triage_loop(state.clone()));

    // Unconditional for exactly the reason above, applied to dictations. Gating this on the pillar
    // being armed would FREEZE the transcript history at the moment somebody switched voice off, which
    // is the opposite of what switching it off is for — the recordings of what they said would then
    // outlive the feature that made them.
    tokio::spawn(voice::run_retention_loop(state.clone()));
    // Held notifications are reconciled at startup for the same reason orphaned runs and stranded
    // approvals are: the daemon may have been down when the meeting ended, and a queue that only
    // drains on the tick would sit there until the NEXT meeting ended instead.
    match notify::flush_due(&state.pool).await {
        Ok(delivered) if delivered > 0 => {
            tracing::warn!("delivered {delivered} notification(s) held over from a previous run")
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not flush held notifications on startup"),
    }
    tokio::spawn(notify::run_flush_loop(state.clone()));

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
                    // Its own key, minted above before `AppState` was built and read back from it
                    // here so the sidecar and the send route present the same one. An absent key
                    // leaves the sidecar unstarted rather than started with the control token: this
                    // process parses MIME written by strangers, and the fallback that hands it
                    // everything is the arrangement being removed.
                    match state.email.sidecar_token.as_deref() {
                        Some(token) => {
                            let path = std::env::current_exe()
                                .unwrap()
                                .parent()
                                .unwrap()
                                .join("email-sidecar.exe");
                            let env = sidecar::email_env(
                                "http://127.0.0.1:8791",
                                token,
                                &email_config,
                                &password,
                            );
                            tokio::spawn(sidecar::supervise(sidecar::EMAIL.to_string(), path, env));
                            tracing::info!("email sidecar supervised");
                        }
                        None => tracing::error!(
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
