//! Council startup validation, roster resolution, and launch orchestration.

use super::*;

/// Why a council could not be started. Every variant is a refusal BEFORE anything is spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// [`CONFIG_DISPLAY_PATH`] is absent, unreadable, or names a roster the daemon will not run.
    NotConfigured,
    /// The roster asks for a local seat and this daemon has no local model to answer with.
    ///
    /// Distinct from `NotConfigured` because the file may be perfectly good: a per-question roster
    /// override arrives in the request body, and the daemon has to be able to say which half is
    /// wrong.
    NoLocalModel,
    /// The question is empty, or the override roster is not one this daemon would accept.
    Invalid(String),
    /// The budget says no, and names the limit and the spend.
    BudgetExhausted(String),
    /// The active provider's measured quota says no.
    QuotaExhausted(String),
    /// Minting or storing failed and the council would run with a key that authenticates nothing.
    Unavailable(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NotConfigured => write!(
                formatter,
                "no council is configured — write a roster to {CONFIG_DISPLAY_PATH} and restart"
            ),
            StartError::NoLocalModel => write!(
                formatter,
                "the roster asks for a local seat and no local model is configured"
            ),
            StartError::Invalid(reason) => write!(formatter, "{reason}"),
            StartError::BudgetExhausted(reason) => write!(formatter, "{reason}"),
            StartError::QuotaExhausted(reason) => write!(formatter, "{reason}"),
            StartError::Unavailable(reason) => write!(formatter, "{reason}"),
        }
    }
}

/// The roster a request may put in place of the configured one, for that question only.
///
/// Holds the DECLARED form, exactly as the file does, so an override may name catalogue agents too
/// and gets the same refusals in the same words when it names them wrongly.
#[derive(Debug, Clone, Deserialize)]
pub struct RosterOverride {
    pub chairman: SeatSpec,
    pub members: Vec<SeatSpec>,
}

/// Turns a declared roster into the seats that will run, reading the catalogue as it is NOW.
///
/// **Not at load, and this is the decision that gets made wrong.** `load_council_config` is pure and
/// runs at startup: it has no pool, and could not usefully have one, because the catalogue is
/// mutable state — the owner may delete `cetico` at three in the afternoon with a daemon that has
/// been up since nine. A roster validated at boot is a promise about a different afternoon.
///
/// The accepted cost: a roster naming a deleted agent is discovered only when somebody asks a
/// question. That refusal happens before the budget check and before any row is written, so it
/// costs the owner one clear error and nothing else. It is the same cost a project's
/// `autopilot.yaml`
/// already pays by naming a project root that has since moved.
async fn resolve_roster(
    state: &crate::state::AppState,
    chairman: &SeatSpec,
    members: &[SeatSpec],
) -> Result<(CouncilSeat, Vec<CouncilSeat>), StartError> {
    let resolved_chairman = resolve_seat(state, chairman, &crate::config::seat_name(0)).await?;
    let mut resolved_members = Vec::with_capacity(members.len());
    for (index, spec) in members.iter().enumerate() {
        resolved_members
            .push(resolve_seat(state, spec, &crate::config::seat_name(index + 1)).await?);
    }
    Ok((resolved_chairman, resolved_members))
}

