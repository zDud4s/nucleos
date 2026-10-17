use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

mod council;
mod devtime;
mod models;
mod pillars;
mod voice;

pub use self::council::*;
pub use self::devtime::*;
pub use self::models::*;
pub use self::pillars::*;
#[cfg(test)]
use self::voice::DEFAULT_HOTKEYS;
pub use self::voice::*;

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
    /// Recurring schedule. Empty means the rule is one-shot and carries `at:` instead.
    #[serde(default)]
    pub cron: String,
    /// The agent's instruction. Empty means the rule runs `command:` instead.
    #[serde(default)]
    pub prompt: String,
    pub cwd: Option<String>,
    /// One-shot alternative to `cron:`: ISO 8601 with an offset, or a naive
    /// `YYYY-MM-DDTHH:MM[:SS]` read in `timezone` (UTC when absent). Fires once, at or after the
    /// instant (late if the daemon was down), and never again.
    #[serde(default)]
    pub at: Option<String>,
    /// Alternative to `prompt:`: an argv-spawned command, no LLM session. Judged by the
    /// classifier before it runs; runs in `cwd` (relative to the project root) or the root.
    #[serde(default)]
    pub command: Option<String>,
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

/// F2b step 2: what the PreToolUse hook does with a verification command that an unattended
/// worktree run executes by itself.
///
/// `Observe` only records it (step 1). `Refuse` makes the hook deny it, pointing the run at the
/// daemon's `verify` tool. Absent means `Observe`, and that default is the whole compatibility
/// story: a project that says nothing behaves exactly as before this key existed.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerifyGuardMode {
    #[default]
    Observe,
    Refuse,
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
    /// Whether the daemon runs the full gate on the target after a merge lands (spec
    /// 2026-10-05 §6.1). Off by default — a project that says nothing behaves exactly as before
    /// this key existed. Read by the post-merge worker (`verify_postgate_worker`) and by
    /// `git_exec::gate_the_merge`, which decides what the gate before publish measures once this is
    /// on. Turning it on is the owner's decision (F3-8).
    #[serde(default)]
    pub gate_after_land: bool,
    /// Whether the daemon reverts a merge the post-merge gate bisected to a confirmed culprit
    /// (spec 2026-10-05 §6.2, D4). Off by default — a project that says nothing behaves exactly as
    /// before this key existed. The post-merge worker reads it only when `gate_after_land` is on as
    /// well, since without that gate there is no culprit. Turning it on is the owner's decision
    /// (F3-8); nothing in the daemon sets it.
    #[serde(default)]
    pub revert_on_red: bool,
    /// Whether the last item of a job and a run's final gate are measured by `verify scope` over
    /// their diff instead of the full gate. Off by default. Read through `scoped_final()` only: it
    /// has an effect only together with `gate_after_land`, which measures the whole target after
    /// landing and is the net behind a partial measurement. Turning it on is the owner's decision
    /// (F3-8); nothing in the daemon sets it.
    #[serde(default)]
    pub scoped_final_gate: bool,
    /// Minimum minutes between the starts of two post-merge batch gates on one target. Off by
    /// default (0 never holds): a project that says nothing behaves exactly as before this key
    /// existed. It holds only the start of a NEW batch gate, so merges that land meanwhile join one
    /// later gate; a recheck, a bisection and a revert are never held. Turning it on is the
    /// owner's decision; nothing in the daemon sets it.
    #[serde(default)]
    pub postgate_min_interval_mins: u64,
    /// A command the post-merge worker asks before it starts a new batch gate: exit 0 means the
    /// machine is idle; any other answer (another exit code, a failure to start, a timeout) means
    /// busy and the gate waits. Off by default (absent never holds). Example:
    /// `python scripts/heavy.py idle`. Turning it on is the owner's decision; nothing in the
    /// daemon sets it.
    #[serde(default)]
    pub postgate_idle_command: Option<String>,
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
    /// Whether the hook refuses a verification command a worktree run runs by itself. Absent is
    /// `observe`. Its only reader is `hooks::observe_verification`.
    #[serde(default)]
    pub verify_guard: VerifyGuardMode,
}

