//! Model selection and assistant catalogue configuration.

use super::*;

// `PartialEq` only: `model_prices` holds `f64`, which has no `Eq`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelsConfig {
    pub claude_model: String,
    pub codex_model: String,
    /// Absent keeps the established CLI path active, which lets local triage ship dark until an
    /// operator explicitly names a model that can keep message bodies on the machine.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub local_triage_model: Option<String>,
    /// Absent leaves voice cleanup unarmed, so a transcript is delivered raw rather than not at all.
    /// Named here rather than in `~/.nucleos/voice.yaml` because pinning models is this file's whole job.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub voice_cleanup_model: Option<String>,
    /// Which agent CLI answers a run. Absent — or naming anything startup does not recognise — keeps
    /// the proven Claude path, so the second runner ships dark until an operator asks for it by
    /// name, the same posture `local_triage_model` gives local inference.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub primary_runner: Option<String>,
    /// Where a job's `plan` stage runs. Absent keeps it on `claude_model`, so a file written before
    /// this key existed routes nothing anywhere.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub plan_model: Option<String>,
    /// Where a job's `review` stage runs, on the same absent-means-unrouted posture as `plan_model`.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub review_model: Option<String>,
    /// Where the queue's conflict resolution runs (`resolver.rs`), on the same absent-means-unrouted
    /// posture as `plan_model`: absent keeps it on `claude_model`.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub resolve_model: Option<String>,
    /// The reasoning effort a conflict resolution is launched with. Absent sends no `--effort`, so
    /// the CLI keeps its own default — exactly what every resolution got before this key existed.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub resolve_effort: Option<String>,
    /// Absent leaves chat turns from the Telegram sidecar answered by the cloud CLI, exactly as they
    /// are today. Naming a model here is what moves them onto this machine.
    ///
    /// Ship-dark like `local_triage_model` and `primary_runner`, and here it matters more than for
    /// either: this key changes the behaviour of a channel already in daily use, so an upgrade must
    /// change nothing at all until somebody asks for it by name.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub local_assistant_model: Option<String>,
    /// Which model a hosted chat turn is sent to over OpenRouter — `openai_compatible.rs`'s
    /// `OpenAiCompatibleChat`, the second `LocalChat` implementation beside `runner::OllamaChat`. Not the
    /// hosted route's alone: since `local_engine` below, the model `local_assistant_model` names
    /// is answered by one or the other of the same two, so what distinguishes this key is the
    /// endpoint and the key it needs, never which client it ends up holding.
    ///
    /// Ship-dark, on exactly the posture `local_assistant_model` already carries: absent, nothing
    /// about a conversation's behaviour changes, and a chat row that somehow already says
    /// `openrouter` is refused (`assistant::NO_HOSTED_MODEL`) rather than answered by the cloud CLI
    /// on the strength of this field never having been read. Naming a model here is what an
    /// operator does once they have also put a key in the OS credential store — this field alone
    /// gets a conversation no further, since `OpenAiCompatibleChat::new` still refuses without one.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub hosted_assistant_model: Option<String>,
    /// Which local server answers a local turn: `ollama`, or `openai_compatible` for any OpenAI-compatible
    /// server running on this machine (llama.cpp, LM Studio, vLLM).
    ///
    /// Absent means `ollama`, which is the ship-dark posture `local_assistant_model` and
    /// `primary_runner` already carry, arriving through the one key where absence is not merely
    /// "unarmed" but "the engine this machine has always used": every `~/.nucleos/nucleos-models.yaml` on
    /// disk was written before this key existed and names none of them, so absence has to resolve
    /// to exactly today's behaviour or a file that worked this morning refuses this afternoon.
    ///
    /// A name this daemon does not serve is REFUSED rather than fallen back to Ollama, and that is
    /// the one place this key parts from `primary_runner` above. There, falling back keeps the
    /// proven path a typo was never trying to leave. Here, a mistyped `openai_compatible` would keep sending
    /// turns to the very server the operator wrote this line to stop using — and the silence about
    /// it, not the typo, is the failure.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub local_engine: Option<String>,
    /// Where that server listens. Loopback only.
    ///
    /// Absent resolves to `runner::OLLAMA_BASE_URL` for the ollama engine — the address the runner
    /// already uses, read from its constant so the two cannot drift — and is REFUSED for `openai_compatible`,
    /// because no port may be guessed. An `openai_compatible` engine quietly resolved onto Ollama's own
    /// `11434` would post a turn's contents — mail, a transcript, a repository — to whatever
    /// program happens to be listening there, under a file that named no address at all.
    ///
    /// Whatever is written here is checked by `is_loopback_url` before it is used. `Brain::Local`'s
    /// promise is that a local turn never leaves this machine, and one line in a config file must
    /// not be able to turn that into "whatever address the file says": an off-machine URL earns
    /// `LocalEngineRefusal::NotLoopback` and the route leaves the menu with it, rather than being
    /// offered and then refusing every turn it is picked for.
    #[serde(default, deserialize_with = "deserialize_optional_model")]
    pub local_base_url: Option<String>,
    /// The context window this local server serves, DECLARED here because for one of the two
    /// engines nothing can discover it: Ollama answers `/api/show`, and an OpenAI-compatible
    /// `/v1/models` states no window at all.
    ///
    /// Absent stays `None` and nothing invents a number. A guessed window is not a harmless default
    /// — it is a figure the daemon would then act on when deciding what fits in a turn — which is
    /// the argument `AssistantChoice::tools` makes for never marking `Some(false)` on a guess,
    /// arriving here as a count instead of a flag.
    #[serde(default)]
    pub local_context_tokens: Option<usize>,
    /// The Ollama model that embeds knowledge rows (spec 5.4, D6). Embeddings are always local:
    /// no text of a project leaves the machine for this.
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
    /// The models a conversation may be moved to, in the order the window offers them.
    ///
    /// A list here rather than a list in the window, because the window cannot know it. The agent
    /// CLI has no `--list-models` (2.1.198) and nothing else enumerates them either, so any list is
    /// somebody's assertion — and an assertion written into the UI goes stale where nobody who can
    /// fix it will see it. Written here, it is next to the model names this daemon already pins.
    ///
    /// The default names ALIASES and not versions. `sonnet` is whatever the CLI currently resolves
    /// Sonnet to; `claude-sonnet-5` is a specific model that stops existing. A picker built out of
    /// versions is a picker that has to be edited every time Anthropic ships, which is the exact
    /// staleness this key exists to avoid — so pinning is available to whoever wants it, and is not
    /// what an untouched install does.
    ///
    /// Cloud only. The local entry is not written here because it is not a choice: it is whatever
    /// `local_assistant_model` names, and a second place to say it is a second place to disagree.
    /// `catalogue` joins the two.
    #[serde(default = "default_assistant_choices")]
    pub assistant_choices: Vec<AssistantChoice>,
    /// When `scripts/refresh-models.py` last read the vendors' price pages (`YYYY-MM-DD`), or `None`
    /// when it never has. Shown beside every estimated cost so its age is visible (spec devtime §4.4).
    #[serde(default)]
    pub prices_as_of: Option<String>,
    /// Vendor-published prices, USD per million tokens, keyed by normalised model id. Written by the
    /// script, never by hand: there is no hand-kept price catalogue (owner, 2026-10-17). Read per
    /// request like the rest of this file, so a refresh needs no restart.
    ///
    /// Filled by [`parse_models_config`] from `raw_model_prices`, entry by entry, so one unreadable
    /// entry costs that entry and never the file.
    #[serde(skip)]
    pub model_prices: std::collections::BTreeMap<String, ModelPrice>,
    /// `model_prices` exactly as the file wrote it, any shape. Read as a bare YAML value so that a
    /// wrong type in one price cannot fail the whole file: model routing must not depend on it
    /// (owner, 2026-10-18). Emptied by [`parse_models_config`] once converted.
    #[serde(default, rename = "model_prices")]
    pub(crate) raw_model_prices: serde_yaml::Value,
    /// One `"<model id>: <reason>"` per price entry [`parse_models_config`] dropped, so the drop is
    /// visible where the cost is shown and not only in the daemon's log.
    #[serde(skip)]
    pub price_warnings: Vec<String>,
}

