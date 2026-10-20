mod cli;
mod http;
mod startup;

use nucleos_core::*;

use auth::Token;
use state::AppState;
use std::sync::Arc;

// The argument helpers moved to `cli.rs` with the subcommands; only the tests below still name them.
#[cfg(test)]
use cli::*;
use startup::*;

const TOKEN_KEY: &str = "daemon-token";
const TELEGRAM_TOKEN_KEY: &str = "telegram-token";
/// The mailbox password (spec §3.4). An app password, in Credential Manager rather than in
/// `~/.nucleos/email.yaml`, so the one secret the pillar needs never sits in a file next to the config.
const EMAIL_PASSWORD_KEY: &str = "email-imap-password";
/// The web search provider's API key, in Credential Manager like every other secret — no key on
/// disk, and in particular not in `~/.nucleos/web.yaml`, which is plain text anybody may open.
const WEB_SEARCH_KEY: &str = "web-search-api-key";
/// OpenRouter's own API key, in Credential Manager for the same reason every secret above is: it
/// never sits in `~/.nucleos/nucleos-models.yaml`, which only ever names the model
/// (`hosted_assistant_model`) and is plain text anybody may open.
/// `models::openai_compatible::OpenAiCompatibleChat::new` refuses outright when this comes back `None` —
/// see its own doc comment for why that refusal happens before any request leaves the machine
/// rather than after a 401 comes back.
const OPENROUTER_KEY: &str = "openrouter-api-key";

