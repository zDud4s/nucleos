//! Query shapes, resolvers and refusals shared by more than one route family.

use super::*;

#[derive(Serialize)]
pub(super) struct Home {
    /// Where NucleOS itself lives: the folder the daemon was started from. `None` when the
    /// working directory was unreadable, never a guess.
    pub(super) root: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ProjectQuery {
    pub(super) project_id: String,
}

/// A read of a project, optionally as one run sees it.
///
/// `run` is what makes the Code mode a review surface rather than a file browser: with it, every
/// reader answers from that run's worktree — the file as the agent left it, not as the trunk has
/// it.
#[derive(Deserialize)]
pub(super) struct ReadQuery {
    pub(super) path: Option<String>,
    pub(super) run: Option<i64>,
}

/// Reads an absent list, a `null` list and an empty list as the same thing: nothing.
///
/// `#[serde(default)]` alone covers only the ABSENT case, and Go's `encoding/json` writes a nil
/// slice as `null` rather than omitting it. That gap ate a real inbox: the sidecar read the mail,
/// the núcleo answered 422, and the batch replayed into the same wall every five minutes. It
/// survived the tests because the fixtures omitted the field, which is a shape the sidecar never
/// sends. There is no message these three encodings could carry that differs.
pub(super) fn absent_or_null_is_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// PURE: builds a `Content-Disposition` for a filename chosen by a stranger.
///
/// Always `attachment`, never `inline`: the bytes came from outside and nothing renders them in
/// place. The name goes out twice — a conservative ASCII form for old clients and the RFC 6266
/// `filename*` form for everything else — and both are built from `safe_filename`'s output, so a
/// name carrying a carriage return cannot end this header and begin one of the sender's choosing.
pub(super) fn content_disposition(filename: &str) -> String {
    let safe = crate::integrations::email::safe_filename(filename);
    // The quoted form cannot carry a quote or a backslash without escaping them, and a filename is
    // not worth the escaping rules — anything outside a plain set becomes an underscore, and the
    // faithful version travels in `filename*` alongside it.
    let ascii: String = safe
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(safe.len());
    for byte in safe.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Turns a folder refusal into a status. `Escapes` and `Unsafe` are both 400: the request named
/// something it may not name, and which of the two rules caught it is not the caller's business —
/// a distinction here would be a probe for how the guard is built.
///
/// The three 409s are not one answer either, and they do not need to be: a caller knows which of
/// the four operations it just asked for, so "conflict" reads as one sentence per route — this is
/// not a folder, this name is taken, this folder still has things in it.
pub(super) fn folder_status(error: crate::files::PathError) -> StatusCode {
    use crate::files::PathError;
    match error {
        PathError::Escapes | PathError::Unsafe => StatusCode::BAD_REQUEST,
        PathError::NotFound => StatusCode::NOT_FOUND,
        PathError::NotADirectory | PathError::NotEmpty | PathError::Exists => StatusCode::CONFLICT,
        PathError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(serde::Serialize)]
pub(super) struct SavedFile {
    /// The name it was ACTUALLY stored under, which can differ from the one it arrived with twice
    /// over: once because the name was made safe, once because it collided. True of a sender's
    /// attachment and of a file the user picked themselves — this is the answer to "where did it
    /// go", and guessing it is how a caller ends up naming a file that is not there.
    pub(super) filename: String,
    pub(super) folder: String,
}

pub(super) fn inspect_status(error: inspect::InspectError) -> StatusCode {
    match error {
        inspect::InspectError::NotFound => StatusCode::NOT_FOUND,
        inspect::InspectError::UnsafePath => StatusCode::BAD_REQUEST,
        // The only one the caller cannot diagnose from the status code alone, so it is the only one
        // worth a line in the log.
        inspect::InspectError::Io(error) => {
            tracing::warn!(%error, "project inspection failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Where a read of this project should happen: the project itself, or one run's worktree.
///
/// **The subject of a review is the run, not the repository**, so every reader in this family takes
/// an optional `run` and answers from that run's checkout when it is given. Without it they answer
/// from the project root exactly as before, which is what every existing caller gets.
///
/// **The `project_id` in the lookup is the guard, and it is the whole security argument here.** A
/// worktree path is otherwise reachable by asking any project for any run id — the daemon holds
/// worktrees for every project in one table — so the row must match the project being asked, not
/// merely exist. A run whose worktree has been released has `removed_at` set and is refused too:
/// the directory is gone, and answering from a stale path would read whatever has since been put
/// there.
pub(super) async fn resolve_read_root(
    state: &AppState,
    id: &str,
    run: Option<i64>,
) -> Result<PathBuf, StatusCode> {
    let Some(run) = run else {
        return resolve_project_root(state, id).await;
    };
    let path: Option<String> = sqlx::query_scalar(
        "SELECT path FROM worktrees \
          WHERE owner_kind = 'run' AND owner_id = ? AND project_id = ? AND removed_at IS NULL",
    )
    .bind(run)
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    path.map(PathBuf::from).ok_or(StatusCode::NOT_FOUND)
}

pub(super) async fn resolve_project_root(
    state: &AppState,
    id: &str,
) -> Result<PathBuf, StatusCode> {
    match inspect::project_root(&state.pool, id).await {
        Ok(Some(root)) => Ok(PathBuf::from(root)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/* --------------------------------------------------- this machine's settings -- */

/// The root every row in [`crate::machine_config`] is relative to, or a refusal when startup could
/// not name one. See [`crate::state::AppState::machine_config_root`].
pub(super) fn machine_config_root(
    state: &AppState,
) -> Result<std::path::PathBuf, (StatusCode, Json<serde_json::Value>)> {
    state
        .machine_config_root
        .clone()
        .ok_or_else(|| refusal(StatusCode::INTERNAL_SERVER_ERROR, "no_machine_root"))
}

/// This project's state directory, `~/.nucleos/projects/<id>/`, or the refusal saying why there is
/// none.
///
/// Two refusals, and they are different facts: `no_machine_root` is this machine having no home
/// directory — the same name `/config/machine` refuses with — and `bad_project_id` is an id that
/// cannot name a directory without meaning another one (`project::state::valid_id`).
pub(super) fn project_state_dir(
    state: &AppState,
    id: &str,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    let root = machine_config_root(state)?;
    crate::project::state::dir(&root, id)
        .ok_or_else(|| refusal(StatusCode::UNPROCESSABLE_ENTITY, "bad_project_id"))
}

/// Where a claimed file actually is, for the read route and the write route alike.
///
/// A state row is joined onto the state directory, a project row onto the project root — and in
/// both cases it is the REGISTRY's string that is joined, never the caller's, for the reason
/// `post_project_write` gives.
pub(super) fn claim_home(
    state: &AppState,
    id: &str,
    root: &std::path::Path,
    claim: &crate::ownership::Claim,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    match claim.home {
        crate::ownership::Home::State => project_state_dir(state, id),
        crate::ownership::Home::Project => Ok(root.to_path_buf()),
    }
}

/// The fence in force in one project, read off the disk on the thread pool.
///
/// `spawn_blocking`, because assembling it opens the pins file and walks whatever bundles are
/// installed. Small work, but filesystem work, and the runtime this daemon shares with every
/// sidecar and every run is not the place to do it inline.
///
/// A machine with no home directory has no library, which is an empty shelf and not a failure: the
/// núcleo's own rows still stand, so the write boundary never disappears because a path lookup came
/// back empty.
///
/// The same is true of a project with no state directory: no pins file means nothing is installed,
/// and the fence is the núcleo's rows alone.
pub(super) async fn claims_for_project(
    state: &AppState,
    id: &str,
    root: PathBuf,
) -> Result<Vec<crate::ownership::Claim>, StatusCode> {
    let library = state.workflow_library.clone();
    let pins = crate::project::state::file(
        state.machine_config_root.as_deref(),
        id,
        crate::project::state::PINS_FILE,
    );
    tokio::task::spawn_blocking(move || match library {
        Some(library) => crate::ownership::claims_for(&root, pins.as_deref(), &library),
        None => crate::ownership::CLAIMS.to_vec(),
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
pub(super) struct WriteRequest {
    /// A claim's identity, forward slashes. Only ever compared — never joined; see below.
    pub(super) path: String,
    pub(super) contents: String,
}

/// A refusal with its reason beside its name — the shape `POST /write` uses for `invalid`.
pub(super) fn refusal_with_detail(
    status: StatusCode,
    name: &'static str,
    detail: String,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({ "refusal": name, "detail": detail })),
    )
}

/* ------------------------------------------ the reach a project declares -- */

/// The longest prefix, op kind or branch one declaration may carry.
///
/// A route limit rather than a schema one, and the reason is the hook's hot path: `ShellRules`
/// re-folds every stored prefix on every comparison, and that comparison runs once per tool call of
/// every run of this project. Nothing else bounds what one POST can put there. A prefix has
/// `SAFE_COMMAND_PREFIXES`' shape — `bash scripts/gates.sh` is twenty-one characters — so this is
/// generous by more than an order of magnitude and still stops a 200 KB string reaching that path.
pub(super) const MAX_DECLARATION_LEN: usize = 512;

/// Refuses a project this daemon has never heard of, without requiring it to be switched ON.
///
/// **Deliberately not `resolve_project_root`, which is what the neighbouring routes use.** That one
/// reads `autopilot_state.project_root`, and `autopilot::set_project_mode` persists that column as
/// `NULL` for `Mode::Off` — so it 404s a project that is on the roster and merely paused. Declaring
/// what a project may do while it is off is a reasonable thing to want, and often the very moment
/// you would want it: the reach is what you set before you switch autonomy on.
///
/// So this asks the weaker question the roster itself asks — is there a row at all. It still closes
/// what it was added for: a typo'd id used to answer 204 and then show the rule back through the
/// GET, which is indistinguishable from a real project with no rules, so the owner's only feedback
/// loop confirmed a `deny` that nothing anywhere would ever enforce.
pub(super) async fn project_is_on_the_roster(
    state: &AppState,
    id: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let known: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM autopilot_state WHERE project_id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| {
                tracing::warn!(%error, project_id = %id, "checking the roster failed");
                refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            })?;
    if known.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "refusal": "no_such_project",
                "detail": format!("this daemon has no project called `{id}`"),
            })),
        ));
    }
    Ok(())
}

/// The emergency stop, asked only of the declarations that WIDEN.
///
/// **`post_project_command`'s argument for needing no stop does not transfer here, and this is the
/// one route family in this file where it does not.** That one says it "writes a database row, like
/// `POST /wip-limit` beside it, and neither of those starts anything" — true, because every reader
/// of those rows is a start point, and the stop already guards every start point. The rows written
/// here are read by `hooks.rs`, per decision, with no cache and no `kill_switch_engaged` call
/// anywhere in it. `set_kill_switch` flips a flag and cancels nothing already running, so with the
/// stop engaged an `allow` declared here still binds the very next tool call of every in-flight run
/// of this project. That is a widening the stop was supposed to make impossible.
///
/// **Only the widenings.** A `deny`, and all four DELETEs, stay open with the stop engaged: an
/// emergency stop that stopped somebody NARROWING autonomy would be holding the door open on the
/// way out. It is the asymmetry `hooks::downgrade_if_unreadable` already takes, where only the
/// `allow` is touched.
///
/// `POST /land-targets` is not gated either, and that is a judgement rather than an oversight: a
/// landing target binds nothing until a landing is attempted, and a landing goes through the queue,
/// which the stop already governs at its own start point.
///
/// Both switches and `unwrap_or(true)`, copied from `post_project_command_run` rather than invented:
/// a stop nobody can read is a stop.
pub(super) async fn declaration_halted(
    state: &AppState,
    id: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let halted = crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
        || crate::governance::autopilot::scoped_kill_engaged(&state.pool, "project", id)
            .await
            .unwrap_or(true);
    if halted {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }
    Ok(())
}

/// Refuses a declared value longer than anything a real one is, naming the length.
pub(super) fn within_length(
    what: &'static str,
    value: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if value.len() > MAX_DECLARATION_LEN {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "too_long",
                "detail": format!(
                    "this {what} is {} bytes; the most one may carry is {MAX_DECLARATION_LEN}",
                    value.len()
                ),
            })),
        ));
    }
    Ok(())
}

