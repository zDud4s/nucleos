//! The assistant factory: given a route (`chats::Brain`) and an optional pinned model, produces
//! the `LocalAssistant` that answers a turn.
//!
//! Replaces two near-identical `main.rs` blocks — the local singleton and the hosted singleton —
//! that each build one `LocalAssistant` once at startup with a model baked in, so a chat's pinned
//! model (`chats::Answering::model`) is silently ignored on both routes today. `assistant_for`
//! reads the pin per call instead; nothing here is a singleton.
//!
//! `assistant_for` is SYNC and does no network work of its own: startup has already probed what a
//! model can do (`capabilities.rs`), so this module only assembles what that probe already found —
//! a network call per turn is exactly what the capability cache upstream exists to prevent. Which
//! model actually answers — the turn's pin, or the route's configured default — is the separate
//! PURE function `resolve_model`, testable with no pool, no client and no runtime.
//!
//! `ConfiguredAssistants` holds ONE `reqwest::Client` per route and hands a clone to every
//! assistant it builds for that route, rather than building a client per turn — a constructor
//! invariant, not something a test compares: two `reqwest::Client`s do not compare for equality,
//! and the property is read in the constructor, not asserted from outside it.
//!
//! Wired in: `AppState.assistants` is `Arc<dyn Assistants>`, `main.rs` builds the one
//! `ConfiguredAssistants` at startup and has no singletons left, and the trait is reached from both
//! call sites in `assistant.rs`'s send path, from `council.rs` (a seat's local availability, twice —
//! the roster and the override), from `team.rs` (a local member's availability), and from
//! `http.rs` (`get_local_model`, `post_chat_title`) and `scheduler.rs` (an errand's own-criterion
//! check). This module sits on every chat turn, not beside it.

/// The sentence `Refusal::HostedModelNamedButNoKey` renders as.
///
/// A named constant for the reason `assistant::NO_LOCAL_MODEL` and `assistant::NO_HOSTED_MODEL`
/// are named ones: `http.rs` turns each into a status and a slug a caller can act on, and a
/// refusal recognised by a fragment of its wording stops being recognised the day somebody
/// improves the sentence. It lives HERE and not beside those two because the fact it reports is
/// the factory's — `assistant.rs` never reads a key.
pub const HOSTED_KEY_MISSING: &str =
    "an OpenRouter key is required before the hosted assistant can answer";

/// Why a route could not produce an assistant for a turn.
///
/// `RouteNotConfigured` renders as the exact sentence `http.rs` already compares with `==`
/// (`assistant::NO_LOCAL_MODEL`, `assistant::NO_HOSTED_MODEL`) — see `message`, below. The variant
/// itself carries no route: `resolve_model` builds it before it knows which route asked, so
/// rendering takes the route as an argument instead of the variant carrying one.
///
/// `HostedModelNamedButNoKey` is deliberately its own variant and not folded into
/// `RouteNotConfigured`: an operator who has named a hosted model but not yet stored a key is
/// half-way through setup, and a message that says "no model configured" tells them the opposite
/// of what is actually missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This route's config key (`config::ModelsConfig::local_assistant_model` /
    /// `hosted_assistant_model`) is absent — the route-off refusal. A pinned model never overrides
    /// it: `resolve_model`'s own doc states why the config key is the switch and the pin only
    /// chooses which model runs once the switch is on.
    RouteNotConfigured,
    /// `config::ModelsConfig::hosted_assistant_model` names a model, but no OpenRouter key is
    /// stored in the OS credential manager. Distinct from `RouteNotConfigured` on purpose — see
    /// this type's own doc comment.
    HostedModelNamedButNoKey,
    /// `chats::Brain::Cloud` asked this factory for an assistant. `Cloud` is the agent CLI — a
    /// different mechanism entirely (`spawn_assistant_turn`, sessions, a subprocess) — and this
    /// factory never serves it. Wrong door, not misconfiguration.
    NotServedByThisFactory,
    /// `can_serve` found this machine cannot actually run the model asked for — a capability the
    /// role needs (the context window, today; tools/vision/structured output later) that the model
    /// did not declare. Carries the picker's own sentence (`capabilities::picker_refusal`), never a
    /// second, looser copy of it — see `can_serve`'s own doc.
    ///
    /// Deliberately its own variant and not folded into `RouteNotConfigured`: that one means the
    /// ROUTE has no model at all, this one means a model was named and this machine cannot run it —
    /// two different problems an operator would act on differently.
    ///
    /// No production caller constructs this yet — `can_serve`'s real body is GREEN's job — but
    /// `http.rs`'s own `RefusingAssistants` test double already does, in the meantime.
    #[cfg_attr(not(test), allow(dead_code))]
    CannotServe(String),
}

impl Refusal {
    /// The sentence this refusal renders as for `brain` — the contract `http.rs:2142` already
    /// compares against with `==`. Only `RouteNotConfigured` varies with the route
    /// (`assistant::NO_LOCAL_MODEL` for `Brain::Local`, `assistant::NO_HOSTED_MODEL` for
    /// `Brain::OpenRouter`); the other two variants already know their own wording and ignore the
    /// argument.
    pub fn message(&self, brain: crate::chats::Brain) -> String {
        match self {
            Refusal::RouteNotConfigured => match brain {
                crate::chats::Brain::Local => crate::assistant::NO_LOCAL_MODEL,
                crate::chats::Brain::OpenRouter => crate::assistant::NO_HOSTED_MODEL,
                // `message` is `pub` on a `pub` enum, so `RouteNotConfigured.message(Brain::Cloud)`
                // is a call anybody can write even though `assistant_for` never reaches here for
                // `Brain::Cloud` (it refuses with `NotServedByThisFactory` first). Returning
                // `NotServedByThisFactory`'s own wording is the honest answer for `Brain::Cloud`
                // whatever the variant, not a placeholder — a panic here would be a panicking chat
                // turn reached by a combination the type system permits.
                crate::chats::Brain::Cloud => {
                    "this route answers through the agent CLI, not this factory"
                }
            }
            .to_string(),
            Refusal::HostedModelNamedButNoKey => HOSTED_KEY_MISSING.to_string(),
            Refusal::NotServedByThisFactory => {
                "this route answers through the agent CLI, not this factory".to_string()
            }
            // `CannotServe` carries its own reason — the picker's own sentence
            // (`capabilities::picker_refusal`) — so this is the one arm that reads the variant's
            // field instead of a fixed string, which is exactly why `message` had to widen from
            // `&'static str` to `String`: a per-model reason cannot be a compile-time constant.
            Refusal::CannotServe(reason) => reason.clone(),
        }
    }
}

/// PURE: the model that answers this turn — the conversation's pin when it has one, the route's
/// configured default when it does not.
///
/// Refuses `RouteNotConfigured` when both are absent, and — decision 6 of the plan — ALSO when a
/// pin exists but the route's config key does not: the config key is the switch, the pin only
/// chooses which model runs once the switch is on. The opposite would let an old `chats` row turn
/// on a route nobody asked for, by naming a model on it.
pub fn resolve_model(pinned: Option<&str>, configured: Option<&str>) -> Result<String, Refusal> {
    match configured {
        Some(configured) => Ok(pinned.unwrap_or(configured).to_string()),
        None => Err(Refusal::RouteNotConfigured),
    }
}

/// Which `mcp_tools::LocalToolBox` constructor a turn's `Brain` gets: `LocalToolBox::new`
/// (`LOCAL_TOOLS`, unrestricted) for `Brain::Local`, `LocalToolBox::for_hosted` (`HOSTED_TOOLS`,
/// six names) for `Brain::OpenRouter`. `LocalToolBox::with_tools` is private and `mcp_tools.rs` is
/// do-not-touch, so this branch is the whole of "the `ToolBox` chosen by the `brain`" —
/// `Brain::Cloud` never reaches here because `assistant_for` refuses it before this runs.
fn toolbox_for(
    brain: crate::chats::Brain,
    base_url: String,
    token: String,
    pool: sqlx::SqlitePool,
) -> Box<dyn crate::local_agent::ToolBox> {
    match brain {
        crate::chats::Brain::Local => {
            Box::new(crate::mcp_tools::LocalToolBox::new(base_url, token, pool))
        }
        crate::chats::Brain::OpenRouter => Box::new(crate::mcp_tools::LocalToolBox::for_hosted(
            base_url, token, pool,
        )),
        crate::chats::Brain::Cloud => {
            // Fail-closed, not a reachable path: `toolbox_for` is private and its only caller,
            // `assistant_for`, already refuses `Brain::Cloud` before this runs. But that guard is
            // one match arm away — a later reorder of `assistant_for`, or a fourth `Brain` variant,
            // must not turn a live chat turn into a panic. Hand back the most restrictive box, the
            // hosted allowlist, rather than the unrestricted local one, so a route that reaches
            // this arm by mistake gets a box that cannot act.
            Box::new(crate::mcp_tools::LocalToolBox::for_hosted(
                base_url, token, pool,
            ))
        }
    }
}