#[tokio::main]
async fn main() {
    http::remember_home();
    if cli::run_subcommand().await {
        return;
    }

    let dirs = directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
        .expect("could not resolve local app data directory");

    // Whether this process is the machine's daemon or a second one somebody is testing with, and
    // where its database and logs go. Resolved HERE, before the first thing that acts on either,
    // because everything below reads the answer and nothing below may reach for the environment
    // again — two readers of one variable is how a daemon ends up binding one port and telling its
    // own tools to call back on another.
    //
    // The reason this exists at all: the port and the data directory were literals, so exercising
    // a change end to end meant running the new build against the live database, on the live port,
    // after stopping whatever was already serving. That is the one experiment nobody can undo.
    let port_override = std::env::var(daemon_client::PORT_VAR).ok();
    let data_dir_override = std::env::var(daemon_client::DATA_DIR_VAR).ok();
    let is_primary =
        daemon_client::is_primary(port_override.as_deref(), data_dir_override.as_deref());
    let data_dir = match data_dir_override.as_deref().filter(|dir| !dir.is_empty()) {
        Some(dir) => std::path::PathBuf::from(dir),
        None => dirs.data_local_dir().to_path_buf(),
    };
    if !is_primary {
        eprintln!(
            "nucleos-core: secondary instance — data in {}, no autostart, no sidecars",
            data_dir.display()
        );
    }

    let log_dir = data_dir.join("logs");
    let _log_guard = logging::init(&log_dir);

    // Only the machine's daemon claims the logon entry. `ensure_registered` overwrites it (on
    // Windows with `schtasks /F`), so a secondary doing this would point the autostart at whatever
    // build is under test — typically one inside a worktree that is about to be deleted, leaving a
    // task that runs nothing.
    if is_primary {
        match std::env::current_exe() {
            Ok(exe_path) => {
                if let Err(e) = autostart::ensure_registered(&exe_path) {
                    tracing::warn!("failed to register the daemon's autostart entry: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("failed to resolve current exe path for autostart registration: {e}")
            }
        }
    }

    let db_path = data_dir.join("nucleos.db");
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
    // After the restore (which may replace the file) and before the pool opens (nothing else holds
    // the file). Only an instance that owns its database applies it: the machine's daemon, or a
    // secondary started with a data directory of its own. A secondary on the default data directory
    // (port override only) shares the primary's file and leaves the marker for the primary's next
    // start. A second unconfigured process on the same directory could still race, and SQLite's
    // locks keep the file safe; no exclusive lock is taken here.
    let owns_database = is_primary
        || data_dir_override
            .as_deref()
            .is_some_and(|dir| !dir.is_empty());
    if owns_database {
        match backup::apply_pending_compaction(&db_path, health::available_space).await {
            Ok(Some(compacted)) => tracing::warn!(
                "compacted the database from {} to {} bytes; safety copy at {}",
                compacted.before_bytes,
                compacted.after_bytes,
                compacted.safety_backup.display()
            ),
            Ok(None) => tracing::info!("no database compaction to apply"),
            // `VACUUM` ran and the request is gone: nothing is retried, the owner has to act.
            Err(e @ backup::BackupError::CompactionUnverified { .. }) => tracing::error!(
                "database compaction finished but the result failed verification; continuing startup, not retrying: {e}"
            ),
            Err(e) => tracing::error!(
                "failed to compact the database; continuing startup and retrying next start: {e}"
            ),
        }
    }
    let pool = storage::open(&db_path)
        .await
        .expect("failed to open local database");
    tracing::info!("nucleos-core database ready at {}", db_path.display());

    startup::reconcile_after_restart(&pool).await;

    let token_value = match secrets::daemon_token(
        secrets::load_secret(TOKEN_KEY),
        // Only a FIRST start mints: a token already in the store is returned as it is, prefixed or
        // not, so the shell, the telegram sidecar and every hook holding it keep working.
        || auth::mint_secret("ctl"),
        |fresh| secrets::store_secret(TOKEN_KEY, fresh),
    ) {
        Ok(token) => token,
        Err(sentence) => {
            // Said on stderr as well as in the log: whoever launched a daemon that refuses to
            // start is the one person who can fix the store, and may not be reading the log.
            tracing::error!("{sentence}");
            eprintln!("{sentence}");
            std::process::exit(1);
        }
    };
    tracing::info!("nucleos-core token loaded from the system credential store");

    // This machine's settings, all under one root — see `machine_config`'s header for why it is
    // `~/.nucleos/` and no longer the directory the daemon happened to be launched from. `None` (no
    // home directory) reads every file as absent, which is what an absent file has always meant:
    // each pillar starts on its defaults.
    let machine_config_root = machine_config::root();
    if machine_config_root.is_none() {
        tracing::warn!(
            "no home directory, so {} cannot be read; every pillar starts on its defaults",
            machine_config::ROOT_DISPLAY
        );
    }
    let machine_file = |file: &str| machine_config_root.as_ref().map(|root| root.join(file));

    let models_config = machine_file(config::MODELS_CONFIG_FILE)
        .map(|path| config::load_models_config(&path))
        .unwrap_or_else(|| Ok(config::ModelsConfig::default()))
        .unwrap_or_else(|e| {
            tracing::warn!(
                "failed to parse {} ({e}), using defaults",
                config::MODELS_CONFIG_DISPLAY_PATH
            );
            config::ModelsConfig::default()
        });
    // The owner's saved choice (Settings) wins over the file, which is the default of an untouched
    // install; `embed::install` is called again by the settings door when the choice changes.
    embed::install(Arc::new(embed::OllamaEmbedder::new(
        execution::runner::OLLAMA_BASE_URL.to_string(),
        embed::model::effective(&pool, &models_config.embedding_model).await,
    )));
    let (triage_runner, local_triage_disabled): (
        Option<Arc<dyn execution::runner::CommandRunner>>,
        Option<String>,
    ) = if let Some(model) = models_config.local_triage_model.clone() {
        let local_runner = execution::runner::OllamaRunner::new(
            execution::runner::OLLAMA_BASE_URL.to_string(),
            model.clone(),
        );
        // The requirement's own field, not `governance::triage::LOCAL_NUM_CTX` directly: this is the number
        // that gets enforced, and it must come FROM the declaration the role states below, not
        // sit beside it as a second copy that could drift out of step. `CAPABILITY_REQUIREMENT`
        // is itself defined from `LOCAL_NUM_CTX`, which is the link that keeps them together.
        let declared = capabilities::discover_ollama_as(
            &reqwest::Client::new(),
            execution::runner::OLLAMA_BASE_URL,
            &model,
            governance::triage::CAPABILITY_REQUIREMENT.context_tokens,
            "local model",
        )
        .await;
        let missing = capabilities::missing_capabilities(
            &governance::triage::CAPABILITY_REQUIREMENT,
            &declared,
        );
        match capabilities::triage_posture(&missing) {
            capabilities::Posture::Unaffected => {
                tracing::info!(%model, "local triage model enabled");
                (Some(Arc::new(local_runner)), None)
            }
            capabilities::Posture::SwitchedOff(reason) => {
                // Local triage is optional at startup: a bad probe must not take down unrelated
                // daemon services, just as a failed autostart registration does not.
                tracing::warn!(%model, %reason, "local triage model disabled");
                (None, Some(reason))
            }
            // `triage_posture` only ever returns `Unaffected` or `SwitchedOff` today —
            // `Posture` is shared across three roles, so the compiler still requires this arm.
            // `nucleos-core` is a daemon the OS autostarts; panicking here over a local triage
            // model's posture would take down email, chats, runs and the scheduler along with
            // it, which is exactly what the comment above this match says a bad probe must not
            // do. Fail closed onto the same posture as `SwitchedOff`: triage refusing to run is
            // the privacy-preserving direction, the whole reason this role switches off rather
            // than falling back like the other two roles below.
            other => {
                tracing::error!(
                    %model,
                    ?other,
                    "triage_posture returned an unexpected posture; disabling local triage"
                );
                let reason = format!("unexpected local-triage posture: {other:?}");
                (None, Some(reason))
            }
        }
    } else {
        (None, None)
    };

    let email_config_path = machine_file(machine_config::EMAIL_FILE);
    let email_config_found = email_config_path
        .as_deref()
        .is_some_and(std::path::Path::exists);
    let email_config = email_config_path
        .as_deref()
        .map(config::load_email_config)
        .unwrap_or_default();
    // Built whether or not the pillar is enabled: it is two small files, and having it always in a
    // known state means enabling email later is a config edit rather than a fresh directory.
    let triage_sandbox = dirs.data_local_dir().join("triage-sandbox");
    if let Err(error) = governance::triage::ensure_sandbox(&triage_sandbox) {
        tracing::warn!(%error, "could not build the triage sandbox — the email pillar will stay off");
    }

    // The folder a person arranges their files in — uploads of their own, the mail they filed, and
    // now a workspace per team run. Created whether or not the email pillar is enabled, for the same
    // reason as the sandbox: a directory that always exists is one less thing to go wrong the day
    // email is switched on, and this one is reachable from its own tab with the pillar off. `None`
    // means every route under it refuses, which is the right answer when it could not be made.
    let files_root = match files::ensure_root(dirs.data_local_dir()) {
        Ok(root) => Some(root),
        Err(error) => {
            tracing::warn!(%error, "could not create the files folder — the Files tab will be unavailable");
            None
        }
    };
    // Where a delete from that folder goes instead of being destroyed — beside the root and never
    // in it (`files::trash_for`). The retention sweep runs here once, off the startup path because
    // an expired folder can be a large tree, and again on every delete; nothing else keeps a timer
    // for it. `None` means a delete refuses rather than removing for good.
    let files_trash = match files::ensure_trash(dirs.data_local_dir()) {
        Ok(trash) => {
            let sweep = trash.clone();
            tokio::task::spawn_blocking(move || {
                match files::purge(&sweep, files::TRASH_RETENTION) {
                    Ok(0) => {}
                    Ok(removed) => {
                        tracing::info!(removed, "cleared expired entries from the files trash")
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not clear expired entries from the files trash")
                    }
                }
            });
            Some(trash)
        }
        Err(error) => {
            tracing::warn!(%error, "could not create the files trash — deleting from the Files tab will be unavailable");
            None
        }
    };

    let voice_config = machine_file(machine_config::VOICE_FILE)
        .as_deref()
        .map(config::load_voice_config)
        .unwrap_or_default();
    let calendar_config = machine_file(machine_config::CALENDAR_FILE)
        .as_deref()
        .map(config::load_calendar_config)
        .unwrap_or_default();
    // Startup copy: ingestion keeps it for the run; the findings routes read the cached file per request.
    let devtime_config = machine_file(machine_config::DEVTIME_FILE)
        .as_deref()
        .map(config::load_devtime_config)
        .unwrap_or_default();
    let verify_config = machine_file(machine_config::VERIFY_FILE)
        .as_deref()
        .map(config::load_verify_config)
        .unwrap_or_default();
    let web_config = machine_file(machine_config::WEB_FILE)
        .as_deref()
        .map(config::load_web_config)
        .unwrap_or_default();
    let browser_config = machine_file(machine_config::BROWSER_FILE)
        .as_deref()
        .map(config::load_browser_config)
        .unwrap_or_default();
    let telegram_config = machine_file(machine_config::TELEGRAM_FILE)
        .as_deref()
        .map(config::load_telegram_config)
        .unwrap_or_default();
    // The path is named once and reused, because two facts come off it: what the file SAYS
    // (`load_github_config`) and whether it EXISTS at all. The second is the pillar's opt-in — see
    // `GithubRuntime::configured` — and deriving it from a second lookup is how the two would come
    // to disagree about which file they mean.
    let github_path = machine_file(machine_config::GITHUB_FILE);
    let github_config = github_path
        .as_deref()
        .map(config::load_github_config)
        .unwrap_or_default();
    let github_configured = github_path.as_deref().is_some_and(std::path::Path::exists);
    // Resolved here and carried on the runtime, so the daemon and the health probe can never end up
    // asking about two different programs — the mistake `cli_probe` names when it says to use "the
    // resolved path, not the configured name".
    let github_binary = std::env::var("NUCLEOS_GH_BIN").unwrap_or_else(|_| "gh".to_owned());
    // The web sidecar's own shared secret, minted per boot and never persisted.
    //
    // NOT the control token, and not for the reason the email sidecar has its own: this traffic
    // only ever flows daemon → sidecar, so the sidecar has nothing to authenticate itself FOR. It
    // needs a secret solely to refuse anything else on the machine that can open a socket. A
    // per-boot random value is therefore strictly better than a long-lived one — there is nothing
    // to leak and nothing to rotate.
    let web_sidecar_token = auth::mint_secret("web");
    // The browser sidecar's, minted the same way and for the same reason. It matters more here: the
    // process it authenticates drives browsers holding the owner's logged-in profiles, so a secret
    // that leaked would hand those sessions to anything on the machine that can open a socket.
    let browser_sidecar_token = auth::mint_secret("browser");
    // The quota sidecar's, the same way again. Nothing of the owner's travels on this connection in
    // either direction — the request is empty and the answer is a handful of percentages — so this
    // secret exists only to keep anything else on the machine from asking the daemon's sidecar how
    // much of the owner's limit is gone.
    let quota_sidecar_token = auth::mint_secret("quota");
    // Cleanup is armed SEPARATELY from transcription, and a failed probe costs only the tidying up.
    //
    // That asymmetry is deliberate. Local triage refuses to run at all when its probe fails, because
    // there the alternative is sending mail bodies off-machine. Here the fallback is a raw transcript
    // that never leaves the laptop, so the same failure should cost the polish and not the dictation —
    // exactly what `integrations::voice::clean_up` does at runtime, decided once here at startup.
    let voice_cleanup_model = match models_config.voice_cleanup_model.clone() {
        Some(model) if voice_config.armed() => {
            // The requirement's own field, not `integrations::voice::CLEANUP_NUM_CTX` directly — see the same
            // note on the local-triage probe above; `CAPABILITY_REQUIREMENT` is defined from
            // `CLEANUP_NUM_CTX`, so the two stay linked without a second number to keep in step.
            let declared = capabilities::discover_ollama_as(
                &reqwest::Client::new(),
                execution::runner::OLLAMA_BASE_URL,
                &model,
                integrations::voice::CAPABILITY_REQUIREMENT.context_tokens,
                "voice cleanup",
            )
            .await;
            let missing = capabilities::missing_capabilities(
                &integrations::voice::CAPABILITY_REQUIREMENT,
                &declared,
            );
            // voice_posture reuses execution::runner::local_triage_decision's own wording for a context gap —
            // a pure mapping from a context-probe result to an operator-readable reason, and every
            // message it produces talks about "the local model" rather than about mail. A second
            // copy would drift from this one.
            match capabilities::voice_posture(&missing) {
                capabilities::Posture::Unaffected => {
                    tracing::info!(%model, "voice cleanup model enabled");
                    Some(model)
                }
                capabilities::Posture::Degraded(reason) => {
                    tracing::warn!(%model, %reason, "voice cleanup disabled; transcripts will be delivered raw");
                    None
                }
                // voice_posture only ever returns Unaffected or Degraded today, for the same
                // reason the local-triage match above keeps this arm despite it: `Posture` is
                // shared across three roles. Treat an unexpected posture as Degraded rather than
                // panic — cleanup is an enhancement, and a bad probe here must cost only the
                // polish, never take the daemon down with it.
                other => {
                    tracing::error!(
                        %model,
                        ?other,
                        "voice_posture returned an unexpected posture; disabling voice cleanup"
                    );
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
    let claude_runner = || execution::runner::ClaudeCliRunner {
        model: models_config.claude_model.clone(),
        plan_model: models_config.plan_model.clone(),
        review_model: models_config.review_model.clone(),
        resolve_model: models_config.resolve_model.clone(),
        resolve_effort: models_config.resolve_effort.clone(),
    };
    let configured_runner = models_config.primary_runner.as_deref();
    let primary_runner: Arc<dyn execution::runner::CommandRunner> = match configured_runner {
        Some("codex") => {
            tracing::info!(model = %models_config.codex_model, "codex CLI selected as the run runner");
            Arc::new(execution::runner::CodexCliRunner {
                model: models_config.codex_model.clone(),
                // A run keeps the user's Codex config; only chats pin a sandbox (`for_chat`).
                sandbox_mode: None,
            })
        }
        Some(other) => {
            tracing::warn!(%other, "unknown primary_runner — keeping the Claude CLI");
            Arc::new(claude_runner())
        }
        None => Arc::new(claude_runner()),
    };
    // The llm-router as an adviser. Off unless `~/.nucleos/router.yaml` turns it on, and off hands
    // back the very runner built above. Read from the same root the settings page writes and the
    // health row probes, so the three never disagree about which file is in force.
    let primary_runner = models::route_advice::front(
        primary_runner,
        &models_config,
        machine_file(machine_config::ROUTER_FILE)
            .map(|path| models::route_advice::load_config(&path))
            .unwrap_or_else(models::route_advice::RouterConfig::off),
    );

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

    // WHICH local server answers a local turn, and whether the local route may run at all, read
    // from the file ONCE, here — above BOTH of its readers: the capability probe immediately below
    // and the assistant factory further down. One resolution and not two is the whole point. A
    // probe that picked its own dialect and a factory that read the file again could disagree about
    // which engine is configured, and the way that disagreement shows up is the local assistant
    // being reported disabled while the server the operator named answers perfectly — so both are
    // handed the same `ResolvedLocalEngine` and cannot disagree.
    //
    // A refusal — an engine name this daemon does not serve, `openai_compatible` with no address, an address
    // off this machine — DISABLES the route rather than falling back to Ollama. That fallback is
    // the worst outcome available: the operator named a server, was told nothing, and their turns
    // went somewhere else. `local_model: None` is how this crate already says "route off" —
    // `assistants::serves(Brain::Local)` refuses `RouteNotConfigured`, which `http.rs` renders as
    // `assistant::NO_LOCAL_MODEL` — so no new refusal variant is needed to say it here.
    let resolved_local_engine = match models_config.local_engine() {
        Ok(resolved) => Some(resolved),
        Err(refusal) => {
            // `error!` and not `warn!`, unlike the hosted half-setup lines below: those describe a
            // setup somebody has not finished, while this one is a line somebody WROTE that this
            // daemon will not honour, and the message names the key that repairs it.
            tracing::error!(
                reason = %refusal.message(),
                "the configured local engine is refused; chats marked local will refuse \
                 rather than answer"
            );
            None
        }
    };

    // The model that answers a chat turn asking to be answered on this machine — resolved here to
    // an `Option<String>` (was, before the assistant factory, a whole `LocalAssistant` built once)
    // and handed to `assistants::ConfiguredAssistants` below, which builds the assistant per turn.
    //
    // Probed against this feature's own window, in whichever dialect the engine resolved above
    // names: a turn accumulates its tool schemas and every result on each round, so it needs more
    // room than a single triage prompt and the probe has to say so, or a local server silently
    // truncates the middle of a conversation. Local triage and voice cleanup above still probe
    // Ollama directly — they are different roles on a different config key, and moving them is
    // not this change.
    //
    // A failed probe falls back rather than disabling, which is the opposite of local triage and for
    // a reason worth stating: triage refuses because the alternative is mail bodies leaving the
    // machine, while this turn reads only the daemon's own state, so the fallback is what already
    // happens today. Warning and carrying on is right here and would be wrong there.
    let local_model = match (
        models_config.local_assistant_model.clone(),
        resolved_local_engine.as_ref(),
    ) {
        (Some(model), Some(resolved)) => {
            // The requirement's own field, not `models::local_agent::TURN_NUM_CTX` directly — see the
            // same note on the local-triage probe above; `CAPABILITY_REQUIREMENT` is defined
            // from `TURN_NUM_CTX`, so the two stay linked without a second number to keep in
            // step.
            let declared = capabilities::discover_local_as(
                &reqwest::Client::new(),
                resolved.engine,
                &resolved.base_url,
                &model,
                models::local_agent::CAPABILITY_REQUIREMENT.context_tokens,
                resolved.declared_context_tokens,
                "local assistant",
            )
            .await;
            let missing = capabilities::missing_capabilities(
                &models::local_agent::CAPABILITY_REQUIREMENT,
                &declared,
            );
            match capabilities::local_assistant_posture(&missing) {
                capabilities::Posture::Unaffected => {
                    tracing::info!(%model, "local assistant enabled for chat turns that ask for it");
                    Some(model)
                }
                capabilities::Posture::FellBackToCli(reason) => {
                    tracing::warn!(%model, %reason, "local assistant disabled; chat turns stay on the CLI");
                    None
                }
                // local_assistant_posture only ever returns Unaffected or FellBackToCli today,
                // for the same reason the two matches above keep this arm despite it: `Posture`
                // is shared across three roles. Treat an unexpected posture as FellBackToCli
                // rather than panic — the cloud CLI already exists as this role's fallback, so
                // failing closed here costs nothing this feature does not already accept
                // elsewhere.
                other => {
                    tracing::error!(
                        %model,
                        ?other,
                        "local_assistant_posture returned an unexpected posture; falling back to the CLI"
                    );
                    None
                }
            }
        }
        // No model named, or an engine this daemon refused: either way there is nothing to probe
        // and nothing to enable, and the refusal was already said out loud where it was read.
        _ => None,
    };

    // The hosted model (and key) that answer a chat turn asking to be answered over OpenRouter —
    // `local_model`'s sibling and the same ship-dark posture: a daemon that has configured neither
    // `hosted_assistant_model` nor an OpenRouter key behaves exactly as one that has never heard of
    // this feature, silently, the same way `local_model` above stays `None` when
    // `local_assistant_model` alone is absent.
    //
    // No probe, unlike `local_model`: that probe exists because Ollama silently truncates a prompt
    // that does not fit its context window, and this daemon is the one that has to know the window
    // before it happens. OpenRouter states each model's window in its own catalogue and this call
    // never claims one of its own, so there is nothing here for a probe to catch.
    let (hosted_model, hosted_key) = match (
        models_config.hosted_assistant_model.clone(),
        secrets::load_secret(OPENROUTER_KEY),
    ) {
        (Some(model), Ok(Some(key))) => {
            tracing::info!(%model, "hosted assistant enabled for chat turns marked openrouter");
            (Some(model), Some(key))
        }
        // A model named but no key stored: an operator part-way through turning this on. Worth a
        // line, unlike the fully-unconfigured case below, because there is now a `chats` row this
        // CAN reach (`assistant::NO_HOSTED_MODEL`) and somebody watching the log should know why it
        // refuses.
        (Some(model), Ok(None)) => {
            tracing::info!(
                %model,
                "hosted_assistant_model is configured but no OpenRouter key is stored; \
                 chats marked openrouter will refuse rather than answer"
            );
            (Some(model), None)
        }
        (Some(model), Err(error)) => {
            tracing::warn!(
                %model,
                %error,
                "could not read the OpenRouter key from the system credential store; \
                 chats marked openrouter will refuse rather than answer"
            );
            (Some(model), None)
        }
        // A key stored with no model named: the other half of an unfinished setup, and just as
        // worth a line — otherwise the only sign anything is wrong is a refusal nobody can explain.
        (None, Ok(Some(key))) => {
            tracing::info!(
                "an OpenRouter key is stored but hosted_assistant_model is not configured; \
                 chats marked openrouter will refuse rather than answer"
            );
            (None, Some(key))
        }
        // Neither configured: the ship-dark default, and silent for the same reason
        // `local_model`'s own equivalent arm is — an untouched install must look exactly like
        // one that predates this feature.
        (None, Ok(None)) => (None, None),
        (None, Err(_)) => (None, None),
    };

    // Model discovery keys: the credential store first, the environment second. Only whether each
    // is present is ever logged. A key stored while the daemon runs takes effect after a restart.
    {
        let key = |name: &str, env: &str| {
            secrets::load_secret(name)
                .ok()
                .flatten()
                .or_else(|| std::env::var(env).ok())
                .filter(|k| !k.trim().is_empty())
        };
        let keys = models::model_catalog::Keys {
            anthropic: key("anthropic-api-key", "ANTHROPIC_API_KEY"),
            openai: key("openai-api-key", "OPENAI_API_KEY"),
        };
        tracing::info!(
            anthropic = keys.anthropic.is_some(),
            openai = keys.openai.is_some(),
            "model discovery keys"
        );
        models::model_catalog::install_keys(keys);
        // Without a key a vendor is still discovered: Codex from the CLI's own cache, Anthropic
        // from the public models.dev catalogue. A newly shipped model shows without a code edit.
        models::model_catalog::enable_keyless();
        // Warm the snapshot off the request path: the first picker open then finds it ready.
        tokio::spawn(async {
            models::model_catalog::current().await;
        });
    }

    // Read after the local model has been probed, because whether a `kind: local` seat is runnable
    // is not something a config file can assert — startup PROVES it, and a roster naming a local
    // seat this daemon cannot answer with is refused rather than quietly re-routed to the cloud.
    let council_config = match council::config_path() {
        Some(path) => config::load_council_config(&path, local_model.is_some()),
        // No home directory means there is nowhere a roster could be, so there is no council —
        // and that is all it means. Warned rather than fatal, like every other missing-pillar
        // path here: a daemon that will not boot costs the operator mail, autopilot and the API
        // over a feature that ships off.
        None => {
            tracing::warn!(
                "no home directory, so {} cannot be read; the council stays off",
                council::CONFIG_DISPLAY_PATH
            );
            None
        }
    };
    // Minted only when there is a council to use it, and never the control token: a seat is an
    // agent CLI deciding what to call next, and `auth::COUNCIL_ROUTES` is what it can reach. A
    // failure to mint leaves `None`, and `council::start` refuses — a seat with a key that
    // authenticates nothing is a council that costs money to answer badly.
    let council_token = match council_config.as_ref() {
        Some(_) => match auth::mint_service_token(&pool, auth::Service::Council).await {
            Ok(token) => Some(token),
            Err(error) => {
                tracing::error!(%error, "could not mint the council's key; the council stays off");
                None
            }
        },
        None => None,
    };

    // The factory reads the SAME resolution the probe above read — resolved once, beside the probe,
    // and destructured here. Reading `local_engine()` a second time would be a second decision, and
    // two decisions about which engine is configured are two decisions that can disagree; this pair
    // cannot, because there is only one of it.
    let (local_model, local_base_url, local_engine, local_context_tokens) =
        match resolved_local_engine {
            Some(resolved) => (
                local_model,
                resolved.base_url,
                resolved.engine,
                resolved.declared_context_tokens,
            ),
            // Inert values beside a `local_model` of `None`: with the route off, nothing ever
            // reads the address or the engine. Today's constant and today's engine, so that if
            // anything ever does read them it reads the daemon's own default rather than half of
            // the configuration that was just refused. `local_model` is stated `None` here rather
            // than left to the probe having skipped: the route being off is what the factory is
            // handed, never something it has to infer from a branch it cannot see.
            None => (
                None,
                execution::runner::OLLAMA_BASE_URL.to_string(),
                config::LocalEngine::Ollama,
                None,
            ),
        };

    // Assembles the assistant that answers a turn from what was just resolved above — one factory
    // in place of the two singletons `local_assistant`/`hosted_assistant` used to be. No production
    // caller until this packet; `AppState.assistants` below is the first one.
    let assistants: Arc<dyn assistants::Assistants> = Arc::new(
        assistants::ConfiguredAssistants::new(
            local_model,
            hosted_model,
            hosted_key,
            "http://127.0.0.1:8791".to_string(),
            token_value.clone(),
            pool.clone(),
            // The RESOLVED address, not `execution::runner::OLLAMA_BASE_URL` directly. Identical for an
            // install that named no engine — `local_engine()` resolves absence to Ollama on that
            // same constant — and the whole point for one that named another server.
            local_base_url,
        )
        .with_local_engine(local_engine, local_context_tokens)
        .with_agent_clis(Arc::new(claude_runner()), models_config.codex_model.clone()),
    );

    // What every settings file looked like when the loaders above read it. A loader answers a file
    // it cannot parse with defaults and a warning, so the owner learns of it from the settings page
    // (this, set against the file as it is now) and from one feed line each, not from a log.
    let startup_config = Arc::new(
        machine_config_root
            .as_deref()
            .map(machine_config::check_all)
            .unwrap_or_default(),
    );
    for check in startup_config.iter().filter(|check| !check.valid) {
        tracing::warn!(
            file = %machine_config::display_path(check.path),
            "this settings file does not parse; the daemon started on its defaults for it"
        );
    }
    machine_config::report_unreadable(&pool, &startup_config).await;

    let state = AppState {
        startup_config,
        token: Token(token_value),
        pool,
        runner: primary_runner,
        triage_runner,
        local_triage_disabled,
        assistants,
        files_root,
        files_trash,
        // `None` when this machine has no home directory to hang a library off. Resolved here and
        // not per request, like `files_root` above: it is a fact about the machine.
        workflow_library: seeded_library(),
        secrets: std::sync::Arc::new(secrets::OsCredentialStore),
        // The root every file above was read from, so the settings page writes the same files the
        // daemon reads. `None` was already warned about where it was resolved.
        machine_config_root: machine_config_root.clone(),
        telegram_doctrine: telegram_config.doctrine,
        email: Arc::new(state::EmailRuntime::from_config(
            &email_config,
            triage_sandbox,
            email_sidecar_token,
        )),
        voice: Arc::new(integrations::voice::VoiceRuntime::from_config(
            &voice_config,
            voice_cleanup_model,
        )),
        calendar: Arc::new(integrations::calendar::CalendarRuntime::from_config(
            &calendar_config,
        )),
        council: Arc::new(council::CouncilRuntime::new(council_config, council_token)),
        channels: Arc::new(channels::ChannelsRuntime::load(
            machine_config_root.as_deref(),
        )),
        github: Arc::new(integrations::github::GithubRuntime::from_config(
            &github_config,
            github_configured,
            github_binary,
        )),
        browser: Arc::new(browser::BrowserRuntime {
            enabled: browser_config.enabled,
            client: browser_client::BrowserClient::new(
                sidecar::BROWSER_ADDR,
                browser_sidecar_token.clone(),
            ),
            modes: Default::default(),
            seats: Default::default(),
        }),
        web: Arc::new(integrations::web::WebRuntime {
            enabled: web_config.enabled,
            trusted_hosts: web_config.trusted_hosts.clone(),
            retain_pages_days: web_config.retain_pages_days,
            client: web_client::WebClient::new(sidecar::WEB_ADDR, web_sidecar_token.clone()),
            // The local model that reads quarantined pages. It is the same one the voice pillar
            // probed at startup — one local model, one place it is pinned — but the consequence of
            // its absence is the opposite: voice degrades to a raw transcript, and this one
            // REFUSES. A typing aid may fail soft; a barrier may not.
            quarantine_model: models_config.voice_cleanup_model.clone(),
            ollama_base_url: execution::runner::OLLAMA_BASE_URL.to_string(),
            http: reqwest::Client::new(),
        }),
        quota: Arc::new(quota::QuotaRuntime::new(
            quota::client::QuotaClient::new(sidecar::QUOTA_ADDR, quota_sidecar_token.clone()),
            models_config.active_runner().to_string(),
        )),
        judge: Arc::new(judge::JudgeRuntime::jev()),
        run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        run_tails: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        progress_timeout: state::DEFAULT_PROGRESS_TIMEOUT,
        run_timeout: state::DEFAULT_RUN_TIMEOUT,
    };

    let app = http::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", daemon_client::port()))
        .await
        .unwrap();
    tracing::info!(
        "nucleos-core listening on {}",
        listener.local_addr().unwrap()
    );
    // Every sidecar below is gated on this, and the reason is not tidiness. Secrets live in
    // Windows Credential Manager, which is NOT under the data directory — so a secondary instance
    // with its own empty database still loads the real Telegram bot token and the real mail
    // password. Two supervised telegram sidecars poll one account and answer every message twice;
    // two browser or web sidecars fight over the same fixed ports. A second daemon is for
    // exercising this process's own HTTP and MCP surface, and it does that without any of them.
    let sidecars_wanted = is_primary;
    let sidecar_path = sidecar::binary(sidecar::ECHO);
    if sidecars_wanted {
        tokio::spawn(sidecar::supervise(
            sidecar::ECHO.to_string(),
            sidecar_path,
            vec![],
        ));
    }

    // The quota sidecar. Supervised beside `echo` and NOT behind a pillar switch, which is the one
    // choice here worth defending, because every other process that reaches off this machine is
    // opt-in.
    //
    // It reaches a vendor only when the owner already has that vendor's CLI signed in on this
    // machine, and it reads the credential that CLI wrote. On a machine with no Claude Code it
    // makes no outbound call at all — it answers `unmeasured` and stops. So the switch an opt-in
    // would offer is one the owner has already thrown, in the other application, and a second one
    // here would mean the notch ships dark with no settings page to light it.
    if sidecars_wanted {
        tokio::spawn(sidecar::supervise(
            sidecar::QUOTA.to_string(),
            sidecar::binary(sidecar::QUOTA),
            sidecar::quota_env(&daemon_client::daemon_url(), &quota_sidecar_token),
        ));
        tracing::info!(addr = sidecar::QUOTA_ADDR, "quota sidecar supervised");
    }

    // The browser sidecar. Started only when the pillar is on, like the web one beside it.
    //
    // Before it starts, every session this database still calls open is retired. Spec §9.1: the
    // adapter is the parent of the browsers, so a daemon restart takes every live session with it —
    // and a row saying "open" about a browser that no longer exists is worse than no row at all,
    // because the shell would offer to take the wheel of it.
    if browser_config.enabled {
        match browser::retire_open_sessions(&state.pool, &chrono::Utc::now().to_rfc3339()).await {
            Ok(retired) if retired > 0 => {
                tracing::warn!("retired {retired} browsing session(s) left open by a previous run")
            }
            Ok(_) => {}
            Err(error) => tracing::error!(%error, "could not retire open browsing sessions"),
        }

        let path = sidecar::binary(sidecar::BROWSER);
        let env = sidecar::browser_env(
            &daemon_client::daemon_url(),
            &browser_sidecar_token,
            &browser_config,
        );
        if sidecars_wanted {
            tokio::spawn(sidecar::supervise(sidecar::BROWSER.to_string(), path, env));
        }
        tracing::info!(
            max_sessions = browser_config.max_sessions,
            "browser sidecar supervised"
        );
    }

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
        let path = sidecar::binary(sidecar::WEB);
        let env = sidecar::web_env(
            &daemon_client::daemon_url(),
            &web_sidecar_token,
            &web_config,
            &search_key,
        );
        if sidecars_wanted {
            tokio::spawn(sidecar::supervise(sidecar::WEB.to_string(), path, env));
        }
        tracing::info!(provider = %web_config.provider, "web sidecar supervised");

        // Retention. Hourly rather than on a timer tied to reads: a cache that is never read again
        // must still empty, or "30 days" means "30 days after the last time anyone looked".
        let retention_state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                match integrations::web::prune(
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
            let telegram_path = sidecar::binary(sidecar::TELEGRAM);
            let telegram_env =
                sidecar::telegram_env(&daemon_client::daemon_url(), &state.token.0, &bot_token);
            if sidecars_wanted {
                tokio::spawn(sidecar::supervise(
                    sidecar::TELEGRAM.to_string(),
                    telegram_path,
                    telegram_env,
                ));
            }
            tracing::info!("telegram sidecar supervised");
        }
        Ok(None) => {
            tracing::info!(
                "no telegram-token stored — telegram sidecar not started (set with --set-telegram-token)"
            );
        }
        Err(e) => {
            tracing::warn!("failed to read telegram-token from the system credential store: {e}");
        }
    }
    tokio::spawn(execution::scheduler::run_scheduler(state.clone()));
    // Its own loop, not a step inside the scheduler's: a job pass can sit inside `run_gate` for the
    // whole gate timeout, and sharing a loop would stall every scheduled rule in the daemon behind
    // one project's test suite.
    tokio::spawn(execution::job::run_job_loop(state.clone()));
    // AFTER `execution::runs::reconcile_orphaned_runs`, which ran near the top of this function and is what
    // marks the abandoned subprocesses `interrupted` — the ordering this depends on, and the same
    // one `execution::job::reconcile_orphaned_jobs` respects. Awaited rather than spawned, so the loop below
    // never meets a half-reconciled run.
    if let Err(error) = team::reconcile_orphaned_team_runs(&state).await {
        tracing::warn!(%error, "could not reconcile the team runs a previous daemon left behind");
    }
    // Its own loop again, and for this pillar's own reason rather than the job's: a team pass
    // launches up to `max_parallel` subprocesses and writes files at a cadence nothing else in the
    // house shares. It runs no gate, so the argument above does not transfer — this one stands on
    // its own.
    tokio::spawn(team::run_team_loop(state.clone()));
    tokio::spawn(team::run_workspace_gc_loop(state.clone()));
    // A third loop and not a branch in either of the two above. What DECIDES that a department
    // starts is a different question from how it runs — the same separation `scheduler.rs` has from
    // `job.rs` — and it could not have gone in `scheduler_tick` at all: that loop is per project,
    // and a department has no project, no root and no HEAD.
    tokio::spawn(team::trigger::run_team_trigger_loop(state.clone()));
    tokio::spawn(repo_trigger::run_repo_poller(state.clone()));
    tokio::spawn(execution::worktree::run_gc(state.pool.clone()));
    // Plan 2026-10-17 F1: every session in a rostered project gets the `verify` box, the main
    // checkout's and a worktree added by hand included, unless the owner switched it off.
    tokio::spawn(verify::provision::run_reconcile_loop(state.pool.clone()));
    // The worktree GC's counterpart inside the database. It collects the directories a finished run
    // leaves on disk; this expires the transcript it leaves in `runs`, the events it leaves in
    // `run_events`, and eventually its line in the feed. Nothing removed any of those, so all three
    // grew for as long as the daemon was ever used — and a transcript is stored twice and indexed a
    // third time, so they grew at three times the obvious rate.
    // The same loop takes the weekly database copy, on the primary instance only.
    tokio::spawn(execution::runs::run_retention_loop(
        state.clone(),
        is_primary,
    ));
    // Slot occupancy every minute, rolled into daily rows, for the analytics page's pressure
    // metric (spec analytics-pipeline §5.3). Its own loop: the retention loop is hourly and this
    // is per minute, and neither belongs on the slot acquire/release path.
    tokio::spawn(analytics_pipeline::sampler::run_slot_sampler(
        state.pool.clone(),
    ));
    // The distiller: turns closed jobs queued by the job loop and the vcs queue into learnings.
    tokio::spawn(distill::run_distill_loop(state.clone()));
    // Its own loop: the cadence is file I/O over the transcripts directory, independent of runs.
    let devtime_projects_dir = if devtime_config.projects_dir.trim().is_empty() {
        commands::home().map(|home| home.join(".claude").join("projects"))
    } else {
        Some(std::path::PathBuf::from(&devtime_config.projects_dir))
    };
    tokio::spawn(devtime::run_ingest_loop(
        state.pool.clone(),
        devtime_config,
        devtime_projects_dir,
    ));
    // The devtime cost's prices, kept fresh by the daemon rather than by somebody remembering
    // `scripts/refresh-models.py`. Installed for the "refresh prices" button even when the file
    // turns `auto_refresh_prices` off: the loop reads that per tick, the button ignores it. The
    // path is the one `models_config_now` reads, so the refresh writes the file the costs come from.
    if let Some(models_path) = config::models_config_path() {
        let refresher = Arc::new(price_refresh::Refresher::for_daemon(models_path));
        price_refresh::install(Arc::clone(&refresher));
        tokio::spawn(price_refresh::run_refresh_loop(refresher));
    }
    // `POST /verify` submits to it, so the handlers need the very instance the worker drains: it is
    // parked in `verify::install` (an `OnceLock`, because `AppState` is built literally in dozens of
    // places) before the worker starts. The restart above hands interrupted units straight back to it.
    let verify_executor = verify::exec::Executor::new(
        state.pool.clone(),
        verify_config,
        machine_config_root.clone(),
    );
    verify::install(verify_executor.clone());
    // Off for every project unless its rules set `gate_after_land`.
    tokio::spawn(verify::postgate_worker::run_postgate_worker(
        verify_executor.clone(),
    ));
    // Opens the correction run of a `fix/` branch the gate worker prepared; with `revert_on_red` off
    // for every project there is never one, and a pass reads an empty query.
    tokio::spawn(verify::postgate_worker::run_correction_loop(state.clone()));
    tokio::spawn(verify::exec::run_executor(verify_executor));
    tokio::spawn(execution::vcs::run_queue_worker(
        state.pool.clone(),
        std::sync::Arc::new(execution::git_exec::GitExecutor {
            machine_root: machine_config_root.clone(),
            pool: Some(state.pool.clone()),
            verify: verify::installed(),
            ..execution::git_exec::GitExecutor::default()
        }),
    ));
    // Its own loop and not a step inside the queue worker's, for the reason `resolver.rs` opens
    // with: the worker holds a pool and a repository lock, and starting an agent needs an
    // `AppState` and the time an agent takes. The queue escalates and lets go; this picks the
    // conflict up afterwards.
    tokio::spawn(resolver::run_resolution_loop(state.clone()));
    // Only when a local model is already configured, and reusing the triage one rather than adding
    // a key: this reads mail-derived text, which is the text that model was chosen for, and
    // `web.rs` sets the precedent of one local model pinned in one place serving more than one
    // reader. Without it the sweep simply never runs, and the observation table stays empty —
    // which is the correct behaviour for a measurement nobody asked for.
    if let Some(model) = models_config.local_triage_model.clone() {
        tokio::spawn(governance::pii_shadow::run_sweep_loop(
            state.pool.clone(),
            execution::runner::OLLAMA_BASE_URL.to_string(),
            model,
        ));
    }

    // Spawned whether or not the pillar is enabled: the loop also owns retention, and bodies
    // already stored do not stop needing to expire because polling was switched off.
    tokio::spawn(governance::triage::run_triage_loop(state.clone()));

    // Unconditional for exactly the reason above, applied to dictations. Gating this on the pillar
    // being armed would FREEZE the transcript history at the moment somebody switched voice off, which
    // is the opposite of what switching it off is for — the recordings of what they said would then
    // outlive the feature that made them.
    tokio::spawn(integrations::voice::run_retention_loop(state.clone()));
    // Held notifications are reconciled at startup for the same reason orphaned runs and stranded
    // approvals are: the daemon may have been down when the meeting ended, and a queue that only
    // drains on the tick would sit there until the NEXT meeting ended instead.
    match integrations::notify::flush_due(&state.pool).await {
        Ok(delivered) if delivered > 0 => {
            tracing::warn!("delivered {delivered} notification(s) held over from a previous run")
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not flush held notifications on startup"),
    }
    tokio::spawn(integrations::notify::run_flush_loop(state.clone()));
    // Messages a previous daemon left waiting are sent once, here; afterwards every slot release drains.
    tokio::spawn(assistant::drain_waiting_queues(state.clone()));
    // Channel loops start only when a channel instance exists (a channel with its secrets present),
    // which never happens in phase 1: the A2 bodies stay unreachable. Seeding runs first so the
    // turn watermark and the routing cursors exist before the first discover/route_once.
    if !channels::registry::all().is_empty() && channels::registry::channel_instances_exist(&state)
    {
        let cadence = channels::bus::Cadence::default();
        channels::routing::seed_all(&state).await;
        tokio::spawn(channels::replies::run_replies_loop(
            state.clone(),
            channels::bus::ChannelsBus::from_wake(),
            cadence.clone(),
        ));
        tokio::spawn(channels::routing::run_routing_loop(
            state.clone(),
            channels::bus::ChannelsBus::from_wake(),
            cadence,
        ));
    }
    // Hourly TTL pruning of the channel tables; cheap, always on.
    tokio::spawn(channels::run_housekeeping(state.clone()));

    // The email pillar starts only after its hook barrier has been PROVEN, and the proof can only
    // be attempted once this listener is serving — the hook reaches the daemon over HTTP, and a
    // daemon that is not up yet produces `ask_daemon.py`'s fail-closed `block`, which looks like
    // success and proves nothing (spec §5.5). Hence a task that waits for `axum::serve` below
    // rather than a check inline here.
    if state.email.enabled {
        let state = state.clone();
        tokio::spawn(async move {
            match governance::triage::verify_hook_barrier(
                &state.pool,
                &state.email.sandbox,
                &daemon_client::daemon_url(),
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
                            let path = sidecar::binary(sidecar::EMAIL);
                            let env = sidecar::email_env(
                                &daemon_client::daemon_url(),
                                token,
                                &email_config,
                                &password,
                            );
                            if sidecars_wanted {
                                tokio::spawn(sidecar::supervise(
                                    sidecar::EMAIL.to_string(),
                                    path,
                                    env,
                                ));
                            }
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
                    tracing::warn!(%error, "could not read the email password from the system credential store")
                }
            }
        });
    } else if email_config_found {
        tracing::info!(
            "email pillar disabled ({} says enabled: false)",
            machine_config::display_path(machine_config::EMAIL_FILE)
        );
    } else {
        // Not the same thing, and saying so cost a diagnosis: a daemon started from the wrong
        // directory once reported the user's config as switched off while that file sat there
        // reading `enabled: true`. The files no longer depend on where the daemon was started, but
        // "off because absent" and "off because it says so" are still two different sentences.
        tracing::info!(
            "email pillar off — {} does not exist",
            machine_config::display_path(machine_config::EMAIL_FILE)
        );
    }

    // A held long-poll (`/hooks/ask-wait`) would keep graceful shutdown waiting forever, and the
    // old binary alive under the installer; after the deadline the process just leaves. The
    // watchdog runs on its own OS thread, so it outlives the runtime.
    http::spawn_exit_watchdog(std::time::Duration::from_secs(10), || std::process::exit(0));
    axum::serve(listener, app)
        .with_graceful_shutdown(http::shutdown_requested())
        .await
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uma equipa que nunca correu nao se desenha como uma tabela vazia.
    ///
    /// Este e o caso de hoje -- `team_runs` esta a zero -- e uma tabela de zeros lia-se como
    /// aprovacao. Nada foi aprovado: nada aconteceu.
    #[test]
    fn a_team_that_never_ran_is_drawn_as_that_and_not_as_an_empty_table() {
        let drawn = render_pressure(&serde_json::json!({
            "outcome": "never_ran",
            "team": "NucleOS",
        }));

        assert_eq!(drawn, "equipa NucleOS · nunca correu");
    }

    /// O que nao foi medido escreve-se `—`, nunca `0`.
    ///
    /// Um pico ausente e um ajuste que se recusou a existir sao as duas coisas que um desenho
    /// descuidado transforma em zeros -- e um zero aqui e uma afirmacao sobre uma janela que
    /// ninguem mediu.
    #[test]
    fn what_was_not_measured_is_drawn_as_a_dash_and_never_as_a_zero() {
        let drawn = render_pressure(&serde_json::json!({
            "outcome": "measured",
            "team": "NucleOS",
            "job": "job-1",
            "rounds": 1,
            "items_measured": 1,
            "items_without_run": 0,
            "items_without_steps": 0,
            "rollups": [{
                "round": 1,
                "agent_name": "Nucleo",
                "items": 1,
                "compacted_items": 0,
                "peak_p50": serde_json::Value::Null,
                "peak_p90": serde_json::Value::Null,
                "steps_median": 3,
                "fit": {"kind": "insufficient"},
                "verdicts": [],
            }],
        }));

        assert!(drawn.contains("job job-1"), "o job aparece: {drawn}");
        let row = drawn.lines().last().expect("a linha do rollup");
        assert!(row.contains("Nucleo"), "o agente aparece: {row}");
        // Os dois picos, as tres colunas do ajuste que nao existe, e o veredicto que nao ha. As
        // contagens que valem zero -- `comp`, `sem run` -- ficam a zero de proposito: essas foram
        // medidas.
        assert_eq!(
            row.matches('—').count(),
            6,
            "cada celula por medir sai como travessao: {row}"
        );
    }

    /// Um ajuste que existe desenha-se com o R2 ao lado.
    ///
    /// O R2 nao e decoracao: com uma variavel e tarefas heterogeneas isto e diagnostico
    /// populacional, e um arranque sem ele le-se com uma confianca que nao tem.
    #[test]
    fn a_fit_that_exists_is_drawn_with_its_r2_beside_it() {
        let drawn = render_pressure(&serde_json::json!({
            "outcome": "measured",
            "team": "NucleOS",
            "job": "job-1",
            "rounds": 1,
            "items_measured": 3,
            "items_without_run": 0,
            "items_without_steps": 0,
            "rollups": [{
                "round": 1,
                "agent_name": "Nucleo",
                "items": 3,
                "compacted_items": 1,
                "peak_p50": 120_000,
                "peak_p90": 180_000,
                "steps_median": 4,
                "fit": {"kind": "line", "intercept": 50_000.0, "slope": 1_400.0, "r2": 0.87},
                "verdicts": ["split_speciality", "trim_prompt"],
            }],
        }));

        assert!(drawn.contains("50000"), "o arranque: {drawn}");
        assert!(drawn.contains("0.87"), "o R2: {drawn}");
        assert!(
            drawn.contains("split_speciality, trim_prompt"),
            "os dois veredictos, e nao so o primeiro: {drawn}"
        );
    }

    /// `nucleos-core --land` on its own is what almost every session types, and it has to keep
    /// meaning what it has always meant. An argument that is not there is not an error.
    #[test]
    fn a_land_with_nothing_after_it_names_no_target() {
        let args = ["nucleos-core".to_owned(), "--land".to_owned()];
        assert_eq!(land_target_from(&args), None);
    }

    /// The branch is the argument straight after the flag, and what follows it is not this
    /// function's business.
    #[test]
    fn a_land_takes_the_branch_that_follows_it() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "release".to_owned(),
            "--verbose".to_owned(),
        ];
        assert_eq!(land_target_from(&args), Some("release".to_owned()));
    }

    /// The same when the branch ends the command line, which is how it is actually typed. The
    /// `get(at + 1)` is what keeps this from being an index past the end.
    #[test]
    fn a_land_takes_a_branch_that_ends_the_command_line() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "release".to_owned(),
        ];
        assert_eq!(land_target_from(&args), Some("release".to_owned()));
    }

    /// A session that wrote `--land --something` meant the flag. Reading the next flag as a
    /// branch name would send a typo to the daemon as a landing target, and the daemon would
    /// refuse it with a message about a branch nobody ever asked for.
    #[test]
    fn a_land_followed_by_another_flag_names_no_target() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "--verbose".to_owned(),
        ];
        assert_eq!(land_target_from(&args), None);
    }

    /// And a command line with no `--land` at all has no target to find, however many other
    /// arguments it carries.
    #[test]
    fn a_command_line_without_land_names_no_target() {
        let args = [
            "nucleos-core".to_owned(),
            "--pressao".to_owned(),
            "--equipa".to_owned(),
            "NucleOS".to_owned(),
        ];
        assert_eq!(land_target_from(&args), None);
    }

    /// `--land=release` is the form that used to fall past the guard and start a daemon. It reads
    /// as the same request the space-separated form does, because that is the only thing a person
    /// who typed it can have meant.
    #[test]
    fn a_land_joined_by_an_equals_names_the_branch_after_it() {
        let args = ["nucleos-core".to_owned(), "--land=release".to_owned()];
        assert_eq!(land_target_from(&args), Some("release".to_owned()));
    }

    /// The two empty forms, and the asymmetry between them is the deliberate answer rather than an
    /// oversight: `--land=` carries no value to send, while `--land ""` carries one and goes as it
    /// was typed — the daemon reads an empty target as the integration branch, which is where a
    /// bare `--land` was going anyway.
    #[test]
    fn an_empty_equals_names_no_target_and_an_empty_argument_travels_as_typed() {
        let joined = ["nucleos-core".to_owned(), "--land=".to_owned()];
        assert_eq!(land_target_from(&joined), None);

        let separate = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            String::new(),
        ];
        assert_eq!(land_target_from(&separate), Some(String::new()));
    }

    /// One `--land` is what anybody types, so the answer to two of them is worth pinning rather
    /// than leaving to whichever way the scan happens to run: the FIRST spelling is the command,
    /// and a second one is a mistake rather than an override. It holds across the two forms, which
    /// is the half a separate scan for `--land=` would have got wrong.
    #[test]
    fn the_first_land_wins_whichever_form_the_others_take() {
        let separated = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "trunk".to_owned(),
            "--land".to_owned(),
            "release".to_owned(),
        ];
        assert_eq!(land_target_from(&separated), Some("trunk".to_owned()));

        let separated_then_joined = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            "trunk".to_owned(),
            "--land=release".to_owned(),
        ];
        assert_eq!(
            land_target_from(&separated_then_joined),
            Some("trunk".to_owned())
        );

        let joined_then_separated = [
            "nucleos-core".to_owned(),
            "--land=release".to_owned(),
            "--land".to_owned(),
            "trunk".to_owned(),
        ];
        assert_eq!(
            land_target_from(&joined_then_separated),
            Some("release".to_owned())
        );
    }

    /// A shell that kept the space in `--land " --verbose"` hands this one argument that is a flag
    /// wearing a space. The daemon trims before it decides what a target is, so this has to trim
    /// before it decides what a flag is — otherwise the CLI calls it a branch and the daemon calls
    /// it a flag, and the two ends of one argument disagree.
    #[test]
    fn an_argument_a_shell_padded_with_a_space_is_still_a_flag() {
        let args = [
            "nucleos-core".to_owned(),
            "--land".to_owned(),
            " --verbose".to_owned(),
        ];
        assert_eq!(land_target_from(&args), None);
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    /// Every part of a packaging request is required: a version is written once, so a guessed
    /// folder or manifest would be a permanent mistake.
    #[test]
    fn packaging_needs_every_argument_and_takes_them_in_any_flag_order() {
        let parsed = workflow_package_args(&words(
            "nucleos-core --workflow-package dev 1.0.0 --manifest m.yaml --from /p",
        ))
        .unwrap();
        assert_eq!(parsed.0, "dev");
        assert_eq!(parsed.1, "1.0.0");
        assert_eq!(
            parsed.2,
            PackageFrom::Project {
                root: std::path::PathBuf::from("/p"),
                manifest: std::path::PathBuf::from("m.yaml"),
            }
        );

        let parsed = workflow_package_args(&words(
            "nucleos-core --workflow-package nucleos-dev 2.0.0 --source core/assets/nucleos-dev",
        ))
        .unwrap();
        assert_eq!(parsed.0, "nucleos-dev");
        assert_eq!(
            parsed.2,
            PackageFrom::Source(std::path::PathBuf::from("core/assets/nucleos-dev"))
        );

        for line in [
            "nucleos-core --workflow-package dev --from /p --manifest m.yaml",
            "nucleos-core --workflow-package dev 1.0.0 --from /p",
            "nucleos-core --workflow-package dev 1.0.0 --from --manifest m.yaml",
            "nucleos-core --workflow-package dev 1.0.0 --source",
            // Two sources at once is a mistake, not a choice to be made for the caller.
            "nucleos-core --workflow-package dev 1.0.0 --source /s --manifest m.yaml",
            "nucleos-core --workflow-package dev 1.0.0 --source /s --from /p --manifest m.yaml",
        ] {
            assert!(workflow_package_args(&words(line)).is_err(), "{line}");
        }
    }

    /// A named checkout is sent as given, made absolute; no name means the working directory.
    #[test]
    fn workflow_sync_names_the_argument_or_the_working_directory() {
        let named = workflow_sync_target(&words("nucleos-core --workflow-sync some/tree")).unwrap();
        assert!(std::path::Path::new(&named).is_absolute());
        assert!(named.replace('\\', "/").ends_with("some/tree"));

        let bare = workflow_sync_target(&words("nucleos-core --workflow-sync")).unwrap();
        assert_eq!(
            std::path::PathBuf::from(bare),
            std::path::absolute(std::env::current_dir().unwrap()).unwrap()
        );
    }

    /// `--project` names which of several projects on one repository the sync is for; absent, the
    /// daemon decides alone, and a flag with no value is refused rather than read as absent.
    #[test]
    fn workflow_sync_passes_the_named_project_through() {
        let line = "nucleos-core --workflow-sync some/tree --project beta";
        assert_eq!(
            workflow_sync_project(&words(line)).unwrap().as_deref(),
            Some("beta")
        );
        assert!(
            workflow_sync_target(&words(line))
                .unwrap()
                .replace('\\', "/")
                .ends_with("some/tree")
        );

        // The flag right after `--workflow-sync` is not mistaken for the checkout, and a checkout
        // after it is still found.
        let before = "nucleos-core --workflow-sync --project beta some/tree";
        assert!(
            workflow_sync_target(&words(before))
                .unwrap()
                .replace('\\', "/")
                .ends_with("some/tree")
        );
        let bare = "nucleos-core --workflow-sync --project beta";
        assert_eq!(
            std::path::PathBuf::from(workflow_sync_target(&words(bare)).unwrap()),
            std::path::absolute(std::env::current_dir().unwrap()).unwrap()
        );
        assert_eq!(
            workflow_sync_project(&words(bare)).unwrap().as_deref(),
            Some("beta")
        );

        assert_eq!(
            workflow_sync_project(&words("nucleos-core --workflow-sync some/tree")).unwrap(),
            None
        );
        for line in [
            "nucleos-core --workflow-sync some/tree --project",
            "nucleos-core --workflow-sync --project --other",
        ] {
            assert!(workflow_sync_project(&words(line)).is_err(), "{line}");
        }
    }

    /// The daemon's ambiguity refusal is printed as the choice it is, one candidate per line with
    /// the flag that picks it; any other body is not this refusal.
    #[test]
    fn an_ambiguous_sync_is_printed_as_a_choice() {
        let body = serde_json::json!({
            "refusal": "ambiguous_project",
            "candidates": [
                { "project_id": "alpha", "project_root": "C:/repo" },
                { "project_id": "beta", "project_root": "C:/repo" },
            ],
        });
        let said = ambiguous_sync_message(&body.to_string()).unwrap();
        assert!(said.contains("alpha") && said.contains("beta"), "{said}");
        assert!(said.contains("--project"), "{said}");
        assert_eq!(
            said.lines().filter(|line| line.contains("C:/repo")).count(),
            2
        );

        assert!(ambiguous_sync_message(r#"{"refusal":"no_project"}"#).is_none());
        assert!(ambiguous_sync_message("not json").is_none());
    }

    /// The daemon names its version without starting anything, and that version is the repository's
    /// `VERSION`: the shell compares the two after an update, so a daemon built from a stale crate
    /// version would be reported as a mismatch forever.
    #[test]
    fn the_version_flag_prints_the_crate_version_and_starts_nothing() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();

        assert_eq!(
            version_output(&args(&["nucleos-core", "--version"])),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(version_output(&args(&["nucleos-core"])), None);
        // The program name is not a flag, whatever it is called.
        assert_eq!(version_output(&args(&["--version"])), None);
        assert_eq!(
            env!("CARGO_PKG_VERSION"),
            include_str!("../../VERSION").trim(),
            "core/Cargo.toml disagrees with VERSION: run scripts/bump-version.py"
        );
    }
}