/// How long the three git reads get, together.
///
/// Short on purpose. This runs while somebody waits on a wizard, all three commands are local, and a
/// repository whose `.git` is on a disconnected network share is exactly the case that would
/// otherwise hold the request open. Missing git facts degrade to `None`, which the page shows as
/// "not read" rather than as "there is no remote".
///
/// `get_project_github_repo` shares it rather than naming a second duration. The two are the same
/// proposition — local git reads with a person waiting on a page — and two numbers here is how one
/// of them ends up generous enough to hang the request the other was written to protect.
pub(super) const DETECT_GIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Each failure gets the status that sends a reader to the right place.
///
/// A switched-off pillar and an absent `gh` are 503: the request was fine and this machine cannot
/// serve it. A missing token is 403, because somebody has to paste a credential. A timeout is 504.
pub(super) fn github_failure_status(failure: &crate::integrations::github::Failure) -> StatusCode {
    match failure {
        crate::integrations::github::Failure::NotConfigured
        | crate::integrations::github::Failure::MissingCli => StatusCode::SERVICE_UNAVAILABLE,
        crate::integrations::github::Failure::MissingToken => StatusCode::FORBIDDEN,
        crate::integrations::github::Failure::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        crate::integrations::github::Failure::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The longest query the name completion will act on.
///
/// Not a safety limit — `mentions::matching` walks the same tree whatever it is given. It is a
/// statement about what the field is for: past this, what arrived is not somebody typing a filename
/// and answering it as though it were would be answering the wrong question slowly.
pub(super) const MENTION_QUERY_LIMIT: usize = 100;

#[derive(serde::Deserialize)]
pub(super) struct MentionQuery {
    /// What has been typed after the `@`. Absent or empty asks for the top level.
    #[serde(default)]
    pub(super) q: String,
}

pub(super) fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

/// The pinned model names as they are on disk right now.
///
/// Re-read per request rather than held on `AppState`, which every caller of this depends on: a
/// pinned choice travels as `--model` on the turn itself, so a name added to the file is one this
/// daemon can already run — and a catalogue cached at startup would spend a whole daemon lifetime
/// refusing it. The daemon's price refresh (`crate::price_refresh`) and `scripts/refresh-models.py`
/// both rewrite this file, and nothing should have to be restarted for that to take.
///
/// An unreadable or missing file answers with the built-in defaults rather than an error, matching
/// what startup does: a picker that fails closed leaves somebody unable to change a model because
/// of a typo in a key that has nothing to do with models.
pub(super) fn models_config() -> crate::config::ModelsConfig {
    crate::config::models_config_now()
}

/// How long the `/api/tags` menu probe waits, and no more.
///
/// A couple of seconds, not `runner::OLLAMA_EXCHANGE_TIMEOUT` (120s) — that timeout is sized for a
/// COLD MODEL LOADING before it can answer a generation request, which has nothing to do with this
/// probe. `get_assistant_models` and `chosen_brain` are called whenever a menu opens and the window
/// may poll either, so a wedged or unreachable Ollama must cost the menu a brief pause and then
/// today's menu — never a hung window while a person is only trying to see what is on offer.
pub(super) const OLLAMA_TAGS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The one shared client every `/api/tags` probe goes through, built once rather than once per
/// request — the same one-client-per-route argument `assistants::ConfiguredAssistants` already
/// makes for the turn-serving routes: `reqwest::Client::new()` (or a fresh `builder()`) per request
/// rebuilds a whole connection pool and TLS backend every time, and these two routes are called
/// often enough (whenever a menu opens, possibly polled) for that cost to be worth avoiding.
pub(super) static OLLAMA_TAGS_CLIENT: std::sync::OnceLock<reqwest::Client> =
    std::sync::OnceLock::new();

/// Builds `OLLAMA_TAGS_CLIENT` the first time it is asked for, and hands back the same client every
/// time after — never a fresh one per call, per this module's own constraint above.
pub(super) fn ollama_tags_client() -> &'static reqwest::Client {
    OLLAMA_TAGS_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(OLLAMA_TAGS_TIMEOUT)
            .build()
            .expect("HTTP client for the /api/tags probe (check TLS and proxy environment)")
    })
}