impl AutopilotRules {
    /// Whether the final gate of a job or a run is scoped: the switch AND `gate_after_land`.
    /// A reader rather than a field read, because a caller that took `scoped_final_gate` straight
    /// off the struct would skip the full gate with no post-merge gate behind it.
    pub fn scoped_final(&self) -> bool {
        self.scoped_final_gate && self.gate_after_land
    }

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
/// Keyed by the project's id and not by a folder, which is the whole of `project/state.rs`'s
/// argument: the same file is found from the main checkout and from every worktree the daemon opens
/// of it, and it does not move when the project's folder does. `machine_root` is
/// `AppState::machine_config_root` in production and a temporary directory in a test.
///
/// No root, or an id that cannot name a directory, reads as a project with no file — the ordinary
/// state, answered with `AutopilotRules::default()`. The file is named once, as
/// [`crate::project::state::AUTOPILOT_FILE`], because two things have to agree about it: this loader
/// and `ownership.rs`, which declares it writable by the app.
pub fn load_schedule_rules(
    machine_root: Option<&Path>,
    project_id: &str,
) -> std::io::Result<AutopilotRules> {
    match crate::project::state::file(
        machine_root,
        project_id,
        crate::project::state::AUTOPILOT_FILE,
    ) {
        Some(path) => load_schedule_rules_from(&path),
        None => Ok(AutopilotRules::default()),
    }
}

/// The same rules, from one file. Absent is `AutopilotRules::default()`.
///
/// Callers poll this every 30s per project from async tasks, so a successful parse is cached per path
/// and keyed by the file's `(len, mtime)`: an unchanged file costs one `metadata` call and a clone,
/// not a read, a YAML parse and a validation. Known limit: two writes inside one mtime tick that
/// leave the length identical are not told apart; the next change that moves either value is picked
/// up. Errors are never cached, so a bad file is reported on every call exactly as before, and a
/// vanished file drops its entry.
pub fn load_schedule_rules_from(path: &Path) -> std::io::Result<AutopilotRules> {
    let Some(meta) = std::fs::metadata(path).ok() else {
        schedule_rules_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
        return Ok(AutopilotRules::default());
    };
    let (len, mtime) = (meta.len(), meta.modified().ok());
    let cached = schedule_rules_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .filter(|entry| cache_hit(entry, len, mtime))
        .map(|entry| entry.rules.clone());
    if let Some(rules) = cached {
        return Ok(rules);
    }
    let rules = parse_schedule_rules(&std::fs::read_to_string(path)?)?;
    schedule_rules_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            path.to_path_buf(),
            RulesCacheEntry {
                len,
                mtime,
                rules: rules.clone(),
            },
        );
    Ok(rules)
}

struct RulesCacheEntry {
    len: u64,
    mtime: Option<std::time::SystemTime>,
    rules: AutopilotRules,
}

type RulesCache = std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, RulesCacheEntry>>;

fn schedule_rules_cache() -> &'static RulesCache {
    static CACHE: std::sync::OnceLock<RulesCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Whether a cached parse still describes the file. An unreadable mtime never hits: with only the
