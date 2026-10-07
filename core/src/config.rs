use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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
pub fn models_config_now() -> ModelsConfig {
    models_config_path()
        .and_then(|path| load_models_config(&path).ok())
        .unwrap_or_default()
}

fn default_assistant_choices() -> Vec<AssistantChoice> {
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

    /// Returns the model catalogue available to a chat's rooted state.
    pub fn catalogue_for_chat(&self, installed: &[String], rooted: bool) -> Vec<AssistantChoice> {
        let mut choices = self.catalogue_scoped(installed, rooted);
        if !rooted {
            choices.retain(|choice| {
                choice.brain != "cloud" || choice.runner.as_deref() != Some("codex")
            });
        }
        choices
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
    fn catalogue_scoped(&self, installed: &[String], every_runner: bool) -> Vec<AssistantChoice> {
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
            (None, LocalEngine::Ollama) => crate::runner::OLLAMA_BASE_URL.to_string(),
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
#[cfg_attr(not(test), allow(dead_code))]
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
/// `AssistantChoice::tools`'s own doc is the reason why. No production caller until the model
/// picker's door wires `capabilities::DiscoveryCache`'s findings onto the menu — GREEN's job, not
/// this phase's; `o_catalogo_marca_quem_nao_declara_ferramentas` and its sibling below exercise it
/// in the meantime.
#[cfg_attr(not(test), allow(dead_code))]
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
        }
    }
}

/// The small Ollama embedding model an untouched install uses.
pub const DEFAULT_EMBEDDING_MODEL: &str = "nomic-embed-text";

fn default_embedding_model() -> String {
    DEFAULT_EMBEDDING_MODEL.to_string()
}

fn deserialize_optional_model<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
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
    serde_yaml::from_str::<ModelsConfig>(contents).map_err(|error| error.to_string())
}

pub fn load_models_config(path: &Path) -> std::io::Result<ModelsConfig> {
    if !path.exists() {
        return Ok(ModelsConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    parse_models_config(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `~/.nucleos/email.yaml` (spec §3.4). Every field has a default, so a partial file is valid and an
/// absent one switches the pillar off in silence rather than blocking startup.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EmailConfig {
    /// Opt-in. The pillar reads a real mailbox, so nothing about it starts by accident.
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    /// The submission host, kept apart from `host` above because reading a mailbox and posting a
    /// message to it are two different servers as often as they are one — `imap.gmail.com` and
    /// `smtp.gmail.com` being the case most people are in. Empty is the shipped state and means
    /// sending is unconfigured: the send route answers 503 rather than guessing a host from the
    /// IMAP one, because a guessed submission server either refuses the login or, far worse,
    /// belongs to somebody else.
    pub smtp_host: String,
    /// 465 (implicit TLS) rather than 587 (STARTTLS): the pillar's premise is that a message the
    /// mailbox's owner cannot recall does not leave this machine in the clear, and only one of the
    /// two is encrypted before the first byte of the conversation.
    pub smtp_port: u16,
    pub username: String,
    pub mailbox: String,
    pub sent_mailbox: Option<String>,
    pub poll_interval_secs: u64,
    /// Which triage classes are worth interrupting a person for.
    ///
    /// An EMPTY list is a valid value meaning "notify about nothing" — only an ABSENT key gives
    /// the `["urgent"]` default. The distinction carries the rollout's first week (spec §9), where
    /// the whole point is to watch the classifier without being paged by it; treating `[]` as
    /// absent would notify from day one and burn the calibration ramp.
    pub notify_classes: Vec<String>,
    /// Hour (UTC) the daily digest is emitted. Validated to `0..=21` so its two-hour window cannot
    /// straddle midnight and emit twice (§6.3).
    pub digest_hour_utc: u8,
    /// How long a classified message keeps its body (§7.2). Defaults to 14 rather than 0 because
    /// the pillar's first state after being switched on is always the calibration week, which is
    /// exactly when the bodies are needed. Capped at 30, where row pruning removes the row anyway.
    pub retain_bodies_days: u8,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            username: String::new(),
            mailbox: "INBOX".to_string(),
            sent_mailbox: None,
            poll_interval_secs: 300,
            notify_classes: vec!["urgent".to_string()],
            digest_hour_utc: 7,
            retain_bodies_days: 14,
        }
    }
}

impl EmailConfig {
    /// Clamps the two validated fields, warning about what it changed. Out-of-range values are
    /// corrected rather than fatal, for the same reason the whole file is: nothing in this config
    /// is worth refusing to start the daemon over.
    fn validated(mut self) -> Self {
        if self.digest_hour_utc > 21 {
            tracing::warn!(
                digest_hour_utc = self.digest_hour_utc,
                "email config: digest_hour_utc must be 0..=21 so the window cannot cross midnight; using 7"
            );
            self.digest_hour_utc = 7;
        }
        if self.retain_bodies_days > 30 {
            tracing::warn!(
                retain_bodies_days = self.retain_bodies_days,
                "email config: retain_bodies_days must be 0..=30; using 30"
            );
            self.retain_bodies_days = 30;
        }
        self
    }
}

/// `~/.nucleos/email.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_email_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in an optional pillar's config cannot stop the daemon from starting. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_email_config(contents: &str) -> Result<EmailConfig, String> {
    serde_yaml::from_str::<EmailConfig>(contents)
        .map(EmailConfig::validated)
        .map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/email.yaml`. Absent or unreadable → defaults, with a warning; never an error, so a
/// typo in an optional pillar's config cannot stop the daemon from starting.
pub fn load_email_config(path: &Path) -> EmailConfig {
    if !path.exists() {
        return EmailConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_email_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "email config: could not be parsed; the pillar stays off");
            EmailConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "email config: could not be read; the pillar stays off");
            EmailConfig::default()
        }
    }
}

/// What the local model is told to do with a raw transcript.
///
/// The four prohibitions are the load-bearing part, and each answers a measured failure of a small
/// model rather than a hypothetical one. Local triage found a 4B model inventing deadlines nobody
/// wrote and inverting who was asking whom, which two forbidding sentences fixed. The third is
/// specific to dictation: a dictated sentence is often a question, and a small model handed a question
/// ANSWERS it — without that line, saying "will this compile?" pastes an opinion about compilation
/// instead of the words that were said.
///
/// The fourth came out of §14.6, from measurement rather than review: the first three forbid adding,
/// distorting and answering, and none of them forbids REWRITING. Asked to tidy "prune dictations
/// after 7 days", the model returned "pruning dictations after seven days" — reproducibly, three
/// times out of three. It had added nothing and changed no meaning, so every existing rule was
/// satisfied; it had also restructured the sentence and spelled out the numeral, which for dictation
/// is the whole failure. Someone dictating a config value, a version, a time or a path needs the
/// characters they said, not a well-phrased paraphrase of them.
pub const DEFAULT_CLEANUP_PROMPT: &str =
    "Tidy the transcript below. Fix punctuation, capitalisation, \
and obvious speech-to-text errors. Remove filler words and false starts.
Do NOT add any information that is not in the transcript.
Do NOT change the meaning, the tone, or who is asking whom.
Do NOT answer, summarise, or comment on the content — you are an editor, not a reader.
Do NOT rephrase or restructure sentences that are already clear, and keep numbers, dates, \
versions, units and paths exactly as they were said — digits stay digits.
Keep the original language. Return only the corrected text.";

/// The chords the pillar ships with when `~/.nucleos/voice.yaml` names none, per platform.
///
/// macOS is the reason this is a constant rather than three literals in `Default`: the
/// `Ctrl+Alt` family is not free there. Cmd+Space is Spotlight, Ctrl+Space switches the input
/// source, Cmd+Option+Space opens Finder search, Ctrl+Cmd+Space is the Character Viewer, and
/// Ctrl+Option is the modifier pair VoiceOver reserves for itself; a global Cmd+Shift+letter
/// would steal an application shortcut in every application at once. Three modifiers held
/// together reach no default macOS binding, which is why that arm adds Command — a chord the
/// desktop already owns does not fail loudly, it simply never reaches this app, and on screen
/// that is indistinguishable from dictation being broken.
///
/// The spelling is `Super` and not `Cmd` because `global-hotkey`'s parser (`hotkey.rs:205`)
/// accepts "COMMAND" | "CMD" | "SUPER" as one and the same modifier, so this name parses and
/// reads the same on every host.
///
/// **UNVERIFIED on a real Mac.** The collision list above is read from Apple's documented
/// shortcuts, not pressed: CI compiles this arm, and only a person on a Mac can confirm that
/// the four chords it ships are free.
#[cfg(target_os = "macos")]
const DEFAULT_HOTKEYS: [&str; 4] = [
    "Ctrl+Alt+Super+Space",
    "Ctrl+Alt+Super+M",
    "Ctrl+Alt+Super+C",
    "Ctrl+Alt+Super+N",
];

/// Windows and Linux leave the `Ctrl+Alt` family alone, so the shorter chords stay — see the
/// macOS arm above for why that platform needs a third modifier.
#[cfg(not(target_os = "macos"))]
const DEFAULT_HOTKEYS: [&str; 4] = ["Ctrl+Alt+Space", "Ctrl+Alt+M", "Ctrl+Alt+C", "Ctrl+Alt+N"];

/// `~/.nucleos/voice.yaml`. Every field defaults, so a partial file is valid and an absent one leaves the
/// pillar off without comment.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VoiceConfig {
    /// Opt-in, like the email pillar: this one opens a microphone.
    pub enabled: bool,
    /// Split into program + args, with the audio path appended last — the contract the Telegram
    /// sidecar's transcriber already uses. Whitespace separates; double quotes protect a path that
    /// contains spaces, which anything installed under `C:\Program Files` needs. Empty means there is
    /// no transcriber, which is indistinguishable from the pillar being off and is treated as such.
    pub stt_command: String,
    /// A resident transcriber on loopback, e.g. `http://127.0.0.1:5018` for whisper.cpp's own
    /// `whisper-server`.
    ///
    /// **Preferred over `stt_command` when both are set**, for the reason `tts_url` is preferred over
    /// `tts_command`, and by a wider margin than that one. Measured here 2026-09-20 with
    /// `ggml-small` on an RTX 3060: a 1.3 s clip costs 1250 ms spawned and 96 ms resident, a 5.6 s
    /// clip 1210 ms against 191 ms. The floor is identical for a one-second clip and a five-second
    /// one because what the spawning path pays for is loading 487 MB of model and waking CUDA, not
    /// transcribing. `transcribe.rs` carries the table.
    ///
    /// That floor is why this key exists at all: progressive dictation re-transcribes the sentence
    /// in flight as it is spoken, and a 1.2 s floor per revision is not progressive.
    pub stt_url: String,
    /// Split into program + args exactly as `stt_command` is, but the text goes on STDIN and a WAV
    /// comes back on STDOUT — `speak.rs` explains why the two contracts differ. Empty means the
    /// núcleo has no voice, which is a smaller loss than having no transcriber: conversation still
    /// works, it just answers in writing.
    pub tts_command: String,
    /// A resident engine on loopback, e.g. `http://127.0.0.1:5017` for Piper's own HTTP server.
    ///
    /// **Preferred over `tts_command` when both are set**, because the difference is not marginal:
    /// measured here, spawning costs ~2.8 s of model loading per sentence against ~0.2 s for the
    /// resident server. `speak.rs` carries the numbers. Somebody who configured both meant the one
    /// that works, so this wins rather than erroring — but it says so in the log, because silently
    /// ignoring a line somebody wrote is how a config file stops being believed.
    pub tts_url: String,
    pub hotkey: String,
    pub memo_hotkey: String,
    /// Toggles hands-free conversation mode. A third chord and not a mode of the first, because the
    /// two do opposite things with the same recording: dictation pastes it into whatever had focus,
    /// conversation sends it to the agent. A single key that guessed between them would guess wrong
    /// in the direction that types a question into a terminal.
    pub conversation_hotkey: String,
    /// Opens the Brain capture box, where the owner types a note into the knowledge store. A chord
    /// of its own because it neither records nor talks: folding it into one of the other three
    /// would make a keystroke that only shows a text box start a microphone.
    pub capture_hotkey: String,
    /// Dictations are a searchable record of everything said, in a pillar whose first requirement is
    /// privacy, so they expire. Memos do not: those are documents somebody asked for.
    pub retain_dictations_days: u8,
    /// Terms said often and heard badly. Applied twice on purpose — as decoding bias and as a
    /// deterministic pass — so a term is fixed even when the bias was not enough.
    pub hints: Vec<String>,
    /// What ends a turn, now that silence does not. A list and not a literal because the spelling a
    /// transcriber returns is a measurement, not a decision: `-l auto` picks a language per segment,
    /// so the same spoken word comes back differently depending on what whisper thought it heard.
    /// Phase 1 measures those spellings and they are added here.
    pub closing_words: Vec<String>,
    /// What throws the accumulated turn away. Two tokens, matched as two — `risca` alone is a verb
    /// somebody says about code.
    pub discard_phrase: String,
    /// What confirms a discard. Throwing away three minutes of thinking on one misheard phrase is
    /// the expensive mistake in this pair, so it takes two utterances and not one.
    pub confirm_words: Vec<String>,
    pub cleanup_prompt: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            stt_command: String::new(),
            stt_url: String::new(),
            tts_command: String::new(),
            tts_url: String::new(),
            hotkey: DEFAULT_HOTKEYS[0].to_string(),
            memo_hotkey: DEFAULT_HOTKEYS[1].to_string(),
            conversation_hotkey: DEFAULT_HOTKEYS[2].to_string(),
            capture_hotkey: DEFAULT_HOTKEYS[3].to_string(),
            retain_dictations_days: 7,
            hints: Vec::new(),
            closing_words: vec!["câmbio".into()],
            discard_phrase: "risca isso".into(),
            confirm_words: vec!["sim".into()],
            cleanup_prompt: DEFAULT_CLEANUP_PROMPT.to_string(),
        }
    }
}

impl VoiceConfig {
    /// Whether there is a working capability here, as opposed to a file that merely exists.
    ///
    /// `enabled: true` with no transcriber is not half-on, it is off: the hotkey would record and then
    /// have nowhere to send the audio, which presents as the feature being broken rather than absent.
    pub fn armed(&self) -> bool {
        // Either engine arms it. Naming only `stt_command` here would leave a machine pointed at a
        // resident server -- the configuration that is six times faster -- reporting no voice at all,
        // which is the regression `a_resident_engine_is_a_voice_even_with_no_command` already had to
        // be written for on the speaking side.
        self.enabled && !(self.stt_command.trim().is_empty() && self.stt_url.trim().is_empty())
    }

    /// Whether this machine can say anything out loud.
    ///
    /// Deliberately NOT folded into `armed`, and the asymmetry is the design. A pillar with no
    /// transcriber is off, because every entry point starts with a recording. A pillar with no
    /// speaker still works: the question is heard, the agent answers, and the answer is read rather
    /// than spoken. Collapsing the two would take a conversation away from someone who has an STT
    /// engine and no TTS one — which is every machine on the day this ships.
    ///
    /// Gated on `armed` all the same: a voice with nothing to say it in response to is not a
    /// capability, and reporting it as one would put a control in the window for a pillar that is off.
    pub fn speaks(&self) -> bool {
        self.armed() && (!self.tts_url.trim().is_empty() || !self.tts_command.trim().is_empty())
    }

    fn validated(mut self) -> Self {
        if self.retain_dictations_days > 30 {
            tracing::warn!(
                retain_dictations_days = self.retain_dictations_days,
                "voice config: retain_dictations_days must be 0..=30; using 30"
            );
            self.retain_dictations_days = 30;
        }
        // A blank prompt would ask the model to do anything it liked with the transcript. Deleting the
        // key is the documented way to reset, so emptying it resolves to the same default.
        if self.cleanup_prompt.trim().is_empty() {
            self.cleanup_prompt = DEFAULT_CLEANUP_PROMPT.to_string();
        }
        self
    }
}

/// `~/.nucleos/voice.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_voice_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in a dictation aid cannot stop the daemon from starting. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_voice_config(contents: &str) -> Result<VoiceConfig, String> {
    serde_yaml::from_str::<VoiceConfig>(contents)
        .map(VoiceConfig::validated)
        .map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/voice.yaml`. Absent, unreadable or malformed → defaults, with a warning; never an error.
///
/// This follows `load_email_config` rather than `load_schedule_rules`, and the choice matters in two
/// directions: a typo in a dictation aid must not stop the daemon from starting, and "off" is the
/// inert state for a file holding a command the daemon spawns.
pub fn load_voice_config(path: &Path) -> VoiceConfig {
    if !path.exists() {
        return VoiceConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_voice_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "voice config: could not be parsed; the pillar stays off");
            VoiceConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "voice config: could not be read; the pillar stays off");
            VoiceConfig::default()
        }
    }
}

/// The calendar's settings.
///
/// Only two things are configurable, because only two things are policy. The zone is what an event
/// means when the caller does not say; the working window is where a PROPOSAL may land — and it is
/// emphatically not when you are busy. Busy comes from real events only, and keeping the two apart
/// is what stops "I do not work Sundays" from quietly becoming "tell me nothing on Sundays".
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarConfig {
    /// An IANA name. Empty means "ask the operating system", which is right far more often than
    /// any name written into a default could be.
    pub default_tz: String,
    pub working_hours_start: String,
    pub working_hours_end: String,
    /// Lowercase three-letter English day names.
    pub working_weekdays: Vec<String>,
    /// Whether an `action`-class message may file a proposal asking for time. Off by default, for
    /// the reason spelled out on `calendar::CalendarRuntime::propose_for_actions`.
    pub propose_time_for_actions: bool,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            default_tz: String::new(),
            working_hours_start: "09:00".to_string(),
            working_hours_end: "18:00".to_string(),
            working_weekdays: ["mon", "tue", "wed", "thu", "fri"]
                .iter()
                .map(|day| (*day).to_string())
                .collect(),
            propose_time_for_actions: false,
        }
    }
}

/// `~/.nucleos/calendar.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_calendar_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in two policy strings cannot stop the daemon from starting. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_calendar_config(contents: &str) -> Result<CalendarConfig, String> {
    serde_yaml::from_str::<CalendarConfig>(contents).map_err(|error| error.to_string())
}

pub fn load_calendar_config(path: &Path) -> CalendarConfig {
    if !path.exists() {
        return CalendarConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_calendar_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "calendar config: could not be parsed; defaults apply");
            CalendarConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "calendar config: could not be read; defaults apply");
            CalendarConfig::default()
        }
    }
}

/// The devtime ingestion's settings.
///
/// Derives `PartialEq` and not `Eq`, because the amber rate is an `f64`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeConfig {
    pub enabled: bool,
    /// Seconds between two ingestion cycles.
    pub cycle_seconds: u64,
    /// Minutes of silence after which a session counts as idle.
    pub idle_minutes: u64,
    /// Where the transcripts live. Empty means `<home>/.claude/projects`.
    pub projects_dir: String,
    /// The share of unparseable lines above which the ingest health goes amber.
    pub parse_failure_amber_rate: f64,
    /// Fewer lines than this and the rate above is not judged.
    pub parse_failure_min_lines: u64,
    /// The rule engine's vocabularies and thresholds (sub-project 2).
    pub rules: DevtimeRulesConfig,
}

impl Default for DevtimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cycle_seconds: 60,
            idle_minutes: 15,
            projects_dir: String::new(),
            parse_failure_amber_rate: 0.02,
            parse_failure_min_lines: 200,
            rules: DevtimeRulesConfig::default(),
        }
    }
}

/// The parser's closed error-class vocabulary as `devtime.yaml` may name it in
/// `rules.vocab.error_signatures`. It lists the same nine tokens as `devtime_parse::ERROR_CLASSES`
/// (the parser's own const gains `wrong_shell` with parser v2), and a parser test holds the two equal:
/// this module must not import the parser, so the list is stated twice and checked once.
pub const RULES_ERROR_CLASSES: [&str; 9] = [
    "exit_nonzero",
    "exit_75",
    "timeout",
    "interrupted",
    "permission_denied",
    "hook_block",
    "edit_not_found",
    "wrong_shell",
    "tool_error",
];

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

/// The rule engine's whole configuration: every threshold, program list and vocabulary a rule reads.
/// Rule code holds no such literal (spec §3.5), so a partial file at any nesting level is valid and
/// every field has the default the spec states.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeRulesConfig {
    pub enabled: bool,
    /// Backlog cap per cycle after a `rules_version` change; sessions touched this cycle always run.
    pub sessions_per_cycle: u32,
    pub commands: DevtimeCommandsConfig,
    pub roles: DevtimeRolesConfig,
    pub vocab: DevtimeVocabConfig,
    pub thresholds: DevtimeThresholdsConfig,
    pub paths: DevtimePathsConfig,
    pub precision: DevtimePrecisionConfig,
    pub unexplained: DevtimeUnexplainedConfig,
    pub adapters: DevtimeAdaptersConfig,
}

impl Default for DevtimeRulesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sessions_per_cycle: 200,
            commands: DevtimeCommandsConfig::default(),
            roles: DevtimeRolesConfig::default(),
            vocab: DevtimeVocabConfig::default(),
            thresholds: DevtimeThresholdsConfig::default(),
            paths: DevtimePathsConfig::default(),
            precision: DevtimePrecisionConfig::default(),
            unexplained: DevtimeUnexplainedConfig::default(),
            adapters: DevtimeAdaptersConfig::default(),
        }
    }
}