/// The menu both the picker and the door work from: the configured catalogue, merged with what
/// this machine actually has pulled.
///
/// The one place `models_config()` and `installed_local_models` are read together, so
/// `chosen_brain` and `get_assistant_models` cannot each assemble their own copy of this join and
/// drift apart the day somebody changes one of them — a filter, a sort, a source — and not the
/// other. Read from the file on every call rather than cached in `AppState`: a pinned choice travels
/// as `--model` on the turn itself, so a name added to the file is one the daemon can already run,
/// and a catalogue cached at startup would spend a whole daemon lifetime refusing it.
///
/// Fails closed on every kind of Ollama trouble (unreachable daemon, non-2xx, unreadable body) to
/// `catalogue()`'s own menu rather than an error or a hang: `installed_local_models` answers an
/// empty list on failure, `catalogue_with_installed` with an empty list IS `catalogue()`, and
/// `OLLAMA_TAGS_CLIENT` — built once and reused, never a fresh client per call — bounds how long a
/// wedged Ollama can hold up either caller to `OLLAMA_TAGS_TIMEOUT`.
pub(super) async fn menu(
    asking: Asking,
) -> (
    crate::config::ModelsConfig,
    Vec<crate::config::AssistantChoice>,
) {
    let config = models_config();
    // "Which models has `ollama pull` fetched?" is an Ollama question, asked of `/api/tags`, which
    // only Ollama serves. On any other resolved engine — and on a refused one, whose local rows
    // `catalogue_scoped` drops entirely — the probe is a dial nothing reads, paid on every menu
    // open against a server that does not answer it, so the list is simply empty instead.
    let installed = if matches!(
        config.local_engine(),
        Ok(crate::config::ResolvedLocalEngine {
            engine: crate::config::LocalEngine::Ollama,
            ..
        })
    ) {
        crate::capabilities::installed_local_models(
            ollama_tags_client(),
            crate::execution::runner::OLLAMA_BASE_URL,
        )
        .await
    } else {
        Vec::new()
    };
    let choices = match asking {
        Asking::Daemon => config.catalogue_with_installed(&installed),
        Asking::Chat => config.catalogue_for_chat(&installed),
    };
    (config, choices)
}