/// A trait, not a bare struct: a later packet puts this in `AppState`, and a test there must be
/// able to observe WHICH model a turn asked for — a concrete struct with a test constructor cannot
/// show that. `ConfiguredAssistants` is the one production implementor here; doubles arrive with
/// their callers later.
///
/// `#[async_trait]` because `can_serve` below is ASYNC while `assistant_for` and `serves` stay
/// sync — a plain `async fn` in a trait used behind `Arc<dyn Assistants>` (`AppState.assistants`)
/// is not object-safe without it, the same reason `local_agent::LocalChat` and `local_agent::ToolBox`
/// already carry the attribute.
#[async_trait::async_trait]
pub trait Assistants: Send + Sync {
    fn assistant_for(
        &self,
        brain: crate::chats::Brain,
        model: Option<&str>,
    ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, Refusal>;

    /// Whether this route can answer at all, without building anything to find out.
    fn serves(&self, brain: crate::chats::Brain) -> Result<(), Refusal>;

    /// Whether this machine can actually serve `model` on `brain` — the picker's question, asked
    /// before a choice is stored rather than discovered at the first turn.
    ///
    /// ASYNC where `serves` and `assistant_for` above are sync: this one may do discovery (an
    /// `/api/show` probe through `capabilities::DiscoveryCache`), which is the whole reason it is a
    /// third method rather than a widening of either.
    ///
    /// No production caller until `http.rs`'s `patch_chat` wires the picker's door onto it — GREEN's
    /// job, not this phase's; `assistants.rs`'s own tests exercise `ConfiguredAssistants::can_serve`
    /// directly in the meantime.
    #[cfg_attr(not(test), allow(dead_code))]
    async fn can_serve(&self, brain: crate::chats::Brain, model: &str) -> Result<(), Refusal>;

    /// What each of `models` declares, as far as this machine can tell — the question the model
    /// picker's menu marks itself with (`config::marked_with`), a different one from `can_serve`'s
    /// pass/fail: a model can be fully declared and still fail its role's requirement, and a model
    /// this daemon knows nothing about is not the same as one that declared nothing.
    ///
    /// A model absent from the returned map is one nothing here knows about — never one known to
    /// lack something; `config::marked_with`'s own doc carries that same distinction forward onto
    /// `AssistantChoice::tools`.
    ///
    /// BATCH rather than one call per model: the misses run CONCURRENTLY, through the same
    /// `DiscoveryCache` `can_serve` uses, so a menu that opens on N unseen models costs one round
    /// trip's time, not N, and a menu that opens warm costs nothing at all.
    #[cfg_attr(not(test), allow(dead_code))]
    async fn declared_for(
        &self,
        brain: crate::chats::Brain,
        models: &[String],
    ) -> std::collections::HashMap<String, crate::capabilities::Declared>;

    /// The chat client a local model is asked through — the engine half of `assistant_for`, handed
    /// out on its own for a caller that wants the ENGINE and not a whole `LocalAssistant`.
    ///
    /// `council::run_local_seat` is that caller: a seat gets its own toolbox
    /// (`mcp_tools::LocalToolBox::for_council`, which is what keeps `create_run` and `create_job`
    /// away from it) and must not be handed the chat route's box along with its client. Asking the
    /// factory for the client alone is what stops the two halves of one daemon drifting onto
    /// different engines — a seat that built its own client reached Ollama on an install whose
    /// chat reached an OpenAI-compatible server, and nothing about that is visible from outside.
    ///
    /// WITH a default body, for the argument `cli_runner`'s default below already makes: the
    /// doubles in this crate exist to answer `assistant_for` and would otherwise each have to
    /// write an arm about a route their own test says nothing about. Refusing
    /// `NotServedByThisFactory` is the honest answer for such a double — wrong door, not
    /// misconfiguration — and a factory that does serve the local route says so by overriding.
    fn local_chat(&self, _model: &str) -> Result<Box<dyn crate::local_agent::LocalChat>, Refusal> {
        Err(Refusal::NotServedByThisFactory)
    }

    /// Returns the configured command runner for an agent CLI.
    fn cli_runner(
        &self,
        _cli: &str,
        _model: Option<&str>,
    ) -> Option<std::sync::Arc<dyn crate::runner::CommandRunner>> {
        None
    }
}

/// Assembles the assistant that answers a turn from what startup already read: one shared
/// `reqwest::Client` per route (see the module doc — a constructor invariant, not a test
/// criterion), each route's configured model, the OpenRouter key if one is stored, and the
/// loopback address/token/pool `mcp_tools::LocalToolBox` needs to answer its own tools.
pub struct ConfiguredAssistants {
    local_client: reqwest::Client,
    hosted_client: reqwest::Client,
    local_model: Option<String>,
    hosted_model: Option<String>,
    hosted_key: Option<String>,
    /// The loopback address of this daemon's own MCP server (e.g. `http://127.0.0.1:8791`), which
    /// `mcp_tools::LocalToolBox` calls back into to answer its own tools — unrelated to the
    /// *model* endpoints `runner::OLLAMA_BASE_URL` and `openai::OPENROUTER_BASE_URL`, despite
    /// the shared "base url" name those two carry.
    loopback_url: String,
    token: String,
    pool: sqlx::SqlitePool,
    /// The address of whichever local server the resolved engine is configured against: Ollama's
    /// own `runner::OLLAMA_BASE_URL` on an install that named no engine, and the `local_base_url`
    /// its `.ai/nucleos-models.yaml` names on one that did. `main.rs` passes
    /// `config::ResolvedLocalEngine::base_url`, never the constant directly, and
    /// `config::ModelsConfig::local_engine` has already refused any address that is not this
    /// machine's — so what is held here is loopback by the time the constructor sees it, and
    /// `Brain::Local`'s promise that a local turn never leaves the machine is kept at config load
    /// rather than re-checked on every read.
    ///
    /// BOTH local readers use it, and that they use the same one is the property: `local_chat`
    /// GENERATES against it and `declared_one` DISCOVERS against it, so the menu cannot probe one
    /// server while the turn it approves goes to another. Which dialect either speaks is
    /// `local_engine` below and not this field — discovery is Ollama's `/api/show` or an
    /// OpenAI-compatible `GET /models` by engine, never `/api/show` unconditionally.
    ///
    /// A constructor parameter and not a constant, which is also what lets a test point discovery
    /// at a loopback stub server instead of a real daemon: that argument is why the field existed
    /// before it carried a configured address at all, and it still holds now that it carries one.
    local_base_url: String,
    /// WHICH local server `local_base_url` above names - Ollama's own dialect (`POST /api/chat`
    /// to generate, `POST /api/show` to discover) or an OpenAI-compatible one (`POST
    /// /chat/completions`, `GET /models`) as served by llama.cpp, LM Studio or vLLM. Read by
    /// `assistant_for`'s and `declared_one`'s `Brain::Local` arms, the only two places in this
    /// crate that speak to that address.
    ///
    /// Defaults to `Ollama` and is set through `with_local_engine` below rather than through an
    /// eighth constructor parameter, for exactly the argument `hosted_base_url`'s own field doc
    /// makes below (`assistants.rs:243-251` before these two fields existed): an eighth argument
    /// would force an edit to every existing seven-argument call site - twelve of them, `main.rs`
    /// included and this module's own frozen tests among them - to say nothing new.
    ///
    /// Defaulting is the compatibility rule itself rather than a convenience: every install
    /// running today has no `local_engine` line at all, `config::ModelsConfig::local_engine`
    /// resolves that absence to `Ollama`, and such an install must keep reaching Ollama without
    /// anybody editing a file.
    local_engine: crate::config::LocalEngine,
    /// The window `config::ModelsConfig::local_context_tokens` declared for that server, or `None`
    /// when nobody declared one - never a guess, never a zero. Its one reader is `declared_one`'s
    /// OpenAI arm, as `capabilities::discover_openai`'s `declared` argument: an OpenAI-compatible
    /// `GET /models` need not state a `context_length` for the model it serves, and this is the
    /// file's answer for when it does not.
    ///
    /// Set alongside the engine by `with_local_engine` because both arrive together from one
    /// `config::ResolvedLocalEngine`, and a declared window that did not travel with the engine
    /// serving it would be a number nothing reads.
    local_declared_context_tokens: Option<usize>,
    /// Where `can_serve`/`declared_for` discover a HOSTED model's capabilities
    /// (`capabilities::discover_openai`, against `{base}/models`) — NOT a constructor
    /// parameter like `local_base_url` above: adding one would grow `new`'s argument list, which
    /// would force an edit to every existing seven-argument call site (`main.rs`'s included, and
    /// the four `can_serve` tests this task's own RED phase already froze), none of which this fix
    /// may touch. Defaults to `openai::OPENROUTER_BASE_URL` — the same constant
    /// `assistant_for` already builds an `OpenAiChat` against — and `with_hosted_base_url`
    /// below is the test-only seam for pointing it at a stub instead.
    hosted_base_url: String,
    /// One probe per (route, model) pair, not per `can_serve`/`declared_for` call — the reason
    /// `capabilities::DiscoveryCache` exists at all (see its own doc). Owned by the factory rather
    /// than built per call: a `Mutex` created fresh on every call would cache nothing across the
    /// two calls `a_pergunta_de_capacidades_corre_uma_vez_por_modelo_e_nao_uma_vez_por_pergunta`
    /// makes. Entries are keyed `"{brain.as_str()}:{model}"`, not by model name alone: a local
    /// model and a hosted model can share a name (nothing stops it), and before this fix both
    /// routes asked the same Ollama anyway, so the two also happening to share a cache entry went
    /// unnoticed. Once the two routes ask different services, sharing a key would let one route's
    /// probe answer the other's question.
    discovery_cache: crate::capabilities::DiscoveryCache,
    /// A short timeout for `can_serve`/`declared_for`'s discovery calls, separate from
    /// `local_client`/`hosted_client` above: those are sized for GENERATION
    /// (`OLLAMA_EXCHANGE_TIMEOUT`/`OPENROUTER_EXCHANGE_TIMEOUT`, ~120s, wide enough for a cold
    /// model to load from disk mid-answer), while `can_serve`/`declared_for` sit on the
    /// menu-open and door paths — the same reason `http.rs`'s own `/api/tags` probe carries a
    /// 2-second `OLLAMA_TAGS_TIMEOUT` rather than reusing a generation client: a wedged Ollama (or
    /// a slow OpenRouter catalogue read) must cost the menu a brief pause, never a two-minute hang.
    introspection_client: reqwest::Client,
    claude_cli: Option<std::sync::Arc<dyn crate::runner::CommandRunner>>,
    codex_model: Option<String>,
}

impl ConfiguredAssistants {
    /// Builds the one client per route HERE, in the constructor, and never again per turn — see
    /// the module doc.
    pub fn new(
        local_model: Option<String>,
        hosted_model: Option<String>,
        hosted_key: Option<String>,
        loopback_url: String,
        token: String,
        pool: sqlx::SqlitePool,
        local_base_url: String,
    ) -> Self {
        Self {
            local_client: reqwest::Client::builder()
                .timeout(crate::runner::OLLAMA_EXCHANGE_TIMEOUT)
                .build()
                .expect("HTTP client for the local model (check TLS and proxy environment)"),
            hosted_client: reqwest::Client::builder()
                .timeout(crate::openai::OPENROUTER_EXCHANGE_TIMEOUT)
                .build()
                .expect("HTTP client for the hosted model (check TLS and proxy environment)"),
            local_model,
            hosted_model,
            hosted_key,
            loopback_url,
            token,
            pool,
            local_base_url,
            // Ollama, always, until `with_local_engine` says otherwise - see that field's own
            // doc: `new`'s seven parameters are what twelve call sites already pass, and an
            // install that has never named an engine must keep the one this daemon always used.
            local_engine: crate::config::LocalEngine::Ollama,
            local_declared_context_tokens: None,
            hosted_base_url: crate::openai::OPENROUTER_BASE_URL.to_string(),
            discovery_cache: crate::capabilities::DiscoveryCache::new(),
            introspection_client: reqwest::Client::builder()
                .timeout(CAPABILITY_PROBE_TIMEOUT)
                .build()
                .expect("HTTP client for capability discovery (check TLS and proxy environment)"),
            claude_cli: None,
            codex_model: None,
        }
    }