/// One row of the conversation's model picker.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct AssistantChoice {
    /// What goes to `--model`, verbatim. An alias or a full name; the CLI accepts both.
    pub id: String,
    /// What the window shows. Separate from `id` because the useful name and the accepted name are
    /// not the same string — `sonnet` is what the CLI takes and "Sonnet" is what a person reads.
    pub label: String,
    /// Which route answers this choice: `cloud` or `local`.
    ///
    /// Carried on the choice rather than inferred from the id, so picking a model sets the route
    /// too and the two cannot come apart. A `chats` row saying `local` while naming a cloud model
    /// would go to Ollama and hand it a name it has never heard.
    pub brain: String,
    /// The effort levels THIS model takes, weakest first. Empty means it has no dial.
    ///
    /// Per model and not one list for all of them, because they genuinely differ: at the time of
    /// writing `gpt-5.6-terra` takes an `ultra` that `gpt-5.5` does not, and `gpt-5.5` stops at
    /// `xhigh` where `gpt-5.6-luna` goes to `max`. A single global list would offer every model the
    /// union, and the ones that do not take the top of it would die at spawn.
    ///
    /// It also subsumes the boolean this replaced: empty is exactly "no dial", which is what a
    /// local model has, and the window reads the list rather than testing `brain == "cloud"`.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// Which agent CLI runs this model: `claude` or `codex`. Absent means `claude`.
    ///
    /// A second axis from `brain`, not a finer grain of it. `brain` says whether the turn goes to
    /// Ollama or to a CLI; this says WHICH CLI — and they are independent, because the local route
    /// is the same either way. Carried so one config file can hold both lists and the daemon shows
    /// the one belonging to the runner it was actually started with.
    #[serde(default)]
    pub runner: Option<String>,
    /// Whether this model declares tool calling, or `None` when nothing here knows.
    ///
    /// `Option<bool>` and not `bool`: "declares no tools" and "we have not asked" are different
    /// facts and only one of them is worth warning somebody about. Filled by `marked_with`, which
    /// never guesses — a choice absent from that function's map keeps `None`, never `Some(false)`.
    ///
    /// `#[serde(default)]` is required, not decoration: `AssistantChoice` is `Deserialize` and is
    /// read from `~/.nucleos/nucleos-models.yaml`'s `assistant_choices`, so without it every existing
    /// config file on disk — written before this field existed — stops parsing.
    #[serde(default)]
    pub tools: Option<bool>,
    /// Whether this machine has this LOCAL model pulled, or `None` when the question does not
    /// apply — which is every route but `local`.
    ///
    /// `Option<bool>` for a different reason than `tools` above: there, `None` means nobody asked;
    /// here it means the question is meaningless. A cloud choice runs in somebody else's data
    /// centre and a hosted one is fetched over HTTP, so neither is "installed" or "not installed",
    /// and answering `Some(false)` for them would put a warning on most of the menu — the exact
    /// failure `tools`'s own doc argues against.
    ///
    /// Filled by `catalogue_with_installed` out of the `/api/tags` list its caller already read;
    /// never probed from here, for the reason that function's own doc gives.
    ///
    /// `#[serde(default)]` for the same reason `tools` carries one, and it matters more here: a
    /// `brain: local` row written into `~/.nucleos/nucleos-models.yaml` by hand names the model and
    /// nothing else, because whether it is pulled is not a fact a config file can assert.
    #[serde(default)]
    pub installed: Option<bool>,
}

/// `low | medium | high | xhigh | max`, exactly as `claude --help` documents them at CLI 2.1.198.
///
/// Ordered weakest-first, and the window shows them in this order: the list is a dial, and a dial
/// whose order is not its magnitude is one people read backwards.
///
/// The DEFAULT for a Claude choice that names none of its own, and the fallback when the catalogue
/// is empty. Not the law: a choice's own `efforts` outranks it, because the CLIs disagree about
/// what levels exist and one hard-coded list cannot be right for both.
pub const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Whether a string is an effort level this daemon will pass on.
///
/// Checked at the API door and not in the column, so a level a CLI later adds needs no migration.
/// A CHECK constraint would make the daemon unable to STORE a level it was told, which is worse
/// than storing one the CLI refuses: the first loses what somebody said, the second says so loudly.
///
/// Against the UNION of the catalogue's levels and not against the chosen model's own. The two
/// checks answer different questions and belong in different places: this one rejects nonsense at
/// the door — a typo, a level no model here has — while the picker offers only the levels the model
/// in front of you actually takes. The door cannot do the second job honestly anyway, because the
/// effort and the model can be set in separate requests and the model can change afterwards.
pub fn is_effort_level(config: &ModelsConfig, value: &str) -> bool {
    config.effort_levels().iter().any(|level| level == value)
        || config
            .assistant_choices
            .iter()
            .any(|choice| choice.efforts.iter().any(|level| level == value))
}

