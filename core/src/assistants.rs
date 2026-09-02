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
    /// *model* endpoints `runner::OLLAMA_BASE_URL` and `openrouter::OPENROUTER_BASE_URL`, despite
    /// the shared "base url" name those two carry.
    loopback_url: String,
    token: String,
    pool: sqlx::SqlitePool,
    /// Where `can_serve` discovers a local model's capabilities (`/api/show`) — a constructor
    /// parameter rather than the hardcoded `runner::OLLAMA_BASE_URL` `assistant_for` still uses, so
    /// a test can point discovery at a loopback stub server instead of a real daemon. `main.rs`
    /// passes the real constant.
    local_base_url: String,
    /// Where `can_serve`/`declared_for` discover a HOSTED model's capabilities
    /// (`capabilities::discover_openrouter`, against `{base}/models`) — NOT a constructor
    /// parameter like `local_base_url` above: adding one would grow `new`'s argument list, which
    /// would force an edit to every existing seven-argument call site (`main.rs`'s included, and
    /// the four `can_serve` tests this task's own RED phase already froze), none of which this fix
    /// may touch. Defaults to `openrouter::OPENROUTER_BASE_URL` — the same constant
    /// `assistant_for` already builds an `OpenRouterChat` against — and `with_hosted_base_url`
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
                .timeout(crate::openrouter::OPENROUTER_EXCHANGE_TIMEOUT)
                .build()
                .expect("HTTP client for the hosted model (check TLS and proxy environment)"),
            local_model,
            hosted_model,
            hosted_key,
            loopback_url,
            token,
            pool,
            local_base_url,
            hosted_base_url: crate::openrouter::OPENROUTER_BASE_URL.to_string(),
            discovery_cache: crate::capabilities::DiscoveryCache::new(),
            introspection_client: reqwest::Client::builder()
                .timeout(CAPABILITY_PROBE_TIMEOUT)
                .build()
                .expect("HTTP client for capability discovery (check TLS and proxy environment)"),
        }
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
/// not `runner::OLLAMA_EXCHANGE_TIMEOUT`/`openrouter::OPENROUTER_EXCHANGE_TIMEOUT`. Matches
/// `http.rs`'s own `OLLAMA_TAGS_TIMEOUT` value for the same reasoning, kept as its own constant
/// rather than imported from there: every timeout in this crate is declared beside the client that
/// uses it (`OLLAMA_EXCHANGE_TIMEOUT` beside the local generation client, `OPENROUTER_EXCHANGE_TIMEOUT`
/// beside the hosted one, `OLLAMA_TAGS_TIMEOUT` beside `http.rs`'s own tags probe), and reaching
/// into `http.rs` from this module for two seconds' worth of a `Duration` would be a stranger
/// dependency than the value it would save.
const CAPABILITY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[async_trait::async_trait]
impl Assistants for ConfiguredAssistants {
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
                let chat = crate::runner::OllamaChat::with_client(
                    self.local_client.clone(),
                    crate::runner::OLLAMA_BASE_URL.to_string(),
                    resolved,
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
                let chat = crate::openrouter::OpenRouterChat::with_client(
                    self.hosted_client.clone(),
                    // The FIELD, not `openrouter::OPENROUTER_BASE_URL` directly. Behaviour-neutral
                    // in production — the field is initialised to that same constant and only
                    // `with_hosted_base_url`, which is `#[cfg(test)]`, ever changes it — but it is
                    // what lets a test point this route at a stub and read which model actually
                    // went on the wire. Without it the seam covers discovery
                    // (`can_serve`/`declared_for`) and stops short of the one path that bills.
                    self.hosted_base_url.clone(),
                    resolved,
                    key,
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
    /// `Brain::Local` -> `capabilities::discover_ollama_as` against `self.local_base_url`;
    /// `Brain::OpenRouter` -> `capabilities::discover_openrouter` against `self.hosted_base_url`;
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
            crate::chats::Brain::OpenRouter => {
                let client = self.introspection_client.clone();
                let base_url = self.hosted_base_url.clone();
                let owned_model = model.to_string();
                self.discovery_cache
                    .get_or_discover(&key, move || async move {
                        crate::capabilities::discover_openrouter(
                            &client,
                            &base_url,
                            &owned_model,
                            required_tokens,
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
    /// /models` — the route `capabilities::discover_openrouter` reads, the same idiom
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
    /// shape `openrouter::assistant_message` reads. The hosted sibling of `stub_show_counting`
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
    /// and `assistant_for` builds the `OpenRouterChat` out of that, so the model on the wire is the
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
}