/// One declared seat, resolved. `who` is what a refusal calls it — see `config::seat_name`.
pub(super) async fn resolve_seat(
    state: &crate::state::AppState,
    spec: &SeatSpec,
    who: &str,
) -> Result<CouncilSeat, StartError> {
    // Checked here and not only in `CouncilConfig::faults`, because an override never passes
    // through the file's validation and this is the one fault whose consequence is silent: a seat
    // naming both would otherwise run as whichever half this function happened to read first.
    if spec.agent.is_some() && (spec.kind.is_some() || spec.model_ref.is_some()) {
        return Err(StartError::Invalid(format!(
            "{who} names both an agent and a model; a seat is filled by one or the other"
        )));
    }

    let Some(agent_id) = spec.agent.as_deref() else {
        let (Some(kind), Some(model_ref)) = (spec.kind, spec.model_ref.clone()) else {
            return Err(StartError::Invalid(format!(
                "{who} names neither a model nor an agent"
            )));
        };
        if model_ref.trim().is_empty() {
            return Err(StartError::Invalid("a seat names no model".to_string()));
        }
        // Deliberately NOT checking `local_assistant` here. A model seat's locality is legible from
        // the file, so `CouncilConfig::faults` has already refused it at load and `start` refuses it
        // for an override — asking a third time would refuse a configured roster that startup
        // accepted, which is a daemon disagreeing with itself.
        return Ok(CouncilSeat::of_model(kind, model_ref));
    };

    let agent = crate::agent::get(&state.pool, agent_id)
        .await
        .map_err(|error| StartError::Unavailable(error.to_string()))?
        .ok_or_else(|| {
            StartError::Invalid(format!(
                "{who} names the agent `{agent_id}`, which is not in the catalogue"
            ))
        })?;

    let kind = crate::config::seat_kind_for_engine(&agent.engine).ok_or_else(|| {
        StartError::Invalid(format!(
            "{who} names the agent `{agent_id}`, whose engine `{}` is not one a seat can run",
            agent.engine
        ))
    })?;
    // The check the file could not make: an agent's engine is a column, so only here is it known
    // that this seat wants this machine. Same refusal and same words as a `{ kind: local }` line —
    // where a seat came from does not change what the machine can serve.
    // `state.local_assistant.is_none()` before the migration to the assistant factory: the field
    // this read no longer exists, so this call site is one of the lines that migration is allowed
    // to touch beyond the `AppState` literal.
    if kind == SeatKind::Local && state.assistants.serves(crate::chats::Brain::Local).is_err() {
        return Err(StartError::NoLocalModel);
    }
    // A seat's row records WHICH MODEL ANSWERED, `NOT NULL`, and that is the whole point of copying
    // it instead of reading the roster back later. An agent may legitimately leave its model unset
    // and let the CLI choose — a team keeps no such record and does not care — but a seat filled by
    // one could only be written down blank. Refused before anything is spent, rather than stored as
    // an empty string somebody would later have to guess the meaning of.
    let model_ref = agent.model.clone().ok_or_else(|| {
        StartError::Invalid(format!(
            "{who} names the agent `{agent_id}`, which names no model — a seat records the model \
             that answered"
        ))
    })?;

    Ok(CouncilSeat {
        kind,
        model_ref,
        agent: Some(SeatAgent {
            id: agent.id,
            name: agent.name,
            prompt: agent.prompt,
            tool_policy: agent.tool_policy,
        }),
    })
}

/// Deletes a council's throwaway MCP config however the council ends.
///
/// Built BEFORE the driver task and captured by it, which is the first of the three cancellation
/// rules: a task aborted before its first poll drops what it captured without running a line of the
/// body, so a guard constructed inside would never exist at all. The same shape as
/// `assistant::TurnGuard`, minus the chat slot.
struct McpConfigGuard {
    path: std::path::PathBuf,
}