/// Why `chosen_brain` could not hand back a route the caller may actually use.
pub(super) enum BrainRefusal {
    /// No catalogue entry — the current `menu()`, installed models included — names this id.
    UnknownModel,
    /// The id names a real choice, but this machine cannot serve what it names —
    /// `Assistants::can_serve`'s own reason, carried verbatim into the refusal body of both
    /// doors that ask — opening a conversation and re-pointing one — so somebody who picked a
    /// model this machine cannot run is told which one and why, not handed a bare 503.
    CannotServe(String),
}

/// The caller whose available models are being selected.
#[derive(Clone, Copy, Debug)]
pub(super) enum Asking {
    /// The daemon's project-independent model menu.
    Daemon,
    /// A chat menu: any chat, rooted or not, may use either CLI.
    Chat,
}

/// Whether the asker may use either CLI: `None` for the daemon's own menu, which follows the
/// active runner.
pub(super) fn rooted_of(asking: Asking) -> Option<bool> {
    match asking {
        Asking::Daemon => None,
        Asking::Chat => Some(true),
    }
}

/// Which route a choice id names and can actually run, or a refusal.
///
/// One function because two routes ask — opening a conversation and re-pointing one — and sharing
/// `menu()` (above) with `get_assistant_models` is how the two are kept from disagreeing about which
/// names are real: the menu the picker is served and the menu the door validates against are the
/// same read, not two copies that could come apart.
///
/// Checks the cheap, file-only `catalogue()` first, and only reaches for `menu()` — installed models
/// included, over `OLLAMA_TAGS_CLIENT` — when the id is not there. This is safe because
/// `catalogue_with_installed` is documented on `config.rs` to always return a SUPERSET of
/// `catalogue()`: the merge only ever adds local entries, and none at all when
/// `local_assistant_model` is unset. So an id found in the cheap set would have been found in the
/// merged one too — no id changes its answer, no refusal changes — while the common cases (every
/// cloud model, the hosted model, the already-configured local model) never touch the network at
/// all. Do not drop this fast path or flip the order: either would put a 2s Ollama probe back in
/// front of every chat creation and patch, cloud models included.
///
/// Once the id resolves to a route, `state.assistants.can_serve` is asked whether this machine can
/// actually run it. Only `Refusal::CannotServe` is surfaced as a refusal here: the other three
/// variants (`RouteNotConfigured`, `HostedModelNamedButNoKey`, `NotServedByThisFactory`) answer "is
/// this ROUTE configured or reachable at all", which is `serves`'s question, asked at turn time —
/// not this door's. `ConfiguredAssistants::can_serve` never returns any of those three for a route
/// this catalogue actually named a choice for, but a double standing in for "nothing is configured"
/// (`assistants::NoAssistants`) may, and treating that as a capability refusal here would refuse
/// every cloud pick the moment `state.assistants` is not fully wired — the wrong door, not a
/// capability gap.
pub(super) async fn chosen_brain(
    state: &AppState,
    id: &str,
    asking: Asking,
) -> Result<crate::chats::Brain, BrainRefusal> {
    chosen_brain_in(state, &models_config(), id, asking).await
}