/// Which programs count as a test, a build, a lint, a mutation, a sleep, a commit or a revert, and
/// which tool names are shell, edit, read or search tools.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeCommandsConfig {
    pub test: Vec<String>,
    pub build: Vec<String>,
    pub lint: Vec<String>,
    pub mutating: Vec<String>,
    pub sleep: Vec<String>,
    pub commit: Vec<String>,
    pub revert_with_paths: Vec<String>,
    pub revert_pathless: Vec<String>,
    /// First words that run a script rather than name a program (`bash scripts/gates.sh`).
    pub interpreters: Vec<String>,
    pub shell_tools: Vec<String>,
    pub edit_tools: Vec<String>,
    pub read_tools: Vec<String>,
    pub search_tools: Vec<String>,
    /// Project id to extra programs, appended to the base lists above.
    pub per_project: BTreeMap<String, DevtimeProjectCommands>,
}

impl Default for DevtimeCommandsConfig {
    fn default() -> Self {
        Self {
            test: owned(&[
                "cargo test",
                "go test",
                "npm test",
                "npx vitest",
                "npx jest",
                "pytest",
                "dotnet test",
                "make test",
            ]),
            build: owned(&[
                "cargo build",
                "cargo check",
                "go build",
                "go vet",
                "npx tsc",
                "mvn",
                "gradle",
                "dotnet build",
                "make",
            ]),
            lint: owned(&["cargo fmt", "cargo clippy", "npx eslint", "gofmt"]),
            mutating: owned(&[
                "cargo fmt",
                "gofmt",
                "git checkout",
                "git restore",
                "git stash",
                "git reset",
                "git apply",
                "git merge",
                "git rebase",
                "git pull",
                "sed",
                "patch",
            ]),
            sleep: owned(&["sleep", "Start-Sleep", "timeout"]),
            commit: owned(&["git commit"]),
            revert_with_paths: owned(&["git checkout", "git restore"]),
            revert_pathless: owned(&["git reset", "git stash"]),
            interpreters: owned(&[
                "bash",
                "sh",
                "python",
                "python3",
                "node",
                "pwsh",
                "powershell",
            ]),
            shell_tools: owned(&["Bash", "PowerShell"]),
            edit_tools: owned(&["Edit", "Write", "MultiEdit", "NotebookEdit"]),
            read_tools: owned(&["Read"]),
            search_tools: owned(&["Grep", "Glob"]),
            per_project: BTreeMap::new(),
        }
    }
}

/// One project's extra test, build and lint programs.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeProjectCommands {
    pub test: Vec<String>,
    pub build: Vec<String>,
    pub lint: Vec<String>,
}

/// Case-insensitive globs (`*` only) over a subagent's `agentType`. The reviewer list is tried first.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeRolesConfig {
    pub reviewer: Vec<String>,
    pub implementer: Vec<String>,
}

impl Default for DevtimeRolesConfig {
    fn default() -> Self {
        Self {
            reviewer: owned(&["*review*"]),
            implementer: owned(&["*executor*", "*implement*", "general-purpose"]),
        }
    }
}

/// The words the parser and the rules match in memory. Only the derived flag or token is ever stored.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeVocabConfig {
    /// A prompt that opens with one of these counts as a correction of the previous answer.
    pub correction_openers: Vec<String>,
    /// Model families, weakest first.
    pub model_strength: Vec<String>,
    /// Error class to extra substrings that classify a failed tool result into it.
    pub error_signatures: BTreeMap<String, Vec<String>>,
}

impl Default for DevtimeVocabConfig {
    fn default() -> Self {
        let mut error_signatures = BTreeMap::new();
        error_signatures.insert(
            "wrong_shell".to_string(),
            owned(&[
                "is not recognized as the name of a cmdlet",
                "is not a valid statement separator",
                "ParserError",
                "Windows Subsystem for Linux",
                "C:/Program Files/Git/",
            ]),
        );
        error_signatures.insert(
            "hook_block".to_string(),
            owned(&["pre-commit hook", "hook rejected", "hook declined"]),
        );
        error_signatures.insert(
            "edit_not_found".to_string(),
            owned(&[
                "matches of the string to replace",
                "File has not been read yet",
            ]),
        );
        error_signatures.insert("permission_denied".to_string(), Vec::new());
        Self {
            correction_openers: owned(&[
                "não",
                "nao",
                "não era isso",
                "nao era isso",
                "errado",
                "está errado",
                "isso não",
                "that's not",
                "that is not",
                "not what i",
                "wrong",
                "no,",
                "nope",
            ]),
            model_strength: owned(&["haiku", "sonnet", "opus"]),
            error_signatures,
        }
    }
}

/// Every numeric threshold a rule compares against.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeThresholdsConfig {
    /// D3: a Grep/Glob streak without an edit.
    pub d3_search_burst: u32,
    /// FLAILING: a streak without a Read or an Edit.
    pub d3_flailing_burst: u32,
    pub d5_foreground_agent_seconds: u64,
    pub d6_foreground_command_seconds: u64,
    /// D1 / THRASH.
    pub thrash_repeats: u32,
    pub thrash_reset_calls: u32,
    /// D14 / LATE SCOPE.
    pub late_scope_fraction: f64,
    pub late_scope_min_files: u32,
    pub d8_poll_repeats: u32,
    pub d9_context_tokens: i64,
    pub f_min_sessions: u32,
    pub f_sequence_len: u32,
    pub f_window_days: u32,
}

impl Default for DevtimeThresholdsConfig {
    fn default() -> Self {
        Self {
            d3_search_burst: 6,
            d3_flailing_burst: 4,
            d5_foreground_agent_seconds: 120,
            d6_foreground_command_seconds: 120,
            thrash_repeats: 3,
            thrash_reset_calls: 40,
            late_scope_fraction: 0.30,
            late_scope_min_files: 3,
            d8_poll_repeats: 3,
            d9_context_tokens: 250_000,
            f_min_sessions: 3,
            f_sequence_len: 3,
            f_window_days: 30,
        }
    }
}

/// What counts as an external path, and which file extensions are code.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimePathsConfig {
    /// Globs, case-insensitive, with `\` read as `/`.
    pub external: Vec<String>,
    pub code_extensions: Vec<String>,
}

impl Default for DevtimePathsConfig {
    fn default() -> Self {
        Self {
            external: owned(&["*/temp/*", "*/tmp/*", "*scratchpad*", "*.log", "*.output"]),
            code_extensions: owned(&[
                "rs", "go", "ts", "tsx", "js", "jsx", "mjs", "py", "java", "kt", "cs", "c", "cc",
                "cpp", "h", "hpp", "swift", "rb", "php", "sql", "sh", "ps1",
            ]),
        }
    }
}

/// How a rule's precision is judged (spec §7).
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimePrecisionConfig {
    /// A rule below this precision is flagged out of phase 2.
    pub floor: f64,
    /// Fewer judged cases than this and the precision is not trusted.
    pub min_cases: u32,
    /// Reported while a rule has fewer than `min_cases` cases.
    pub prior_default: f64,
    /// Rule id to its starting precision, when it is not `prior_default`.
    pub priors: BTreeMap<String, f64>,
}

impl Default for DevtimePrecisionConfig {
    fn default() -> Self {
        let mut priors = BTreeMap::new();
        // Spec §5: C2 "arranca com precisão baixa".
        priors.insert("C2".to_string(), 0.2);
        Self {
            floor: 0.8,
            min_cases: 20,
            prior_default: 0.5,
            priors,
        }
    }
}

/// When a slow turn that no rule explains is worth listing.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeUnexplainedConfig {
    pub median_multiple: f64,
    pub min_turn_seconds: u64,
    pub max_explained_fraction: f64,
    /// Main-lane calls per turn that split turns into classes `c0..cN`.
    pub class_bounds: Vec<u32>,
    /// A class with fewer turns has no median, and nothing in it is listed.
    pub min_class_turns: u32,
    pub window_days: u32,
}

impl Default for DevtimeUnexplainedConfig {
    fn default() -> Self {
        Self {
            median_multiple: 3.0,
            min_turn_seconds: 300,
            max_explained_fraction: 0.2,
            class_bounds: vec![0, 5, 20],
            min_class_turns: 10,
            window_days: 30,
        }
    }
}

/// Where the optional adapter sources live. Empty means absent.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevtimeAdaptersConfig {
    pub permission_log: String,
    pub heavy_log: String,
}

/// What the grammar alone cannot say: a signature for a class the parser does not have, a fraction
/// outside [0, 1], or a count that must be at least one. A file that fails this is refused whole.
pub fn validate_devtime(config: &DevtimeConfig) -> Result<(), String> {
    let rules = &config.rules;
    for class in rules.vocab.error_signatures.keys() {
        if !RULES_ERROR_CLASSES.contains(&class.as_str()) {
            return Err(format!(
                "rules.vocab.error_signatures names `{class}`, which is not an error class"
            ));
        }
    }
    let fractions = [
        (
            "rules.thresholds.late_scope_fraction",
            rules.thresholds.late_scope_fraction,
        ),
        ("rules.precision.floor", rules.precision.floor),
        (
            "rules.precision.prior_default",
            rules.precision.prior_default,
        ),
        (
            "rules.unexplained.max_explained_fraction",
            rules.unexplained.max_explained_fraction,
        ),
    ];
    for (name, value) in fractions {
        if !(0.0..=1.0).contains(&value) {
            return Err(format!("{name} must be within [0, 1], got {value}"));
        }
    }
    for (rule, prior) in &rules.precision.priors {
        if !(0.0..=1.0).contains(prior) {
            return Err(format!(
                "rules.precision.priors.{rule} must be within [0, 1], got {prior}"
            ));
        }
    }
    let counts = [
        (
            "rules.thresholds.thrash_repeats",
            rules.thresholds.thrash_repeats,
        ),
        (
            "rules.thresholds.d3_search_burst",
            rules.thresholds.d3_search_burst,
        ),
        (
            "rules.thresholds.d3_flailing_burst",
            rules.thresholds.d3_flailing_burst,
        ),
        (
            "rules.thresholds.f_min_sessions",
            rules.thresholds.f_min_sessions,
        ),
        (
            "rules.thresholds.f_sequence_len",
            rules.thresholds.f_sequence_len,
        ),
        (
            "rules.thresholds.f_window_days",
            rules.thresholds.f_window_days,
        ),
    ];
    for (name, value) in counts {
        if value == 0 {
            return Err(format!("{name} must be at least 1"));
        }
    }
    Ok(())
}

/// `~/.nucleos/devtime.yaml`'s grammar, and the only place that decides what a valid one is.
/// Refuses unknown keys and malformed YAML; [`load_devtime_config`] turns that refusal into defaults.
pub fn parse_devtime_config(contents: &str) -> Result<DevtimeConfig, String> {
    let config =
        serde_yaml::from_str::<DevtimeConfig>(contents).map_err(|error| error.to_string())?;
    validate_devtime(&config)?;
    Ok(config)
}

pub fn load_devtime_config(path: &Path) -> DevtimeConfig {
    if !path.exists() {
        return DevtimeConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_devtime_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "devtime config: could not be parsed; defaults apply");
            DevtimeConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "devtime config: could not be read; defaults apply");
            DevtimeConfig::default()
        }
    }
}

/// The verification executor's machine settings, `~/.nucleos/verify.yaml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct VerifyConfig {
    /// Weight units that may run at once across the whole machine.
    pub capacity: u32,
    /// A queued request that has waited this long moves up one priority level.
    pub aging_seconds: u64,
    /// The wall clock one unit may take before it is killed.
    pub unit_timeout_seconds: u64,
    /// The ceiling, in GB, on the whole machine's warm state.
    pub disk_cap_gb: u64,
    /// The argv prefixed to every unit when it runs (a machine's broker). Empty means none.
    pub broker_prefix: Vec<String>,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            capacity: 4,
            aging_seconds: 300,
            unit_timeout_seconds: 1800,
            disk_cap_gb: 60,
            broker_prefix: Vec::new(),
        }
    }
}

/// `~/.nucleos/verify.yaml`'s grammar, and the only place that decides what a valid one is.
/// Refuses unknown keys, malformed YAML and a zero capacity, aging or timeout;
/// [`load_verify_config`] turns that refusal into defaults.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_verify_config(contents: &str) -> Result<VerifyConfig, String> {
    // An empty file (or only comments) is "say nothing", which is the defaults.
    if contents.trim().is_empty() {
        return Ok(VerifyConfig::default());
    }
    let config =
        serde_yaml::from_str::<VerifyConfig>(contents).map_err(|error| error.to_string())?;
    if config.capacity < 1 {
        return Err("capacity must be at least 1".to_string());
    }
    if config.aging_seconds < 1 {
        return Err("aging_seconds must be at least 1".to_string());
    }
    if config.unit_timeout_seconds < 1 {
        return Err("unit_timeout_seconds must be at least 1".to_string());
    }
    if config.disk_cap_gb < 1 {
        return Err("disk_cap_gb must be at least 1".to_string());
    }
    Ok(config)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn load_verify_config(path: &Path) -> VerifyConfig {
    if !path.exists() {
        return VerifyConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_verify_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "verify config: could not be parsed; defaults apply");
            VerifyConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "verify config: could not be read; defaults apply");
            VerifyConfig::default()
        }
    }
}

/// `~/.nucleos/web.yaml`. The web pillar's settings, including the one list in this system that decides
/// what counts as trustworthy.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WebConfig {
    /// Opt-in, like every pillar that reaches the network.
    pub enabled: bool,
    /// `brave` or `searxng`. Validated by the sidecar, which is what has to build one.
    pub provider: String,
    pub searxng_url: String,
    pub retain_pages_days: i64,
    pub max_page_bytes: i64,
    pub fetch_timeout_seconds: u32,
    /// Hosts whose extracted text may reach an agent as written — and only then when the owner
    /// asked (`trust.rs`, spec §5.2).
    ///
    /// Empty by default, and that is the load-bearing choice in this struct. Every other field's
    /// default is a convenience; this one's is a refusal. A `~/.nucleos/web.yaml` that is missing,
    /// unreadable, or malformed therefore trusts NOTHING rather than falling back to a list nobody
    /// can see — the opposite direction from `VoiceConfig`, whose defaults are all benign.
    pub trusted_hosts: Vec<String>,
    /// Whether a pillar may search on its own, with nobody watching (spec §10.5).
    ///
    /// Separate from `enabled`, and off by default, because the query leaves the machine: enriching
    /// a correspondent means searching a person's name, and nobody decided to share that.
    ///
    /// It lives in the file format and NOT yet on `WebRuntime`, because nothing consumes it: no
    /// pillar reaches the web in this version. Carrying it into the runtime would be a switch that
    /// grants nothing, and a switch that grants nothing is one somebody later assumes is working.
    /// The first pillar to search is what moves it across.
    pub pillar_search_enabled: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "brave".to_string(),
            searxng_url: String::new(),
            retain_pages_days: 30,
            max_page_bytes: 2_000_000,
            fetch_timeout_seconds: 20,
            trusted_hosts: Vec::new(),
            pillar_search_enabled: false,
        }
    }
}

/// `~/.nucleos/web.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_web_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a broken file costs fidelity and never safety. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_web_config(contents: &str) -> Result<WebConfig, String> {
    serde_yaml::from_str::<WebConfig>(contents).map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/web.yaml`. Absent, unreadable or malformed → defaults, with a warning.
///
/// The failure mode is deliberately asymmetric with the rest of this module: falling back to
/// defaults here means falling back to an EMPTY allowlist, so a broken file costs fidelity (more
/// pages go through the local model) and never costs safety. A loader that errored instead would
/// stop the daemon over a typo in a convenience list; one that guessed a permissive list would be
/// the worst of both.
pub fn load_web_config(path: &Path) -> WebConfig {
    if !path.exists() {
        return WebConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_web_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "web config: could not be parsed; the pillar stays off and nothing is trusted");
            WebConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "web config: could not be read; the pillar stays off and nothing is trusted");
            WebConfig::default()
        }
    }
}

/// `~/.nucleos/browser.yaml`. The browser pillar's switch and its ceilings (spec §8).
///
/// The site lists are deliberately NOT here, and that omission is the pillar's central invariant:
/// they live in `browser_sites` and grow only when a person finishes a login (spec §5.2). A field in
/// this file would be a way to grant a profile access to a host by editing a gitignored YAML, which
/// is exactly the path §5.2 exists to close.
///
/// There is also no field that widens the fence. Spec §6.4: the boundary of §6.2 is not
/// configurable, because a switch to loosen it is a switch somebody eventually finds a reason to
/// flip at 2am.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    /// Opt-in, like every pillar that reaches the network. Ships off (spec §14.2).
    pub enabled: bool,
    /// Live tabs. They compete with the local model for the same card, so this is a ceiling rather
    /// than a target.
    pub max_sessions: u32,
    pub cache_size_mb: u32,
    /// Persistent profiles. Above this the sidecar refuses and names one to forget.
    pub max_profiles: u32,
    /// The whole profiles directory, ephemeral included. Reaching it sweeps first and refuses second.
    pub disk_budget_mb: u32,
    pub load_timeout_seconds: u32,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        // The numbers spec §8 ships, so the documented file and an absent file behave alike.
        Self {
            enabled: false,
            max_sessions: 2,
            cache_size_mb: 100,
            max_profiles: 20,
            disk_budget_mb: 3000,
            load_timeout_seconds: 30,
        }
    }
}

/// `~/.nucleos/browser.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_browser_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a broken file costs a capability and never grants one. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_browser_config(contents: &str) -> Result<BrowserConfig, String> {
    serde_yaml::from_str::<BrowserConfig>(contents).map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/browser.yaml`. Absent, unreadable or malformed → defaults, with a warning.
///
/// Defaults mean the pillar is OFF, so a broken file costs a capability and never grants one — the
/// same asymmetry [`load_web_config`] has, and here it is easier to justify: there is nothing in
/// this file whose default is more permissive than what somebody would have written.
pub fn load_browser_config(path: &Path) -> BrowserConfig {
    if !path.exists() {
        return BrowserConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_browser_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "browser config: could not be parsed; the pillar stays off");
            BrowserConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "browser config: could not be read; the pillar stays off");
            BrowserConfig::default()
        }
    }
}

/// `~/.nucleos/telegram.yaml`. Per-developer, gitignored, and read for exactly one thing: the standing
/// doctrine a Telegram turn falls back on when the chat itself gave no instructions.
///
/// Ships with no field this widens into a capability, unlike `GithubConfig` below — the whole
/// content is a paragraph of text that becomes `append_system_prompt` when nothing else would have.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TelegramConfig {
    /// The standing instructions a Telegram turn is launched with when the chat has none of its
    /// own. `None` — absent, blank, or whitespace-only — means every turn is launched exactly as it
    /// was before this file existed: see `chats.rs`'s identical treatment of a blank `instructions`
    /// column.
    #[serde(default, deserialize_with = "deserialize_blank_as_none")]
    pub doctrine: Option<String>,
}

/// A blank or whitespace-only string reads as `None`, mirroring `chats.rs`'s
/// `instructions.filter(|text| !text.trim().is_empty())` for the same reason: a doctrine of empty
/// spaces would spend an argv slot saying nothing.
fn deserialize_blank_as_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.filter(|text| !text.trim().is_empty()))
}

/// `~/.nucleos/telegram.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_telegram_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo never invents a doctrine nobody wrote. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_telegram_config(contents: &str) -> Result<TelegramConfig, String> {
    serde_yaml::from_str::<TelegramConfig>(contents).map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/telegram.yaml`. Absent, unreadable or malformed → default (`doctrine: None`), with a
/// warning — the same asymmetry `load_web_config` and `load_browser_config` both take: a typo in a
/// per-developer file must cost fidelity (no doctrine prepended) and never stop the daemon, and
/// never invent a doctrine nobody wrote.
pub fn load_telegram_config(path: &Path) -> TelegramConfig {
    if !path.exists() {
        return TelegramConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_telegram_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "telegram config: could not be parsed; every turn is launched exactly as before");
            TelegramConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "telegram config: could not be read; every turn is launched exactly as before");
            TelegramConfig::default()
        }
    }
}

/// `~/.nucleos/github.yaml`. The GitHub pillar's switch and the two lists that decide what runs without
/// anybody watching.
///
/// **`enabled` defaults to TRUE, which is the opposite of every other pillar that reaches the
/// network, and the asymmetry is the whole design rather than an oversight.** `WebConfig` and
/// `BrowserConfig` ship off because for them "off" and "grants nothing" are the same state. Here
/// they are not: what a missing file has to produce is a pillar *capable of everything and
/// autonomous in nothing* — a person can still approve any operation, and no operation runs without
/// one. That is what the two empty lists below deliver, and switching the pillar off as well would
/// take away the capability the owner asked for in order to withhold an autonomy the empty lists had
/// already withheld.
///
/// So the refusal in this struct lives in `autonomous_reads` and `autonomous_actions`, exactly where
/// `WebConfig::trusted_hosts` puts its own: every other field's default is a convenience, and those
/// two are a refusal. A file that is missing, unreadable or malformed asks a human about every last
/// `gh` invocation.
///
/// Neither list is authoritative on its own. `github::Policy` intersects both with a compiled
/// ceiling, so what is written here can only ever NARROW what the code already allows — which is
/// what makes a per-developer gitignored YAML a defensible place for this at all.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GithubConfig {
    pub enabled: bool,
    /// `gh` prefixes a run may execute through Bash without asking. Subset of
    /// `github::READ_CEILING`; an entry outside it is dropped with a warning.
    pub autonomous_reads: Vec<String>,
    /// `github::ActOp` kinds the núcleo executes without asking. Subset of
    /// `github::ACTION_CEILING`, which does not contain `api_read` and never will.
    pub autonomous_actions: Vec<String>,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            autonomous_reads: Vec::new(),
            autonomous_actions: Vec::new(),
        }
    }
}

/// `~/.nucleos/github.yaml`'s grammar, and the only place that decides what a valid one is.
///
/// Split out of [`load_github_config`] because a write route needs a parser that can REFUSE, and
/// the loader by design cannot: it answers a malformed file with defaults precisely so that
/// a typo in a convenience list cannot take the daemon down. Two questions, one grammar — the loader
/// calls this and then decides what to do with the `Err`, which is what keeps the file the door
/// accepts and the file the daemon reads the same file. See `machine_config.rs` for the door.
pub fn parse_github_config(contents: &str) -> Result<GithubConfig, String> {
    serde_yaml::from_str::<GithubConfig>(contents).map_err(|error| error.to_string())
}

/// Reads `~/.nucleos/github.yaml`. Absent, unreadable or malformed -> defaults, with a warning.
///
/// The same asymmetry `load_web_config` has and the same reason: falling back to defaults here means
/// falling back to two EMPTY lists, so a broken file costs convenience — everything starts asking —
/// and never costs safety. A loader that errored would take the daemon down over a typo in a
/// convenience list; one that guessed would be the worst of both.
///
/// The warning says what was lost in the words the owner needs, because "could not parse" alone
/// reads like a pillar that stopped working and this one has not.
pub fn load_github_config(path: &Path) -> GithubConfig {
    if !path.exists() {
        return GithubConfig::default();
    }
    match std::fs::read_to_string(path).map(|text| parse_github_config(&text)) {
        Ok(Ok(config)) => config,
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "github config: could not be parsed; the pillar stays capable and stops being autonomous");
            GithubConfig::default()
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "github config: could not be read; the pillar stays capable and stops being autonomous");
            GithubConfig::default()
        }
    }
}

/// How many seats one council may hold.
///
/// Eight, from the Python orchestrator this pillar was ported out of, where it was the parallelism
/// cap. Here it is the roster cap and the parallelism cap at once, because the roster IS the
/// parallelism: every seat of phase 1 is launched together, so a ninth seat would be a ninth agent
/// process and a ninth cloud bill for one question. A number in this file may lower the fan-out and
/// may not raise it, exactly as `MAX_ITEMS_CEILING` may not — `.ai/` is gitignored, so nobody
/// reviews what is written here.
pub const MAX_COUNCIL_SEATS: usize = 8;

/// The wall clock one seat gets, when the file does not say.
///
/// 600 seconds, the same default the Python council ran on. It is per SEAT and not per council: the
/// seats of phase 1 run concurrently, so a council of eight is still bounded by one of these plus
/// phase 2 plus phase 3.
pub const DEFAULT_COUNCIL_TIMEOUT_SECONDS: u64 = 600;

/// The ceiling on that clock, whatever the file asks for.
///
/// A council holds no worktree and takes no concurrency slot (that is decision 7 of the design), so
/// nothing else in the daemon would ever end one. The clock is therefore the only thing that does,
/// and an unbounded one is a council that stays `running` for as long as the daemon lives.
pub const MAX_COUNCIL_TIMEOUT_SECONDS: u64 = 3_600;

/// How many deliberation rounds a council runs when the file does not say.
///
/// One, which is what the pillar has always done: every seat answers, every seat ranks the others
/// blind, a chairman synthesises. The second round is opt-in and the default is not a placeholder —
/// a second round asks every seat the question again, so it roughly doubles what phase 1 cost, and
/// a feature that expensive is one somebody chooses rather than one they inherit.
pub const DEFAULT_COUNCIL_ROUNDS: u32 = 1;

/// The most rounds this file accepts.
///
/// Three, and the ceiling is a decision rather than an arbitrary stop. The owner chose 1-3 rounds
/// with an early stop (spec 2026-10-02): each round after the first revises against the ranking
/// and critiques of the one before, and a round whose Borda order did not move and that disputes
/// no new answer ends the deliberation rather than paying for another (`council::tally::
/// should_stop_early`). Three is where that stops being bounded by money the operator can see
/// coming — `council::tally::call_ceiling` is the bill — and past it a council is a loop, which
/// is an argument to have here rather than a value to type into a roster.
pub const MAX_COUNCIL_ROUNDS: u32 = 3;

/// Where one seat's answer comes from.
///
/// Two variants and no `Auto`. Which machine a question leaves — or does not leave — is the whole
/// reason a mixed roster is a feature, and a variant that decided it for the operator would make
/// the roster stop being the statement of that.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SeatKind {
    /// An agent CLI, through `runner.rs`.
    Cloud,
    /// A model on this machine, through `local_agent.rs`.
    Local,
}

impl SeatKind {
    pub fn as_db_str(self) -> &'static str {
        match self {
            SeatKind::Cloud => "cloud",
            SeatKind::Local => "local",
        }
    }
}

