//! Council roster configuration and validation.

use super::*;

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
    pub(super) fn faults_in(&self, who: &str, local_available: bool) -> Vec<String> {
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

pub(super) fn default_council_timeout() -> u64 {
    DEFAULT_COUNCIL_TIMEOUT_SECONDS
}

pub(super) fn default_council_rounds() -> u32 {
    DEFAULT_COUNCIL_ROUNDS
}

impl CouncilConfig {
    /// Everything wrong with a roster, said as one list rather than as the first thing noticed.
    ///
    /// An operator editing this file by hand is going to get more than one thing wrong at once, and
    /// a loader that reports only the first turns a single correction into three restarts.
    pub(super) fn faults(&self, local_available: bool) -> Vec<String> {
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
    pub(super) fn seats(&self) -> impl Iterator<Item = &SeatSpec> {
        std::iter::once(&self.chairman).chain(self.members.iter())
    }

    pub(super) fn validated(mut self) -> Self {
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