    /// Adds the agent CLI runners configured at daemon startup.
    pub fn with_agent_clis(
        mut self,
        claude: std::sync::Arc<dyn crate::runner::CommandRunner>,
        codex_model: String,
    ) -> Self {
        self.claude_cli = Some(claude);
        self.codex_model = Some(codex_model);
        self
    }

    /// Points the local route at the engine `config::ModelsConfig::local_engine()` resolved, with
    /// whatever context window the same file declared for it.
    ///
    /// A builder and not a constructor parameter, for the reason `local_engine`'s own field doc
    /// gives: `new` already takes seven arguments at twelve call sites, and none of those sites
    /// has anything new to say. The ADDRESS still travels as `new`'s seventh argument - the
    /// engine and its address arrive together in one `config::ResolvedLocalEngine`, so `main.rs`
    /// passes `resolved.base_url` there and `resolved.engine` here, out of the same value.
    ///
    /// Not `#[cfg(test)]` like `with_hosted_base_url` above: this is production wiring that tests
    /// also use, not a seam that exists only for them.
    pub fn with_local_engine(
        mut self,
        engine: crate::config::LocalEngine,
        declared_context_tokens: Option<usize>,
    ) -> Self {
        self.local_engine = engine;
        self.local_declared_context_tokens = declared_context_tokens;
        self
    }

    /// Test-only seam for `hosted_base_url` — the same reason `local_base_url` is already a
    /// constructor parameter (pointing discovery at a loopback stub instead of the real
    /// catalogue), but as a method rather than a constructor parameter so it costs no existing
    /// call site anything; see `hosted_base_url`'s own field doc for why.
    #[cfg(test)]
    pub fn with_hosted_base_url(mut self, hosted_base_url: String) -> Self {
        self.hosted_base_url = hosted_base_url;
        self
    }
}

/// `can_serve`/`declared_for`'s own timeout — see `introspection_client`'s field doc for why it is
/// not `runner::OLLAMA_EXCHANGE_TIMEOUT`/`openai::OPENROUTER_EXCHANGE_TIMEOUT`. Matches
/// `http.rs`'s own `OLLAMA_TAGS_TIMEOUT` value for the same reasoning, kept as its own constant
/// rather than imported from there: every timeout in this crate is declared beside the client that
/// uses it (`OLLAMA_EXCHANGE_TIMEOUT` beside the local generation client, `OPENROUTER_EXCHANGE_TIMEOUT`
/// beside the hosted one, `OLLAMA_TAGS_TIMEOUT` beside `http.rs`'s own tags probe), and reaching
/// into `http.rs` from this module for two seconds' worth of a `Duration` would be a stranger
/// dependency than the value it would save.
const CAPABILITY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[async_trait::async_trait]
impl Assistants for ConfiguredAssistants {
    fn cli_runner(
        &self,
        cli: &str,
        pinned: Option<&str>,
    ) -> Option<std::sync::Arc<dyn crate::runner::CommandRunner>> {
        match cli {
            "codex" => self.codex_model.as_ref().map(|configured| {
                std::sync::Arc::new(crate::runner::CodexCliRunner::for_chat(
                    pinned.unwrap_or(configured).to_string(),
                )) as std::sync::Arc<dyn crate::runner::CommandRunner>
            }),
            "claude" => self.claude_cli.clone(),
            _ => None,
        }
    }