/// The one place the catalogue's vocabulary and the council's meet.
///
/// `agents.engine` says which program answers; `SeatKind` says which machine. They are two
/// nomenclatures for overlapping facts, and the divergence is deliberate — renaming the council's
/// columns to match would touch a shipped table to change no behaviour. What is NOT acceptable is
/// two translations, so there is one, here, walked in both directions by a test.
pub const ENGINE_SEAT_KINDS: &[(&str, SeatKind)] = &[
    ("claude", SeatKind::Cloud),
    ("codex", SeatKind::Cloud),
    ("local", SeatKind::Local),
];

/// PURE: where an agent of the catalogue would run, or `None` for an engine no seat can host.
pub fn seat_kind_for_engine(engine: &str) -> Option<SeatKind> {
    ENGINE_SEAT_KINDS
        .iter()
        .find(|(name, _)| *name == engine)
        .map(|(_, kind)| *kind)
}

/// One seat as a roster DECLARES it: either a model, or an agent of the house catalogue.
///
/// Separate from `CouncilSeat` — the seat that will actually run — because the two are checked at
/// different times by different things. The FORM is checked here, at load, and is pure. The
/// REFERENCE is checked by `council::start` against a catalogue the owner edits while the daemon
/// runs. Collapsing them into one type with everything optional would leave every later reader
/// asking which half is set, and one of them would eventually guess.
///
/// Both forms are valid forever. The tempting cleanup — migrate the file, drop `{ kind, ref }` — is
/// refused for a concrete reason: the roster lives at `~/.nucleos/council.yaml`, outside any
/// checkout, so it is one person's configuration on one machine and nothing shipped here can
/// rewrite it. A form retired here does not produce an error on the machines still using it;
/// `load_council_config` returns `None` on a roster with faults, so it produces a council that
/// silently stops existing at the next daemon start.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SeatSpec {
    pub kind: Option<SeatKind>,
    /// Spelled `ref` in the file, because that is what the design and the Python it came from call
    /// it, and `model_ref` in Rust, because `ref` is a keyword.
    #[serde(rename = "ref")]
    pub model_ref: Option<String>,
    /// An `agents` row id. Never resolved here — see `council::resolve_seat`.
    pub agent: Option<String>,
}

/// PURE: what a roster's nth position is called in a refusal, chairman first.
///
/// Shared with `council::start` rather than written twice, so a roster refused at load and the same
/// roster refused at start name the same seat the same way. An owner correcting a file should not
/// have to work out that "seat 1" and "the second member" are the same line.
pub fn seat_name(index: usize) -> String {
    if index == 0 {
        "the chairman".to_string()
    } else {
        format!("seat {}", index - 1)
    }
}

impl SeatSpec {
    /// Everything wrong with this seat's FORM. Reads no database, by construction.
    ///
    /// `local_available` bears only on the model form: an agent's engine lives in a table, so
    /// whether an agent seat wants this machine is not knowable from the file. `council::start`
    /// asks that question where it can be answered, and refuses with the same words.
    fn faults_in(&self, who: &str, local_available: bool) -> Vec<String> {
        let mut faults = Vec::new();
        let names_a_model = self.kind.is_some() || self.model_ref.is_some();
        match (self.agent.as_deref(), names_a_model) {
            (Some(agent), false) => {
                if agent.trim().is_empty() {
                    faults.push(format!("{who} names an empty agent"));
                }
            }
            // Refused rather than resolved in favour of one of them. Two sources for the same fact
            // is where they eventually disagree, and the disagreement would be silent: whichever
            // half lost would still be sitting there, read by whoever edits the file next.
            (Some(_), true) => faults.push(format!(
                "{who} names both an agent and a model; a seat is filled by one or the other"
            )),
            (None, false) => faults.push(format!("{who} names neither a model nor an agent")),
            (None, true) => match (self.kind, self.model_ref.as_deref()) {
                (Some(kind), Some(model_ref)) => {
                    if model_ref.trim().is_empty() {
                        faults.push(format!("{who} names no model"));
                    }
                    // Refused rather than quietly re-routed to the cloud. An operator who wrote
                    // `local` asked for a question that does not leave this machine, and answering
                    // it in the cloud anyway is the one failure this check exists to prevent.
                    if kind == SeatKind::Local && !local_available {
                        faults.push(format!(
                            "{who} asks for a local model and no local model is configured"
                        ));
                    }
                }
                (None, Some(_)) => {
                    faults.push(format!("{who} names a model but not where it runs"))
                }
                (Some(_), None) => faults.push(format!("{who} names no model")),
                (None, None) => {}
            },
        }
        faults
    }
}

/// What an agent brings to a seat, copied out of the catalogue when the council convenes.
///
/// A copy and not an id to look up later. The catalogue is editable while a deliberation runs, and
/// a seat that re-read its own prompt between phase 1 and phase 3 would be two different seats
/// wearing one `seat_idx`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatAgent {
    pub id: String,
    pub name: String,
    pub prompt: String,
    /// `mcp_only` | `none`. Only ever NARROWS what a seat may reach: `mcp_only` is exactly what
    /// every seat gets today, `unrestricted` is refused at the catalogue, and `none` takes the box
    /// away. Nothing here can hand a seat a tool a seat does not already have.
    pub tool_policy: String,
}

/// One seat as it will RUN: where it runs, which model answers, and — when the catalogue filled it
/// — who it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilSeat {
    pub kind: SeatKind,
    pub model_ref: String,
    /// Resolved at `council::start` and never before.
    pub agent: Option<SeatAgent>,
}

impl CouncilSeat {
    /// The seat a `{ kind, ref }` line resolves to: a model, and nobody in particular.
    pub fn of_model(kind: SeatKind, model_ref: impl Into<String>) -> Self {
        Self {
            kind,
            model_ref: model_ref.into(),
            agent: None,
        }
    }

    /// Whether this seat may hold tools AT ALL, before the phase decides whether it gets any.
    ///
    /// Two independent narrowings that meet with an `&&`: the phase says no to everything after
    /// phase 1, and an agent may say no to everything full stop.
    pub fn allows_tools(&self) -> bool {
        self.agent
            .as_ref()
            .is_none_or(|agent| agent.tool_policy != "none")
    }

    pub fn agent_id(&self) -> Option<&str> {
        self.agent.as_ref().map(|agent| agent.id.as_str())
    }
}

/// `~/.nucleos/council.yaml`. Absent means there is no council — this pillar has no useful default,
/// because a roster nobody chose is a list of models nobody agreed to pay for.
///
/// `deny_unknown_fields`, and here it is load-bearing rather than tidy: `member:` for `members:`
/// would otherwise parse into a council with no seats, which is a council that answers every
/// question with the chairman's own opinion while looking like it deliberated.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CouncilConfig {
    #[serde(default = "default_council_timeout")]
    pub timeout_seconds: u64,
    /// 1 to `MAX_COUNCIL_ROUNDS`. Each round past the first is a further deliberation round: after
    /// the blind ranking every seat is shown the same anonymised peer answers it ranked, plus where
    /// the council placed them, and revises its own answer — and the chairman then synthesises the
    /// revised ones. A round that changes nothing ends the deliberation early.
    ///
    /// A FAULT rather than a clamp when it is anything else, which is the one place this field
    /// parts company with `timeout_seconds` two lines up. A clock outside its bounds has an obvious
    /// nearest legal value and costs nothing to guess at; `rounds: 4` does not, because both
    /// candidates are defensible and they differ by the price of a whole extra round. So it joins
    /// the fault list and the council stays off, which is the branch `load_council_config` took
    /// over `load_models_config`'s on purpose: the operator is told, and nothing is spent guessing.
    #[serde(default = "default_council_rounds")]
    pub rounds: u32,
    /// Which of the daemon's own decisions get the council's opinion before a person sees them.
    ///
    /// Absent means none of them, which is what every roster written before this field already
    /// meant. `#[serde(default)]` on the struct AND on each flag, so `consumers: { job_review:
    /// true }` is a legal half-answer: an operator turning one on should not have to write the
    /// other down to leave it alone.
    #[serde(default)]
    pub consumers: CouncilConsumers,
    pub chairman: SeatSpec,
    pub members: Vec<SeatSpec>,
}

/// The internal callers a configured council is allowed to advise.
///
/// **Both false by default, and both are advice rather than authority.** A consumer costs minutes
/// of paid deliberation in front of something that was going to happen anyway, so neither is
/// inherited — and neither decides anything: the job's `review` node still runs and still writes
/// its own opinion, and a proposal gets a NOTE on its event log while the human keeps the verdict.
/// `.ai/decisions.md` fixed that second half — the arbiter of an ambiguity is the person, and the
/// council gates nothing — so a flag here that approved anything would contradict a standing
/// decision rather than extend a feature.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CouncilConsumers {
    /// Before a job's `review` node starts, put the job's task to the council and leave the
    /// synthesis in the job's artifacts directory for the node to read. The job waits while the
    /// council deliberates.
    #[serde(default)]
    pub job_review: bool,
    /// When a run is stopped for an approval, put the refused action to the council and write the
    /// synthesis onto the proposal as a note. Best-effort and out of band: the proposal is created
    /// and the run is parked exactly as they were, whatever the council does or fails to do.
    #[serde(default)]
    pub proposal_advice: bool,
}

fn default_council_timeout() -> u64 {
    DEFAULT_COUNCIL_TIMEOUT_SECONDS
}

fn default_council_rounds() -> u32 {
    DEFAULT_COUNCIL_ROUNDS
}

impl CouncilConfig {
    /// Everything wrong with a roster, said as one list rather than as the first thing noticed.
    ///
    /// An operator editing this file by hand is going to get more than one thing wrong at once, and
    /// a loader that reports only the first turns a single correction into three restarts.
    fn faults(&self, local_available: bool) -> Vec<String> {
        let mut faults = Vec::new();

        if self.members.is_empty() {
            faults.push("the roster has no members".to_string());
        }
        if self.members.len() > MAX_COUNCIL_SEATS {
            faults.push(format!(
                "the roster has {} members, above the ceiling of {MAX_COUNCIL_SEATS}",
                self.members.len()
            ));
        }
        // Named in the list rather than clamped. See the field's own note: there is no obvious
        // nearest legal value for a round count past the ceiling, and guessing one spends money the
        // operator did not agree to spend.
        if !(1..=MAX_COUNCIL_ROUNDS).contains(&self.rounds) {
            faults.push(format!(
                "rounds is {}; a council runs 1 to {MAX_COUNCIL_ROUNDS} rounds",
                self.rounds
            ));
        }
        for (index, seat) in self.seats().enumerate() {
            faults.extend(seat.faults_in(&seat_name(index), local_available));
        }

        faults
    }

    /// The chairman first, then the members, which is the order `faults` numbers them in.
    fn seats(&self) -> impl Iterator<Item = &SeatSpec> {
        std::iter::once(&self.chairman).chain(self.members.iter())
    }

    fn validated(mut self) -> Self {
        if self.timeout_seconds == 0 {
            tracing::warn!(
                "council config: timeout_seconds must be above zero; using {DEFAULT_COUNCIL_TIMEOUT_SECONDS}"
            );
            self.timeout_seconds = DEFAULT_COUNCIL_TIMEOUT_SECONDS;
        }
        if self.timeout_seconds > MAX_COUNCIL_TIMEOUT_SECONDS {
            tracing::warn!(
                timeout_seconds = self.timeout_seconds,
                "council config: timeout_seconds is capped at {MAX_COUNCIL_TIMEOUT_SECONDS}"
            );
            self.timeout_seconds = MAX_COUNCIL_TIMEOUT_SECONDS;
        }
        self
    }
}

/// Reads the roster at `path` — `council::config_path()` in production, a tempdir in the tests
/// below. Absent, unreadable, malformed or invalid → `None`, with a warning.
///
/// This follows `load_web_config` and NOT `load_models_config`, and the direction was chosen rather
/// than inherited. Erroring would stop the daemon — mail, autopilot, voice and the API with it —
/// over a typo in a list of model names, and the fallback here is the feature being OFF rather than
/// a permissive one. A council nobody can start costs the operator a council; a daemon that will
/// not boot costs them everything else.
///
/// `local_available` is passed in rather than read, because whether this machine can answer locally
/// is not configuration — startup PROVES it by probing the model, and only `main.rs` holds that
/// answer.
pub fn load_council_config(path: &Path, local_available: bool) -> Option<CouncilConfig> {
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(path).map(|text| parse_council_config(&text, local_available)) {
        Ok(Ok(config)) => Some(config),
        Ok(Err(error)) => {
            tracing::warn!(%error, path = %path.display(), "council config: unusable; there is no council");
            None
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "council config: could not be read; there is no council");
            None
        }
    }
}

/// `~/.nucleos/council.yaml`'s grammar AND its roster rules, which for this file are the same question:
/// a council whose seats do not add up is not a council, so `faults` belongs on this side of the
/// door rather than after it.
///
/// `local_available` is a parameter for the reason [`load_council_config`] gives — whether this
/// machine can answer locally is proved by probing at startup, not asserted by a file. The write
/// route therefore passes `true`: the door refuses what is wrong about the ROSTER however the
/// machine is configured, and leaves "no local model is up right now" to startup, which is where
/// that fact is actually known. Refusing a local seat at the door because Ollama happens to be
/// down would make the file uneditable on exactly the machine that needs it edited.
pub fn parse_council_config(
    contents: &str,
    local_available: bool,
) -> Result<CouncilConfig, String> {
    let config: CouncilConfig =
        serde_yaml::from_str(contents).map_err(|error| error.to_string())?;
    let faults = CouncilConfig::faults(&config, local_available);
    if !faults.is_empty() {
        return Err(faults.join("; "));
    }
    Ok(config.validated())
}

/// `deny_unknown_fields` on every rule type and on the file itself: without it a typo like
/// `schedule:` for `schedules:` parses cleanly into an empty ruleset, and all autonomy for that
/// project silently stops. That direction is fail-closed, which is precisely why nobody notices —
/// the contract this module advertises is "error on malformed YAML rather than guess", and a
/// misspelt key is malformed.
// `PartialEq` without `Eq`: `GraphConfig` carries an `Option<f64>` now, and floats are not `Eq`.
// Nothing outside this module ever needed the total equality.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRule {
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub cwd: Option<String>,
    /// The IANA zone the cron is read in — `Europe/Lisbon`, `America/New_York`.
    ///
    /// Absent means UTC, which is what every rule written before this field already meant, so no
    /// existing schedule moves by adding it. It exists because UTC is the one answer that is wrong
    /// twice a year for most of the world: `0 8 * * *` is 08:00 local in winter and 09:00 in
    /// summer, and the daily task that quietly starts an hour late for half the year is a worse
    /// failure than one that never runs, because nothing about it looks broken.
    pub timezone: Option<String>,
    /// Absent means the rule keeps today's behaviour: one run, one gate. Opt-in per rule.
    pub graph: Option<GraphConfig>,
}

/// The ceiling the daemon puts on a rule's fan-out, whatever the file asks for.
///
/// A project's `autopilot.yaml` lives in `~/.nucleos/projects/<id>/` and travels with nobody, so
/// it is per-developer configuration that no review ever sees. A number in it therefore cannot be
/// the only thing standing between one trigger and an unbounded number of runs — the file may
/// lower the fan-out, never raise it.
pub const MAX_ITEMS_CEILING: usize = 5;

/// The ceiling the daemon puts on how many EXTRA implement runs one red gate may buy.
///
/// The same argument `MAX_ITEMS_CEILING` makes, against the same file. A retry is a whole run, and
/// the project's `autopilot.yaml` is per-developer configuration no review ever sees — a number in
/// it cannot be the only thing standing between one red gate and an unbounded number of
/// re-implements. It may lower the budget; it may not raise it past what the daemon is willing to
/// spend on one item.
///
/// Three rather than five, and lower than the fan-out ceiling on purpose: past the third attempt the
/// evidence is that the item cannot be made to pass, and every further run is taken from the items
/// queued behind it that were never the ones that broke.
pub const MAX_GATE_RETRIES_CEILING: usize = 3;

/// The ceiling the daemon puts on how many ROUNDS one job may run.
///
/// The symmetric argument to `MAX_ITEMS_CEILING`'s, against a different threat. That one guards a
/// number in a per-developer file no review ever sees; this guards a number in an HTTP body that a
/// model filled in from a conversation, which is reviewed less still — an assistant asked to "keep
/// going until it's done" can write 10 000 as easily as 10.
///
/// `MAX_ITEMS_CEILING` deliberately does NOT rise to meet it. Five stays the ceiling PER ROUND, and
/// depth comes from rounds, which are counted in the database where a restart cannot lose them.
/// Together they are 100 items in the worst case, each with its own gate — which is a lot, and is
/// exactly why the per-job budget rather than either counter is the brake expected to fire first.
pub const MAX_ROUNDS_CEILING: i64 = 20;