/// Loads the current models configuration, falling back to defaults on failure.
///
/// An unreadable or unparseable file is NOT the same as an absent one: absent means defaults, but a
/// typo in the YAML silently becoming defaults would drop a local routing and send turns to the
/// cloud. So the failure is logged with its path and cause, and the last configuration that did
/// parse keeps applying; only a daemon that never saw a good one falls back to defaults.
///
/// Handlers call this on the async path, so it costs one `metadata` call and re-reads the file only
/// when its length or modification time moved.
pub fn models_config_now() -> ModelsConfig {
    static CACHE: std::sync::Mutex<Option<ModelsConfigCache>> = std::sync::Mutex::new(None);
    match models_config_path() {
        Some(path) => cached_models_config(&path, &CACHE),
        None => ModelsConfig::default(),
    }
}

/// A parsed file and the (path, length, mtime) it was parsed at.
struct ModelsConfigCache {
    path: std::path::PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    config: ModelsConfig,
}

/// The stat-checked read behind [`models_config_now`]. A missing (or unstat-able) file answers the
/// default. A failed read or parse answers the last good configuration (the default if there is
/// none) and is never cached as a hit, so the next call tries again.
fn cached_models_config(
    path: &Path,
    cache: &std::sync::Mutex<Option<ModelsConfigCache>>,
) -> ModelsConfig {
    let Ok(meta) = std::fs::metadata(path) else {
        return ModelsConfig::default();
    };
    let (len, modified) = (meta.len(), meta.modified().ok());
    if let Ok(guard) = cache.lock()
        && let Some(hit) = guard.as_ref().filter(|hit| {
            hit.path == path && hit.len == len && modified.is_some() && hit.modified == modified
        })
    {
        return hit.config.clone();
    }
    let loaded = load_models_config(path);
    let ok = loaded.is_ok();
    let mut last_good = cache
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|hit| hit.config.clone()));
    let config = resolve_models_config(loaded, &mut last_good, path);
    if ok && let Ok(mut guard) = cache.lock() {
        *guard = Some(ModelsConfigCache {
            path: path.to_path_buf(),
            len,
            modified,
            config: config.clone(),
        });
    }
    config
}

/// The decision inside [`models_config_now`], apart from the file and the static so a test can drive it.
pub(super) fn resolve_models_config(
    loaded: std::io::Result<ModelsConfig>,
    last_good: &mut Option<ModelsConfig>,
    path: &Path,
) -> ModelsConfig {
    match loaded {
        Ok(config) => {
            *last_good = Some(config.clone());
            config
        }
        Err(error) => {
            tracing::error!(
                %error,
                path = %path.display(),
                "models config: could not be loaded; keeping the last good configuration"
            );
            last_good.clone().unwrap_or_default()
        }
    }
}

pub(super) fn default_assistant_choices() -> Vec<AssistantChoice> {
    ["Opus", "Sonnet", "Fable"]
        .into_iter()
        .map(|label| AssistantChoice {
            id: label.to_lowercase(),
            label: label.to_string(),
            brain: "cloud".to_string(),
            efforts: EFFORT_LEVELS
                .iter()
                .map(|level| level.to_string())
                .collect(),
            // Absent rather than `Some("claude")`: these are what an untouched install offers, and
            // an untouched install runs the Claude CLI. Writing it out would suggest the field is
            // required, and a config that named the other runner would then have to edit all three.
            runner: None,
            // Nothing has asked a cloud CLI's model whether it declares tools — `marked_with`
            // fills this in, and an untouched install has never called it.
            tools: None,
            installed: None,
        })
        .collect()
}