    /// WHICH client a local model gets, for both of the callers that need one: `assistant_for`'s
    /// `Brain::Local` arm below, which wraps it in a `LocalAssistant` with the chat route's
    /// toolbox, and `council::run_local_seat`, which pairs it with the council's own box. The
    /// choice lives here rather than in either of them because the two of them agreeing is the
    /// property: a seat that built its own `runner::OllamaChat` against `runner::OLLAMA_BASE_URL`
    /// reached Ollama on an install whose chat reached the OpenAI-compatible server its owner
    /// configured, and a seat that quietly answers as a different model — or fails because no
    /// Ollama is running at all — says nothing about why.
    ///
    /// `serves` first, which is the ROUTE's switch and not the model's: `local_assistant_model` is
    /// what turns the local route on, and a caller naming a model of its own does not get to turn
    /// it on by naming one. Harmless to repeat when `assistant_for` has just asked the same
    /// question, for the reason its own comment gives below.
    fn local_chat(&self, model: &str) -> Result<Box<dyn crate::local_agent::LocalChat>, Refusal> {
        self.serves(crate::chats::Brain::Local)?;
        // `self.local_base_url` on BOTH engines, where this read the CONSTANT
        // `runner::OLLAMA_BASE_URL` directly until the engine field arrived. That constant is why
        // the field covered discovery (`declared_one`) only and never the path that generates: a
        // factory pointed at some other local server had its capability probes go there and its
        // turns go to Ollama anyway. Production behaviour is unchanged by the switch, because
        // `main.rs` passes the resolved address as the seventh argument and for an install that
        // named no engine that address IS this same constant.
        let chat: Box<dyn crate::local_agent::LocalChat> = match self.local_engine {
            crate::config::LocalEngine::Ollama => Box::new(crate::runner::OllamaChat::with_client(
                self.local_client.clone(),
                self.local_base_url.clone(),
                model.to_string(),
            )),
            crate::config::LocalEngine::OpenAi => Box::new(crate::openai::OpenAiChat::with_client(
                self.local_client.clone(),
                self.local_base_url.clone(),
                model.to_string(),
                // `None`, always, on this route - the other half of the choice that made
                // `with_client`'s key an `Option`, whose `Some(key)` case is the hosted arm of
                // `assistant_for`. `config::ModelsConfig::local_engine` has already refused any
                // address that is not this machine's, so what is dialled here is a loopback
                // server, and a loopback server is sent no `Authorization` header at all rather
                // than an empty or invented one.
                None,
            )),
        };
        Ok(chat)
    }