/// The rounds actually allowed, after the daemon's own ceiling.
///
/// A free function rather than an accessor on a struct, because unlike `max_items` this number
/// arrives loose in a request body and there is no struct to hang it on that a caller could not
/// sidestep. Same purpose though: a caller that used the asked-for number directly would honour what
/// the model wrote and leave the ceiling decorative.
///
/// `None` — nobody asked for rounds — resolves to ONE, never to the ceiling. Resolving it upward
/// would switch rounds on for every `graph:` rule already scheduled, silently.
pub fn rounds_allowed(asked: Option<i64>) -> i64 {
    asked.unwrap_or(1).clamp(1, MAX_ROUNDS_CEILING)
}

#[cfg(test)]
mod rounds_ceiling_tests {
    use super::*;

    /// The number arrives in a request body a model filled in from a conversation. An assistant
    /// asked to "keep going until it's done" writes 10 000 as easily as 10, and a ceiling applied
    /// anywhere other than the way in is one a forgetful caller walks past.
    #[test]
    fn the_rounds_a_caller_asks_for_are_cut_to_the_daemons_ceiling() {
        // Nobody asked: one round, never the ceiling. Resolving upward would switch rounds on for
        // every `graph:` rule already scheduled, silently.
        assert_eq!(rounds_allowed(None), 1);
        assert_eq!(rounds_allowed(Some(5)), 5);
        assert_eq!(rounds_allowed(Some(10_000)), MAX_ROUNDS_CEILING);
        // Below the floor is a job that could never do anything, which is not what any caller meant.
        assert_eq!(rounds_allowed(Some(0)), 1);
        assert_eq!(rounds_allowed(Some(-3)), 1);
    }

    /// The per-round ceiling deliberately does NOT rise to meet the round ceiling. Depth comes from
    /// rounds, which are counted in the database where a restart cannot lose them; fan-out stays
    /// where the argument for it was written.
    #[test]
    fn the_per_round_fan_out_is_unchanged_by_rounds_existing() {
        assert_eq!(MAX_ITEMS_CEILING, 5);
        assert_eq!(MAX_ROUNDS_CEILING, 20);
    }
}

fn default_max_items() -> usize {
    MAX_ITEMS_CEILING
}

/// What a rule that said nothing about retries asks for: one.
///
/// The default that costs something, and deliberately so — a red gate is most often a near miss, and
/// one more implement run told what the gate said is cheaper than the item it saves. One and not
/// more, because the second retry is where an item that cannot be made to pass starts eating the
/// runs the items behind it were queued for.
///
/// The `jobs.gate_retries` COLUMN defaults to 0 instead, and the two disagree on purpose: this is
/// what a rule asks for when it says nothing, and 0 is what a job already scheduled keeps when
/// nobody asked at all.
pub const DEFAULT_GATE_RETRIES: usize = 1;

fn default_gate_retries() -> usize {
    DEFAULT_GATE_RETRIES
}

fn default_true() -> bool {
    true
}

/// Turns one scheduled rule into a job: a sequence of runs over one shared worktree, rather than a
/// single run capped by one context window.
// `PartialEq` without `Eq`: `budget_usd` is an `Option<f64>`, and floats have no total equality.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    #[serde(default = "default_max_items")]
    max_items: usize,
    // Off unless a rule asks (owner decision, 2026-10-02): one core suite per intermediate item paid
    // for states nobody ships. The last item's gate runs regardless, over the merged job branch.
    #[serde(default)]
    pub gate_after_each_item: bool,
    #[serde(default = "default_true")]
    pub review: bool,
    #[serde(default = "default_gate_retries")]
    gate_retries: usize,
    /// The most this one job may spend, in USD, before its own brakes stop it.
    ///
    /// `None` — the key absent — means what every `graph:` rule has always meant: only the house
    /// limit governs this job. That is the behaviour of every rule already sitting in somebody's
    /// `autopilot.yaml`, and it must stay theirs, so there is no default number here.
    ///
    /// A PUBLIC field, unlike `max_items` and `gate_retries`. Those two are private behind an
    /// accessor because the accessor applies a CEILING against a per-developer file no review sees.
    /// A budget has no ceiling to apply: it runs UNDER the house limit rather than instead of it, so
    /// whatever the file writes here can only ever tighten what this job is allowed to spend. There
    /// is no number a rule could put in this key that buys it more than the daemon already allows.
    ///
    /// The value must be a finite, non-negative number; `load_schedule_rules` refuses the rest
    /// rather than clamping, for the reasons written at `validate_rules`.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// The team to direct this rule's jobs, by id. Absent is the sequential job in one shared
    /// checkout — what every `graph:` rule already sitting in somebody's gitignored file means, and
    /// what it must keep meaning.
    ///
    /// A PUBLIC field with no accessor, like `budget_usd` and unlike `max_items`. The two private
    /// ones are private because their accessor applies a ceiling against a per-developer file
    /// nobody reviews. There is nothing to clamp here: naming a team buys this job no more of
    /// anything the daemon pays for, because what a team spends is checkouts, and every checkout is
    /// still refused by the project's slot count and by the free-disk floor.
    ///
    /// Whether the id names a team that exists is NOT checked here, and cannot be: this is a file
    /// and teams live in the database. `job::start` reads it at the moment the job is made, which is
    /// the only moment the answer is current — a rule written last month can name a team deleted
    /// this morning, and nothing in between would have said so.
    #[serde(default)]
    pub team: Option<String>,
}

impl GraphConfig {
    /// The fan-out actually allowed, after the daemon's own ceiling.
    ///
    /// Private field plus this accessor on purpose: a caller that read `max_items` straight off the
    /// struct would silently honour whatever the file said, and the ceiling would be advisory.
    pub fn max_items(&self) -> usize {
        self.max_items.min(MAX_ITEMS_CEILING)
    }

    /// The retries actually allowed, after the daemon's own ceiling.
    ///
    /// Private field plus this accessor for the same reason `max_items` has one, and it is worth
    /// saying twice because the failure is silent: a caller that read `gate_retries` straight off
    /// the struct would honour whatever the project's `autopilot.yaml` asked for, and the ceiling
    /// above would be decorative — present in the code, absent from every job that actually runs.
    pub fn gate_retries(&self) -> usize {
        self.gate_retries.min(MAX_GATE_RETRIES_CEILING)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepoTrigger {
    pub name: String,
    pub branch: String,
    pub prompt: String,
}

/// Spec A D7: the judge's thresholds for this project. Both absent means the measured defaults.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JudgeConfig {
    #[serde(default)]
    pub allow_at: Option<f64>,
    #[serde(default)]
    pub deny_at: Option<f64>,
}

/// Spec B D4: the resolver's thresholds for this project. All absent means 0.85 everywhere.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JudgeResolveConfig {
    #[serde(default)]
    pub off_task_at: Option<f64>,
    #[serde(default)]
    pub needed_at: Option<f64>,
    #[serde(default)]
    pub avoidable_at: Option<f64>,
    #[serde(default)]
    pub fixable_at: Option<f64>,
}

// `PartialEq` without `Eq`, transitively: a `ScheduleRule`'s `graph:` block holds a float now.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AutopilotRules {
    #[serde(default)]
    pub schedules: Vec<ScheduleRule>,
    #[serde(default)]
    pub repo_triggers: Vec<RepoTrigger>,
    #[serde(default)]
    pub gate_command: Option<String>,
    /// Whether a breached health readout should be written as a new intent record.
    ///
    /// **Absent and off by default**: a repository that says nothing behaves exactly as it did
    /// before this key existed. `deny_unknown_fields` above means a misspelling is a startup error,
    /// rather than a silently ignored line.
    ///
    /// Turning this on records the breach as a line in the project's feed for an operator to
    /// review; it does not enqueue a job, start a run, or touch the approval queue. The
    /// `schedules` and `repo_triggers` lists stay empty because the file's own doctrine is
    /// "Creating this file must not start anything"; this key does not overrule that doctrine.
    #[serde(default)]
    pub health_breach_intent: bool,
    /// Whether the VCS queue measures a merge before it publishes it.
    ///
    /// **Off by default, and the default is the whole of the compatibility story**: a repository
    /// that says nothing lands exactly as it landed before this key existed. `deny_unknown_fields`
    /// above is what makes the opposite true too — a project that MEANT to switch this on and
    /// misspelled it gets a startup error rather than a queue that quietly went on publishing
    /// unmeasured.
    ///
    /// On, the queue runs `gate_command` against the COMPUTED merge — the commit `compute_merge`
    /// left in the integration worktree, which is what the target branch is about to become — and
    /// publishes only if it agrees. Nothing is ever reverted, because nothing is published until
    /// the measurement agrees.
    ///
    /// It costs the gate's own wall clock per merge, and it costs it while holding that
    /// repository's queue. What that lengthens is the time until a branch appears on the target,
    /// and not anybody's prompt: `--land` prints a ticket and returns without waiting, and always
    /// did.
    ///
    /// Setting this without a `gate_command` is refused rather than ignored — see
    /// `git_exec::gate_the_merge`, which is the only reader.
    #[serde(default)]
    pub gate_before_publish: bool,
    /// Whether a job asks if the owner is at the keyboard before it starts its next node.
    ///
    /// `Option`, and the absent case is the brake ON. That is deliberately not the same as
    /// `Some(true)`: `load_schedule_rules` returns `AutopilotRules::default()` for a project with
    /// no rules file at all, and a plain `bool` would have made that derived default `false` — a
    /// brake that switched itself off on every project that never configured anything. The zero
    /// value therefore means "not configured", and `attention_brake()` below supplies the answer.
    ///
    /// Off, `attention::attention_permits_new_run` is never consulted for this project's jobs and
    /// they start nodes with the owner watching. That is the point of switching it off — a job is
    /// night work and nobody can watch night work happen — and it is also the whole cost: an
    /// autonomous node may move the worktree under a person who is working in it.
    #[serde(default)]
    pub attention_brake: Option<bool>,
    /// Spec A D7: `judge: { allow_at, deny_at }`. Read through `judge_thresholds()` only, for the
    /// reason `gate_retries()` gives: a caller reading the raw numbers would honour a loosening
    /// the daemon promises never to honour.
    #[serde(default)]
    pub judge: JudgeConfig,
    /// Spec B D4: read through `resolve_thresholds()` only, for the reason `judge_thresholds()`
    /// gives: a caller reading the raw numbers would honour a loosening the daemon never honours.
    #[serde(default)]
    pub judge_resolve: JudgeResolveConfig,
    /// This project's ceiling, in GB, on the warm state the verify executor keeps for it. Absent
    /// means the executor's own default.
    #[serde(default)]
    pub verify_disk_cap_gb: Option<u64>,
}

impl AutopilotRules {
    /// Whether the attention brake applies, resolving the unconfigured case to ON.
    ///
    /// A reader rather than a field read for the reason `gate_retries` is one: a caller that took
    /// `attention_brake` straight off the struct would have to decide what `None` means every time
    /// it asked, and the direction it is cheapest to get wrong is the one that starts a node while
    /// somebody is typing.
    pub fn attention_brake(&self) -> bool {
        self.attention_brake.unwrap_or(true)
    }

    /// The thresholds the judge decides with, after D7's tightening, with a `warn` for every value
    /// pulled back. Warned on every read and not once: it is read per consultation, and a loosened
    /// safety threshold is the one line in this file that should keep being loud.
    pub fn judge_thresholds(&self) -> crate::judge::Thresholds {
        let (thresholds, warnings) =
            crate::judge::Thresholds::tightened(self.judge.allow_at, self.judge.deny_at);
        for warning in warnings {
            tracing::warn!(%warning, "autopilot.yaml: a judge threshold was pulled back to its limit");
        }
        thresholds
    }

    pub fn resolve_thresholds(&self) -> crate::judge::resolve::ResolveThresholds {
        let c = &self.judge_resolve;
        let (thresholds, warnings) = crate::judge::resolve::ResolveThresholds::toward_caution(
            c.off_task_at,
            c.needed_at,
            c.avoidable_at,
            c.fixable_at,
        );
        for warning in warnings {
            tracing::warn!(%warning, "autopilot.yaml: a resolver threshold was pulled toward caution");
        }
        thresholds
    }
}

/// A project's rules, from `~/.nucleos/projects/<project_id>/autopilot.yaml`.
///
/// Keyed by the project's id and not by a folder, which is the whole of `project_state.rs`'s
/// argument: the same file is found from the main checkout and from every worktree the daemon opens
/// of it, and it does not move when the project's folder does. `machine_root` is
/// `AppState::machine_config_root` in production and a temporary directory in a test.
///
/// No root, or an id that cannot name a directory, reads as a project with no file — the ordinary
/// state, answered with `AutopilotRules::default()`. The file is named once, as
/// [`crate::project_state::AUTOPILOT_FILE`], because two things have to agree about it: this loader
/// and `ownership.rs`, which declares it writable by the app.
pub fn load_schedule_rules(
    machine_root: Option<&Path>,
    project_id: &str,
) -> std::io::Result<AutopilotRules> {
    match crate::project_state::file(
        machine_root,
        project_id,
        crate::project_state::AUTOPILOT_FILE,
    ) {
        Some(path) => load_schedule_rules_from(&path),
        None => Ok(AutopilotRules::default()),
    }
}

/// The same rules, from one file. Absent is `AutopilotRules::default()`.
pub fn load_schedule_rules_from(path: &Path) -> std::io::Result<AutopilotRules> {
    if !path.exists() {
        return Ok(AutopilotRules::default());
    }
    parse_schedule_rules(&std::fs::read_to_string(path)?)
}