/// Resolves a model choice for one caller using a supplied model configuration.
pub(super) async fn chosen_brain_in(
    state: &AppState,
    config: &crate::config::ModelsConfig,
    id: &str,
    asking: Asking,
) -> Result<crate::chats::Brain, BrainRefusal> {
    let cheap_choices = match asking {
        Asking::Daemon => config.catalogue(),
        Asking::Chat => config.catalogue_for_chat(&[]),
    };
    let brain = if let Some(choice) = cheap_choices.into_iter().find(|choice| choice.id == id) {
        crate::chats::Brain::from_wire(&choice.brain)
    } else if crate::models::model_catalog::cached_or_fallback()
        .iter()
        .any(|found| {
            found.id == id
                && crate::models::model_catalog::admits(
                    config.active_runner(),
                    rooted_of(asking),
                    found.runner,
                )
        })
    {
        // A model the vendor lists (or the built-in catalogue names) that no config row does.
        // Looked up without the network, like the cheap catalogue above.
        crate::chats::Brain::Cloud
    } else {
        let (_, choices) = menu(asking).await;
        choices
            .into_iter()
            .find(|choice| choice.id == id)
            .map(|choice| crate::chats::Brain::from_wire(&choice.brain))
            .ok_or(BrainRefusal::UnknownModel)?
    };

    if let Err(crate::assistants::Refusal::CannotServe(reason)) =
        state.assistants.can_serve(brain, id).await
    {
        return Err(BrainRefusal::CannotServe(reason));
    }

    Ok(brain)
}

/// Tells an absent field from one explicitly sent as `null`.
///
/// `Option<Option<T>>` alone does NOT do this. Serde maps a JSON `null` onto the OUTER option, so
/// `{"model": null}` and `{}` both arrive as `None` and the unpin is silently dropped — the field
/// has three states in the type and two on the wire. Deserializing the inner option and wrapping it
/// in `Some` is what makes the third reachable: absent never calls this at all and falls to
/// `Default`, while `null` calls it and comes back `Some(None)`.
///
/// Found by a test that pinned a value and then cleared it. The ones that only cleared an already
/// empty field passed either way, which is the shape of test that agrees with any implementation.
pub(super) fn sent_even_if_null<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

/// The scope a refinement approval asked for, or `None` when `raw` names none a refinement can
/// live in (`machine`, `job:7`, a missing id, no colon at all).
pub(super) fn refinement_target(raw: &str) -> Option<crate::knowledge::Scope> {
    let (kind, id) = raw.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    match kind {
        "project" => Some(crate::knowledge::Scope::Project(id.to_owned())),
        "team" => Some(crate::knowledge::Scope::Team(id.to_owned())),
        "agent" => Some(crate::knowledge::Scope::Agent(id.to_owned())),
        _ => None,
    }
}