    fn assistant_for(
        &self,
        brain: crate::chats::Brain,
        model: Option<&str>,
    ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, Refusal> {
        // `serves` carries the availability rule now — calling it first means a refusal here and a
        // refusal from `serves` are the same check, not two copies of it. Harmless to repeat below:
        // `resolve_model`'s and the key's own checks only ever fail for the same reason `serves`
        // just ruled out, so they cannot fail here having passed there.
        self.serves(brain)?;
        match brain {
            crate::chats::Brain::Cloud => Err(Refusal::NotServedByThisFactory),
            crate::chats::Brain::Local => {
                let resolved = resolve_model(model, self.local_model.as_deref())?;
                // THROUGH `local_chat`, which now holds the engine choice this arm used to make
                // inline. ONE place decides which client a local model gets, so a chat turn here
                // and a council seat (`council::run_local_seat`, the other caller) cannot drift
                // onto different engines. The pin is resolved first and the resolved name is what
                // travels: choosing between a conversation's pin and the route's default is this
                // method's question, not `local_chat`'s, which is handed a model already chosen.
                let chat = self.local_chat(&resolved)?;
                let toolbox = toolbox_for(
                    brain,
                    self.loopback_url.clone(),
                    self.token.clone(),
                    self.pool.clone(),
                );
                Ok(std::sync::Arc::new(
                    // Already boxed by `local_chat`, where the engine chose which of the two
                    // `LocalChat` implementations this is - one `Box::new` per arm instead of one
                    // here.
                    crate::local_agent::LocalAssistant::new(chat, toolbox),
                ))
            }
            crate::chats::Brain::OpenRouter => {
                // The model is resolved BEFORE the key is read — the order two tests pin. Reading
                // the key first would answer "you configured nothing" with the same refusal as
                // "you configured half of it", which is the wrong sentence for an operator who has
                // named a hosted model but not yet stored a key.
                let resolved = resolve_model(model, self.hosted_model.as_deref())?;
                let key = self
                    .hosted_key
                    .clone()
                    .ok_or(Refusal::HostedModelNamedButNoKey)?;
                let chat = crate::openai::OpenAiChat::with_client(
                    self.hosted_client.clone(),
                    // The FIELD, not `openai::OPENROUTER_BASE_URL` directly. Behaviour-neutral
                    // in production — the field is initialised to that same constant and only
                    // `with_hosted_base_url`, which is `#[cfg(test)]`, ever changes it — but it is
                    // what lets a test point this route at a stub and read which model actually
                    // went on the wire. Without it the seam covers discovery
                    // (`can_serve`/`declared_for`) and stops short of the one path that bills.
                    self.hosted_base_url.clone(),
                    resolved,
                    // `Some`, always, on this route: `with_client`'s key became an `Option` so a
                    // keyless local server can be asked with no `Authorization` header at all, and
                    // the hosted route is the other half of that choice — the refusal for a missing
                    // key is `Refusal::HostedModelNamedButNoKey` above, already read, so what
                    // reaches here is a key and is sent as one.
                    Some(key),
                );
                let toolbox = toolbox_for(
                    brain,
                    self.loopback_url.clone(),
                    self.token.clone(),
                    self.pool.clone(),
                );
                Ok(std::sync::Arc::new(
                    crate::local_agent::LocalAssistant::new(Box::new(chat), toolbox),
                ))
            }
        }
    }

    /// The availability rule itself: the same configuration `assistant_for` consults, checked in
    /// the same order — the route's configured model first, the key second for the hosted route —
    /// so the two tests pinning that order cover this too. `assistant_for` calls this first, which
    /// is what makes it safe to add without a test of its own.
    fn serves(&self, brain: crate::chats::Brain) -> Result<(), Refusal> {
        match brain {
            crate::chats::Brain::Cloud => Err(Refusal::NotServedByThisFactory),
            crate::chats::Brain::Local => {
                resolve_model(None, self.local_model.as_deref())?;
                Ok(())
            }
            crate::chats::Brain::OpenRouter => {
                resolve_model(None, self.hosted_model.as_deref())?;
                self.hosted_key
                    .as_ref()
                    .ok_or(Refusal::HostedModelNamedButNoKey)?;
                Ok(())
            }
        }
    }

    /// `Brain::Cloud` refuses `NotServedByThisFactory` without touching the network — the agent
    /// CLI is a binary, not a served model, per this trait's own doc. `Brain::OpenRouter` with no
    /// stored key refuses `HostedModelNamedButNoKey` — "we could not ask" — before touching the
    /// network either, the same order `assistant_for` already checks in (model resolved, then the
    /// key). Every other case discovers `model` through `declared_one` below and refuses
    /// `Refusal::CannotServe(capabilities::picker_refusal(&missing))` on any gap against the
    /// role's own requirement.
    async fn can_serve(&self, brain: crate::chats::Brain, model: &str) -> Result<(), Refusal> {
        // The agent CLI is a binary, not a served model this factory could probe — wrong door, not
        // misconfiguration (`Refusal::NotServedByThisFactory`'s own doc). Checked first and
        // returned before anything below touches the network, which is what
        // `a_rota_da_nuvem_nao_e_sondada_para_saber_se_serve` pins.
        if brain == crate::chats::Brain::Cloud {
            return Err(Refusal::NotServedByThisFactory);
        }
        // A hosted model named with no key stored is "we could not ask", never a guess — the same
        // fact `assistant_for`'s own `Brain::OpenRouter` arm already answers with this refusal for
        // a turn. Checked before `declared_one` so a keyless hosted pick never reaches the network
        // at all — the controller's own send-back on this packet found that, before this check
        // existed, EVERY hosted `can_serve` call probed the LOCAL Ollama instead, which is a
        // second bug this same guard also forecloses: there is no discovery attempt left to
        // misroute.
        if brain == crate::chats::Brain::OpenRouter && self.hosted_key.is_none() {
            return Err(Refusal::HostedModelNamedButNoKey);
        }

        // Both remaining routes answer through the same `LocalAssistant` `assistant_for` builds
        // for them (the name is about running inside this daemon's own process, not about which
        // network route serves it), so they share its one declared requirement — there is no
        // route-specific REQUIREMENT to choose between. What differs between them is where to ask,
        // which `declared_one` below is the one place that decides.
        let requirement = crate::local_agent::CAPABILITY_REQUIREMENT;
        let declared = self.declared_one(brain, model).await;

        let missing = crate::capabilities::missing_capabilities(&requirement, &declared);
        match crate::capabilities::picker_refusal(&missing) {
            Some(reason) => Err(Refusal::CannotServe(reason)),
            None => Ok(()),
        }
    }

    async fn declared_for(
        &self,
        brain: crate::chats::Brain,
        models: &[String],
    ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
        // The same "we could not ask" rule `can_serve` applies — see its own comment — extended
        // to a batch: every model in this group shares one `brain`, so a missing key rules out the
        // whole group at once rather than one model at a time. `marked_with` renders an id absent
        // from this map as `None`, never `Some(false)`, which is the honest answer for "nobody
        // asked" as opposed to "asked and declared nothing".
        if brain == crate::chats::Brain::OpenRouter && self.hosted_key.is_none() {
            return std::collections::HashMap::new();
        }

        let probes = models.iter().map(|model| {
            let key = model.clone();
            async move {
                let declared = self.declared_one(brain, &key).await;
                (key, declared)
            }
        });

        crate::join::all(probes).await.into_iter().collect()
    }
}

impl ConfiguredAssistants {
    /// One model's declaration for `brain`, routed to the discovery method that route actually
    /// understands — the fix for the controller's send-back on this packet: `can_serve` and
    /// `declared_for` used to send EVERY non-Cloud route through `discover_ollama_as` against the
    /// LOCAL Ollama, so asking about a hosted model asked the wrong service about a name it had
    /// never heard, failed closed, and refused (or unmarked) a model that may have served fine.
    /// `Brain::Local` -> `capabilities::discover_ollama_as` or `capabilities::discover_openai`
    /// against `self.local_base_url`, by `self.local_engine` - the same fact `assistant_for`'s own
    /// `Brain::Local` arm decides for a turn, so the menu cannot probe one server while the turn
    /// it approves goes to another;
    /// `Brain::OpenRouter` -> `capabilities::discover_openai` against `self.hosted_base_url`;
    /// `Brain::Cloud` -> the CLI's own constant declaration, no network call, per
    /// `capabilities::declared_for_cli`'s own doc. Both network routes go over
    /// `self.introspection_client` — see its own field doc for why that is not
    /// `local_client`/`hosted_client` — and are cached together by `format!("{}:{model}",
    /// brain.as_str())`, never by `model` alone; see `discovery_cache`'s own field doc for why the
    /// route has to be part of the key.
    ///
    /// Callers decide whether `Brain::OpenRouter` may be asked at all: this method does not check
    /// `self.hosted_key`, so a caller that skips that check would genuinely probe the public
    /// OpenRouter catalogue (no key is needed to read it) — `can_serve` and `declared_for` both
    /// choose not to, on purpose, per their own comments.
    async fn declared_one(
        &self,
        brain: crate::chats::Brain,
        model: &str,
    ) -> crate::capabilities::Declared {
        let required_tokens = crate::local_agent::CAPABILITY_REQUIREMENT.context_tokens;
        let key = format!("{}:{model}", brain.as_str());

        match brain {
            crate::chats::Brain::Cloud => crate::capabilities::declared_for_cli(),
            crate::chats::Brain::Local => {
                let client = self.introspection_client.clone();
                let base_url = self.local_base_url.clone();
                let owned_model = model.to_string();
                // ONE cache key (`key`, built above from the brain and the model) and ONE client
                // across both engines: which dialect that address answers is a property of the
                // server, not a second route, so this stays one question per (route, model) pair
                // exactly as `discovery_cache`'s own field doc requires. The engine cannot change
                // under a warm entry either - it is read from the file once, at startup.
                match self.local_engine {
                    crate::config::LocalEngine::Ollama => {
                        self.discovery_cache
                            .get_or_discover(&key, move || async move {
                                crate::capabilities::discover_ollama_as(
                                    &client,
                                    &base_url,
                                    &owned_model,
                                    required_tokens,
                                    "model picker",
                                )
                                .await
                            })
                            .await
                    }
                    crate::config::LocalEngine::OpenAi => {
                        // The declared window from the config file, and the one place it is read.
                        // Unlike the hosted arm below - where OpenRouter's catalogue always states
                        // its own `context_length`, so a fallback would never be reached - a
                        // llama.cpp or LM Studio `GET /models` often states no window at all, and
                        // `local_context_tokens` is how the operator answers for it. `None` when
                        // they did not, which `discover_openai` reports as undeclared rather than
                        // filling in with a guess.
                        let declared = self.local_declared_context_tokens;
                        self.discovery_cache
                            .get_or_discover(&key, move || async move {
                                crate::capabilities::discover_openai(
                                    &client,
                                    &base_url,
                                    &owned_model,
                                    required_tokens,
                                    declared,
                                )
                                .await
                            })
                            .await
                    }
                }
            }
            crate::chats::Brain::OpenRouter => {
                let client = self.introspection_client.clone();
                let base_url = self.hosted_base_url.clone();
                let owned_model = model.to_string();
                self.discovery_cache
                    .get_or_discover(&key, move || async move {
                        crate::capabilities::discover_openai(
                            &client,
                            &base_url,
                            &owned_model,
                            required_tokens,
                            // `None`: the hosted route declares nothing locally. OpenRouter's
                            // catalogue always states its own `context_length`, so the declared
                            // fallback would never be reached — and passing a number that is never
                            // read is how a later reader concludes one of them is in force.
                            None,
                        )
                        .await
                    })
                    .await
            }
        }
    }
}

/// Hands back one fixed `LocalAssistant` for every request, whatever the route or model asked for.
///
/// What the migrated `assistant.rs` sites need: each sets one fake assistant and asserts on what it
/// answered, and none of them cares which route or model the turn asked for — that is
/// `RecordingAssistants`'s job below, not this one's.
#[cfg(test)]
pub struct FixedAssistants(pub std::sync::Arc<crate::local_agent::LocalAssistant>);

#[cfg(test)]
#[async_trait::async_trait]
impl Assistants for FixedAssistants {
    fn assistant_for(
        &self,
        _brain: crate::chats::Brain,
        _model: Option<&str>,
    ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, Refusal> {
        Ok(self.0.clone())
    }

    fn serves(&self, _brain: crate::chats::Brain) -> Result<(), Refusal> {
        Ok(())
    }

    // As trivial as `serves` above: this double answers for every route, so it can serve every
    // model too.
    async fn can_serve(&self, _brain: crate::chats::Brain, _model: &str) -> Result<(), Refusal> {
        Ok(())
    }

    // Trivial for the same reason `can_serve` above is: nothing that reaches for this double asks
    // what a model declares, so an empty map — nothing known about anybody — is honest and costs
    // no test its own double.
    async fn declared_for(
        &self,
        _brain: crate::chats::Brain,
        _models: &[String],
    ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
        std::collections::HashMap::new()
    }
}

/// Records every `(Brain, Option<String>)` it is asked for, in call order, then hands back one
/// fixed assistant — the reason `Assistants` is a trait and not a bare struct: a test needs to
/// observe WHICH model a turn asked the factory for, and a concrete `ConfiguredAssistants` with no
/// test seam cannot show that.
///
/// Interior mutability because `assistant_for` takes `&self`: the same `Mutex` idiom
/// `capabilities::DiscoveryCache` uses, and for the same reason — the trait method has no `&mut
/// self` to record into.
#[cfg(test)]
pub struct RecordingAssistants {
    assistant: std::sync::Arc<crate::local_agent::LocalAssistant>,
    pub calls: std::sync::Mutex<Vec<(crate::chats::Brain, Option<String>)>>,
}

#[cfg(test)]
impl RecordingAssistants {
    pub fn new(assistant: std::sync::Arc<crate::local_agent::LocalAssistant>) -> Self {
        Self {
            assistant,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Assistants for RecordingAssistants {
    fn assistant_for(
        &self,
        brain: crate::chats::Brain,
        model: Option<&str>,
    ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, Refusal> {
        self.calls
            .lock()
            .expect("recording assistants mutex poisoned")
            .push((brain, model.map(|m| m.to_string())));
        Ok(self.assistant.clone())
    }

    fn serves(&self, _brain: crate::chats::Brain) -> Result<(), Refusal> {
        Ok(())
    }

    // As trivial as `serves` above: this double answers for every route, so it can serve every
    // model too. `calls` above already exists to record WHICH model a turn asked `assistant_for`
    // for; recording `can_serve` too is not this double's job.
    async fn can_serve(&self, _brain: crate::chats::Brain, _model: &str) -> Result<(), Refusal> {
        Ok(())
    }

    // Trivial for the same reason `can_serve` above is: `calls` already exists to record what
    // matters to this double, and no test reaches for `declared_for` through it.
    async fn declared_for(
        &self,
        _brain: crate::chats::Brain,
        _models: &[String],
    ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
        std::collections::HashMap::new()
    }
}

/// Refuses everything with `Refusal::RouteNotConfigured` — the ship-dark default the `AppState`
/// literals that do not exercise an assistant need: an untouched install serves no route.
#[cfg(test)]
pub struct NoAssistants;

#[cfg(test)]
#[async_trait::async_trait]
impl Assistants for NoAssistants {
    fn assistant_for(
        &self,
        _brain: crate::chats::Brain,
        _model: Option<&str>,
    ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, Refusal> {
        Err(Refusal::RouteNotConfigured)
    }

    fn serves(&self, _brain: crate::chats::Brain) -> Result<(), Refusal> {
        Err(Refusal::RouteNotConfigured)
    }

    // As trivial as `serves` above: an untouched install serves no route, so it can serve no model
    // either.
    async fn can_serve(&self, _brain: crate::chats::Brain, _model: &str) -> Result<(), Refusal> {
        Err(Refusal::RouteNotConfigured)
    }

    // Trivial for the same reason `can_serve` above is: an untouched install knows nothing about
    // any model, so an empty map is the honest answer.
    async fn declared_for(
        &self,
        _brain: crate::chats::Brain,
        _models: &[String],
    ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
        std::collections::HashMap::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::Brain;

    /// An in-memory database with this crate's schema on it — copied from `assistant.rs`'s own
    /// `test_pool`, which every `mcp_tools::LocalToolBox` constructor used below also needs.
    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn configured_assistants_hand_each_cli_its_own_runner() {
        let claude_fake = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
        let expected: std::sync::Arc<dyn crate::runner::CommandRunner> = claude_fake.clone();
        let configured = ConfiguredAssistants::new(
            None,
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        )
        .with_agent_clis(claude_fake, "gpt-5.6-terra".to_string());

        let claude = configured.cli_runner("claude", None);
        assert!(claude.is_some());
        assert!(std::sync::Arc::ptr_eq(claude.as_ref().unwrap(), &expected));
        assert!(configured.cli_runner("codex", Some("gpt-5.5")).is_some());
        assert!(configured.cli_runner("codex", None).is_some());
        assert!(configured.cli_runner("mystery", None).is_none());

        let unconfigured = ConfiguredAssistants::new(
            None,
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        );
        assert!(unconfigured.cli_runner("claude", None).is_none());
        assert!(unconfigured.cli_runner("codex", None).is_none());
    }

    // --- A. Model resolution — PURE, no pool, no client, no runtime -----------------------------

    #[test]
    fn um_modelo_fixado_na_conversa_ganha_ao_configurado() {
        let resolved = resolve_model(Some("qwen3:8b"), Some("qwen3.5:4b"));
        assert_eq!(resolved, Ok("qwen3:8b".to_string()));
    }

    #[test]
    fn sem_modelo_fixado_a_rota_usa_o_seu_configurado() {
        let resolved = resolve_model(None, Some("qwen3.5:4b"));
        assert_eq!(resolved, Ok("qwen3.5:4b".to_string()));
    }

    #[test]
    fn uma_rota_sem_modelo_configurado_recusa_como_rota_desligada() {
        let resolved = resolve_model(None, None);
        assert_eq!(resolved, Err(Refusal::RouteNotConfigured));
    }

    #[test]
    fn um_modelo_fixado_numa_rota_desligada_nao_a_liga() {
        let resolved = resolve_model(Some("qwen3:8b"), None);
        assert_eq!(
            resolved,
            Err(Refusal::RouteNotConfigured),
            "a pinned model must not switch on a route the operator never configured — the config \
             key is the switch, the pin only chooses which model runs once it is on"
        );
    }

    #[test]
    fn a_recusa_por_rota_desligada_e_a_recusa_por_falta_de_chave_sao_distintas() {
        let route_off = Refusal::RouteNotConfigured;
        let no_key = Refusal::HostedModelNamedButNoKey;

        assert_ne!(route_off, no_key);
        assert_ne!(
            route_off.message(Brain::OpenRouter),
            no_key.message(Brain::OpenRouter),
            "a route switched off and a route half-configured with no key are different problems \
             and must not read as the same sentence"
        );
    }

    // --- B. The refusal wording — the `http.rs` contract -----------------------------------------

    #[test]
    fn a_rota_local_desligada_recusa_com_a_frase_que_o_http_ja_compara() {
        assert_eq!(
            Refusal::RouteNotConfigured.message(Brain::Local),
            crate::assistant::NO_LOCAL_MODEL
        );
    }

    #[test]
    fn a_rota_alojada_desligada_recusa_com_a_frase_que_o_http_ja_compara() {
        assert_eq!(
            Refusal::RouteNotConfigured.message(Brain::OpenRouter),
            crate::assistant::NO_HOSTED_MODEL
        );
    }

    // --- C. The toolbox is chosen by the route -----------------------------------------------------

    #[tokio::test]
    async fn a_rota_alojada_leva_exactamente_as_seis_ferramentas_da_lista_branca() {
        let toolbox = toolbox_for(
            Brain::OpenRouter,
            "http://127.0.0.1:1".to_string(),
            "unused".to_string(),
            test_pool().await,
        );

        let mut offered: Vec<String> = toolbox
            .schemas()
            .into_iter()
            .map(|schema| {
                schema["function"]["name"]
                    .as_str()
                    .expect("every schema this router produces names its own tool")
                    .to_string()
            })
            .collect();
        offered.sort();

        let mut expected: Vec<String> = crate::mcp_tools::HOSTED_TOOLS
            .iter()
            .map(|name| name.to_string())
            .collect();
        expected.sort();

        assert_eq!(offered, expected);
    }

    #[tokio::test]
    async fn a_rota_local_leva_a_caixa_irrestrita() {
        let toolbox = toolbox_for(
            Brain::Local,
            "http://127.0.0.1:1".to_string(),
            "unused".to_string(),
            test_pool().await,
        );

        let mut offered: Vec<String> = toolbox
            .schemas()
            .into_iter()
            .map(|schema| {
                schema["function"]["name"]
                    .as_str()
                    .expect("every schema this router produces names its own tool")
                    .to_string()
            })
            .collect();
        offered.sort();

        let mut expected: Vec<String> = crate::mcp_tools::LOCAL_TOOLS
            .iter()
            .map(|name| name.to_string())
            .collect();
        expected.sort();

        assert_eq!(offered, expected);

        let hosted: std::collections::HashSet<&str> =
            crate::mcp_tools::HOSTED_TOOLS.iter().copied().collect();
        let local: std::collections::HashSet<&str> =
            crate::mcp_tools::LOCAL_TOOLS.iter().copied().collect();
        assert!(
            local.is_superset(&hosted) && local != hosted,
            "the local box must carry at least one tool the hosted allowlist does not — otherwise \
             the two boxes have been quietly tidied into one"
        );
    }

    // --- D. The factory -----------------------------------------------------------------------

    #[tokio::test]
    async fn a_fabrica_constroi_um_assistente_para_uma_rota_configurada() {
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        );

        assert!(factory.assistant_for(Brain::Local, None).is_ok());
    }

    #[tokio::test]
    async fn a_rota_da_nuvem_nao_e_servida_por_esta_fabrica() {
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            Some("gpt-oss".to_string()),
            Some("key".to_string()),
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        );

        assert!(
            matches!(
                factory.assistant_for(Brain::Cloud, None),
                Err(Refusal::NotServedByThisFactory)
            ),
            "Cloud answers through the agent CLI, a different mechanism this factory never touches \
             — refusing it must read as wrong door, not as a misconfigured route"
        );
    }

    #[tokio::test]
    async fn uma_rota_alojada_com_modelo_e_sem_chave_recusa_por_falta_de_chave() {
        let factory = ConfiguredAssistants::new(
            None,
            Some("gpt-oss".to_string()),
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        );

        let result = factory.assistant_for(Brain::OpenRouter, None);
        assert!(
            matches!(result, Err(Refusal::HostedModelNamedButNoKey)),
            "a hosted model named with no key stored is an operator half-way through setup, not a \
             route that was never turned on — RouteNotConfigured would say the opposite of what is \
             actually missing"
        );
    }

    #[tokio::test]
    async fn sem_configuracao_nenhuma_a_fabrica_nao_serve_rota_nenhuma() {
        let factory = ConfiguredAssistants::new(
            None,
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        );

        assert!(
            matches!(
                factory.assistant_for(Brain::Local, None),
                Err(Refusal::RouteNotConfigured)
            ),
            "an untouched install must refuse the local route as switched off, not for any other \
             reason"
        );
        assert!(
            matches!(
                factory.assistant_for(Brain::OpenRouter, None),
                Err(Refusal::RouteNotConfigured)
            ),
            "an untouched install must refuse the hosted route as switched off, not for any other \
             reason"
        );
    }

    // --- E. `can_serve` — the picker's own question, asked before a choice is stored -------------

    /// A loopback Ollama `/api/show`, answering the same canned body every time — the idiom
    /// `capabilities.rs`'s own stubs already use for this exact route. Delegates to
    /// `stub_show_counting` below rather than keeping a second listener-setup, and drops the hit
    /// counter the tests here that do not need one.
    async fn stub_show(body: serde_json::Value) -> String {
        let (base_url, _hits) = stub_show_counting(body).await;
        base_url
    }

    /// The same route as `stub_show`, with a hit counter the caller keeps — what
    /// `a_rota_da_nuvem_nao_e_sondada_para_saber_se_serve` and
    /// `a_pergunta_de_capacidades_corre_uma_vez_por_modelo_e_nao_uma_vez_por_pergunta` need to prove
    /// a call count, not just a value.
    async fn stub_show_counting(
        body: serde_json::Value,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        let app = axum::Router::new().route(
            "/api/show",
            axum::routing::post(move || {
                let body = body.clone();
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), hits)
    }

    /// A window far below `local_agent::CAPABILITY_REQUIREMENT.context_tokens` (16384).
    fn small_window_body() -> serde_json::Value {
        serde_json::json!({
            "model_info": {
                "general.architecture": "qwen2",
                "qwen2.context_length": 2048
            },
            "capabilities": ["completion"]
        })
    }

    /// A window comfortably above `local_agent::CAPABILITY_REQUIREMENT.context_tokens` (16384).
    fn large_window_body() -> serde_json::Value {
        serde_json::json!({
            "model_info": {
                "general.architecture": "qwen2",
                "qwen2.context_length": 32768
            },
            "capabilities": ["completion"]
        })
    }

    #[tokio::test]
    async fn a_fabrica_recusa_um_modelo_cuja_janela_nao_chega() {
        let base_url = stub_show(small_window_body()).await;
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            base_url,
        );

        let result = factory.can_serve(Brain::Local, "qwen3:8b").await;

        match result {
            Err(Refusal::CannotServe(reason)) => {
                assert!(
                    reason.contains("2048"),
                    "must name what the model has: {reason}"
                );
                assert!(
                    reason.contains(
                        &crate::local_agent::CAPABILITY_REQUIREMENT
                            .context_tokens
                            .to_string()
                    ),
                    "must name what the role needs: {reason}"
                );
            }
            other => panic!(
                "a window far below the local turn's requirement must refuse with the picker's \
                 own sentence, got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn a_fabrica_aceita_um_modelo_que_a_maquina_serve() {
        let base_url = stub_show(large_window_body()).await;
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            base_url,
        );

        let result = factory.can_serve(Brain::Local, "qwen3:8b").await;

        assert!(
            result.is_ok(),
            "a window well above the local turn's requirement must serve: {result:?}"
        );
    }

    #[tokio::test]
    async fn a_rota_da_nuvem_nao_e_sondada_para_saber_se_serve() {
        let (base_url, hits) = stub_show_counting(large_window_body()).await;
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            Some("gpt-oss".to_string()),
            Some("key".to_string()),
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            base_url,
        );

        let result = factory.can_serve(Brain::Cloud, "whatever-cli-model").await;

        assert!(
            matches!(result, Err(Refusal::NotServedByThisFactory)),
            "the agent CLI is not a served model and must refuse wrong-door, not probe: {result:?}"
        );
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Brain::Cloud must never touch the network to answer can_serve"
        );
    }

    #[tokio::test]
    async fn a_pergunta_de_capacidades_corre_uma_vez_por_modelo_e_nao_uma_vez_por_pergunta() {
        let (base_url, hits) = stub_show_counting(large_window_body()).await;
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            base_url,
        );

        let _ = factory.can_serve(Brain::Local, "qwen3:8b").await;
        let _ = factory.can_serve(Brain::Local, "qwen3:8b").await;

        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "two asks for the same model must probe the network once, not twice — \
             DiscoveryCache's own job"
        );
    }

    /// A loopback OpenRouter catalogue, answering the same canned body every time over `GET
    /// /models` — the route `capabilities::discover_openai` reads, the same idiom
    /// `stub_show_counting` above uses for Ollama's own `POST /api/show`.
    async fn stub_models_counting(
        body: serde_json::Value,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        let app = axum::Router::new().route(
            "/models",
            axum::routing::get(move || {
                let body = body.clone();
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), hits)
    }

    /// Regression for the controller's send-back on this packet: `can_serve` used to route EVERY
    /// non-Cloud brain through `discover_ollama_as` against the LOCAL Ollama, so asking about a
    /// hosted model asked the wrong service about a name it had never heard, failed closed, and
    /// refused a model that may have served fine. This is the negative that would have caught it:
    /// a local `/api/show` stub with its own hit counter, asked about a HOSTED brain instead —
    /// the counter must stay at zero. The positive half (a hosted probe lands on the hosted
    /// stub) is asserted alongside it via `with_hosted_base_url`, the test-only seam this fix
    /// added, so this test proves the ROUTE was followed correctly rather than merely that the
    /// local one was skipped for some unrelated reason (a missing key, say — a hosted key IS
    /// stored here, so that short-circuit is not what is under test).
    #[tokio::test]
    async fn um_modelo_alojado_nao_e_sondado_contra_o_ollama_local() {
        let (local_base_url, local_hits) = stub_show_counting(large_window_body()).await;
        let (hosted_base_url, hosted_hits) =
            stub_models_counting(serde_json::json!({ "data": [] })).await;
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            Some("anthropic/claude-sonnet-4.5".to_string()),
            Some("key".to_string()),
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            local_base_url,
        )
        .with_hosted_base_url(hosted_base_url);

        let _ = factory
            .can_serve(Brain::OpenRouter, "anthropic/claude-sonnet-4.5")
            .await;

        assert_eq!(
            local_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a hosted pick must never probe the local Ollama — can_serve(OpenRouter, _) asked \
             the wrong service before this fix, and this counter is what would have caught it"
        );
        assert_eq!(
            hosted_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a hosted pick must reach the hosted catalogue exactly once"
        );
    }

    // --- F. which model actually goes on the wire ------------------------------------------------

    /// A loopback `/chat/completions` that KEEPS the request bodies it was sent, answering the one
    /// shape `openai::assistant_message` reads. The hosted sibling of `stub_show_counting`
    /// above: that one counts calls because the fact under test is how many, this one records them
    /// because the fact under test is what was asked.
    async fn stub_completions_recording() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let recorded = recorded.clone();
                async move {
                    recorded
                        .lock()
                        .expect("the stub's recorder is never held across an await")
                        .push(body);
                    axum::Json(serde_json::json!({
                        "choices": [{ "message": { "content": "ok" } }]
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), seen)
    }

    /// The safety proof for the menu carrying hosted rows the config file wrote itself.
    ///
    /// `config.rs` used to GENERATE the single hosted entry, and its comment gave the reason: a
    /// hand-written entry could "name a model the daemon never built a client for, and then a
    /// person picks one model and a different one answers, silently". This is that failure, asked
    /// at the seam where it would happen — `resolve_model` returns `pinned.unwrap_or(configured)`
    /// and `assistant_for` builds the `OpenAiChat` out of that, so the model on the wire is the
    /// one that was picked and not the configured default.
    ///
    /// End-to-end through `verdict` — one exchange, no tools — rather than an assertion about
    /// `resolve_model` alone, which `um_modelo_fixado_na_conversa_ganha_ao_configurado` already
    /// covers: the fact worth pinning here is what reaches the endpoint, and only a request can
    /// say that.
    ///
    /// A REGRESSION GUARD, not a specification of new behaviour: this already held before
    /// `catalogue_with_installed` learned to keep the file's hosted rows, and it is what makes
    /// keeping them safe. It was never observed failing on purpose — doing so would have sent a
    /// request to OpenRouter's real endpoint, which no test here may do.
    #[tokio::test]
    async fn a_pinned_hosted_model_beats_the_configured_one_on_the_hosted_route() {
        let (hosted_base_url, seen) = stub_completions_recording().await;
        let factory = ConfiguredAssistants::new(
            None,
            Some("anthropic/claude-sonnet-4.5".to_string()),
            Some("key".to_string()),
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        )
        .with_hosted_base_url(hosted_base_url);

        let assistant = factory
            .assistant_for(Brain::OpenRouter, Some("openai/gpt-5.6"))
            .expect("a hosted route with a model and a key configured serves");
        assistant
            .verdict("hello")
            .await
            .expect("the stub answers the shape the reader consumes");

        let bodies = seen
            .lock()
            .expect("the stub's recorder is never held across an await");
        assert_eq!(bodies.len(), 1, "expected exactly one exchange: {bodies:?}");
        assert_eq!(
            bodies[0].pointer("/model").and_then(|value| value.as_str()),
            Some("openai/gpt-5.6"),
            "the PINNED model must be the one on the wire; the configured default answering here \
             is the silent swap that kept hosted rows out of the config file"
        );
    }

    // --- G. Which local engine answers the local route -------------------------------------------

    /// A loopback stand-in for whatever local server the operator actually runs, answering BOTH
    /// dialects on ONE address: Ollama's `POST /api/chat`, with a hit counter, and the
    /// OpenAI-compatible `POST /chat/completions`, recording the bodies it was sent.
    ///
    /// Two separate listeners would have been the obvious shape and would have left the counter
    /// inert: `local_base_url` is a SINGLE value, so a counter sitting on a second address
    /// nothing can name could never be incremented, whatever the route did with it. Co-hosting
    /// both routes is what lets the zero mean something — a local turn that dialled Ollama as
    /// well as the OpenAI server lands here too, and is counted.
    async fn stub_both_local_dialects() -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        let ollama_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = ollama_hits.clone();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/chat",
                axum::routing::post(move || {
                    let counted = counted.clone();
                    async move {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(serde_json::json!({
                            "message": { "role": "assistant", "content": "ok" }
                        }))
                    }
                }),
            )
            .route(
                "/chat/completions",
                axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let recorded = recorded.clone();
                    async move {
                        recorded
                            .lock()
                            .expect("the stub's recorder is never held across an await")
                            .push(body);
                        axum::Json(serde_json::json!({
                            "choices": [{ "message": { "content": "ok" } }]
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), ollama_hits, seen)
    }

    /// The discovery half of `stub_both_local_dialects` above: one address answering BOTH
    /// capability routes — Ollama's `POST /api/show` and the OpenAI-compatible `GET /models`,
    /// which is what `capabilities::discover_openai` actually requests — each with its own
    /// counter, and co-hosted for the same reason that stub co-hosts its two.
    async fn stub_both_local_catalogues(
        catalogue: serde_json::Value,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let show_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted_show = show_hits.clone();
        let models_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted_models = models_hits.clone();
        let app = axum::Router::new()
            .route(
                "/api/show",
                axum::routing::post(move || {
                    let counted_show = counted_show.clone();
                    async move {
                        counted_show.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(large_window_body())
                    }
                }),
            )
            .route(
                "/models",
                axum::routing::get(move || {
                    let catalogue = catalogue.clone();
                    let counted_models = counted_models.clone();
                    async move {
                        counted_models.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(catalogue)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), show_hits, models_hits)
    }

    /// Without this, `local_engine: openai` is a line in a config file that changes nothing at the
    /// only moment it matters: `assistant_for`'s `Brain::Local` arm reads the CONSTANT
    /// `runner::OLLAMA_BASE_URL` and builds an `OllamaChat` whatever the operator wrote, so the
    /// turn is posted to an Ollama that may not even be installed while the llama.cpp or LM Studio
    /// server the menu offered sits idle — and the operator is told the local route is broken.
    ///
    /// The zero on the Ollama counter is the half that carries the proof. A route that dialled
    /// BOTH servers would answer the question correctly and still be wrong: the second request is
    /// a whole turn's worth of a model nobody chose, sent to a service this install may have
    /// deliberately stopped running.
    #[tokio::test]
    async fn a_local_turn_reaches_the_configured_openai_server_and_never_the_ollama_wire() {
        let (base_url, ollama_hits, seen) = stub_both_local_dialects().await;
        let factory = ConfiguredAssistants::new(
            Some("qwen3-coder-30b".to_string()),
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            base_url,
        )
        .with_local_engine(crate::config::LocalEngine::OpenAi, None);

        let assistant = factory
            .assistant_for(Brain::Local, None)
            .expect("a local route with a model configured serves");
        assistant
            .verdict("hello")
            .await
            .expect("the stub answers the shape the reader consumes");

        let bodies = seen
            .lock()
            .expect("the stub's recorder is never held across an await");
        assert_eq!(bodies.len(), 1, "expected exactly one exchange: {bodies:?}");
        assert_eq!(
            bodies[0].pointer("/model").and_then(|value| value.as_str()),
            Some("qwen3-coder-30b"),
            "the configured local model must be the one on the OpenAI-compatible wire"
        );
        assert_eq!(
            ollama_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an OpenAI-engined local route must never touch Ollama's own /api/chat — dialling \
             both is a second turn nobody asked for, not a harmless extra request"
        );
    }

    /// The "an existing install keeps working" guard, and the reason the new field defaults rather
    /// than being asked for: every machine running this daemon today has no `local_engine` line at
    /// all, and its local route must keep going to Ollama at `local_base_url`. Without this test
    /// the switch could land as "OpenAI unless told otherwise", or as an engine that must be
    /// declared before the route serves, and every one of those installs would have its local
    /// chats repointed at a server it does not run — with nothing in the config file changed to
    /// explain it.
    #[tokio::test]
    async fn the_local_route_is_still_ollama_when_no_engine_is_configured() {
        let (base_url, ollama_hits, seen) = stub_both_local_dialects().await;
        // No `.with_local_engine(..)` call at all: this is the untouched install.
        let factory = ConfiguredAssistants::new(
            Some("qwen3:8b".to_string()),
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            base_url,
        );

        let assistant = factory
            .assistant_for(Brain::Local, None)
            .expect("a local route with a model configured serves");
        assistant
            .verdict("hello")
            .await
            .expect("the stub answers the shape the reader consumes");

        assert_eq!(
            ollama_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an install that never named an engine must still reach Ollama's /api/chat exactly \
             once"
        );
        assert!(
            seen.lock()
                .expect("the stub's recorder is never held across an await")
                .is_empty(),
            "and must not also speak the OpenAI dialect on the side"
        );
    }

    /// The menu's half of the same fact, one layer below the turn: `declared_one` asks Ollama's
    /// `POST /api/show` for every `Brain::Local` model, so an OpenAI-engined install would have
    /// every local model probed against a service it does not run. That probe fails closed, which
    /// means the picker greys out — or refuses — models the configured server serves perfectly
    /// well, and the operator is shown a menu with no local row in it and no reason given.
    ///
    /// Asserted as a pair, not as a single positive: the `/api/show` counter sits on the SAME
    /// address as the catalogue, so a route that probed both would be caught here rather than
    /// passing on the strength of its second attempt.
    #[tokio::test]
    async fn a_local_model_is_discovered_against_the_openai_models_route() {
        let (base_url, show_hits, models_hits) = stub_both_local_catalogues(serde_json::json!({
            "data": [{
                "id": "qwen3-coder-30b",
                "context_length": 32768,
                "supported_parameters": ["tools"],
                "architecture": { "input_modalities": ["text"] }
            }]
        }))
        .await;
        let factory = ConfiguredAssistants::new(
            Some("qwen3-coder-30b".to_string()),
            None,
            None,
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            base_url,
        )
        .with_local_engine(crate::config::LocalEngine::OpenAi, Some(32_768));

        let _ = factory.can_serve(Brain::Local, "qwen3-coder-30b").await;

        assert_eq!(
            models_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an OpenAI-engined local route must read the server's own catalogue at {{base}}/models \
             exactly once"
        );
        assert_eq!(
            show_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "and must never ask Ollama's /api/show about a model that server has never heard of — \
             that probe fails closed and takes the local rows out of the menu"
        );
    }

    /// A refused engine must leave the local route OFF, not half on. `ModelsConfig::local_engine()`
    /// refuses three ways — an engine name nobody serves, `openai` with no address, an address off
    /// this machine — and `main.rs` answers each by building the factory with `local_model: None`,
    /// which is the route-off state this crate already has a vocabulary for.
    ///
    /// Without this, the tempting repair is a fallback: refuse the engine and quietly serve the
    /// route through Ollama anyway. That is the worst of the three outcomes — the operator wrote a
    /// line asking for a specific server, got no refusal, and their turns go somewhere else. A
    /// disabled route says so, in the sentence `http.rs` already compares against.
    #[tokio::test]
    async fn a_refused_local_engine_leaves_the_local_route_refusing_rather_than_served() {
        // Exactly what `main.rs` passes when `ModelsConfig::local_engine()` returned `Err`: no
        // local model. The hosted route is configured beside it, so what is asserted below is this
        // route being off and not a factory that was handed nothing at all.
        let factory = ConfiguredAssistants::new(
            None,
            Some("anthropic/claude-sonnet-4.5".to_string()),
            Some("key".to_string()),
            "http://127.0.0.1:8791".to_string(),
            "token".to_string(),
            test_pool().await,
            crate::runner::OLLAMA_BASE_URL.to_string(),
        );

        assert_eq!(
            factory.serves(Brain::Local),
            Err(Refusal::RouteNotConfigured),
            "a local engine this daemon refused must leave the route disabled — serving it from \
             the default engine would answer a turn from a server the operator did not name"
        );
    }
}