impl ModelsConfig {
    /// Returns the CLI configured for a cloud model id.
    pub fn runner_of(&self, id: &str) -> Option<&'static str> {
        self.assistant_choices
            .iter()
            .find(|choice| choice.brain == "cloud" && choice.id == id)
            .map(|choice| match choice.runner.as_deref() {
                Some("codex") => "codex",
                _ => "claude",
            })
    }

    /// Returns the model catalogue a chat is offered: both CLIs, whether or not it has a project.
    pub fn catalogue_for_chat(&self, installed: &[String]) -> Vec<AssistantChoice> {
        self.catalogue_scoped(installed, true)
    }

    /// The model a conversation runs on when it has pinned none — what the runner was built with.
    ///
    /// Reported beside the catalogue so the window can name the unpinned state instead of leaving
    /// it blank. It is deliberately NOT forced into the catalogue as an entry: `claude_model`
    /// defaults to `claude-sonnet-5` while the catalogue offers the alias `sonnet`, and listing
    /// both would put one model on the menu twice under two names.
    pub fn configured_model(&self) -> &str {
        if self.active_runner() == "codex" {
            &self.codex_model
        } else {
            &self.claude_model
        }
    }

    /// Every model a conversation may be moved to: the configured cloud list, then the local model
    /// if one is named.
    ///
    /// Delegates to `catalogue_with_installed(&[])` rather than keeping a second copy of this
    /// logic -- "an empty installed list behaves exactly as today" is then true by construction,
    /// not by two implementations happening to agree, and
    /// `um_api_tags_mudo_degrada_para_o_configurado_e_nunca_para_um_menu_vazio` /
    /// `sem_modelo_local_configurado_os_instalados_nao_entram_no_menu` both lean on that.
    pub fn catalogue(&self) -> Vec<AssistantChoice> {
        self.catalogue_with_installed(&[])
    }

    /// `catalogue()`'s menu, with the local entry widened to every model this machine has
    /// installed (`capabilities::installed_local_models` reads `/api/tags`; this function never
    /// touches the network itself, `installed` is just the names that call already produced).
    ///
    /// **Installed models join the menu only when `local_assistant_model` is configured.** This is
    /// not a detail: a picker offering a local model the assistant is not running would produce
    /// `NO_LOCAL_MODEL` at the first turn -- a refusal earned by nothing the person did wrong.
    /// `assistants::resolve_model` refuses `RouteNotConfigured` whatever a pin says when no model
    /// is configured for the route, so a machine with Ollama running and a dozen models pulled but
    /// no `local_assistant_model` in the file must show exactly today's menu -- no local entries at
    /// all. Everything below the local-entry block is unchanged from before this packet.
    ///
    /// The configured model always appears, whether or not `installed` names it: a model can be
    /// configured and not yet pulled, and this must not hide the one local entry the file asked
    /// for. An installed model that is ALSO the configured one contributes no second entry -- one
    /// id, one row: `installed` and the configured model are merged into one id list, then
    /// deduplicated.
    ///
    /// Sorted rather than left in `installed`'s own order: this daemon does not control what order
    /// `/api/tags` answers in, and a menu that reshuffles between two reads of the same machine is
    /// one nobody can build a habit of using -- the same argument `chats.rs`'s `subagents_from`
    /// already makes for sorting over trusting a map's order.
    ///
    /// `catalogue_with_installed(&[])` must equal `catalogue()` exactly, so an unreachable or quiet
    /// Ollama degrades to precisely today's behaviour rather than to some other empty state --
    /// trivially true here since `catalogue()` now calls this function with an empty slice.
    pub fn catalogue_with_installed(&self, installed: &[String]) -> Vec<AssistantChoice> {
        self.catalogue_scoped(installed, false)
    }

    /// Builds a model catalogue limited to one runner or open to every runner.
    pub(super) fn catalogue_scoped(
        &self,
        installed: &[String],
        every_runner: bool,
    ) -> Vec<AssistantChoice> {
        // Only the models belonging to the CLI this daemon was actually started with. The file may
        // hold both lists — `scripts/refresh-models.py` writes both when it can reach both — and
        // offering `sonnet` to a daemon running Codex would produce a turn that dies at spawn.
        //
        // Unknown names fall to `claude`, matching `main.rs`: a typo in `primary_runner` there
        // keeps the proven path rather than switching binaries, and the menu must agree with it or
        // it would offer models for a CLI that is not running.
        let active = self.active_runner();
        let mut choices = self.assistant_choices.clone();
        // Cloud only. The local route is the same whichever CLI is configured, so filtering it by
        // the runner would hide a working model for a reason that has nothing to do with it.
        choices.retain(|choice| {
            choice.brain != "cloud"
                || every_runner
                || choice.runner.as_deref().unwrap_or("claude") == active
        });
        // A file that says nothing about the running CLI would otherwise produce an empty menu.
        // The configured model always works — it is what the runner was built with.
        if choices.iter().all(|choice| choice.brain != "cloud") {
            choices.insert(
                0,
                AssistantChoice {
                    id: self.configured_model().to_string(),
                    label: self.configured_model().to_string(),
                    brain: "cloud".to_string(),
                    efforts: Vec::new(),
                    runner: Some(active.to_string()),
                    // An agent CLI's capabilities are never discovered from here — see
                    // `capabilities::declared_for_cli` — so this entry is never marked by
                    // `marked_with` either.
                    tools: None,
                    installed: None,
                },
            );
        }
        // Which server the local route resolves to, asked once for both questions below: whether
        // these rows belong on the menu at all, and whether "is it installed?" is even a question
        // this engine's server can be asked.
        let local_route = self.local_engine();
        if let (Some(local), Ok(resolved)) = (&self.local_assistant_model, local_route.as_ref()) {
            // `installed` answers one question -- "has `ollama pull` fetched this?" -- and it is
            // asked of `/api/tags`, which only Ollama serves. On any other engine the question
            // stops applying exactly as it never applied to a cloud row, and `installed`'s own doc
            // says `None` is what that means. `Some(false)` would hang a "not installed" warning on
            // every local row whose one obvious repair -- `ollama pull` -- has nothing to do with
            // the server actually serving them.
            let pulling_is_a_question = resolved.engine == LocalEngine::Ollama;
            // The file's own `brain: local` rows are marked IN PLACE rather than regenerated, so a
            // row written with a readable label ("Llama 3.2 3B") keeps it instead of being replaced
            // by its bare id. Their whole point is naming models this machine may NOT have yet:
            // Ollama has no endpoint that enumerates what is pullable, so a model nobody has
            // installed can only reach the menu by somebody writing it down.
            for choice in choices.iter_mut().filter(|choice| choice.brain == "local") {
                choice.installed = pulling_is_a_question.then(|| installed.contains(&choice.id));
            }
            let listed: Vec<String> = choices
                .iter()
                .filter(|choice| choice.brain == "local")
                .map(|choice| choice.id.clone())
                .collect();
            // Merge the configured model into `installed`, drop whatever the file already listed,
            // then dedupe and sort so the result is independent of both the input order and of
            // whether the configured model was already in the list -- one id, one row, in an order
            // that does not depend on Ollama's own.
            let mut local_ids: Vec<String> = installed.to_vec();
            if !local_ids.contains(local) {
                local_ids.push(local.clone());
            }
            local_ids.retain(|id| !listed.contains(id));
            local_ids.sort();
            local_ids.dedup();
            for id in local_ids {
                // Read BEFORE `id` is moved into `label` below, and against the caller's list
                // rather than against the configured name: the configured model is the one entry
                // that appears whether or not it is pulled, so it is exactly the one that must be
                // allowed to say `false`.
                let pulled = pulling_is_a_question.then(|| installed.contains(&id));
                choices.push(AssistantChoice {
                    id: id.clone(),
                    label: id,
                    brain: "local".to_string(),
                    // Ollama has no effort dial. An empty list rather than a flag, so the window
                    // reads one thing — what levels are on offer — and never a rule about routes.
                    efforts: Vec::new(),
                    runner: None,
                    // Unmarked here: `catalogue_with_installed` never touches the network, so
                    // whether THIS local entry declares tools is `marked_with`'s job, over what
                    // discovery actually found — never guessed at build time.
                    tools: None,
                    installed: pulled,
                });
            }
        } else {
            // Two causes, one consequence, and the same argument behind both: a menu must not
            // offer a route that can only earn a refusal.
            //
            // No model configured is the switch, exactly as the hosted block below states it:
            // `local_assistant_model` is what turns this route on, and `assistants::resolve_model`
            // refuses `RouteNotConfigured` whatever a pin says while it is absent. So a file that
            // lists local models with no route configured lists nothing the picker may offer.
            //
            // A REFUSED engine -- unknown name, `openai_compatible` with no address, an address off this
            // machine -- is the second, and dropping the rows is half the fix rather than an
            // extra: refusing only at `local_engine()` would leave the picker offering the models,
            // so the person picks one, the turn dies, and nothing on screen connects that to the
            // line in `~/.nucleos/nucleos-models.yaml` that caused it.
            choices.retain(|choice| choice.brain != "local");
        }
        match &self.hosted_assistant_model {
            // `hosted_assistant_model` is the SWITCH for this route, not merely its default:
            // `assistants::resolve_model` refuses `RouteNotConfigured` whatever a pin says when the
            // route's own config key is absent, so a file listing hosted models with no key
            // configured must show none of them. Offering one would be a refusal earned by nothing
            // the person did -- the same argument the local block above makes for installed models
            // staying off the menu until `local_assistant_model` names one.
            None => choices.retain(|choice| choice.brain != "openrouter"),
            // The file's own `brain: openrouter` rows are already in `choices` (they arrived with
            // `assistant_choices` and the `retain` above only filters `cloud`), so this adds the
            // CONFIGURED model and nothing else -- and only when the file did not already list it.
            // One id, one row, exactly as the local block deduplicates its own.
            Some(hosted)
                if choices
                    .iter()
                    .any(|choice| choice.brain == "openrouter" && &choice.id == hosted) => {}
            Some(hosted) => {
                choices.push(AssistantChoice {
                    // GENERATED from the configured model rather than left to the file, because this is
                    // what answers a chat that pins nothing: it must be pickable whether or not the
                    // file also lists it, the same reason the configured LOCAL model always appears
                    // whether or not `installed` names it.
                    //
                    // This used to be the ONLY hosted entry, and the reason given was that a
                    // hand-written one "could name a model the daemon never built a client for, and
                    // then a person picks one model and a different one answers, silently". That was
                    // wrong about this code: `assistants::resolve_model` returns
                    // `pinned.unwrap_or(configured)` and `assistant_for` builds the `OpenAiCompatibleChat`
                    // out of that resolved name, so the client is built PER TURN from the pick. The
                    // model on the wire is the one that was picked --
                    // `assistants.rs`'s `a_pinned_hosted_model_beats_the_configured_one_on_the_hosted_route`
                    // is the proof, at the seam where the silent swap would have happened.
                    id: hosted.clone(),
                    label: hosted.clone(),
                    brain: "openrouter".to_string(),
                    // OpenRouter is not one model behind one dial: some of the models it fronts take a
                    // reasoning effort and some do not, and a name alone does not tell this daemon
                    // which. Unlike the local entries' "Ollama has none", this is "unknown from here" --
                    // and an empty list is the only honest answer to that, so the window offers no dial
                    // for this route rather than guessing one that might not exist for the model named.
                    efforts: Vec::new(),
                    // Not filtered by `active_runner()` and never spawned as either CLI:
                    // `OpenAiCompatibleChat` is reached over HTTP, so which agent CLI is installed has
                    // nothing to do with whether this entry belongs on the menu.
                    runner: None,
                    // Unmarked here for the same reason the local entry above is: this function never
                    // touches the network, so whether OpenRouter's catalogue declares tools for this
                    // model is `marked_with`'s job, not a guess made at build time.
                    tools: None,
                    installed: None,
                });
            }
        }
        choices
    }

    /// Which agent CLI this daemon runs. Unknown names fall to `claude`, exactly as `main.rs` does.
    pub fn active_runner(&self) -> &str {
        match self.primary_runner.as_deref() {
            Some("codex") => "codex",
            _ => "claude",
        }
    }

    /// Every effort level any model on the menu takes, weakest-first, without repeats.
    ///
    /// The union, and only for the door's typo check — the picker offers each model its own list.
    /// Ordered by first appearance rather than sorted, because these are magnitudes and not words:
    /// the strongest model's list is the longest, so walking them in order puts `low` before `max`
    /// and never alphabetically between them.
    pub fn effort_levels(&self) -> Vec<String> {
        let mut levels: Vec<String> = Vec::new();
        for choice in self.catalogue() {
            for level in choice.efforts {
                if !levels.contains(&level) {
                    levels.push(level);
                }
            }
        }
        if levels.is_empty() {
            levels = EFFORT_LEVELS
                .iter()
                .map(|level| level.to_string())
                .collect();
        }
        levels
    }

    /// PURE: which local server a local turn is sent to, or why the route may not run at all.
    ///
    /// Sync, and it touches no network: `catalogue()` is sync and pure and this is what it asks, so
    /// "with no Ollama on this machine nothing changes" stays true by construction rather than by
    /// somebody remembering not to probe. Nothing here asks a port what is listening on it — the
    /// only questions are which engine the file names and whether the address it gives is this
    /// machine's, and both are answerable from the file alone.
    ///
    /// Three refusals and no fallbacks, each argued at the field it reads: `UnknownEngine` at
    /// `local_engine`, `NoBaseUrl` at `local_base_url`, `NotLoopback` at both. Absence is the one
    /// thing that resolves rather than refuses, and it resolves to the engine and address this
    /// daemon has always used.
    pub fn local_engine(&self) -> Result<ResolvedLocalEngine, LocalEngineRefusal> {
        let engine = match self.local_engine.as_deref() {
            // Absent is today's engine, never a new default nobody chose.
            None | Some("ollama") => LocalEngine::Ollama,
            Some("openai_compatible") => LocalEngine::OpenAiCompatible,
            Some(other) => return Err(LocalEngineRefusal::UnknownEngine(other.to_string())),
        };
        let base_url = match (&self.local_base_url, engine) {
            (Some(written), _) => written.clone(),
            // Read from the runner's own constant, so the default address and the address the
            // runner posts to cannot drift apart without a test failing.
            (None, LocalEngine::Ollama) => crate::execution::runner::OLLAMA_BASE_URL.to_string(),
            (None, LocalEngine::OpenAiCompatible) => return Err(LocalEngineRefusal::NoBaseUrl),
        };
        if !is_loopback_url(&base_url) {
            // Carried verbatim rather than described: a refusal that paraphrases the address it
            // refused cannot tell its reader which line to go and edit.
            return Err(LocalEngineRefusal::NotLoopback(base_url));
        }
        Ok(ResolvedLocalEngine {
            engine,
            base_url,
            declared_context_tokens: self.local_context_tokens,
        })
    }
}