/// length to go on, a same-length edit would be served stale.
fn cache_hit(entry: &RulesCacheEntry, len: u64, mtime: Option<std::time::SystemTime>) -> bool {
    mtime.is_some() && entry.len == len && entry.mtime == mtime
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
        let invalid = |reason: &str| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("schedule `{}`: {reason}", rule.name),
            )
        };
        let has_cron = !rule.cron.trim().is_empty();
        if has_cron && rule.at.is_some() {
            return Err(invalid(
                "give either cron: or at:, not both; cron: recurs and at: fires once",
            ));
        }
        if !has_cron && rule.at.is_none() {
            return Err(invalid("needs cron: (recurring) or at: (once)"));
        }
        let has_prompt = !rule.prompt.trim().is_empty();
        if has_prompt && rule.command.is_some() {
            return Err(invalid(
                "give either prompt: or command:, not both; prompt: starts an agent and command: runs a program",
            ));
        }
        if !has_prompt && rule.command.is_none() {
            return Err(invalid(
                "needs prompt: (an agent run) or command: (a program)",
            ));
        }
        if let Some(command) = rule.command.as_deref() {
            if rule.graph.is_some() {
                return Err(invalid(
                    "graph: needs a prompt, so it cannot go with command:",
                ));
            }
            if let Err(fault) =
                crate::project::commands::validate(&rule.name, command, rule.cwd.as_deref())
            {
                return Err(invalid(&format!("command: {fault}")));
            }
        }
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
        let chat_catalogue = config.catalogue_for_chat(&[]);
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
    fn every_chat_is_offered_both_clis() {
        for primary_runner in [None, Some("codex".to_string())] {
            let config = ModelsConfig {
                primary_runner: primary_runner.clone(),
                assistant_choices: chat_choices(),
                ..ModelsConfig::default()
            };
            let catalogue = config.catalogue_for_chat(&[]);
            let offered: Vec<&str> = catalogue.iter().map(|choice| choice.id.as_str()).collect();
            assert!(
                offered.contains(&"sonnet"),
                "primary_runner {primary_runner:?}: Claude must be offered: {offered:?}"
            );
            assert!(
                offered.contains(&"gpt-5.5"),
                "primary_runner {primary_runner:?}: Codex must be offered: {offered:?}"
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

    #[test]
    fn cache_hit_needs_equal_len_and_a_known_equal_mtime() {
        let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(5);
        let entry = RulesCacheEntry {
            len: 10,
            mtime: Some(t),
            rules: AutopilotRules::default(),
        };
        assert!(cache_hit(&entry, 10, Some(t)));
        assert!(!cache_hit(&entry, 11, Some(t)));
        assert!(!cache_hit(
            &entry,
            10,
            Some(t + std::time::Duration::from_secs(1))
        ));
        assert!(!cache_hit(&entry, 10, None));
    }

    #[test]
    fn a_cached_rules_file_follows_edits_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autopilot.yaml");
        std::fs::write(&path, "verify_disk_cap_gb: 3\n").unwrap();
        let first = load_schedule_rules_from(&path).unwrap();
        assert_eq!(first.verify_disk_cap_gb, Some(3));
        // Unchanged file: the cached parse comes back equal.
        assert_eq!(load_schedule_rules_from(&path).unwrap(), first);
        // A different length is a different file, whatever the mtime tick says.
        std::fs::write(&path, "verify_disk_cap_gb: 120\n").unwrap();
        assert_eq!(
            load_schedule_rules_from(&path).unwrap().verify_disk_cap_gb,
            Some(120)
        );
        // A bad edit is an error on every call, never a stale success.
        std::fs::write(&path, "verify_disk_cap_gb: 0\n").unwrap();
        assert!(load_schedule_rules_from(&path).is_err());
        assert!(load_schedule_rules_from(&path).is_err());
        // Deleted: the missing-file answer, not the old content.
        std::fs::write(&path, "verify_disk_cap_gb: 7\n").unwrap();
        assert_eq!(
            load_schedule_rules_from(&path).unwrap().verify_disk_cap_gb,
            Some(7)
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            load_schedule_rules_from(&path).unwrap(),
            AutopilotRules::default()
        );
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

    /// A rule is either recurring (`cron:`) or one-shot (`at:`). Both is ambiguous and neither
    /// has no time at all, so each is refused at parse time, naming the rule.
    #[test]
    fn a_schedule_rule_takes_cron_or_at_but_never_both_or_neither() {
        let cron_only = parse_schedule_rules(
            "schedules:\n  - name: r1\n    cron: \"0 8 * * *\"\n    prompt: p\n",
        )
        .unwrap();
        assert_eq!(cron_only.schedules[0].at, None);

        let at_only = parse_schedule_rules(
            "schedules:\n  - name: r1\n    at: \"2026-10-22T09:00:00Z\"\n    prompt: p\n",
        )
        .unwrap();
        assert_eq!(
            at_only.schedules[0].at.as_deref(),
            Some("2026-10-22T09:00:00Z")
        );
        assert!(at_only.schedules[0].cron.is_empty());

        let both = parse_schedule_rules(
            "schedules:\n  - name: r1\n    cron: \"0 8 * * *\"\n    at: \"2026-10-22T09:00:00Z\"\n    prompt: p\n",
        )
        .unwrap_err();
        assert_eq!(both.kind(), std::io::ErrorKind::InvalidData);
        assert!(both.to_string().contains("r1"), "{both}");
        assert!(both.to_string().contains("either cron: or at:"), "{both}");

        let neither =
            parse_schedule_rules("schedules:\n  - name: r1\n    prompt: p\n").unwrap_err();
        assert_eq!(neither.kind(), std::io::ErrorKind::InvalidData);
        assert!(neither.to_string().contains("r1"), "{neither}");
        assert!(neither.to_string().contains("needs cron:"), "{neither}");
    }

    /// A rule either starts an agent (`prompt:`) or runs a command (`command:`). A command has no
    /// agent, so it cannot carry a graph, and its text must pass the project-command validation.
    #[test]
    fn a_schedule_rule_takes_prompt_or_command_but_never_both_or_neither() {
        let command_only = parse_schedule_rules(
            "schedules:\n  - name: r1\n    at: \"2026-10-22T09:00:00Z\"\n    command: git --version\n",
        )
        .unwrap();
        assert_eq!(
            command_only.schedules[0].command.as_deref(),
            Some("git --version")
        );
        assert!(command_only.schedules[0].prompt.is_empty());

        let both = parse_schedule_rules(
            "schedules:\n  - name: r1\n    at: \"2026-10-22T09:00:00Z\"\n    prompt: p\n    command: git --version\n",
        )
        .unwrap_err();
        assert_eq!(both.kind(), std::io::ErrorKind::InvalidData);
        assert!(both.to_string().contains("r1"), "{both}");

        let neither =
            parse_schedule_rules("schedules:\n  - name: r1\n    at: \"2026-10-22T09:00:00Z\"\n")
                .unwrap_err();
        assert_eq!(neither.kind(), std::io::ErrorKind::InvalidData);
        assert!(neither.to_string().contains("r1"), "{neither}");

        let with_graph = parse_schedule_rules(
            "schedules:\n  - name: r1\n    at: \"2026-10-22T09:00:00Z\"\n    command: git --version\n    graph:\n      budget_usd: 1.0\n",
        )
        .unwrap_err();
        assert_eq!(with_graph.kind(), std::io::ErrorKind::InvalidData);
        assert!(with_graph.to_string().contains("r1"), "{with_graph}");

        let bad_cwd = parse_schedule_rules(
            "schedules:\n  - name: r1\n    at: \"2026-10-22T09:00:00Z\"\n    command: git --version\n    cwd: ../outside\n",
        )
        .unwrap_err();
        assert_eq!(bad_cwd.kind(), std::io::ErrorKind::InvalidData);
        assert!(bad_cwd.to_string().contains("r1"), "{bad_cwd}");
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
    fn gate_after_land_is_off_by_default_and_existing_files_parse_unchanged() {
        assert!(!AutopilotRules::default().gate_after_land);

        let rules = parse_schedule_rules("gate_command: x\ngate_before_publish: true\n").unwrap();
        assert!(!rules.gate_after_land);
        assert_eq!(rules.gate_command.as_deref(), Some("x"));
        assert!(rules.gate_before_publish);

        for contents in ["", "   \n"] {
            let rules = parse_schedule_rules(contents).unwrap();
            assert!(
                !rules.gate_after_land,
                "{contents:?} must leave the switch off"
            );
        }
    }

    #[test]
    fn gate_after_land_parses_when_set() {
        let rules = parse_schedule_rules("gate_command: x\ngate_after_land: true\n").unwrap();
        assert!(rules.gate_after_land);
        assert_eq!(rules.gate_command.as_deref(), Some("x"));
    }

    #[test]
    fn revert_on_red_is_off_by_default_and_parses_when_set() {
        assert!(!AutopilotRules::default().revert_on_red);

        // Files written before the switch existed parse unchanged, with it off.
        let rules = parse_schedule_rules("gate_command: x\ngate_after_land: true\n").unwrap();
        assert!(!rules.revert_on_red, "an existing file must not turn it on");

        let rules =
            parse_schedule_rules("gate_command: x\ngate_after_land: true\nrevert_on_red: true\n")
                .unwrap();
        assert!(rules.revert_on_red);
        assert!(rules.gate_after_land);
        assert_eq!(rules.gate_command.as_deref(), Some("x"));
    }

    #[test]
    fn scoped_final_gate_is_off_by_default_and_needs_gate_after_land() {
        assert!(!AutopilotRules::default().scoped_final_gate);
        assert!(!AutopilotRules::default().scoped_final());

        // Files written before the switch existed parse unchanged, with it off.
        let rules = parse_schedule_rules("gate_command: x\ngate_after_land: true\n").unwrap();
        assert!(
            !rules.scoped_final_gate,
            "an existing file must not turn it on"
        );
        assert!(!rules.scoped_final());

        // The switch alone is not enough: without the post-merge gate there is no net.
        let rules = parse_schedule_rules("gate_command: x\nscoped_final_gate: true\n").unwrap();
        assert!(rules.scoped_final_gate);
        assert!(!rules.scoped_final(), "no gate_after_land, no scoped final");

        // gate_after_land alone changes nothing either.
        let rules = parse_schedule_rules("gate_command: x\ngate_after_land: true\n").unwrap();
        assert!(!rules.scoped_final());

        let rules = parse_schedule_rules(
            "gate_command: x\ngate_after_land: true\nscoped_final_gate: true\n",
        )
        .unwrap();
        assert!(rules.scoped_final());
        assert_eq!(rules.gate_command.as_deref(), Some("x"));
    }

    #[test]
    fn postgate_quiet_rules_are_off_by_default_and_parse_when_set() {
        // F3-12: both calm rules are off unless the owner writes them.
        let rules = AutopilotRules::default();
        assert_eq!(rules.postgate_min_interval_mins, 0);
        assert_eq!(rules.postgate_idle_command, None);

        // Files written before the rules existed parse unchanged, with both off.
        let rules = parse_schedule_rules("gate_command: x\ngate_after_land: true\n").unwrap();
        assert_eq!(rules.postgate_min_interval_mins, 0);
        assert_eq!(rules.postgate_idle_command, None);

        let rules = parse_schedule_rules(
            "gate_command: x\ngate_after_land: true\npostgate_min_interval_mins: 45\n\
             postgate_idle_command: python scripts/heavy.py idle\n",
        )
        .unwrap();
        assert_eq!(rules.postgate_min_interval_mins, 45);
        assert_eq!(
            rules.postgate_idle_command.as_deref(),
            Some("python scripts/heavy.py idle")
        );
        assert_eq!(rules.gate_command.as_deref(), Some("x"));
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
            crate::execution::runner::OLLAMA_BASE_URL,
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
    fn verify_cache_days_defaults_to_seven() {
        assert_eq!(VerifyConfig::default().cache_days, 7);
        assert_eq!(parse_verify_config("capacity: 2\n").unwrap().cache_days, 7);
    }

    #[test]
    fn verify_cache_days_is_read() {
        assert_eq!(
            parse_verify_config("cache_days: 30\n").unwrap().cache_days,
            30
        );
    }

    #[test]
    fn a_verify_cache_days_of_zero_is_refused() {
        assert!(parse_verify_config("cache_days: 0\n").is_err());
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
    fn the_verify_guard_switch_defaults_to_observe() {
        assert_eq!(
            parse_schedule_rules("gate_command: x\n")
                .unwrap()
                .verify_guard,
            VerifyGuardMode::Observe
        );
        assert_eq!(
            parse_schedule_rules("verify_guard: observe\n")
                .unwrap()
                .verify_guard,
            VerifyGuardMode::Observe
        );
        assert_eq!(
            parse_schedule_rules("verify_guard: refuse\n")
                .unwrap()
                .verify_guard,
            VerifyGuardMode::Refuse
        );
    }

    #[test]
    fn an_unknown_verify_guard_value_is_refused() {
        assert!(parse_schedule_rules("verify_guard: block\n").is_err());
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