/// The same rules, from text that is not on disk yet.
///
/// Split out of [`load_schedule_rules`] so that the app can hold a candidate to exactly the standard
/// the daemon will hold the file to, BEFORE writing it. Without this the only validator was "read it
/// back afterwards", which is a check that happens after the damage.
///
/// The two must not drift, and the shape here is what stops them: the loader reads bytes and then
/// calls this, so there is one parser and one range check rather than a second pair kept in step by
/// hand.
pub fn parse_schedule_rules(contents: &str) -> std::io::Result<AutopilotRules> {
    // A file with no YAML document in it -- empty, or nothing but comments -- is a fourth state, and
    // it must land with "absent" rather than with "unreadable". serde_yaml returns EndOfStream here,
    // which would otherwise become `GateConfig::Unreadable` and report `gate errored` on every
    // completed run. The way an operator switches a gate off for an afternoon is to comment the
    // `gate_command:` line out; in this repository's own config that leaves comments only.
    if contents.trim().is_empty() {
        return Ok(AutopilotRules::default());
    }
    let rules: AutopilotRules = serde_yaml::from_str(contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    validate_rules(&rules)?;
    Ok(rules)
}

/// Refuses the values that parse as numbers but that no rule could have meant.
///
/// Here rather than in a `Deserialize` detail on purpose: serde's job is the shape of the file, and
/// this is a range check on a value whose shape was fine. `load_schedule_rules` is already the place
/// where "the file says something impossible" becomes `InvalidData`, so it stays the one place a
/// caller has to look, and a hand-built `GraphConfig` in a test is not silently held to a rule that
/// only the file-reading path enforces.
///
/// Refused rather than clamped, both times, because the project's `autopilot.yaml` is per-developer
/// configuration no review ever sees. A number quietly corrected there is a number nobody learns was
/// wrong: the file would keep reading as though it had asked for something, and the job would behave
/// as though it had asked for something else.
///
/// - **Negative.** A negative allowance is not a number anyone meant to write. Clamped to zero it
///   would stop the job at its first node, which is a real behaviour change bought by a typo.
/// - **Non-finite.** The sharp one, and the reason "reject negatives" is not the whole rule. Every
///   comparison against NaN is false, and `job::job_over_budget` decides with
///   `spent + reserve <= limit`: a NaN limit makes that false for ever, so the brake reads as
///   already blown and the job stops at its FIRST node while the file reads as though it had asked
///   for something generous. An infinity is the same class of answer from the other end — a ceiling
///   that can never be reached is not a ceiling, and a rule that wants no ceiling of its own says so
///   by leaving the key out, which is what `None` already means.
fn validate_rules(rules: &AutopilotRules) -> std::io::Result<()> {
    if rules.verify_disk_cap_gb == Some(0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "verify_disk_cap_gb must be at least 1; a zero cap would evict all warm state on \
             every unit — leave the key out to use the machine default",
        ));
    }
    for rule in &rules.schedules {
        let Some(budget) = rule.graph.as_ref().and_then(|graph| graph.budget_usd) else {
            continue;
        };
        if !budget.is_finite() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "schedule `{}`: budget_usd must be a finite number, got `{budget}`; a ceiling \
                     that cannot be compared against is not a ceiling — leave the key out to run \
                     under the house limit alone",
                    rule.name
                ),
            ));
        }
        if budget < 0.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "schedule `{}`: budget_usd must not be negative, got `{budget}`; it is not \
                     clamped to zero because that would stop the job at its first node while the \
                     file still read as though it had asked for something",
                    rule.name
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn chat_choices() -> Vec<AssistantChoice> {
        vec![
            AssistantChoice {
                id: "sonnet".to_string(),
                label: "Sonnet".to_string(),
                brain: "cloud".to_string(),
                efforts: ["low", "medium", "high", "xhigh", "max"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                runner: Some("claude".to_string()),
                tools: None,
                installed: None,
            },
            AssistantChoice {
                id: "opus".to_string(),
                label: "Opus".to_string(),
                brain: "cloud".to_string(),
                efforts: ["low", "medium", "high", "xhigh", "max"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                runner: Some("claude".to_string()),
                tools: None,
                installed: None,
            },
            AssistantChoice {
                id: "gpt-5.6-terra".to_string(),
                label: "GPT-5.6 Terra".to_string(),
                brain: "cloud".to_string(),
                efforts: ["low", "medium", "high", "xhigh", "max", "ultra"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                runner: Some("codex".to_string()),
                tools: None,
                installed: None,
            },
            AssistantChoice {
                id: "gpt-5.5".to_string(),
                label: "GPT-5.5".to_string(),
                brain: "cloud".to_string(),
                efforts: ["low", "medium", "high", "xhigh"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                runner: Some("codex".to_string()),
                tools: None,
                installed: None,
            },
        ]
    }

    #[test]
    fn runner_of_names_the_cli_a_cloud_choice_belongs_to() {
        let config = ModelsConfig {
            assistant_choices: chat_choices(),
            ..ModelsConfig::default()
        };
        assert_eq!(config.runner_of("gpt-5.5"), Some("codex"));
        assert_eq!(config.runner_of("sonnet"), Some("claude"));

        let mut mystery = config.clone();
        mystery.assistant_choices[0].runner = Some("mystery".to_string());
        assert_eq!(mystery.runner_of("sonnet"), Some("claude"));
        mystery.assistant_choices[0].runner = None;
        assert_eq!(mystery.runner_of("sonnet"), Some("claude"));
        assert_eq!(config.runner_of("not-in-the-catalogue"), None);
    }

    #[test]
    fn a_rooted_chat_is_offered_both_clis_in_one_menu() {
        let config = ModelsConfig {
            assistant_choices: chat_choices(),
            ..ModelsConfig::default()
        };
        let chat_catalogue = config.catalogue_for_chat(&[], true);
        let offered: Vec<&str> = chat_catalogue
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert!(
            ["sonnet", "opus", "gpt-5.6-terra", "gpt-5.5"]
                .into_iter()
                .all(|id| offered.contains(&id))
        );
        let catalogue = config.catalogue();
        let unchanged: Vec<&str> = catalogue.iter().map(|choice| choice.id.as_str()).collect();
        assert_eq!(unchanged, vec!["sonnet", "opus"]);
    }

    #[test]
    fn an_unrooted_chat_is_never_offered_a_codex_model() {
        for primary_runner in [None, Some("codex".to_string())] {
            let config = ModelsConfig {
                primary_runner,
                assistant_choices: chat_choices(),
                ..ModelsConfig::default()
            };
            assert!(
                config
                    .catalogue_for_chat(&[], false)
                    .iter()
                    .all(|choice| choice.runner.as_deref() != Some("codex"))
            );
        }
    }

    #[test]
    fn the_effort_door_knows_every_clis_levels() {
        let config = ModelsConfig {
            assistant_choices: chat_choices(),
            ..ModelsConfig::default()
        };
        assert!(is_effort_level(&config, "ultra"));
        assert!(!is_effort_level(&config, "nonsense"));
        assert!(!config.effort_levels().contains(&"ultra".to_string()));
    }

    /// The picker must not offer a route the daemon cannot take.
    ///
    /// A conversation moved to `local` with no local model configured is refused at the first turn
    /// with `NO_LOCAL_MODEL` — a refusal earned by nothing the person did, arriving a message later
    /// than the choice that caused it. No model named, no entry, and the route is simply not there.
    #[test]
    fn the_catalogue_offers_no_local_model_when_none_is_configured() {
        let config = ModelsConfig::default();

        let catalogue = config.catalogue();

        assert!(
            catalogue.iter().all(|choice| choice.brain == "cloud"),
            "a local route was offered with no local model behind it: {catalogue:?}"
        );
    }

    /// And it names the local model rather than the word "local", which is the whole point of the
    /// change: a person picks a model, not a routing decision they have to translate.
    #[test]
    fn the_catalogue_names_the_local_model_when_one_is_configured() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            ..ModelsConfig::default()
        };

        let catalogue = config.catalogue();

        let local = catalogue
            .iter()
            .find(|choice| choice.brain == "local")
            .expect("a configured local model is not on the menu");
        assert_eq!(local.id, "qwen3.5:4b");
        assert_eq!(local.label, "qwen3.5:4b");
        // Ollama has no effort dial, and a control that turns nothing is worse than no control.
        assert!(local.efforts.is_empty());
    }

    // ---------------------------------------------------------------------------------------
    // `catalogue_with_installed` -- the models this machine actually has, merged in
    // ---------------------------------------------------------------------------------------

    /// Both the configured model and every OTHER installed model must be on the menu, each as a
    /// `brain: "local"` entry -- the whole point of this packet: a machine with a dozen models
    /// pulled offers more than the one name in the config file.
    #[test]
    fn o_catalogo_lista_os_modelos_instalados_nesta_maquina() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            ..ModelsConfig::default()
        };
        let installed = vec!["qwen3.5:4b".to_string(), "llama3.2:3b".to_string()];

        let catalogue = config.catalogue_with_installed(&installed);

        let local_ids: Vec<&str> = catalogue
            .iter()
            .filter(|choice| choice.brain == "local")
            .map(|choice| choice.id.as_str())
            .collect();
        assert!(
            local_ids.contains(&"qwen3.5:4b"),
            "the configured model is missing: {catalogue:?}"
        );
        assert!(
            local_ids.contains(&"llama3.2:3b"),
            "an installed model this machine actually has is missing: {catalogue:?}"
        );
    }

    /// An unreachable, quiet, or never-called Ollama must degrade to EXACTLY today's menu -- never
    /// to some other empty state invented for the occasion. Asserted against `catalogue()` itself
    /// rather than a hand-written expectation, so "no behaviour change without a reachable Ollama"
    /// is a fact this test enforces rather than a claim in a comment.
    #[test]
    fn um_api_tags_mudo_degrada_para_o_configurado_e_nunca_para_um_menu_vazio() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            ..ModelsConfig::default()
        };

        let with_nothing_installed = config.catalogue_with_installed(&[]);

        assert_eq!(with_nothing_installed, config.catalogue());
    }

    /// A model can be configured and not yet pulled. The daemon must not hide the one entry it was
    /// explicitly told about just because Ollama's own list disagrees with the config file.
    #[test]
    fn o_modelo_configurado_aparece_mesmo_que_o_api_tags_nao_o_liste() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            ..ModelsConfig::default()
        };
        let installed = vec!["llama3.2:3b".to_string()];

        let catalogue = config.catalogue_with_installed(&installed);

        assert!(
            catalogue
                .iter()
                .any(|choice| choice.brain == "local" && choice.id == "qwen3.5:4b"),
            "a configured-but-not-yet-pulled model must still be on the menu: {catalogue:?}"
        );
    }

    /// The configured model and an installed model can name the same id. That must produce ONE
    /// entry, not two rows a person cannot tell apart in the picker.
    #[test]
    fn um_modelo_instalado_que_ja_e_o_configurado_nao_aparece_duas_vezes() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            ..ModelsConfig::default()
        };
        let installed = vec!["qwen3.5:4b".to_string()];

        let catalogue = config.catalogue_with_installed(&installed);

        let matches: Vec<&AssistantChoice> = catalogue
            .iter()
            .filter(|choice| choice.id == "qwen3.5:4b")
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "the configured model doubled up with its own installed entry: {catalogue:?}"
        );
    }

    /// The switch rule this packet turns on: with no `local_assistant_model` configured, installed
    /// models never reach the menu, however many this machine has pulled -- exactly what keeps
    /// ship-dark true, and exactly what `assistants::resolve_model` needs to stay correct, since it
    /// refuses `RouteNotConfigured` whatever a pin says when the route itself has no model.
    #[test]
    fn sem_modelo_local_configurado_os_instalados_nao_entram_no_menu() {
        let config = ModelsConfig::default();
        let installed = vec!["qwen3.5:4b".to_string(), "llama3.2:3b".to_string()];

        let catalogue = config.catalogue_with_installed(&installed);

        assert!(
            catalogue.iter().all(|choice| choice.brain != "local"),
            "installed models reached the menu with no local_assistant_model configured: {catalogue:?}"
        );
        assert_eq!(catalogue, config.catalogue());
    }

    /// Two reads of one unchanged machine must not reshuffle the menu -- `chats.rs`'s
    /// `subagents_from` makes this exact argument about sorting rather than trusting a map's order,
    /// and a picker that reorders itself between two identical reads is a picker a person cannot
    /// build a habit of using. The two calls below deliberately reverse `installed`'s own order --
    /// this daemon does not control the order Ollama's `/api/tags` answers in, so a genuine
    /// guarantee has to survive the SAME models arriving in a DIFFERENT order, not just the same
    /// slice handed back twice.
    #[test]
    fn a_ordem_dos_modelos_no_menu_nao_muda_entre_duas_leituras_iguais() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            ..ModelsConfig::default()
        };

        let one = config
            .catalogue_with_installed(&["llama3.2:3b".to_string(), "mixtral:8x7b".to_string()]);
        let other = config
            .catalogue_with_installed(&["mixtral:8x7b".to_string(), "llama3.2:3b".to_string()]);

        assert_eq!(
            one, other,
            "the same installed models in a different input order must not reshuffle the menu"
        );
    }

    /// The cloud list belongs to the Claude CLI. Offering `sonnet` to a daemon running Codex would
    /// produce a turn that dies at spawn, and an effort dial Codex has never been verified to read.
    /// The shipped choices belong to the Claude CLI. A daemon started on Codex must not be offered
    /// them: `sonnet` there is a turn that dies at spawn.
    #[test]
    fn codex_is_not_offered_the_claude_models() {
        let config = ModelsConfig {
            primary_runner: Some("codex".to_string()),
            ..ModelsConfig::default()
        };

        let catalogue = config.catalogue();

        assert_eq!(catalogue.len(), 1, "{catalogue:?}");
        assert_eq!(catalogue[0].id, config.codex_model);
        assert_eq!(config.configured_model(), config.codex_model);
        assert_eq!(config.active_runner(), "codex");
    }

    /// And when the file DOES name Codex models, those are what it gets — the two lists live side
    /// by side and the runner decides which is on the menu.
    #[test]
    fn a_file_holding_both_lists_shows_only_the_running_ones() {
        let both = vec![
            AssistantChoice {
                id: "sonnet".to_string(),
                label: "Sonnet".to_string(),
                brain: "cloud".to_string(),
                efforts: vec!["high".to_string()],
                runner: Some("claude".to_string()),
                tools: None,
                installed: None,
            },
            AssistantChoice {
                id: "gpt-5.6-terra".to_string(),
                label: "GPT-5.6-Terra".to_string(),
                brain: "cloud".to_string(),
                efforts: vec!["high".to_string(), "ultra".to_string()],
                runner: Some("codex".to_string()),
                tools: None,
                installed: None,
            },
        ];

        let on_claude = ModelsConfig {
            assistant_choices: both.clone(),
            ..ModelsConfig::default()
        };
        let on_codex = ModelsConfig {
            assistant_choices: both,
            primary_runner: Some("codex".to_string()),
            ..ModelsConfig::default()
        };

        assert_eq!(
            on_claude
                .catalogue()
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            vec!["sonnet"]
        );
        assert_eq!(
            on_codex
                .catalogue()
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            vec!["gpt-5.6-terra"]
        );
        // `ultra` exists on one CLI and not the other, which is the whole reason the levels are
        // carried per model rather than as one list for everything.
        assert!(on_codex.effort_levels().contains(&"ultra".to_string()));
        assert!(!on_claude.effort_levels().contains(&"ultra".to_string()));
    }

    /// The shipped default names aliases, not versions. `claude-sonnet-5` stops existing; `sonnet`
    /// is whatever the CLI currently resolves Sonnet to — so an untouched install does not need
    /// editing every time a model ships.
    #[test]
    fn the_shipped_choices_are_aliases_rather_than_pinned_versions() {
        let catalogue = ModelsConfig::default().catalogue();

        let ids: Vec<&str> = catalogue.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["opus", "sonnet", "fable"]);
        assert!(
            catalogue
                .iter()
                .all(|c| c.efforts == EFFORT_LEVELS.map(str::to_string).to_vec()),
            "a Claude CLI choice was shipped without the levels its CLI documents"
        );
    }

    /// A file written before this key existed must keep working, and must still produce a picker.
    /// An empty catalogue would be a menu with nothing on it — the feature silently absent.
    #[test]
    fn a_config_without_the_new_key_still_offers_a_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(
            &path,
            "claude_model: claude-sonnet-5
codex_model: gpt-5.6-terra
local_assistant_model: qwen3.5:4b
",
        )
        .unwrap();

        let config = load_models_config(&path).unwrap();

        assert_eq!(config.configured_model(), "claude-sonnet-5");
        let catalogue = config.catalogue();
        let ids: Vec<&str> = catalogue.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["opus", "sonnet", "fable", "qwen3.5:4b"]);
    }

    /// Ship-dark on the posture `local_assistant_model` already established: an untouched install,
    /// and every file written before this key existed, names no hosted model — so nothing about a
    /// conversation's behaviour can change until an operator writes the key in themselves.
    #[test]
    fn hosted_assistant_model_is_absent_by_default() {
        assert_eq!(ModelsConfig::default().hosted_assistant_model, None);
    }

    /// The key round-trips through the same reader `local_assistant_model` does, once an operator
    /// does name a model — `deserialize_optional_model` trims whitespace and turns a blank string
    /// into `None` for this key exactly as it already does for that one, so a file edited by hand
    /// with a trailing space does not silently misconfigure the route.
    #[test]
    fn a_configured_hosted_model_reads_back_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(
            &path,
            "claude_model: claude-sonnet-5
codex_model: gpt-5.6-terra
hosted_assistant_model: \"  anthropic/claude-sonnet-4.5  \"
",
        )
        .unwrap();

        let config = load_models_config(&path).unwrap();

        assert_eq!(
            config.hosted_assistant_model.as_deref(),
            Some("anthropic/claude-sonnet-4.5")
        );
    }

    /// Same posture as the local route's regression guard: with no hosted model configured, the
    /// route is simply not on the menu. Offering one anyway would let a person pick `openrouter`
    /// and hit `NO_HOSTED_MODEL` at the first turn -- a refusal earned by nothing they did.
    #[test]
    fn the_catalogue_offers_no_hosted_model_when_none_is_configured() {
        let config = ModelsConfig::default();

        let catalogue = config.catalogue();

        assert!(
            catalogue.iter().all(|choice| choice.brain != "openrouter"),
            "a hosted route was offered with no hosted model behind it: {catalogue:?}"
        );
    }

    /// `catalogue()` must generate the hosted entry from `hosted_assistant_model`, exactly as it
    /// already does for the local route -- the argument on `catalogue`'s own doc comment. Anything
    /// else lets an operator hand-write an `assistant_choices` entry with `brain: "openrouter"`
    /// whose `id` names a different model than `hosted_assistant_model`: `chosen_brain` resolves
    /// the picked id through this same catalogue and gets `Brain::OpenRouter`, so the turn is
    /// answered by the `OpenAiCompatibleChat` `main.rs` built from `hosted_assistant_model` -- a
    /// different model than the one the person picked, silently.
    #[test]
    fn the_catalogue_names_the_hosted_model_when_one_is_configured() {
        let config = ModelsConfig {
            hosted_assistant_model: Some("anthropic/claude-sonnet-4.5".to_string()),
            ..ModelsConfig::default()
        };

        let catalogue = config.catalogue();

        let hosted: Vec<&AssistantChoice> = catalogue
            .iter()
            .filter(|choice| choice.brain == "openrouter")
            .collect();
        assert_eq!(
            hosted.len(),
            1,
            "expected exactly one hosted entry: {catalogue:?}"
        );
        // Same string as the model `main.rs` actually built the `OpenAiCompatibleChat` with -- the two
        // cannot come apart because there is only one place either of them is written.
        assert_eq!(hosted[0].id, "anthropic/claude-sonnet-4.5");
    }

    /// The `retain` a few lines up in `catalogue()` filters `cloud` entries by `active_runner()`
    /// because `sonnet` offered to a daemon running Codex is a turn that dies at spawn. A hosted
    /// model has nothing to do with which agent CLI is installed -- `OpenAiCompatibleChat` is reached
    /// over HTTP, not spawned as either CLI -- so the hosted entry must survive that filter
    /// regardless of `primary_runner`. Pinned because someone reading the retain in isolation could
    /// reasonably "tidy" it into filtering every entry, hosted included.
    #[test]
    fn the_hosted_model_stays_on_the_menu_whichever_cli_is_installed() {
        let on_claude = ModelsConfig {
            hosted_assistant_model: Some("anthropic/claude-sonnet-4.5".to_string()),
            ..ModelsConfig::default()
        };
        let on_codex = ModelsConfig {
            hosted_assistant_model: Some("anthropic/claude-sonnet-4.5".to_string()),
            primary_runner: Some("codex".to_string()),
            ..ModelsConfig::default()
        };

        for config in [&on_claude, &on_codex] {
            let catalogue = config.catalogue();
            let hosted: Vec<&AssistantChoice> = catalogue
                .iter()
                .filter(|choice| choice.brain == "openrouter")
                .collect();
            assert_eq!(
                hosted.len(),
                1,
                "hosted entry dropped for primary_runner={:?}: {catalogue:?}",
                config.primary_runner
            );
            assert_eq!(hosted[0].id, "anthropic/claude-sonnet-4.5");
        }
    }

    /// The file may name hosted models of its own, and they join the menu BESIDE the one
    /// `hosted_assistant_model` generates. Three facts, because they are one rule:
    ///
    /// 1. A `brain: openrouter` row written into `assistant_choices` reaches the menu. The comment
    ///    that used to forbid this feared an entry naming "a model the daemon never built a client
    ///    for" -- but `assistants::resolve_model` returns `pinned.unwrap_or(configured)` and
    ///    `assistant_for` builds the `OpenAiCompatibleChat` from that resolved name, so the client is
    ///    built PER TURN out of the pick. The fear does not describe this code;
    ///    `assistants.rs`'s `a_pinned_hosted_model_beats_the_configured_one_on_the_hosted_route`
    ///    is the proof at the seam where it would have happened.
    /// 2. The generated entry survives, and exactly once: `hosted_assistant_model` is what answers
    ///    a chat that pins nothing, so it must be pickable whether or not the file also lists it --
    ///    the same argument the local block above already makes for the configured local model
    ///    appearing whether or not `installed` names it.
    /// 3. `hosted_assistant_model` stays the SWITCH. With none configured, `Assistants::serves`
    ///    refuses `RouteNotConfigured`, so the file's hosted rows must not reach the menu either:
    ///    offering them would be a refusal earned by nothing the person did, which is what
    ///    `the_catalogue_offers_no_hosted_model_when_none_is_configured` already pins for the
    ///    generated entry.
    ///
    /// Runner-independent for the reason the test above gives, asserted again here because these
    /// rows now arrive through the same list the `retain` filters.
    #[test]
    fn hosted_rows_from_the_file_join_the_menu_beside_the_generated_one() {
        let hosted_row = |id: &str| AssistantChoice {
            id: id.to_string(),
            label: id.to_string(),
            brain: "openrouter".to_string(),
            efforts: Vec::new(),
            runner: None,
            tools: None,
            installed: None,
        };
        let hosted_ids = |config: &ModelsConfig| -> Vec<String> {
            config
                .catalogue()
                .into_iter()
                .filter(|choice| choice.brain == "openrouter")
                .map(|choice| choice.id)
                .collect()
        };

        // The file names one hosted model, the config key names another: both are pickable.
        for runner in [None, Some("codex".to_string())] {
            let beside = ModelsConfig {
                hosted_assistant_model: Some("anthropic/claude-sonnet-4.5".to_string()),
                assistant_choices: vec![hosted_row("openai/gpt-5.6")],
                primary_runner: runner.clone(),
                ..ModelsConfig::default()
            };
            assert_eq!(
                hosted_ids(&beside),
                vec![
                    "openai/gpt-5.6".to_string(),
                    "anthropic/claude-sonnet-4.5".to_string(),
                ],
                "primary_runner={runner:?}: a hosted row from the file and the generated default \
                 must both be on the menu, whichever agent CLI is running"
            );
        }

        // The file names the configured model too: one id, one row.
        let repeated = ModelsConfig {
            hosted_assistant_model: Some("anthropic/claude-sonnet-4.5".to_string()),
            assistant_choices: vec![
                hosted_row("openai/gpt-5.6"),
                hosted_row("anthropic/claude-sonnet-4.5"),
            ],
            ..ModelsConfig::default()
        };
        assert_eq!(
            hosted_ids(&repeated),
            vec![
                "openai/gpt-5.6".to_string(),
                "anthropic/claude-sonnet-4.5".to_string(),
            ],
            "the configured model listed in the file must not also be generated as a second row"
        );

        // No hosted model configured: the route is off, so its rows are not offered.
        let switched_off = ModelsConfig {
            hosted_assistant_model: None,
            assistant_choices: vec![hosted_row("openai/gpt-5.6")],
            ..ModelsConfig::default()
        };
        assert!(
            hosted_ids(&switched_off).is_empty(),
            "hosted_assistant_model is the switch: with none configured the route refuses \
             RouteNotConfigured, so its rows must not be on the menu either"
        );
    }

    /// A local row says whether this machine HAS the model, which is what lets the picker offer one
    /// it does not — the whole reason `assistant_choices` may now carry `brain: local` rows. Ollama
    /// publishes no endpoint listing what is PULLABLE (only `/api/tags`, what is already pulled), so
    /// a model nobody has installed reaches the menu only by somebody writing it down.
    ///
    /// Five facts, one rule:
    /// 1. A row this machine has reads `Some(true)`; one it does not, `Some(false)`.
    /// 2. A row from the file keeps its own LABEL instead of being regenerated from its bare id.
    /// 3. The configured model appears whether or not it is pulled -- so it is precisely the entry
    ///    that must be able to say `false`, rather than being assumed present because it is named.
    /// 4. `installed` stays `None` wherever the question is meaningless: a cloud model runs in
    ///    somebody else's data centre and a hosted one is fetched over HTTP.
    /// 5. `local_assistant_model` is still the SWITCH, exactly as `hosted_assistant_model` is for
    ///    its own route -- a file listing local models with the route off lists nothing offerable.
    #[test]
    fn a_local_row_is_marked_by_whether_this_machine_has_it() {
        let written_by_hand = || AssistantChoice {
            id: "llama3.2:3b".to_string(),
            label: "Llama 3.2 3B".to_string(),
            brain: "local".to_string(),
            efforts: Vec::new(),
            runner: None,
            tools: None,
            // Absent in the file: whether a model is pulled is not a fact a config file can assert.
            installed: None,
        };
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            hosted_assistant_model: Some("openai/gpt-4o".to_string()),
            assistant_choices: vec![written_by_hand()],
            ..ModelsConfig::default()
        };

        let catalogue = config.catalogue_with_installed(&["gemma3:4b".to_string()]);
        let by_id = |id: &str| -> AssistantChoice {
            catalogue
                .iter()
                .find(|choice| choice.id == id)
                .unwrap_or_else(|| panic!("{id} is not on the menu: {catalogue:?}"))
                .clone()
        };

        assert_eq!(
            by_id("gemma3:4b").installed,
            Some(true),
            "a model `/api/tags` named must read as present"
        );
        assert_eq!(
            by_id("llama3.2:3b").installed,
            Some(false),
            "a row the file names and the machine does not have must read as absent, not be hidden"
        );
        assert_eq!(
            by_id("llama3.2:3b").label,
            "Llama 3.2 3B",
            "a hand-written row keeps the label it was written with, rather than its bare id"
        );
        assert_eq!(
            by_id("qwen3.5:4b").installed,
            Some(false),
            "the configured model is the one entry that always appears, so it is the one that most \
             needs to be able to say it is not pulled"
        );
        assert_eq!(
            by_id("openai/gpt-4o").installed,
            None,
            "a hosted model is fetched over HTTP; `installed` is not a question about it"
        );

        let switched_off = ModelsConfig {
            local_assistant_model: None,
            assistant_choices: vec![written_by_hand()],
            ..ModelsConfig::default()
        };
        assert!(
            switched_off
                .catalogue_with_installed(&["gemma3:4b".to_string()])
                .iter()
                .all(|choice| choice.brain != "local"),
            "local_assistant_model is the switch: with none configured the route refuses \
             RouteNotConfigured, so its rows must not be on the menu either"
        );
    }

    /// The format `scripts/refresh-models.py` writes, parsed by the code that has to read it.
    ///
    /// A fixture copied from that script's actual output rather than a shape invented here. The two
    /// are a contract with nothing enforcing it — the script writes YAML, this reads YAML, and
    /// neither imports the other — so the only thing standing between a format change and a picker
    /// that silently falls back to defaults is a test that holds a real sample.
    #[test]
    fn the_refresh_scripts_output_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(
            &path,
            concat!(
                "claude_model: claude-sonnet-5
",
                "codex_model: gpt-5.6-terra
",
                "local_assistant_model: qwen3.5:4b
",
                "
",
                "# Escrito por `scripts/refresh-models.py`.
",
                "assistant_choices:
",
                "  - { id: \"opus\", label: \"Opus\", brain: cloud, runner: claude, efforts: [low, medium, high, xhigh, max] }
",
                "  - { id: \"sonnet\", label: \"Sonnet\", brain: cloud, runner: claude, efforts: [low, medium, high, xhigh, max] }
",
                "  - { id: \"gpt-5.6-terra\", label: \"GPT-5.6-Terra\", brain: cloud, runner: codex, efforts: [low, medium, high, xhigh, max, ultra] }
",
                "  - { id: \"gpt-5.5\", label: \"GPT-5.5\", brain: cloud, runner: codex, efforts: [low, medium, high, xhigh] }
",
            ),
        )
        .unwrap();

        let config = load_models_config(&path).unwrap();

        // On Claude, which is what this file's `primary_runner` (absent) means.
        let catalogue = config.catalogue();
        let ids: Vec<&str> = catalogue.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["opus", "sonnet", "qwen3.5:4b"]);
        assert_eq!(
            catalogue[0].efforts,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        // The local model came from its own key and is on the menu whichever CLI is running.
        assert!(catalogue.last().unwrap().efforts.is_empty());

        // The Codex half is in the same file and appears only when Codex is the runner.
        let on_codex = ModelsConfig {
            primary_runner: Some("codex".to_string()),
            ..config
        };
        let ids: Vec<String> = on_codex.catalogue().iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids, vec!["gpt-5.6-terra", "gpt-5.5", "qwen3.5:4b"]);
        // Per-model levels survived the round trip: `ultra` is on one of these and not the other.
        assert_eq!(on_codex.catalogue()[0].efforts.last().unwrap(), "ultra");
        assert_eq!(on_codex.catalogue()[1].efforts.last().unwrap(), "xhigh");
    }

    /// Ordered weakest-first, because the window draws them in this order and a dial whose order is
    /// not its magnitude is one people read backwards.
    #[test]
    fn the_effort_levels_are_the_ones_the_cli_documents() {
        let config = ModelsConfig::default();

        assert_eq!(EFFORT_LEVELS, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(config.effort_levels(), EFFORT_LEVELS.to_vec());
        assert!(is_effort_level(&config, "xhigh"));
        assert!(!is_effort_level(&config, "XHIGH"));
        assert!(!is_effort_level(&config, "maximum"));
    }

    use super::*;

    fn rules_from(yaml: &str) -> std::io::Result<AutopilotRules> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects").join("p")).unwrap();
        std::fs::write(
            dir.path().join("projects").join("p").join("autopilot.yaml"),
            yaml,
        )
        .unwrap();
        load_schedule_rules(Some(dir.path()), "p")
    }

    #[test]
    fn a_rule_without_a_graph_block_keeps_todays_behaviour() {
        let rules =
            rules_from("schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n")
                .expect("a rule with no graph block still parses");
        assert_eq!(rules.schedules[0].graph, None);
    }

    #[test]
    fn a_graph_block_defaults_to_gating_only_the_last_item_and_reviewing() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph: {}\n",
        )
        .expect("an empty graph block is valid and fully defaulted");
        let graph = rules.schedules[0].graph.as_ref().expect("graph present");
        assert_eq!(graph.max_items(), MAX_ITEMS_CEILING);
        assert!(!graph.gate_after_each_item);
        assert!(graph.review);
    }

    #[test]
    fn the_daemon_ceiling_wins_over_the_file() {
        // The file is per-developer and gitignored, so nobody reviews this number. It may lower the
        // fan-out; it may not raise it past what the daemon is willing to run in one trigger.
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_items: 500\n",
        )
        .expect("an oversized max_items parses");
        assert_eq!(rules.schedules[0].graph.as_ref().unwrap().max_items(), 5);
    }

    #[test]
    fn a_lower_max_items_is_honoured() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_items: 2\n",
        )
        .expect("a smaller max_items parses");
        assert_eq!(rules.schedules[0].graph.as_ref().unwrap().max_items(), 2);
    }

    /// One retry, for a rule that said nothing about retries.
    ///
    /// The default that costs something, and deliberately so: a red gate is most often a near miss —
    /// an import the node forgot, a test it did not know to update — and one more implement run told
    /// what the gate said is cheaper than the item it saves. One and not more, because the second
    /// retry is where an item that cannot be made to pass starts eating the runs the items behind it
    /// were queued for.
    ///
    /// The `jobs.gate_retries` COLUMN defaults to 0, not to this. The two disagree on purpose: this
    /// is what a rule asks for when it says nothing, and that is what a job already scheduled keeps
    /// when nobody asked at all.
    #[test]
    fn gate_retries_defaults_to_one() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph: {}\n",
        )
        .expect("an empty graph block is valid and fully defaulted");
        assert_eq!(
            rules.schedules[0]
                .graph
                .as_ref()
                .expect("graph present")
                .gate_retries(),
            1
        );
    }

    /// The same argument `max_items` is guarded by, against the same file.
    ///
    /// a project's `autopilot.yaml` is per-developer configuration no review ever sees, and a
    /// retry is a whole run: a number in that file cannot be the only thing standing between one red
    /// gate and an unbounded number of re-implements. It may lower the budget; it may not raise it
    /// past what the daemon is willing to spend on one item.
    #[test]
    fn an_oversized_gate_retries_is_cut_by_the_ceiling() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      gate_retries: 99\n",
        )
        .expect("an oversized gate_retries parses");
        assert_eq!(
            rules.schedules[0].graph.as_ref().unwrap().gate_retries(),
            3,
            "the ceiling is what governs, not the file"
        );
        assert_eq!(MAX_GATE_RETRIES_CEILING, 3);
    }

    #[test]
    fn a_misspelt_graph_key_is_malformed_rather_than_ignored() {
        // Same fail-closed contract the rest of this module keeps: a typo that parsed cleanly would
        // silently drop the setting, and the rule would run a shape nobody asked for.
        let error = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_item: 2\n",
        )
        .expect_err("an unknown key inside graph is an error");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    /// The overnight case is the one this field exists for. A job fired at 03:00 with nobody
    /// watching is precisely the job that can eat the whole house allowance before morning, and
    /// until now it was the only kind that could not be given a ceiling of its own: `POST /jobs`
    /// has taken `budget_usd` since the column did, and the scheduler hard-coded `None` because
    /// there was nowhere in a rule to write one.
    ///
    /// A PUBLIC field, unlike `max_items` and `gate_retries`. Those two are private behind an
    /// accessor because the accessor applies a CEILING; a budget has no ceiling to apply, because
    /// it runs UNDER the house limit rather than instead of it and can therefore only ever tighten.
    /// Public is what `gate_after_each_item` and `review` already are, for the same reason.
    #[test]
    fn a_graph_block_may_name_its_own_budget() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      budget_usd: 5.0\n",
        )
        .expect("a graph block naming a budget parses");
        assert_eq!(
            rules.schedules[0].graph.as_ref().unwrap().budget_usd,
            Some(5.0),
            "the number the file asked for is the number the rule carries"
        );
    }

    /// Absent means absent, and never zero. A rule that says nothing about money keeps exactly
    /// today's behaviour — only the house limit governs — and every `graph:` rule already sitting in
    /// somebody's `autopilot.yaml` says nothing about money. Defaulting this to a
    /// number would put a ceiling on all of them overnight, and the first evidence would be a job
    /// stopping for a limit nobody set.
    ///
    /// The block here is non-empty on purpose: what must yield `None` is the absence of this one
    /// key inside a `graph:` block that is otherwise present and saying things.
    #[test]
    fn a_graph_block_without_a_budget_leaves_the_house_limit_alone() {
        let rules = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_items: 2\n",
        )
        .expect("a graph block that says nothing about money parses");
        assert_eq!(rules.schedules[0].graph.as_ref().unwrap().budget_usd, None);
    }

    /// A rule may name the team that will direct its jobs, and a rule that does not keeps the
    /// sequential queue every `graph:` rule has meant until now.
    ///
    /// Both halves matter and the second more. `#[serde(deny_unknown_fields)]` means the key had to
    /// be declared before any file could carry it, so the first half is the whole of "the nightly
    /// job can be run by a team". And every rule already sitting in somebody's `autopilot.yaml`
    /// omits it, so the second half is the promise that none of those nights changes shape because
    /// this landed.
    ///
    /// Nothing here checks that the team exists, and nothing here can: this is a file and the
    /// catalogue is a table. `job::start` reads it when the job is made, which is the only moment
    /// the answer is current.
    #[test]
    fn a_graph_block_may_name_the_team_that_will_direct_it() {
        let directed = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      team: crew\n",
        )
        .expect("a graph block naming a team parses");
        assert_eq!(
            directed.schedules[0]
                .graph
                .as_ref()
                .unwrap()
                .team
                .as_deref(),
            Some("crew")
        );

        let plain = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      max_items: 2\n",
        )
        .expect("a graph block that says nothing about a team parses");
        assert_eq!(plain.schedules[0].graph.as_ref().unwrap().team, None);
    }

    /// Malformed, not clamped. The posture this module advertises is that it falls back to defaults
    /// when the file is ABSENT but errors on malformed YAML rather than guessing, and a negative
    /// allowance is not a number anyone meant to write.
    ///
    /// Clamping it silently to zero would be the worse of the two failures: the job would stop at
    /// its first node while the project's `autopilot.yaml` still read as though it had asked for
    /// something, and the file is per-developer configuration that no review ever sees.
    #[test]
    fn a_negative_budget_is_malformed_rather_than_clamped() {
        let error = rules_from(
            "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      budget_usd: -1.0\n",
        )
        .expect_err("a negative allowance is not a number anyone meant");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        // Refused for the RIGHT reason. Before `budget_usd` existed as a field, `deny_unknown_fields`
        // refused this same YAML as an unknown key — so without this line the assertion above is
        // green on both sides of what the test claims, and would stay green if the field were added
        // and the range check forgotten.
        assert!(
            !error.to_string().contains("unknown field"),
            "the key must be recognised and its VALUE refused: {error}"
        );
    }

    /// NaN is the sharp case, and the reason "reject negatives" is not the whole rule.
    ///
    /// Every comparison against NaN is false. `job::job_over_budget` decides with
    /// `spent + reserve <= limit`, so a NaN limit makes that false forever: the brake reads as
    /// already blown and the job stops at its FIRST node, while the file reads as though it had
    /// asked for something generous. An infinity is the same class of answer from the other end — a
    /// ceiling that can never be reached is not a ceiling, and a rule that wanted no ceiling says so
    /// by leaving the key out.
    #[test]
    fn a_budget_that_is_not_a_finite_number_is_refused() {
        for value in [".nan", ".inf"] {
            let error = rules_from(&format!(
                "schedules:\n  - name: r1\n    cron: \"0 3 * * *\"\n    prompt: do it\n    graph:\n      budget_usd: {value}\n"
            ))
            .err()
            .unwrap_or_else(|| panic!("{value} must be refused, not accepted as a budget"));
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{value}");
            // As in the negative case: `deny_unknown_fields` refuses this YAML today for a reason
            // that has nothing to do with the value, so the kind check alone would pass before the
            // field exists and after a finiteness check was left out.
            assert!(
                !error.to_string().contains("unknown field"),
                "{value} must be recognised as a budget and refused as a number: {error}"
            );
        }
    }

    fn email_config_from(yaml: &str) -> EmailConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("email.yaml");
        std::fs::write(&path, yaml).unwrap();
        load_email_config(&path)
    }

    #[test]
    fn an_absent_email_config_leaves_the_pillar_off() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_email_config(&dir.path().join("email.yaml"));
        assert_eq!(config, EmailConfig::default());
        assert!(!config.enabled);
    }

    /// A typo in an optional pillar's config must not stop the daemon from starting.
    #[test]
    fn an_unparseable_email_config_leaves_the_pillar_off() {
        assert_eq!(
            email_config_from("enabled: [this is not a bool"),
            EmailConfig::default()
        );
    }

    #[test]
    fn a_partial_email_config_keeps_the_defaults_for_the_rest() {
        let config = email_config_from("enabled: true\nhost: imap.gmail.com\nusername: me@x.com\n");
        assert!(config.enabled);
        assert_eq!(config.host, "imap.gmail.com");
        assert_eq!(config.port, 993);
        assert_eq!(config.mailbox, "INBOX");
        assert_eq!(config.notify_classes, vec!["urgent".to_string()]);
        assert_eq!(config.retain_bodies_days, 14);
    }

    #[test]
    fn sem_pasta_de_enviados_o_pilar_arranca() {
        let config = email_config_from("enabled: true\nhost: imap.example.com\n");

        assert!(
            config.enabled,
            "the rest of the email config must still load"
        );
        assert_eq!(config.sent_mailbox, None);
    }

    /// The distinction the rollout's first week depends on: an EMPTY list means "notify about
    /// nothing", and only an absent key means "urgent". Collapsing the two would page the user from
    /// day one and burn the calibration ramp.
    #[test]
    fn an_empty_notify_list_is_a_real_setting() {
        assert_eq!(
            email_config_from("notify_classes: []\n").notify_classes,
            Vec::<String>::new()
        );
        assert_eq!(
            email_config_from("enabled: true\n").notify_classes,
            vec!["urgent".to_string()]
        );
    }

    /// A window starting after 21:00 UTC would cross midnight and emit two digests (§6.3).
    #[test]
    fn a_digest_hour_that_would_cross_midnight_is_corrected() {
        assert_eq!(
            email_config_from("digest_hour_utc: 23\n").digest_hour_utc,
            7
        );
        assert_eq!(
            email_config_from("digest_hour_utc: 21\n").digest_hour_utc,
            21
        );
        assert_eq!(email_config_from("digest_hour_utc: 0\n").digest_hour_utc, 0);
    }

    /// Above 30 days the row itself is gone, so a larger value would keep nothing extra (§7.2).
    #[test]
    fn retention_is_capped_at_the_row_pruning_horizon() {
        assert_eq!(
            email_config_from("retain_bodies_days: 90\n").retain_bodies_days,
            30
        );
        assert_eq!(
            email_config_from("retain_bodies_days: 0\n").retain_bodies_days,
            0
        );
    }

    fn council_config_from(yaml: &str, local_available: bool) -> Option<CouncilConfig> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("council.yaml");
        std::fs::write(&path, yaml).unwrap();
        load_council_config(&path, local_available)
    }

    const A_GOOD_ROSTER: &str = "chairman: { kind: cloud, ref: claude-opus-4-8 }\n\
                                 members:\n\
                                 \x20\x20- { kind: cloud, ref: claude-opus-4-8 }\n\
                                 \x20\x20- { kind: cloud, ref: gpt-5.6-terra }\n";

    #[test]
    fn a_council_roster_parses_with_its_default_clock() {
        let config = council_config_from(A_GOOD_ROSTER, false).expect("a cloud-only roster loads");
        assert_eq!(config.timeout_seconds, DEFAULT_COUNCIL_TIMEOUT_SECONDS);
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.chairman.kind, Some(SeatKind::Cloud));
        assert_eq!(
            config.members[1].model_ref.as_deref(),
            Some("gpt-5.6-terra")
        );
    }

    /// The form is checked here; the reference is not. A roster naming an agent nobody has created
    /// LOADS — `council::start` is what refuses it, against a catalogue that changes while the
    /// daemon runs. See `council::resolve_roster`.
    #[test]
    fn a_seat_may_name_an_agent_and_the_file_does_not_check_it_exists() {
        let config = council_config_from(
            "chairman: { agent: sintetizador }\n\
             members:\n\
             \x20\x20- { agent: cetico }\n\
             \x20\x20- { kind: local, ref: qwen3.5:4b }\n",
            true,
        )
        .expect("both forms in one roster");
        assert_eq!(config.chairman.agent.as_deref(), Some("sintetizador"));
        assert_eq!(config.members[0].agent.as_deref(), Some("cetico"));
        assert_eq!(config.members[0].kind, None);
        assert_eq!(config.members[1].kind, Some(SeatKind::Local));
    }

    /// The faults a seat's FORM can have, each of them fatal to the whole roster and each of them
    /// named, because an operator editing this file by hand gets more than one thing wrong at once.
    #[test]
    fn a_seat_names_one_thing_or_the_other_and_never_both_or_neither() {
        let both = "chairman: { kind: cloud, ref: m }\n\
                    members:\n\
                    \x20\x20- { kind: cloud, ref: m, agent: cetico }\n";
        assert!(
            council_config_from(both, true).is_none(),
            "a seat filled twice is refused rather than resolved in favour of one"
        );
        let neither = "chairman: { kind: cloud, ref: m }\nmembers:\n  - {}\n";
        assert!(council_config_from(neither, true).is_none());
        let no_kind = "chairman: { kind: cloud, ref: m }\nmembers:\n  - { ref: m }\n";
        assert!(
            council_config_from(no_kind, true).is_none(),
            "a model with no `kind` names no machine"
        );
        // And the typo `deny_unknown_fields` is there to catch: a misspelt `agent` must be an
        // arrest at startup, not a seat that quietly does not exist.
        let typo = "chairman: { kind: cloud, ref: m }\nmembers:\n  - { agente: cetico }\n";
        assert!(council_config_from(typo, true).is_none());
    }

    /// Both directions, because either gap is a bug that only shows at runtime: an engine with no
    /// seat kind is an agent that cannot sit, and a seat kind no engine reaches is a machine the
    /// catalogue can never target.
    #[test]
    fn every_catalogue_engine_maps_to_a_seat_kind_and_every_seat_kind_has_an_engine() {
        for engine in crate::agent::ENGINES {
            assert!(
                seat_kind_for_engine(engine).is_some(),
                "the catalogue accepts `{engine}` and no seat can host it"
            );
        }
        for (engine, _) in ENGINE_SEAT_KINDS {
            assert!(
                crate::agent::ENGINES.contains(engine),
                "`{engine}` translates to a seat and no agent may declare it"
            );
        }
        for kind in [SeatKind::Cloud, SeatKind::Local] {
            assert!(
                ENGINE_SEAT_KINDS.iter().any(|(_, mapped)| *mapped == kind),
                "no engine reaches {kind:?}"
            );
        }
        assert_eq!(seat_kind_for_engine("gemini"), None);
    }

    #[test]
    fn an_absent_council_config_means_there_is_no_council() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_council_config(&dir.path().join("council.yaml"), true).is_none());
    }

    /// Absent and malformed reach the same inert state, and neither is an error.
    ///
    /// The direction is the one `load_web_config` takes and not `load_models_config`'s: erroring
    /// would stop the daemon — mail, autopilot, voice and the API with it — over a typo in a list of
    /// model names. The `member:` case is the one worth pinning, because `deny_unknown_fields` is
    /// the only thing standing between that typo and a council that answers every question with the
    /// chairman's own opinion while looking like it deliberated.
    #[test]
    fn a_malformed_council_yaml_leaves_the_feature_off() {
        assert!(council_config_from("chairman: [this is not a seat", true).is_none());
        assert!(council_config_from("", true).is_none());
        assert!(
            council_config_from(
                "chairman: { kind: cloud, ref: m }\nmember:\n  - { kind: cloud, ref: m }\n",
                true
            )
            .is_none(),
            "`member:` for `members:` must not parse into a seatless council"
        );
        assert!(
            council_config_from("chairman: { kind: sideways, ref: m }\nmembers: []\n", true)
                .is_none(),
            "a `kind` that is neither cloud nor local is not a seat"
        );
    }

    /// An operator who wrote `local` asked for a question that does not leave this machine.
    /// Answering it in the cloud anyway is the one failure this check exists to prevent, so the
    /// council is refused rather than re-routed.
    #[test]
    fn a_local_seat_without_a_local_model_is_refused() {
        const MIXED: &str = "chairman: { kind: cloud, ref: claude-opus-4-8 }\n\
                             members:\n\
                             \x20\x20- { kind: cloud, ref: claude-opus-4-8 }\n\
                             \x20\x20- { kind: local, ref: qwen3.5:4b }\n";

        assert!(council_config_from(MIXED, false).is_none());
        assert_eq!(
            council_config_from(MIXED, true)
                .expect("the same roster loads once a local model exists")
                .members
                .len(),
            2
        );

        // The chairman is a seat too, and is checked on the same rule.
        assert!(
            council_config_from(
                "chairman: { kind: local, ref: qwen3.5:4b }\nmembers:\n  - { kind: cloud, ref: m }\n",
                false
            )
            .is_none()
        );
    }

    #[test]
    fn a_roster_that_is_empty_or_oversized_is_refused() {
        assert!(
            council_config_from("chairman: { kind: cloud, ref: m }\nmembers: []\n", true).is_none(),
            "a council with no members is the chairman talking to itself"
        );

        let mut oversized = "chairman: { kind: cloud, ref: m }\nmembers:\n".to_string();
        for _ in 0..=MAX_COUNCIL_SEATS {
            oversized.push_str("  - { kind: cloud, ref: m }\n");
        }
        assert!(council_config_from(&oversized, true).is_none());

        let mut at_the_ceiling = "chairman: { kind: cloud, ref: m }\nmembers:\n".to_string();
        for _ in 0..MAX_COUNCIL_SEATS {
            at_the_ceiling.push_str("  - { kind: cloud, ref: m }\n");
        }
        assert_eq!(
            council_config_from(&at_the_ceiling, true)
                .expect("the ceiling itself is allowed")
                .members
                .len(),
            MAX_COUNCIL_SEATS
        );
    }

    #[test]
    fn a_seat_that_names_no_model_is_refused() {
        assert!(
            council_config_from(
                "chairman: { kind: cloud, ref: m }\nmembers:\n  - { kind: cloud, ref: \"  \" }\n",
                true
            )
            .is_none()
        );
    }

    /// Nothing else in the daemon would ever end a council — it holds no worktree and takes no
    /// concurrency slot — so the clock is the only thing that does, and an unbounded one is a
    /// council that stays `running` for as long as the daemon lives.
    #[test]
    fn the_seat_clock_is_bounded_at_both_ends() {
        let long = format!("timeout_seconds: 99999\n{A_GOOD_ROSTER}");
        assert_eq!(
            council_config_from(&long, true).unwrap().timeout_seconds,
            MAX_COUNCIL_TIMEOUT_SECONDS
        );

        let zero = format!("timeout_seconds: 0\n{A_GOOD_ROSTER}");
        assert_eq!(
            council_config_from(&zero, true).unwrap().timeout_seconds,
            DEFAULT_COUNCIL_TIMEOUT_SECONDS
        );

        let chosen = format!("timeout_seconds: 120\n{A_GOOD_ROSTER}");
        assert_eq!(
            council_config_from(&chosen, true).unwrap().timeout_seconds,
            120
        );
    }

    /// A round count outside `1..=MAX_COUNCIL_ROUNDS` is a FAULT, not a clamp — the whole council is
    /// refused and the operator is told, rather than quietly handed whichever neighbour the loader
    /// guessed. The difference is money: rounding `rounds: 4` down to 3 still buys a third round
    /// nobody asked for, and rounding it up is not a shape this file knows how to run at all.
    ///
    /// Three is the ceiling since the owner chose 1-3 rounds with an early stop (spec 2026-10-02),
    /// so `rounds: 3` moves from the refused list to the accepted one and `rounds: 4` takes its
    /// place as the first value past the edge.
    ///
    /// The clock in the test above IS clamped, and the asymmetry is the thing being held: a clock
    /// has an obvious nearest legal value and a round count does not.
    #[test]
    fn a_fourth_round_is_not_a_shape_this_file_accepts() {
        assert_eq!(
            council_config_from(A_GOOD_ROSTER, true).unwrap().rounds,
            DEFAULT_COUNCIL_ROUNDS,
            "a file that says nothing about rounds runs the council it always ran"
        );
        assert_eq!(
            council_config_from(&format!("rounds: 2\n{A_GOOD_ROSTER}"), true)
                .unwrap()
                .rounds,
            2
        );
        assert_eq!(
            council_config_from(&format!("rounds: 3\n{A_GOOD_ROSTER}"), true)
                .unwrap()
                .rounds,
            3,
            "three rounds is the ceiling, and the ceiling itself is a shape this file runs"
        );

        for refused in ["rounds: 0", "rounds: 4", "rounds: 99"] {
            assert!(
                council_config_from(&format!("{refused}\n{A_GOOD_ROSTER}"), true).is_none(),
                "`{refused}` must leave the council off rather than be clamped into one"
            );
        }
        // Not a `u32` at all: serde refuses it before `faults` is ever reached, and the loader's
        // parse branch reports it. Asserted because the OUTCOME has to be the same either way —
        // there is no council, and the daemon still boots.
        assert!(council_config_from(&format!("rounds: -1\n{A_GOOD_ROSTER}"), true).is_none());
    }

    /// The default is the one that matters here: every roster ever written predates this field, and
    /// each consumer spends minutes of paid deliberation in front of something that was going to
    /// happen anyway. Inheriting either by upgrading is the failure this test exists to catch.
    #[test]
    fn a_roster_that_says_nothing_about_consumers_advises_nobody() {
        let silent = council_config_from(A_GOOD_ROSTER, true).unwrap();
        assert!(!silent.consumers.job_review);
        assert!(!silent.consumers.proposal_advice);

        // And a half-answer leaves the other half alone rather than being refused for being
        // incomplete: turning one consumer on must not require writing the other one down.
        let half = council_config_from(
            &format!("consumers: {{ job_review: true }}\n{A_GOOD_ROSTER}"),
            true,
        )
        .expect("naming one consumer is a complete roster");
        assert!(half.consumers.job_review);
        assert!(!half.consumers.proposal_advice);

        let both = council_config_from(
            &format!("consumers: {{ job_review: true, proposal_advice: true }}\n{A_GOOD_ROSTER}"),
            true,
        )
        .unwrap();
        assert!(both.consumers.job_review && both.consumers.proposal_advice);

        // `deny_unknown_fields` on the nested struct too. A misspelt consumer that parsed into a
        // silent `false` would be the worst kind of failure here: the operator believes they have
        // turned advice on, the daemon boots, and nothing ever says otherwise.
        assert!(
            council_config_from(
                &format!("consumers: {{ job_reviews: true }}\n{A_GOOD_ROSTER}"),
                true
            )
            .is_none(),
            "a misspelt consumer is an arrest, not a consumer that quietly stays off"
        );
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        assert_eq!(load_models_config(&path).unwrap(), ModelsConfig::default());
    }

    #[test]
    fn parses_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();

        let config = load_models_config(&path).unwrap();
        assert_eq!(config.claude_model, "claude-opus-4-8");
        assert_eq!(config.codex_model, "gpt-5.6-sol");
    }

    /// Local inference must remain an explicit opt-in: an absent or blank model keeps the proven
    /// CLI path active, while a real name is preserved for startup to construct the local runner.
    #[test]
    fn local_triage_model_is_optional_and_absent_means_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.claude_model, "claude-opus-4-8");
        assert_eq!(absent.local_triage_model, None);

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nlocal_triage_model: qwen3.5:4b\n",
        )
        .unwrap();
        assert_eq!(
            load_models_config(&path).unwrap().local_triage_model,
            Some("qwen3.5:4b".to_string())
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nlocal_triage_model: \"\"\n",
        )
        .unwrap();
        assert_eq!(load_models_config(&path).unwrap().local_triage_model, None);
    }

    /// Per-role models are how a plan turn is priced apart from the implement turns that follow it.
    /// Both keys keep the `local_triage_model` posture: absent or blank means the role is NOT routed
    /// anywhere, so a file written before these keys existed changes nothing about what runs. The
    /// blank case is the one worth pinning — a key left in the file with its value deleted reads as
    /// "turn this off", and `Some("")` would instead pass an empty string to `--model`.
    #[test]
    fn models_config_reads_the_per_role_keys_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nplan_model: claude-opus-4-8\nreview_model: claude-haiku-4-5\n",
        )
        .unwrap();
        let named = load_models_config(&path).unwrap();
        assert_eq!(named.plan_model, Some("claude-opus-4-8".to_string()));
        assert_eq!(named.review_model, Some("claude-haiku-4-5".to_string()));

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.plan_model, None, "an older file routes nothing");
        assert_eq!(absent.review_model, None, "an older file routes nothing");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nplan_model: \"\"\nreview_model: \"   \"\n",
        )
        .unwrap();
        let blank = load_models_config(&path).unwrap();
        assert_eq!(blank.plan_model, None, "a blanked key means off, not empty");
        assert_eq!(
            blank.review_model, None,
            "a blanked key means off, not empty"
        );
    }

    /// The conflict resolver's model and effort are the same posture as the per-role keys above:
    /// named is honoured, absent or blank changes nothing about what a resolution runs on.
    #[test]
    fn models_config_reads_the_resolve_keys_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-sonnet-5