/// Which server answers the local route. Two values and not a boolean, for the reason
/// `trust::Requester` gives: `is_ollama: bool` reads fine here and terribly at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalEngine {
    /// The engine this daemon has always used, and what an untouched config file still gets.
    Ollama,
    /// Any OpenAI-compatible server on this machine — llama.cpp, LM Studio, vLLM. Named for the
    /// wire protocol and not for the vendor: nothing about this route leaves the loopback.
    OpenAiCompatible,
}

/// A local route that may actually run: which server, at which address, with whatever window the
/// file declared for it.
///
/// The resolved address travels WITH the engine rather than being re-derived by each caller,
/// because deriving it twice is how a turn ends up posted somewhere the menu never approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLocalEngine {
    pub engine: LocalEngine,
    /// Checked loopback before this value existed — a `ResolvedLocalEngine` is never off-machine.
    pub base_url: String,
    /// `None` means nobody declared one, never "zero" and never a guess. Only
    /// `local_context_tokens` can put a number here.
    pub declared_context_tokens: Option<usize>,
}

/// Why a local route may not run. Every arm names the config key that repairs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEngineRefusal {
    /// `local_engine` names something neither engine answers to, carried verbatim.
    UnknownEngine(String),
    /// `local_engine: openai_compatible` with no address — the one case where a default would be a guessed
    /// port rather than a remembered one.
    NoBaseUrl,
    /// The resolved address is somewhere other than this machine, carried verbatim so the message
    /// and the menu can both name it.
    NotLoopback(String),
}