impl Drop for McpConfigGuard {
    fn drop(&mut self) {
        // Best-effort: the file is in the temp directory and the next council writes its own. A
        // failure here is worth nothing to report and would be reported on every shutdown.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where one council's MCP config lives.
///
/// The council id is a uuid this daemon generated, so unlike `assistant::mcp_config_path` there is
/// no untrusted string to encode — but it is filtered anyway rather than trusted, because the cost
/// is one line and the failure it prevents (a `Path::join` that discards its base when the joined
/// component is absolute) is an arbitrary write and delete.
///
/// **The process id is in the name, and it is not decoration.** The temp directory is shared by
/// every process on the machine, so a name built only from the id is the SAME path in two of them
/// — and both write it and both delete it. On this machine that is not hypothetical: the daemon
/// runs while suites run, and several checkouts run suites at once, each with tests that use fixed
/// ids. One deleting the other's config mid-turn is a failure with no cause visible anywhere near
/// it. `transcribe.rs` already names its recordings this way, for the same reason.
pub(super) fn mcp_config_path(council_id: &str) -> std::path::PathBuf {
    let safe: String = council_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect();
    std::env::temp_dir().join(format!(
        "nucleos-council-{}-{safe}.json",
        std::process::id()
    ))
}

/// Starts a council: validates, checks the budget once, writes the record, and spawns the driver.
///
/// Returns as soon as the record exists. Everything after that happens in the background and is
/// read back through `GET /council/{id}` — a council is minutes of work, and a caller holding an
/// HTTP connection open for it would be one dropped request away from a deliberation nobody can
/// find.
///
/// **The budget is read once, here.** Not between phases, which is the tempting place and the wrong
/// one: a council stopped after phase 1 has paid for every answer and produced no synthesis, which
/// is the worst point on the curve. In spend terms a council is atomic — it is refused whole or run
/// whole.
pub async fn start(
    state: &crate::state::AppState,
    question: &str,
    roster: Option<RosterOverride>,
) -> Result<String, StartError> {
    start_with(state, question, roster, None, BTreeMap::new()).await
}

/// [`start`], with the two things a request may ask for beyond the roster: how many critique rounds
/// to run (`None` is the file's), and a role per seat, keyed by the seat index as a string.
///
/// Both are validated BEFORE the budget is read and before anything is written, with the other
/// refusals: a round count outside `1..=config::MAX_COUNCIL_ROUNDS`, a key that is not the index of
/// a seat on this roster, and a role outside the closed set (`formats::Role::parse`, exact and
/// case-sensitive) are each `Invalid`. A typo that quietly became "no role" or "the default rounds"
/// would run — and bill — a council nobody asked for.
pub async fn start_with(
    state: &crate::state::AppState,
    question: &str,
    roster: Option<RosterOverride>,
    rounds: Option<u32>,
    roles: BTreeMap<String, String>,
) -> Result<String, StartError> {
    let configured = state.council.config().ok_or(StartError::NotConfigured)?;
    let token = state
        .council
        .token()
        .ok_or_else(|| {
            StartError::Unavailable(
                "the council has no daemon key; its seats could not call a tool".to_string(),
            )
        })?
        .to_string();

    let question = question.trim();
    if question.is_empty() {
        return Err(StartError::Invalid("the question is empty".to_string()));
    }

    let rounds = rounds.unwrap_or(configured.rounds);
    if !(1..=crate::config::MAX_COUNCIL_ROUNDS).contains(&rounds) {
        return Err(StartError::Invalid(format!(
            "rounds is {rounds}; a council runs 1 to {} rounds",
            crate::config::MAX_COUNCIL_ROUNDS
        )));
    }
    let seat_count = roster
        .as_ref()
        .map_or(configured.members.len(), |roster| roster.members.len());
    let roles = seat_roles(&roles, seat_count)?;

    let (chairman, members) = match &roster {
        Some(override_roster) => {
            let (chairman, members) = (&override_roster.chairman, &override_roster.members);
            if members.is_empty() {
                return Err(StartError::Invalid("the roster has no members".to_string()));
            }
            if members.len() > crate::config::MAX_COUNCIL_SEATS {
                return Err(StartError::Invalid(format!(
                    "a roster may hold at most {} members",
                    crate::config::MAX_COUNCIL_SEATS
                )));
            }
            // The same rule the file is validated against, applied to a roster that never touches
            // the file. An override is for ONE question and writes no configuration, so the
            // daemon's own limits have to be checked here rather than assumed from the file.
            //
            // Only the declared-model seats: an agent seat's locality is a column, and
            // `resolve_seat` refuses it there with this same variant.
            let wants_local = std::iter::once(chairman)
                .chain(members.iter())
                .any(|seat| seat.kind == Some(SeatKind::Local));
            if wants_local && state.assistants.serves(crate::chats::Brain::Local).is_err() {
                return Err(StartError::NoLocalModel);
            }
            resolve_roster(state, chairman, members).await?
        }
        None => resolve_roster(state, &configured.chairman, &configured.members).await?,
    };

    if let crate::governance::budget::BudgetDecision::Pause { reason, source, .. } =
        crate::quota::permits_new_run(state, chrono::Utc::now()).await
    {
        return Err(if source == crate::quota::PAUSE_SOURCE {
            StartError::QuotaExhausted(reason)
        } else {
            StartError::BudgetExhausted(reason)
        });
    }

    let id = crate::auth::generate_uuid_v4();
    // The fallible steps that need no row run BEFORE the insert: a failure after it would leave a
    // council `running` with no driver, forever.
    let mcp_path = mcp_config_path(&id);
    write_mcp_config(&mcp_path).map_err(StartError::Unavailable)?;
    // The request's count when it gave one, bounded above by `MAX_COUNCIL_ROUNDS` like the file's
    // — so a caller can ask for more deliberation, never for more than the daemon will ever run.
    let rounds = i64::from(rounds);
    if let Err(error) = insert_council_with_roles(
        &state.pool,
        &id,
        question,
        &chairman,
        &members,
        rounds,
        &roles,
    )
    .await
    {
        let _ = std::fs::remove_file(&mcp_path);
        return Err(StartError::Unavailable(error.to_string()));
    }

    let _ = crate::feed::append(
        &state.pool,
        None,
        "council_started",
        &format!("council convened with {} seats", members.len()),
        None,
        Some(&crate::feed::Subject::Council(id.clone())),
    )
    .await;

    // Built here and captured below, so the config is removed however the driver ends — including
    // by being aborted before it ever polls.
    let guard = McpConfigGuard { path: mcp_path };
    let driver = Driver {
        state: state.clone(),
        id: id.clone(),
        question: question.to_string(),
        chairman,
        members,
        token,
        timeout: std::time::Duration::from_secs(configured.timeout_seconds),
        rounds,
        roles,
        launched: std::sync::atomic::AtomicUsize::new(0),
    };
    tokio::spawn(async move {
        let _guard = guard;
        driver.run().await;
    });

    Ok(id)
}

/// Writes the seats' MCP config to `path`; the error is the sentence a caller gets.
pub(super) fn write_mcp_config(path: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(&crate::assistant::build_mcp_config(&exe))
        .map_err(|error| error.to_string())?;
    crate::storage::write_atomic(path, &bytes).map_err(|error| error.to_string())
}

/// A request's `{seat index: role}` as one entry per seat, or the refusal that names what is wrong.
///
/// The key is parsed as an unsigned index, so `seat-0` and `-1` are refused as not being one, and
/// an index at or past `seat_count` as naming a seat this roster does not have.
fn seat_roles(
    requested: &BTreeMap<String, String>,
    seat_count: usize,
) -> Result<Vec<Option<formats::Role>>, StartError> {
    let mut roles = vec![None; seat_count];
    for (key, value) in requested {
        let seat_idx: usize = key
            .parse()
            .map_err(|_| StartError::Invalid(format!("`{key}` is not a seat index")))?;
        let Some(slot) = roles.get_mut(seat_idx) else {
            return Err(StartError::Invalid(format!(
                "seat {seat_idx} is not on a roster of {seat_count} seats"
            )));
        };
        let role = formats::Role::parse(value).ok_or_else(|| {
            let known: Vec<&str> = formats::Role::ALL.iter().map(|r| r.as_str()).collect();
            StartError::Invalid(format!(
                "`{value}` is not a role; a seat may be one of {}",
                known.join(", ")
            ))
        })?;
        *slot = Some(role);
    }
    Ok(roles)
}