codex_model: gpt-5.6-sol
resolve_model: opus
resolve_effort: high
",
        )
        .unwrap();
        let named = load_models_config(&path).unwrap();
        assert_eq!(named.resolve_model, Some("opus".to_string()));
        assert_eq!(named.resolve_effort, Some("high".to_string()));

        std::fs::write(
            &path,
            "claude_model: claude-sonnet-5
codex_model: gpt-5.6-sol
",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.resolve_model, None, "an older file routes nothing");
        assert_eq!(absent.resolve_effort, None, "an older file sends no effort");

        std::fs::write(
            &path,
            "claude_model: claude-sonnet-5
codex_model: gpt-5.6-sol
resolve_model: \"\"
resolve_effort: \"  \"
",
        )
        .unwrap();
        let blank = load_models_config(&path).unwrap();
        assert_eq!(
            blank.resolve_model, None,
            "a blanked key means off, not empty"
        );
        assert_eq!(
            blank.resolve_effort, None,
            "a blanked key means off, not empty"
        );
    }

    /// Absent and malformed must reach the SAME inert state, and neither may be an error.
    ///
    /// Written in the negative because the failure this guards is a daemon that will not start: this
    /// reader deliberately follows `load_email_config`, not `load_schedule_rules`, so a typo in a
    /// dictation aid cannot take down mail, autopilot and the API with it. The malformed case also has
    /// to land on `armed() == false` rather than merely on defaults — the file holds a command the
    /// daemon spawns, and "off" is the only safe reading of a file it could not understand.
    #[test]
    fn voice_config_absent_and_malformed_both_mean_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.yaml");

        let absent = load_voice_config(&path);
        assert!(!absent.armed());
        assert_eq!(absent.cleanup_prompt, DEFAULT_CLEANUP_PROMPT);

        std::fs::write(&path, "enabled: [this is not a bool\n  stt_command:\n").unwrap();
        let malformed = load_voice_config(&path);
        assert!(!malformed.armed());
        assert_eq!(malformed, VoiceConfig::default());

        std::fs::write(
            &path,
            "enabled: true\nstt_command: whisper-cli -m model.bin\n",
        )
        .unwrap();
        assert!(load_voice_config(&path).armed());
    }

    /// `enabled: true` with nothing to transcribe with is off, not half-on.
    ///
    /// The direction matters: the alternative is a hotkey that records audio and then has nowhere to
    /// send it, which reads as a broken feature rather than an absent one. A blank prompt resolves to
    /// the default for the same reason deleting the key does — that is the documented reset.
    #[test]
    fn voice_enabled_without_a_transcriber_is_not_armed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.yaml");

        std::fs::write(&path, "enabled: true\nstt_command: \"   \"\n").unwrap();
        assert!(!load_voice_config(&path).armed());

        std::fs::write(
            &path,
            "enabled: true\nstt_command: whisper-cli\ncleanup_prompt: \"  \"\nretain_dictations_days: 99\n",
        )
        .unwrap();
        let clamped = load_voice_config(&path);
        assert_eq!(clamped.cleanup_prompt, DEFAULT_CLEANUP_PROMPT);
        assert_eq!(clamped.retain_dictations_days, 30);
    }

    /// A resident transcriber is a transcriber, and `armed()` may not be a synonym for `stt_command`.
    ///
    /// The same regression `a_resident_engine_is_a_voice_even_with_no_command` guards on the speaking
    /// side, and it bites harder here: `armed()` gates the WHOLE pillar, so a machine pointed only at
    /// a resident `whisper-server` -- the configuration that is six times faster -- would report
    /// having no voice at all and hide every control for it.
    #[test]
    fn a_resident_transcriber_arms_the_pillar_with_no_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.yaml");

        std::fs::write(&path, "enabled: true\nstt_url: http://127.0.0.1:5018\n").unwrap();
        let resident = load_voice_config(&path);
        assert_eq!(resident.stt_url, "http://127.0.0.1:5018");
        assert!(resident.armed());

        // And blank is still blank, by both keys at once: whitespace is what a half-finished edit
        // leaves behind, and it must not arm anything.
        std::fs::write(
            &path,
            "enabled: true\nstt_url: \"   \"\nstt_command: \"  \"\n",
        )
        .unwrap();
        assert!(!load_voice_config(&path).armed());
    }

    /// The shipped chords have to be chords THIS platform's own desktop leaves free.
    ///
    /// macOS is the one that breaks: `Ctrl+Alt+Space` is taken by the system's input-source
    /// switcher there, so registering it wins nothing and costs the person a key they already
    /// use. Adding the Command key clears the whole family at once. `Super` is the spelling and
    /// not `Cmd` only because `global-hotkey`'s parser treats "COMMAND" | "CMD" | "SUPER" as one
    /// modifier, so the name is free and this one reads the same on every host.
    ///
    /// The second half is what keeps the constant honest: a default written twice is two places
    /// to change, and the platform that gets forgotten is the one nobody develops on.
    #[test]
    fn the_default_hotkeys_avoid_this_platforms_system_chords() {
        #[cfg(target_os = "macos")]
        let expected = [
            "Ctrl+Alt+Super+Space",
            "Ctrl+Alt+Super+M",
            "Ctrl+Alt+Super+C",
            "Ctrl+Alt+Super+N",
        ];
        #[cfg(not(target_os = "macos"))]
        let expected = ["Ctrl+Alt+Space", "Ctrl+Alt+M", "Ctrl+Alt+C", "Ctrl+Alt+N"];

        assert_eq!(
            DEFAULT_HOTKEYS, expected,
            "the defaults this platform ships have to be the chords its desktop leaves free"
        );

        let defaults = VoiceConfig::default();
        assert_eq!(
            defaults.hotkey, DEFAULT_HOTKEYS[0],
            "the dictation default must come from DEFAULT_HOTKEYS, not from a second literal"
        );
        assert_eq!(
            defaults.memo_hotkey, DEFAULT_HOTKEYS[1],
            "the memo default must come from DEFAULT_HOTKEYS, not from a second literal"
        );
        assert_eq!(
            defaults.conversation_hotkey, DEFAULT_HOTKEYS[2],
            "the conversation default must come from DEFAULT_HOTKEYS, not from a second literal"
        );
        assert_eq!(
            defaults.capture_hotkey, DEFAULT_HOTKEYS[3],
            "the capture default must come from DEFAULT_HOTKEYS, not from a second literal"
        );
    }

    /// The second runner ships dark, so what this key parses to is what decides whether a run is
    /// answered by the proven CLI or by one nobody asked for.
    ///
    /// The unrecognised case is the one worth a test: startup maps anything but `codex` back to the
    /// Claude runner, and it can only do that if loading SUCCEEDS and hands it the name. Were the
    /// deserializer to reject an unknown value instead, a typo in one key would take the whole
    /// daemon down — email, scheduler and all — rather than costing the operator the runner they
    /// misspelled.
    #[test]
    fn primary_runner_is_optional_and_an_unknown_name_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\n",
        )
        .unwrap();
        let absent = load_models_config(&path).unwrap();
        assert_eq!(absent.codex_model, "gpt-5.6-sol");
        assert_eq!(
            absent.primary_runner, None,
            "an absent key must not opt a daemon into the second runner"
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nprimary_runner: codex\n",
        )
        .unwrap();
        assert_eq!(
            load_models_config(&path).unwrap().primary_runner,
            Some("codex".to_string()),
            "the one recognised name must survive parsing for startup to act on"
        );

        std::fs::write(
            &path,
            "claude_model: claude-opus-4-8\ncodex_model: gpt-5.6-sol\nprimary_runner: gemini\n",
        )
        .unwrap();
        let unknown = load_models_config(&path)
            .expect("an unrecognised runner must cost the operator a warning, not the daemon");
        assert_eq!(
            unknown.primary_runner,
            Some("gemini".to_string()),
            "startup needs the name it did not recognise in order to warn about it"
        );
        assert_ne!(
            unknown.primary_runner.as_deref(),
            Some("codex"),
            "nothing but the exact name may reach the codex branch of the runner match"
        );
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(&path, "not: [valid, yaml for this struct").unwrap();
        assert!(load_models_config(&path).is_err());
    }

    #[test]
    fn schedule_rules_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_schedule_rules(Some(dir.path()), "p").unwrap(),
            AutopilotRules::default()
        );
    }

    /// Spec A D7: a project's judge thresholds are read, pulled to the measured limits, and a
    /// misspelt key is a startup error like every other key in this file.
    #[test]
    fn a_projects_judge_thresholds_only_tighten() {
        let absent = parse_schedule_rules("gate_command: \"true\"\n").unwrap();
        assert_eq!(
            absent.judge_thresholds(),
            crate::judge::Thresholds::default()
        );
        let tighter = parse_schedule_rules("judge:\n  allow_at: 0.9\n  deny_at: 0.05\n").unwrap();
        assert_eq!(
            tighter.judge_thresholds(),
            crate::judge::Thresholds {
                allow_at: 0.9,
                deny_at: 0.05
            }
        );
        let looser = parse_schedule_rules("judge:\n  allow_at: 0.5\n  deny_at: 0.6\n").unwrap();
        assert_eq!(
            looser.judge_thresholds(),
            crate::judge::Thresholds {
                allow_at: crate::judge::ALLOW_AT_FLOOR,
                deny_at: crate::judge::DENY_AT_CEILING,
            }
        );
        assert!(parse_schedule_rules("judge:\n  allow: 0.9\n").is_err());
    }

    /// Spec B D4: a project's resolver thresholds are read, pulled toward caution, and a misspelt
    /// key is a startup error like every other key in this file.
    #[test]
    fn a_projects_resolver_thresholds_only_move_toward_caution() {
        use crate::judge::resolve::ResolveThresholds;
        let absent = parse_schedule_rules("gate_command: \"true\"\n").unwrap();
        assert_eq!(absent.resolve_thresholds(), ResolveThresholds::default());
        let careful =
            parse_schedule_rules("judge_resolve:\n  off_task_at: 0.7\n  fixable_at: 0.95\n")
                .unwrap();
        assert_eq!(careful.resolve_thresholds().off_task_at, 0.7);
        assert_eq!(careful.resolve_thresholds().fixable_at, 0.95);
        let loose = parse_schedule_rules("judge_resolve:\n  avoidable_at: 0.5\n").unwrap();
        assert_eq!(loose.resolve_thresholds().avoidable_at, 0.85);
        assert!(parse_schedule_rules("judge_resolve:\n  offtask: 0.7\n").is_err());
    }

    /// The brake a project never configured is ON, and the derived `Default` is exactly why this
    /// is worth an assertion of its own: `attention_brake` is the one field here whose zero value
    /// would otherwise have meant "off", on every project that has no rules file at all.
    #[test]
    fn a_project_that_configured_nothing_keeps_the_attention_brake() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            load_schedule_rules(Some(dir.path()), "p")
                .unwrap()
                .attention_brake()
        );
        assert!(AutopilotRules::default().attention_brake());
    }

    #[test]
    fn health_breach_intent_is_absent_and_off_by_default() {
        let rules = AutopilotRules::default();
        assert!(!rules.health_breach_intent);
        assert!(!rules_from("schedules: []\n").unwrap().health_breach_intent);
    }

    #[test]
    fn an_unknown_key_in_the_rules_block_is_a_startup_error() {
        let error = rules_from("health_breach_intnt: true\n")
            .expect_err("a misspelled rules key must be rejected at startup");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn the_attention_brake_is_switched_off_by_name_and_back_on_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects").join("p")).unwrap();
        let path = dir.path().join("projects").join("p").join("autopilot.yaml");

        std::fs::write(&path, "attention_brake: false\n").unwrap();
        assert!(
            !load_schedule_rules(Some(dir.path()), "p")
                .unwrap()
                .attention_brake()
        );

        // Spelled out rather than left to the test above: "absent" and "present and true" are
        // different inputs that must reach the same answer, and only one of them is the default.
        std::fs::write(&path, "attention_brake: true\n").unwrap();
        assert!(
            load_schedule_rules(Some(dir.path()), "p")
                .unwrap()
                .attention_brake()
        );
    }

    #[test]
    fn schedule_rules_parses_two_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects").join("p")).unwrap();
        std::fs::write(
            dir.path().join("projects").join("p").join("autopilot.yaml"),
            "schedules:\n\
             \x20\x20- name: nightly-build\n\
             \x20\x20\x20\x20cron: \"0 2 * * *\"\n\
             \x20\x20\x20\x20prompt: \"run the nightly build\"\n\
             \x20\x20\x20\x20cwd: /repo\n\
             \x20\x20- name: morning-report\n\
             \x20\x20\x20\x20cron: \"0 8 * * *\"\n\
             \x20\x20\x20\x20prompt: \"summarize overnight activity\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(Some(dir.path()), "p").unwrap();
        assert_eq!(rules.schedules.len(), 2);

        assert_eq!(rules.schedules[0].name, "nightly-build");
        assert_eq!(rules.schedules[0].cron, "0 2 * * *");
        assert_eq!(rules.schedules[0].prompt, "run the nightly build");
        assert_eq!(rules.schedules[0].cwd, Some("/repo".to_string()));

        assert_eq!(rules.schedules[1].name, "morning-report");
        assert_eq!(rules.schedules[1].cron, "0 8 * * *");
        assert_eq!(rules.schedules[1].prompt, "summarize overnight activity");
    }

    #[test]
    fn schedule_rules_entry_without_cwd_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects").join("p")).unwrap();
        std::fs::write(
            dir.path().join("projects").join("p").join("autopilot.yaml"),
            "schedules:\n\
             \x20\x20- name: morning-report\n\
             \x20\x20\x20\x20cron: \"0 8 * * *\"\n\
             \x20\x20\x20\x20prompt: \"summarize overnight activity\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(Some(dir.path()), "p").unwrap();
        assert_eq!(rules.schedules[0].cwd, None);
    }

    /// The state between "no file" and "broken file". serde_yaml reports a document-less stream as
    /// EndOfStream, which reads as malformed — so without this, commenting out the `gate_command:`
    /// line (the obvious way to switch a gate off) turns every completed worktree run into
    /// `gate errored`. Both spellings, because a comments-only file is the realistic one.
    #[test]
    fn schedule_rules_with_no_yaml_document_is_not_a_gate_rather_than_an_error() {
        for contents in ["", "   \n\n", "# just a comment\n# and another\n"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("projects").join("p")).unwrap();
            std::fs::write(
                dir.path().join("projects").join("p").join("autopilot.yaml"),
                contents,
            )
            .unwrap();

            let rules = load_schedule_rules(Some(dir.path()), "p")
                .unwrap_or_else(|e| panic!("{contents:?} must not be an error, got {e}"));
            assert_eq!(rules.gate_command, None);
            assert!(
                !rules.gate_before_publish,
                "a file with nothing in it must not switch a brake on"
            );
            assert!(rules.schedules.is_empty());
            assert!(rules.repo_triggers.is_empty());
        }
    }

    #[test]
    fn schedule_rules_malformed_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects").join("p")).unwrap();
        std::fs::write(
            dir.path().join("projects").join("p").join("autopilot.yaml"),
            "schedules: [not, valid, for this struct",
        )
        .unwrap();

        assert!(load_schedule_rules(Some(dir.path()), "p").is_err());
    }

    #[test]
    fn repo_triggers_parse_and_default_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects").join("p")).unwrap();
        std::fs::write(
            dir.path().join("projects").join("p").join("autopilot.yaml"),
            "repo_triggers:\n\
             \x20\x20- name: review-main\n\
             \x20\x20\x20\x20branch: main\n\
             \x20\x20\x20\x20prompt: \"review new commits on main\"\n",
        )
        .unwrap();

        let rules = load_schedule_rules(Some(dir.path()), "p").unwrap();
        assert!(rules.schedules.is_empty());
        assert_eq!(rules.repo_triggers.len(), 1);
        assert_eq!(rules.repo_triggers[0].name, "review-main");
        assert_eq!(rules.repo_triggers[0].branch, "main");
        assert_eq!(rules.repo_triggers[0].prompt, "review new commits on main");
    }

    /// Absent, unreadable or malformed → defaults, the same asymmetry `load_web_config` and
    /// `load_browser_config` both take: a typo in a per-developer YAML must cost fidelity (no
    /// doctrine to prepend) and never stop the daemon, and never invent a doctrine nobody wrote.
    #[test]
    fn um_telegram_yaml_malformado_cai_no_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram.yaml");
        std::fs::write(&path, "doctrine: [this is not a string\n").unwrap();

        let config = load_telegram_config(&path);

        assert_eq!(
            config.doctrine, None,
            "a malformed telegram.yaml must fall back to the default, not invent a doctrine"
        );
    }

    // ---------------------------------------------------------------------------------------
    // `marked_with` — each local choice marked by what its model was found to declare
    // ---------------------------------------------------------------------------------------

    fn unmarked_choice(id: &str) -> AssistantChoice {
        AssistantChoice {
            id: id.to_string(),
            label: id.to_string(),
            brain: "local".to_string(),
            efforts: Vec::new(),
            runner: None,
            tools: None,
            installed: None,
        }
    }

    /// `Some(true)` for a model that declared tools, `Some(false)` for one that explicitly did
    /// not — never guessed for either, and never the same answer for two different models.
    #[test]
    fn o_catalogo_marca_quem_nao_declara_ferramentas() {
        let choices = vec![
            unmarked_choice("qwen3.5:4b"),
            unmarked_choice("llama3.2:3b"),
        ];
        let mut declared = std::collections::HashMap::new();
        declared.insert(
            "qwen3.5:4b".to_string(),
            crate::capabilities::Declared {
                context: Ok(()),
                tools: false,
                vision: false,
                structured_output: false,
            },
        );
        declared.insert(
            "llama3.2:3b".to_string(),
            crate::capabilities::Declared {
                context: Ok(()),
                tools: true,
                vision: false,
                structured_output: false,
            },
        );

        let marked = marked_with(choices, &declared);

        let by_id = |id: &str| -> &AssistantChoice {
            marked
                .iter()
                .find(|choice| choice.id == id)
                .unwrap_or_else(|| panic!("marked_with dropped {id}: {marked:?}"))
        };
        assert_eq!(
            by_id("qwen3.5:4b").tools,
            Some(false),
            "a model that explicitly did not declare tools must be marked Some(false), not left \
             None"
        );
        assert_eq!(
            by_id("llama3.2:3b").tools,
            Some(true),
            "a model that declared tools must be marked Some(true)"
        );
    }

    /// A choice `declared` never mentions must stay `None` — not marked servable, not marked
    /// broken. `AssistantChoice::tools`'s own doc is the reason: "declares no tools" and "we have
    /// not asked" are different facts, and only one of them is worth warning anybody about.
    #[test]
    fn um_modelo_de_capacidades_desconhecidas_nao_e_marcado_de_lado_nenhum() {
        let choices = vec![unmarked_choice("novo-modelo")];
        let declared = std::collections::HashMap::new();

        let marked = marked_with(choices, &declared);

        assert_eq!(
            marked.len(),
            1,
            "marked_with must not drop or add choices, only mark them: {marked:?}"
        );
        assert_eq!(
            marked[0].tools, None,
            "a model absent from the declared map must be marked neither servable nor broken: \
             {marked:?}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // `local_engine()` -- WHICH local server a turn is sent to, and whether it is on this machine
    // ---------------------------------------------------------------------------------------

    /// A local route with no address is a route with nowhere to go, and the obvious repair is the
    /// one thing that must never happen: falling back to Ollama's port because it is the only
    /// local port this daemon has ever known. `local_engine: openai_compatible` on `11434` would post a turn
    /// -- mail, a transcript, a repository's contents -- to whatever program happens to be
    /// listening there, under a config file that named no address at all. Somebody who wrote the
    /// engine and forgot the URL has to be told so at the config, not left to infer it from a
    /// reply that came back from the wrong server.
    #[test]
    fn a_local_openai_compatible_engine_with_no_base_url_is_refused_rather_than_guessing_a_port() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            local_engine: Some("openai_compatible".to_string()),
            local_base_url: None,
            ..ModelsConfig::default()
        };

        match config.local_engine() {
            Err(LocalEngineRefusal::NoBaseUrl) => {}
            other => panic!(
                "an openai_compatible engine with no address must be refused, never resolved onto Ollama's \
                 own port: {other:?}"
            ),
        }

        let refusal = LocalEngineRefusal::NoBaseUrl;
        assert!(
            refusal.message().contains("local_base_url"),
            "a refusal that does not name the key that fixes it sends its reader looking: {}",
            refusal.message()
        );
    }

    /// `Brain::Local`'s promise is that a local turn never leaves this machine. Written in a doc
    /// comment that is a claim; asserted here it is a check. Without it, `local_base_url` makes
    /// "local" mean "whatever address the file says", and one line in `~/.nucleos/nucleos-models.yaml`
    /// is enough to post mail and repository contents to a third party under the name of the
    /// route chosen precisely to avoid that.
    ///
    /// Both halves are asserted together, because either one alone is a half-fix. Refusing only
    /// at `local_engine()` leaves the picker still offering the local rows: the person picks a
    /// model, the turn dies, and nothing on screen connects the refusal to the address. A menu
    /// must not offer a route that cannot legally run.
    #[test]
    fn a_local_engine_pointed_off_this_machine_is_refused_and_its_route_leaves_the_menu() {
        let off_machine = "https://api.example.com/v1";
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            local_engine: Some("openai_compatible".to_string()),
            local_base_url: Some(off_machine.to_string()),
            ..ModelsConfig::default()
        };

        match config.local_engine() {
            Err(LocalEngineRefusal::NotLoopback(named)) => assert_eq!(
                named, off_machine,
                "the refusal must carry the address it refused, or its message cannot name it"
            ),
            other => panic!(
                "an address off this machine must be refused, whatever `local` is written beside \
                 it: {other:?}"
            ),
        }

        let installed = vec!["qwen3.5:4b".to_string(), "llama3.2:3b".to_string()];
        assert!(
            config
                .catalogue_with_installed(&installed)
                .iter()
                .all(|choice| choice.brain != "local"),
            "a route that will refuse every turn must not be on the menu: {:?}",
            config.catalogue_with_installed(&installed)
        );
        assert!(
            config
                .catalogue_scoped(&installed, true)
                .iter()
                .all(|choice| choice.brain != "local"),
            "the every-runner scope feeds the same picker and must drop them too: {:?}",
            config.catalogue_scoped(&installed, true)
        );

        // The single rule both halves rest on, asserted where it is written rather than only
        // through its two callers -- a loopback test that is wrong is wrong in both of them.
        assert!(is_loopback_url("http://127.0.0.1:11434"));
        assert!(is_loopback_url("http://localhost:1234/v1"));
        assert!(
            is_loopback_url("http://[::1]:1234/v1"),
            "a server bound to the IPv6 loopback is on this machine; refusing it would send \
             somebody editing the code instead of the config"
        );
        assert!(!is_loopback_url(off_machine));
        assert!(
            !is_loopback_url("http://127.0.0.1.example.com/v1"),
            "a host that merely BEGINS with the loopback address belongs to whoever registered \
             it -- a prefix match here is the whole hole this check exists to close"
        );
    }

    /// Every `~/.nucleos/nucleos-models.yaml` on this machine was written before these three keys
    /// existed and names none of them. If absence resolved to anything but Ollama on
    /// `runner::OLLAMA_BASE_URL`, a file that worked this morning would answer a refusal this
    /// afternoon, for a feature its owner never asked for -- the same "an existing file keeps
    /// working" argument every `#[serde(default)]` on these fields is making.
    ///
    /// Asserted against the constant rather than a second copy of the literal, so the default and
    /// the runner's own address cannot drift apart without this failing.
    #[test]
    fn no_local_engine_configured_still_resolves_to_ollama_on_the_loopback_constant() {
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            ..ModelsConfig::default()
        };
        assert_eq!(
            (
                &config.local_engine,
                &config.local_base_url,
                &config.local_context_tokens
            ),
            (&None, &None, &None),
            "the fixture is the pre-existing file: all three keys absent"
        );

        let resolved = config
            .local_engine()
            .expect("a file naming none of the new keys must keep resolving, not start refusing");

        assert_eq!(
            resolved.engine,
            LocalEngine::Ollama,
            "absent means today's engine, never a new default nobody chose"
        );
        assert_eq!(
            resolved.base_url,
            crate::runner::OLLAMA_BASE_URL,
            "absent means the address the runner already uses, read from the constant itself"
        );
        assert_eq!(
            resolved.declared_context_tokens, None,
            "nobody declared a window, and a guessed one is a number the daemon would then act on"
        );
    }

    /// `installed` answers exactly one question -- "has `ollama pull` fetched this?" -- and it is
    /// asked of `/api/tags`, which only Ollama serves. Point the local route at an
    /// OpenAI-compatible server and the question stops applying, precisely as it never applied to
    /// a cloud row, and `AssistantChoice::installed`'s own doc says `None` is what that means.
    ///
    /// `Some(false)` would be worse than a wrong answer: it would hang a "not installed" warning
    /// on every local row on the menu, and the one repair anybody would reach for -- `ollama
    /// pull` -- has nothing to do with the server actually serving them. That is the failure the
    /// `tools` field's doc argues against, arriving through the other field.
    #[test]
    fn a_local_row_is_unmarked_rather_than_uninstalled_when_the_engine_is_not_ollama() {
        let written_by_hand = || AssistantChoice {
            id: "llama3.2:3b".to_string(),
            label: "Llama 3.2 3B".to_string(),
            brain: "local".to_string(),
            efforts: Vec::new(),
            runner: None,
            tools: None,
            installed: None,
        };
        let config = ModelsConfig {
            local_assistant_model: Some("qwen3.5:4b".to_string()),
            local_engine: Some("openai_compatible".to_string()),
            local_base_url: Some("http://127.0.0.1:1234/v1".to_string()),
            assistant_choices: vec![written_by_hand()],
            ..ModelsConfig::default()
        };

        let catalogue = config.catalogue_with_installed(&["qwen3.5:4b".to_string()]);
        let local: Vec<&AssistantChoice> = catalogue
            .iter()
            .filter(|choice| choice.brain == "local")
            .collect();
        assert!(
            !local.is_empty(),
            "the route is configured and the address is on this machine, so its rows belong on \
             the menu: {catalogue:?}"
        );
        for choice in &local {
            assert_eq!(
                choice.installed, None,
                "`installed` is an `/api/tags` question and this engine does not serve it, so the \
                 row is unmarked, not marked absent: {choice:?}"
            );
        }

        // The other side of the same rule: on Ollama the question DOES apply, so this must not
        // have been bought by blanking the field for every local row everywhere.
        let on_ollama = ModelsConfig {
            local_engine: None,
            local_base_url: None,
            ..config.clone()
        };
        assert!(
            on_ollama
                .catalogue_with_installed(&["qwen3.5:4b".to_string()])
                .iter()
                .filter(|choice| choice.brain == "local")
                .any(|choice| choice.installed.is_some()),
            "on Ollama `/api/tags` answers the question and the menu must still say so"
        );
    }

    #[test]
    fn devtime_config_absent_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_devtime_config(&dir.path().join("devtime.yaml"));
        assert_eq!(config, DevtimeConfig::default());
        assert!(config.enabled);
        assert_eq!(config.cycle_seconds, 60);
        assert_eq!(config.idle_minutes, 15);
        assert_eq!(config.projects_dir, "");
        assert_eq!(config.parse_failure_amber_rate, 0.02);
        assert_eq!(config.parse_failure_min_lines, 200);
    }

    #[test]
    fn devtime_config_partial_file_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devtime.yaml");
        std::fs::write(
            &path,
            "enabled: false
cycle_seconds: 30
",
        )
        .unwrap();
        let config = load_devtime_config(&path);
        assert!(!config.enabled);
        assert_eq!(config.cycle_seconds, 30);
        assert_eq!(config.idle_minutes, 15);
        assert_eq!(config.parse_failure_min_lines, 200);
        // The parser agrees with the loader, and refuses a key it does not know.
        assert_eq!(
            parse_devtime_config(
                "idle_minutes: 5
"
            )
            .unwrap()
            .idle_minutes,
            5
        );
        assert!(
            parse_devtime_config(
                "no_such_key: 1
"
            )
            .is_err()
        );
    }

    #[test]
    fn an_empty_verify_file_takes_the_defaults() {
        assert_eq!(parse_verify_config("").unwrap(), VerifyConfig::default());
        assert_eq!(
            parse_verify_config("# nothing\n").unwrap(),
            VerifyConfig::default()
        );
        let defaults = VerifyConfig::default();
        assert_eq!(defaults.capacity, 4);
        assert_eq!(defaults.aging_seconds, 300);
        assert_eq!(defaults.unit_timeout_seconds, 1800);
        assert_eq!(defaults.disk_cap_gb, 60);
        assert!(defaults.broker_prefix.is_empty());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_verify_config(&dir.path().join("verify.yaml")),
            VerifyConfig::default()
        );
    }

    #[test]
    fn verify_settings_are_read() {
        let config = parse_verify_config(
            "capacity: 2\naging_seconds: 60\nunit_timeout_seconds: 900\ndisk_cap_gb: 10\nbroker_prefix: [python, heavy.py, --, x]\n",
        )
        .unwrap();
        assert_eq!(config.capacity, 2);
        assert_eq!(config.aging_seconds, 60);
        assert_eq!(config.unit_timeout_seconds, 900);
        assert_eq!(config.disk_cap_gb, 10);
        assert_eq!(config.broker_prefix, vec!["python", "heavy.py", "--", "x"]);
    }

    #[test]
    fn a_verify_capacity_of_zero_is_refused() {
        assert!(parse_verify_config("capacity: 0\n").is_err());
        assert!(parse_verify_config("aging_seconds: 0\n").is_err());
        assert!(parse_verify_config("unit_timeout_seconds: 0\n").is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("verify.yaml");
        std::fs::write(&path, "capacity: 0\n").unwrap();
        assert_eq!(load_verify_config(&path), VerifyConfig::default());
    }

    #[test]
    fn a_verify_disk_cap_of_zero_is_refused() {
        assert!(parse_verify_config("disk_cap_gb: 0\n").is_err());
    }

    #[test]
    fn a_project_verify_disk_cap_of_zero_is_refused() {
        assert!(parse_schedule_rules("verify_disk_cap_gb: 0\n").is_err());
    }

    #[test]
    fn an_unknown_verify_key_is_refused() {
        assert!(parse_verify_config("no_such_key: 1\n").is_err());
    }

    #[test]
    fn the_project_verify_disk_cap_is_read_from_autopilot() {
        let rules = parse_schedule_rules("verify_disk_cap_gb: 12\n").unwrap();
        assert_eq!(rules.verify_disk_cap_gb, Some(12));
        assert_eq!(
            parse_schedule_rules("gate_command: x\n")
                .unwrap()
                .verify_disk_cap_gb,
            None
        );
    }

    #[test]
    fn devtime_config_malformed_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devtime.yaml");
        std::fs::write(
            &path,
            "cycle_seconds: [not, a, number
",
        )
        .unwrap();
        assert_eq!(load_devtime_config(&path), DevtimeConfig::default());
        assert!(
            parse_devtime_config(
                "cycle_seconds: [not, a, number
"
            )
            .is_err()
        );
    }

    #[test]
    fn devtime_rules_config_defaults_match_the_spec() {
        let rules = DevtimeConfig::default().rules;
        assert!(rules.enabled);
        assert_eq!(rules.sessions_per_cycle, 200);
        assert_eq!(rules.commands.test.len(), 8);
        assert!(rules.commands.test.contains(&"make test".to_string()));
        assert!(rules.commands.build.contains(&"make".to_string()));
        assert!(rules.commands.lint.contains(&"gofmt".to_string()));
        assert!(rules.commands.mutating.contains(&"git restore".to_string()));
        assert_eq!(rules.commands.sleep, ["sleep", "Start-Sleep", "timeout"]);
        assert_eq!(rules.commands.commit, ["git commit"]);
        assert_eq!(
            rules.commands.revert_with_paths,
            ["git checkout", "git restore"]
        );
        assert_eq!(rules.commands.revert_pathless, ["git reset", "git stash"]);
        assert_eq!(rules.commands.shell_tools, ["Bash", "PowerShell"]);
        assert_eq!(rules.commands.read_tools, ["Read"]);
        assert_eq!(rules.commands.search_tools, ["Grep", "Glob"]);
        assert!(rules.commands.per_project.is_empty());
        assert_eq!(rules.roles.reviewer, ["*review*"]);
        assert_eq!(
            rules.roles.implementer,
            ["*executor*", "*implement*", "general-purpose"]
        );
        assert_eq!(rules.vocab.model_strength, ["haiku", "sonnet", "opus"]);
        assert!(rules.vocab.correction_openers.contains(&"nope".to_string()));
        let signatures = &rules.vocab.error_signatures;
        assert_eq!(signatures["wrong_shell"].len(), 5);
        assert_eq!(signatures["hook_block"].len(), 3);
        assert_eq!(signatures["edit_not_found"].len(), 2);
        assert!(signatures["permission_denied"].is_empty());
        let t = &rules.thresholds;
        assert_eq!(t.d3_search_burst, 6);
        assert_eq!(t.d3_flailing_burst, 4);
        assert_eq!(t.d5_foreground_agent_seconds, 120);
        assert_eq!(t.d6_foreground_command_seconds, 120);
        assert_eq!(t.thrash_repeats, 3);
        assert_eq!(t.thrash_reset_calls, 40);
        assert_eq!(t.late_scope_fraction, 0.3);
        assert_eq!(t.late_scope_min_files, 3);
        assert_eq!(t.d8_poll_repeats, 3);
        assert_eq!(t.d9_context_tokens, 250_000);
        assert_eq!(t.f_min_sessions, 3);
        assert_eq!(t.f_sequence_len, 3);
        assert_eq!(t.f_window_days, 30);
        assert_eq!(rules.paths.external.len(), 5);
        assert!(rules.paths.code_extensions.contains(&"rs".to_string()));
        assert_eq!(rules.precision.floor, 0.8);
        assert_eq!(rules.precision.min_cases, 20);
        assert_eq!(rules.precision.prior_default, 0.5);
        assert_eq!(rules.precision.priors.get("C2"), Some(&0.2));
        let u = &rules.unexplained;
        assert_eq!(u.median_multiple, 3.0);
        assert_eq!(u.min_turn_seconds, 300);
        assert_eq!(u.max_explained_fraction, 0.2);
        assert_eq!(u.class_bounds, [0, 5, 20]);
        assert_eq!(u.min_class_turns, 10);
        assert_eq!(u.window_days, 30);
        assert_eq!(rules.adapters.permission_log, "");
        assert_eq!(rules.adapters.heavy_log, "");
        // The shipped defaults are themselves a valid file.
        assert!(validate_devtime(&DevtimeConfig::default()).is_ok());
    }

    #[test]
    fn devtime_rules_partial_nested_file_keeps_other_defaults() {
        let config =
            parse_devtime_config("rules:\n  thresholds:\n    d3_search_burst: 8\n").unwrap();
        let mut expected = DevtimeRulesConfig::default();
        assert_eq!(config.rules.thresholds.d3_search_burst, 8);
        expected.thresholds.d3_search_burst = 8;
        assert_eq!(config.rules, expected);
        assert_eq!(config.cycle_seconds, 60);
    }

    #[test]
    fn devtime_rules_unknown_nested_key_or_bad_class_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devtime.yaml");
        for bad in [
            "rules:\n  thresholds:\n    nope: 1\n",
            "rules:\n  vocab:\n    error_signatures:\n      bogus: [x]\n",
            "rules:\n  thresholds:\n    late_scope_fraction: 1.5\n",
            "rules:\n  thresholds:\n    thrash_repeats: 0\n",
            "rules:\n  precision:\n    floor: -0.1\n",
        ] {
            assert!(parse_devtime_config(bad).is_err(), "should refuse: {bad}");
            std::fs::write(&path, bad).unwrap();
            assert_eq!(load_devtime_config(&path), DevtimeConfig::default());
        }
    }

    /// Embeddings are always local (spec 5.4, D6): the key names an Ollama model, and a file that
    /// predates it must keep working, so absence is the small default rather than an error.
    #[test]
    fn embedding_model_defaults_to_a_small_ollama_model_and_can_be_overridden() {
        assert_eq!(DEFAULT_EMBEDDING_MODEL, "nomic-embed-text");
        assert_eq!(
            ModelsConfig::default().embedding_model,
            DEFAULT_EMBEDDING_MODEL
        );

        let absent = parse_models_config(
            "claude_model: sonnet
codex_model: gpt
",
        )
        .unwrap();
        assert_eq!(absent.embedding_model, DEFAULT_EMBEDDING_MODEL);

        let named = parse_models_config(
            "claude_model: sonnet
codex_model: gpt
embedding_model: mxbai-embed-large
",
        )
        .unwrap();
        assert_eq!(named.embedding_model, "mxbai-embed-large");
    }
}