impl LocalEngineRefusal {
    /// What an operator is told, in the voice of this crate's other refusals: the fault, then the
    /// key in `~/.nucleos/nucleos-models.yaml` that fixes it.
    ///
    /// Naming the key is the whole point. A refusal that only describes the fault sends its reader
    /// looking for which line produced it, and "local model refused" reads like a defect in the
    /// daemon rather than a line somebody wrote.
    ///
    /// `String` and not `&'static str` like `LandRefusal::message`, because two of the three carry
    /// the offending value and a refusal that cannot quote it is one nobody can act on.
    pub fn message(&self) -> String {
        match self {
            Self::UnknownEngine(named) => format!(
                "`local_engine: {named}` names no local engine this daemon serves; write `ollama` \
                 or `openai_compatible` in {MODELS_CONFIG_DISPLAY_PATH}, or remove the key to keep Ollama"
            ),
            Self::NoBaseUrl => format!(
                "`local_engine: openai_compatible` needs a `local_base_url` in {MODELS_CONFIG_DISPLAY_PATH}: no port \
                 is guessed here, because the only port worth guessing is Ollama's and a turn sent \
                 to it would reach whatever is listening there"
            ),
            Self::NotLoopback(refused) => format!(
                "`local_base_url: {refused}` is not on this machine, and the local route only ever \
                 talks to the loopback; fix `local_base_url` in {MODELS_CONFIG_DISPLAY_PATH}, or choose a \
                 hosted route deliberately"
            ),
        }
    }
}

/// PURE: whether an address is served by this machine, decided on the PARSED host and never on the
/// string.
///
/// `url::Url` and not a prefix test: `http://127.0.0.1.example.com/v1` BEGINS with the loopback
/// address and belongs to whoever registered that domain, and every hand-rolled extractor gets
/// that wrong at least once. Closing that hole is this function's entire job. The crate costs
/// this module nothing to reach for: `core/Cargo.toml` already declared `url` for `trust.rs`,
/// which refuses to parse a URL by hand for exactly the same reason, and that line predates this
/// function.
///
/// `127.0.0.0/8` whole and not `127.0.0.1` alone — a server bound to `127.0.0.2` is just as much on
/// this machine — plus the IPv6 loopback and the name `localhost`, which resolves to one of them.
/// Any other hostname is refused even though a resolver might point it at 127.0.0.1 today: this
/// question is answered once, at the config, and a name's answer can change between here and the
/// connection.
///
/// Scheme-checked too. `file://` and `ws://` parse happily and are not what `LocalChat` posts to.
pub fn is_loopback_url(raw: &str) -> bool {
    let Ok(parsed) = url::Url::parse(raw) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    match parsed.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// PURE: the menu, with each local choice marked by what its model was found to declare.
///
/// A model absent from `declared` stays `None` — nothing is ever marked `Some(false)` on a guess;
/// `AssistantChoice::tools`'s own doc is the reason why.
pub fn marked_with(
    choices: Vec<AssistantChoice>,
    declared: &std::collections::HashMap<String, crate::capabilities::Declared>,
) -> Vec<AssistantChoice> {
    choices
        .into_iter()
        .map(|mut choice| {
            choice.tools = declared.get(&choice.id).map(|found| found.tools);
            choice
        })
        .collect()
}

impl Default for ModelsConfig {
    fn default() -> Self {
        ModelsConfig {
            claude_model: "claude-sonnet-5".to_string(),
            codex_model: "gpt-5.6-terra".to_string(),
            local_triage_model: None,
            voice_cleanup_model: None,
            primary_runner: None,
            plan_model: None,
            review_model: None,
            resolve_model: None,
            resolve_effort: None,
            local_assistant_model: None,
            hosted_assistant_model: None,
            local_engine: None,
            local_base_url: None,
            local_context_tokens: None,
            embedding_model: default_embedding_model(),
            assistant_choices: default_assistant_choices(),
            prices_as_of: None,
            model_prices: std::collections::BTreeMap::new(),
            raw_model_prices: serde_yaml::Value::Null,
            price_warnings: Vec::new(),
        }
    }
}

/// The small Ollama embedding model an untouched install uses.
pub const DEFAULT_EMBEDDING_MODEL: &str = "nomic-embed-text";

pub(super) fn default_embedding_model() -> String {
    DEFAULT_EMBEDDING_MODEL.to_string()
}

pub(super) fn deserialize_optional_model<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty()))
}

/// The file the pinned model names live in, relative to [`crate::machine_config::root`].
///
/// A constant because several places need it and they must not drift: startup builds the runner
/// from this file, and `GET /assistant/models` re-reads it per request so a choice added to it works
/// without a restart. The second reader is the reason it stopped being a literal in `main.rs`.
pub const MODELS_CONFIG_FILE: &str = "nucleos-models.yaml";

/// The same file as a person is shown it, and the only spelling a message uses. See
/// [`crate::machine_config::ROOT_DISPLAY`].
pub const MODELS_CONFIG_DISPLAY_PATH: &str = "~/.nucleos/nucleos-models.yaml";

/// One model's published prices. A `None` field is a price the script could not read: tokens of that
/// kind make the model "unpriced", never priced at zero.
///
/// The top-level rates are the base tier, which is all `price_of` callers that predate tiers see.
/// The rest is what the vendors' tables say beyond one flat rate (read 2026-10-18):
/// - `tiers`: Anthropic prices Claude Haiku 5.5 at $0.10 in / $0.50 out up to 100,000 prompt tokens
///   and $0.50 / $2.50 above, cache rates scaling with it. The tier keys on ONE request's prompt
///   length (input + cache reads + cache writes) and the higher price applies to the whole request.
///   OpenRouter publishes the same shape as `pricing.overrides[].min_prompt_tokens`.
/// - `cache_write` is the 5-minute write (1.25x input at Anthropic), `cache_write_1h` the 1-hour one
///   (2x input); both vendors publish them apart.
/// - `fast`: the request's `speed: fast` rate (Opus 5.5: $8 / $40).
/// - `geo_us_mult`: `inference_geo: "us"` multiplies every category (1.1 at Anthropic).
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
pub struct ModelPrice {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    /// The 5-minute cache write.
    #[serde(default)]
    pub cache_write: Option<f64>,
    /// The 1-hour cache write.
    #[serde(default)]
    pub cache_write_1h: Option<f64>,
    /// Prompt-length tiers over the base rates, in any order; see [`ModelPrice::rates_for`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<PriceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<FastPrice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_us_mult: Option<f64>,
    /// `anthropic` or `openrouter`; informational.
    #[serde(default)]
    pub source: Option<String>,
}

/// The rates for requests whose prompt is at least `min_prompt_tokens`, OpenRouter's own key and
/// meaning; Anthropic's "for prompts over 100,000 tokens" is written as 100,001. A tier carries only
/// the rates it overrides; a `None` rate is the base one.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
pub struct PriceTier {
    pub min_prompt_tokens: u64,
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_write_1h: Option<f64>,
}

/// Fast mode's input and output rates. Caching multipliers apply on top of them.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
pub struct FastPrice {
    pub input: f64,
    pub output: f64,
}

/// The rates one request is billed at, USD per million tokens; `None` is unpriced.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub cache_write_1h: Option<f64>,
}

impl Rates {
    fn scaled(self, by: f64) -> Rates {
        let times = |rate: Option<f64>| rate.map(|r| r * by);
        Rates {
            input: times(self.input),
            output: times(self.output),
            cache_read: times(self.cache_read),
            cache_write: times(self.cache_write),
            cache_write_1h: times(self.cache_write_1h),
        }
    }
}

impl ModelPrice {
    /// The rates for ONE request: `prompt_tokens` is its input + cache reads + cache writes, `fast` its
    /// `speed: fast`, `geo_us` its `inference_geo: "us"`.
    ///
    /// The tier is the one with the highest threshold the prompt reaches, overlaid on the base rates.
    /// Fast replaces input and output and scales the cache rates by the same factor; residency then
    /// multiplies everything. A mode the entry has no price for answers every rate `None`: billing a
    /// fast or US-resident request at the standard rate is a wrong cost, and a wrong cost is worse
    /// than none (spec devtime §4.4).
    pub fn rates_for(&self, prompt_tokens: u64, fast: bool, geo_us: bool) -> Rates {
        let mut rates = Rates {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            cache_write_1h: self.cache_write_1h,
        };
        if let Some(tier) = self
            .tiers
            .iter()
            .filter(|tier| prompt_tokens >= tier.min_prompt_tokens)
            .max_by_key(|tier| tier.min_prompt_tokens)
        {
            rates = Rates {
                input: tier.input.or(rates.input),
                output: tier.output.or(rates.output),
                cache_read: tier.cache_read.or(rates.cache_read),
                cache_write: tier.cache_write.or(rates.cache_write),
                cache_write_1h: tier.cache_write_1h.or(rates.cache_write_1h),
            };
        }
        if fast {
            let Some(price) = &self.fast else {
                return Rates::default();
            };
            let factor = rates.input.filter(|r| *r > 0.0).map(|r| price.input / r);
            let cache = |rate: Option<f64>| rate.zip(factor).map(|(r, f)| r * f);
            rates = Rates {
                input: Some(price.input),
                output: Some(price.output),
                cache_read: cache(rates.cache_read),
                cache_write: cache(rates.cache_write),
                cache_write_1h: cache(rates.cache_write_1h),
            };
        }
        if geo_us {
            let Some(mult) = self.geo_us_mult else {
                return Rates::default();
            };
            rates = rates.scaled(mult);
        }
        rates
    }

    /// Every rate this entry carries, named, for validation: base, tiers, fast and the multiplier.
    fn named_rates(&self) -> Vec<(String, f64)> {
        let mut named = Vec::new();
        let mut push = |name: String, rate: Option<f64>| {
            if let Some(rate) = rate {
                named.push((name, rate));
            }
        };
        for (field, rate) in [
            ("input", self.input),
            ("output", self.output),
            ("cache_read", self.cache_read),
            ("cache_write", self.cache_write),
            ("cache_write_1h", self.cache_write_1h),
            ("geo_us_mult", self.geo_us_mult),
        ] {
            push(field.to_string(), rate);
        }
        for (index, tier) in self.tiers.iter().enumerate() {
            for (field, rate) in [
                ("input", tier.input),
                ("output", tier.output),
                ("cache_read", tier.cache_read),
                ("cache_write", tier.cache_write),
                ("cache_write_1h", tier.cache_write_1h),
            ] {
                push(format!("tiers[{index}].{field}"), rate);
            }
        }
        if let Some(fast) = &self.fast {
            push("fast.input".to_string(), Some(fast.input));
            push("fast.output".to_string(), Some(fast.output));
        }
        named
    }
}

/// Converts the file's raw `model_prices` value entry by entry: what parses and validates is kept,
/// everything else is dropped with a warning and a `"<id>: <reason>"` line in the second half.
fn convert_model_prices(
    raw: serde_yaml::Value,
) -> (std::collections::BTreeMap<String, ModelPrice>, Vec<String>) {
    let mut prices = std::collections::BTreeMap::new();
    let mut dropped = Vec::new();
    let entries = match raw {
        serde_yaml::Value::Null => return (prices, dropped),
        serde_yaml::Value::Mapping(entries) => entries,
        _ => {
            tracing::warn!("model_prices is not a mapping; every price ignored");
            dropped.push("model_prices: not a mapping of model id to price".to_string());
            return (prices, dropped);
        }
    };
    for (key, value) in entries {
        let Some(id) = key.as_str().map(str::to_string) else {
            tracing::warn!(key = ?key, "model_prices key is not a model id; entry dropped");
            dropped.push(format!("{key:?}: the key is not a model id"));
            continue;
        };
        // The mapping is checked here because serde would read a sequence positionally, as
        // `input, output, ...`, and price a list nobody meant as one.
        let parsed = if value.is_mapping() {
            serde_yaml::from_value::<ModelPrice>(value).map_err(|error| error.to_string())
        } else {
            Err("not a mapping of price fields".to_string())
        };
        let reason = match parsed {
            Err(reason) => reason,
            Ok(price) => match price
                .named_rates()
                .into_iter()
                .find(|(_, rate)| !rate.is_finite() || *rate < 0.0)
            {
                Some((field, _)) => format!("{field} is not a finite, non-negative number"),
                None => {
                    prices.insert(id, price);
                    continue;
                }
            },
        };
        // A wrong or unreadable price is a wrong cost. It drops that one model's entry (so the model
        // reads "unpriced") and nothing else: one bad line must not take model routing down with it
        // (owner, 2026-10-18).
        tracing::warn!(model = %id, %reason, "model_prices entry unreadable; entry dropped");
        dropped.push(format!("{id}: {reason}"));
    }
    (prices, dropped)
}

/// The second lookup `price_of` tries, and the key the script writes (spec devtime §4.4): lowercase,
/// no `[..]` context suffix, no `-YYYYMMDD`, no `-latest`, `.` -> `-` in `claude-*` ids. Mirrored by
/// `normalise_id` in `scripts/refresh-models.py`; change both or neither.
pub fn normalise_model_id(id: &str) -> String {
    let mut text = id.trim().to_ascii_lowercase();
    if text.ends_with(']')
        && let Some(open) = text.rfind('[')
    {
        text.truncate(open);
    }
    if let Some((head, tail)) = text.rsplit_once('-')
        && tail.len() == 8
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        text = head.to_string();
    }
    if let Some(head) = text.strip_suffix("-latest") {
        text = head.to_string();
    }
    if text.starts_with("claude-") {
        text = text.replace('.', "-");
    }
    text
}

impl ModelsConfig {
    /// Exact id, then normalised id. Never a prefix: a neighbour's price is a wrong cost, and a wrong
    /// cost is worse than none (spec devtime §4.4). An unversioned alias (`sonnet`) has no price.
    pub fn price_of(&self, model: &str) -> Option<&ModelPrice> {
        self.model_prices
            .get(model)
            .or_else(|| self.model_prices.get(&normalise_model_id(model)))
    }
}

/// Where the daemon reads [`MODELS_CONFIG_FILE`], or `None` when there is nowhere to read it from —
/// which every reader treats as an absent file, i.e. [`ModelsConfig::default`].
///
/// `None` under `cargo test` as well, and that is deliberate rather than a convenience: this file
/// used to resolve against the working directory, a test runs from `core/`, `core/.ai/` never
/// existed, and a whole family of tests was written against the defaults that absence produced.
/// Resolving it now would hand those tests whatever this machine's owner has configured — a local
/// model in one place, none in another — and a unit test must not read a real `~/.nucleos`.
pub fn models_config_path() -> Option<std::path::PathBuf> {
    // `testkit` too: the binary's tests (`http.rs`) build this library without `cfg(test)`.
    if cfg!(any(test, feature = "testkit")) {
        return None;
    }
    crate::machine_config::root().map(|root| root.join(MODELS_CONFIG_FILE))
}

/// `nucleos-models.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Unlike its seven neighbours this file's loader could already refuse, so splitting the parser out
/// buys no new strictness — it buys the write route a function with the shape every other claim's
/// validator has, and it keeps the door and the loader reading one grammar rather than two.
pub fn parse_models_config(contents: &str) -> Result<ModelsConfig, String> {
    let mut config =
        serde_yaml::from_str::<ModelsConfig>(contents).map_err(|error| error.to_string())?;
    let (prices, dropped) = convert_model_prices(std::mem::take(&mut config.raw_model_prices));
    config.model_prices = prices;
    config.price_warnings = dropped;
    Ok(config)
}

pub fn load_models_config(path: &Path) -> std::io::Result<ModelsConfig> {
    if !path.exists() {
        return Ok(ModelsConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    parse_models_config(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod stat_cache_tests {
    use super::*;

    #[test]
    fn an_edited_models_file_is_re_read_and_an_unchanged_one_is_served_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MODELS_CONFIG_FILE);
        let cache = std::sync::Mutex::new(None);

        std::fs::write(
            &path,
            "claude_model: opus
codex_model: gpt-a
",
        )
        .unwrap();
        let first = cached_models_config(&path, &cache);
        assert_eq!(first.claude_model, "opus");
        assert_eq!(cached_models_config(&path, &cache), first);

        // A different length, so the check holds even on a coarse-mtime filesystem.
        std::fs::write(
            &path,
            "claude_model: sonnet-long
codex_model: gpt-a
",
        )
        .unwrap();
        assert_eq!(
            cached_models_config(&path, &cache).claude_model,
            "sonnet-long"
        );
    }

    #[test]
    fn a_missing_models_file_keeps_the_default_and_a_bad_one_is_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MODELS_CONFIG_FILE);
        let cache = std::sync::Mutex::new(None);

        assert_eq!(cached_models_config(&path, &cache), ModelsConfig::default());

        std::fs::write(
            &path,
            "claude_model: [not, a, string
",
        )
        .unwrap();
        assert_eq!(cached_models_config(&path, &cache), ModelsConfig::default());
        assert!(cache.lock().unwrap().is_none());
    }
}
