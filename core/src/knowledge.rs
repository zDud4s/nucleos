//! What the agent knows, and what a run is told because of it.
//!
//! The gap this closes was named from outside this repository: 87 migrations and no table for
//! anything the agent learned, so every run started from the same prompt with the same blind spots
//! for ever. `0088_refinements.sql` carried the first half of the design and
//! `0143_knowledge.sql` carries this one: ONE store, read in four layers, where the layer is the
//! nature of the knowledge rather than four tables behind a facade.
//!
//! **Split the way `classifier.rs` is split from `hooks.rs`.** [`render`] is pure and holds the
//! only interesting question — what does a node see, in what order, and how much of it — so it is
//! table tests with no database. The storage below is deliberately dumb.
//!
//! This is the phase that moves the store; it deliberately changes no behaviour. The block a node
//! reads after this file lands is the block it read before, over the table's new name and under the
//! old rules. Selection, the five signals and the budget arrive next, and the pool-facing half of
//! them lands in `brief.rs` rather than here.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};

/// How much of what is known a single node's prompt may carry.
///
/// A ceiling and not a target. The run pays for every token of its own brief, and a store that
/// grows for a year would silently take the context the work needs — the failure mode being that
/// nobody notices, because a prompt does not get slower, it gets emptier of room.
pub const RENDER_CHARS: usize = 4_000;

/// One item's share of the room when it got in on its score.
pub const PER_ITEM_CHARS: usize = 600;

/// And one FLOOR item's share, which is half of it, because five floors at 600 would eat 3,000 of the
/// ~3,400 useful characters and leave less than one whole item behind. The five signals would then
/// decide WHICH row fills each floor and nothing else, which is the blindness this module exists to
/// end. At 300 the floors cost ~1,500, three whole items fit in what is left, and the scoring layer
/// decides something again.
pub const FLOOR_ITEM_CHARS: usize = 300;

const PREAMBLE: &str = "\n\nEarlier work on this project left the notes below under the two named admission rules. \
     Each item names its layer. Your brief above is still what you were asked to do; these are \
     things already known about the project you are doing it in:";

/// The nature of what is known, which is what makes one store rather than four.
///
/// Unknown values are not an error and not a default: [`Layer::parse`] returns `Option` and every
/// reader `filter_map`s it, so a row whose layer this binary does not recognise is INVISIBLE rather
/// than counted. That is the second defence the migration leans on when it declines to write a
/// CHECK constraint — the vocabulary lives here, and the write is checked against it before it
/// happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Layer {
    /// Facts about the thing being worked on.
    Semantic,
    /// What happened, measured: the consolidator's half.
    Episodic,
    /// How work is done here.
    Procedural,
    /// What one job knows while it is still running. Closes with the job.
    Working,
}

impl Layer {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "semantic" => Some(Layer::Semantic),
            "episodic" => Some(Layer::Episodic),
            "procedural" => Some(Layer::Procedural),
            "working" => Some(Layer::Working),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Semantic => "semantic",
            Layer::Episodic => "episodic",
            Layer::Procedural => "procedural",
            Layer::Working => "working",
        }
    }
}

/// The four kinds, in the order a node reads them.
///
/// Ordered deliberately and not alphabetically: an instruction changes what the node does, a fact
/// changes what it believes, and the last two only matter once it is doing the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Kind {
    Prompt,
    Memory,
    Skill,
    Subagent,
}

impl Kind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "prompt" => Some(Kind::Prompt),
            "memory" => Some(Kind::Memory),
            "skill" => Some(Kind::Skill),
            "subagent" => Some(Kind::Subagent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Prompt => "prompt",
            Kind::Memory => "memory",
            Kind::Skill => "skill",
            Kind::Subagent => "subagent",
        }
    }

    /// Which layer a kind of 0088's vintage belongs to.
    ///
    /// The same translation `0143_knowledge.sql` writes over the migrated rows, kept here so the
    /// door and the migration cannot disagree about what a `memory` is. A fact about the project is
    /// semantic; the other three are how work is done here.
    fn layer(self) -> Layer {
        match self {
            Kind::Memory => Layer::Semantic,
            Kind::Prompt | Kind::Skill | Kind::Subagent => Layer::Procedural,
        }
    }
}

/// Whose knowledge this is, and — through [`Scope::chain`] — what it inherits.
///
/// Two columns rather than one, so the hot-path index can serve the question. `scope_id` is TEXT
/// and polymorphic, because a project id is TEXT and a job id is INTEGER, which is also why the
/// store carries no foreign key for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The house. `scope_id` is NULL, and only here.
    Machine,
    Project(String),
    /// A job, and the project it belongs to when the caller knows it.
    #[cfg_attr(not(test), allow(dead_code))]
    Job {
        id: i64,
        project: Option<String>,
    },
}

impl Scope {
    /// Parse the two stored columns before using scope as an ordering signal.
    fn parse(kind: &str, id: Option<&str>) -> Option<Self> {
        match (kind, id) {
            ("machine", None) => Some(Scope::Machine),
            ("project", Some(id)) => Some(Scope::Project(id.to_owned())),
            ("job", Some(id)) => Some(Scope::Job {
                id: id.parse().ok()?,
                project: None,
            }),
            _ => None,
        }
    }

    /// The scopes a reader in this one is entitled to, most general first.
    ///
    /// `machine` → `project` → `job` inherits downwards, and the most specific wins where they
    /// contradict.
    fn chain(&self) -> Vec<(&'static str, Option<String>)> {
        let mut chain = vec![("machine", None)];
        match self {
            Scope::Machine => {}
            Scope::Project(id) => chain.push(("project", Some(id.clone()))),
            Scope::Job { id, project } => {
                if let Some(project) = project {
                    chain.push(("project", Some(project.clone())));
                }
                chain.push(("job", Some(id.to_string())));
            }
        }
        chain
    }

    /// The two columns as they are written.
    fn columns(&self) -> (&'static str, Option<String>) {
        match self {
            Scope::Machine => ("machine", None),
            Scope::Project(id) => ("project", Some(id.clone())),
            Scope::Job { id, .. } => ("job", Some(id.to_string())),
        }
    }

    /// The scope 0088 could express, which is the only one its rows can have had.
    fn of_project(project_id: Option<&str>) -> Scope {
        match project_id {
            None => Scope::Machine,
            Some(id) => Scope::Project(id.to_owned()),
        }
    }
}

/// One candidate, with everything the pure selection needs and nothing it would have to fetch.
///
/// `s_fts` arrives ALREADY CALCULATED, and that is what purity costs: the rank is SQLite's, computed
/// by `brief` — the one function here that talks to a database. A pure function that had to rank text
/// itself would be a second, worse implementation of FTS5.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Known {
    pub id: i64,
    pub layer: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub source: String,
    pub generator: Option<String>,
    pub evidence: Option<String>,
    pub observations: Option<i64>,
    pub fingerprint: Option<String>,
    pub points_at: Option<String>,
    pub expires_after_runs: Option<i64>,
    pub last_confirmed_at: Option<String>,
    pub shown_count: i64,
    /// Briefings whose run reached an outcome. `0` is NOT MEASURED, and it is a different thing from
    /// `green_count == 0` with this above zero, which is measured badly.
    pub outcome_count: i64,
    pub green_count: i64,
    pub last_shown_at: Option<String>,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// SQLite's rank for this row against the context's query, normalised. `0.0` when the row
    /// did not match at all, which on day one is every row.
    ///
    /// Not a column of `knowledge`: the rank belongs to a QUERY, so `#[sqlx(default)]` leaves it
    /// zero for every reader that selects `COLUMNS`, and `brief` sets it after the fetch.
    #[sqlx(default)]
    pub s_fts: f64,
    /// Cosine against the briefing's query vector, set by `brief`; `0.0` for any other reader.
    /// Not a column of `knowledge`, so it is not in `COLUMNS`.
    #[sqlx(default)]
    pub s_sim: f64,
    pub status: String,
    pub proposal_id: Option<i64>,
    pub supersedes: Option<i64>,
    pub origin_run_id: Option<i64>,
    pub created_at: String,
    pub activated_at: Option<String>,
    pub ended_at: Option<String>,
}

/// The kind of node whose work is being briefed, in the order the job graph uses them.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Plan,
    Implement,
    Review,
    Replan,
}

/// What the work is, as far as the selection is allowed to know it.
#[derive(Clone)]
pub struct Context {
    /// The scope and its chain (machine -> project -> job).
    pub chain: Vec<Scope>,
    /// The files the worktree touched, or the ones the item declares.
    pub files: Vec<String>,
    /// The communities and modules of the project map those files inhabit — the signal this project
    /// has for free because it already builds the map.
    pub communities: Vec<String>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub node: Option<NodeKind>,
    /// The normalised signature of the red gate, when there is one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub gate: Option<String>,
    /// Whether this briefing has a query vector (spec 5.2): absence is per briefing, not per row.
    /// Set only by `brief`.
    pub query_embedded: bool,
}

impl Context {
    /// Every context outside a job inherits only this chain (spec sections 3.4 and 6).
    pub fn for_project(project_id: Option<&str>) -> Self {
        let mut chain = vec![Scope::Machine];
        if let Some(project_id) = project_id {
            chain.push(Scope::Project(project_id.to_owned()));
        }
        Self {
            chain,
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        }
    }
}

/// The room, and the three numbers that decide how it is spent.
pub struct Budget {
    pub render_chars: usize,
    pub per_item_chars: usize,
    pub floor_item_chars: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            render_chars: RENDER_CHARS,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        }
    }
}

/// One candidate's TRACE and not the signal: whether it was shown and the five scores that decided
/// why it won or lost.
pub struct Scored {
    pub knowledge_id: i64,
    pub shown: bool,
    pub s_fts: f64,
    pub s_sim: f64,
    pub s_scope: f64,
    pub s_structure: f64,
    pub s_recency: f64,
    pub s_use: f64,
    /// The weighted sum of the signals above — the one number the order uses, carried so the
    /// trace can say why a row won or lost.
    pub score: f64,
}

impl Scored {
    fn weighted(&self, query_embedded: bool) -> f64 {
        let w = weights(query_embedded);
        self.s_fts * w.fts
            + self.s_sim * w.sim
            + self.s_structure * w.structure
            + self.s_use * w.use_
            + self.s_scope * w.scope
            + self.s_recency * w.recency
    }
}

/// What a node reads, and what it was not shown.
pub struct Brief {
    pub block: Option<String>,
    /// One entry per candidate, shown or not, with the five signals — this is what `brief` writes to
    /// `run_knowledge`, and the reason the trace can answer WHICH signal elected a row.
    pub trace: Vec<Scored>,
}

/// The midpoint of the bounded utility range: without any outcomes, there is evidence for neither
/// success nor failure, so the neutral value must favour neither end.
const NEUTRAL_UTILITY: f64 = 0.5;

/// The midpoint of the bounded recency range: without a show timestamp, there is evidence for
/// neither end, so the neutral value must favour neither end.
const NEUTRAL_RECENCY: f64 = 0.5;

/// The weight of each signal. Two tables, each summing to 1.0: one for a briefing that has a query
/// vector and one for a briefing that has none (spec 5.2).
struct Weights {
    /// Confirmed by owner 2026-09-23. The text match against this very work is the most direct evidence of relevance, but it is 0.0 for every row on day one, so it cannot be the whole score.
    fts: f64,
    /// SIM: confirmed by owner 2026-10-06 (destilador spec 5.2); taken from FTS because both measure relevance to the task.
    sim: f64,
    /// Confirmed by owner 2026-09-23. Files and map communities overlap: the signal this project has for free because it already builds the map (spec §5.1).
    structure: f64,
    /// Confirmed by owner 2026-09-23. Measured outcomes. When the other signals are equal,
    /// `use_ * NEUTRAL_UTILITY >= recency` guarantees that a never-shown row never scores below a
    /// failed row. At the extreme they tie, and `(layer, kind, id)` breaks the tie (spec §5.4).
    use_: f64,
    /// Confirmed by owner 2026-09-23. The chain already decides entitlement; specificity only tips a contradiction towards the most specific scope (spec §3.4).
    scope: f64,
    /// Confirmed by owner 2026-09-23. Decay (spec §8.1) measures less than it seems — run-less contexts leave no trace to refresh it (D15) — so it weighs least.
    recency: f64,
}

const WITH_SIM: Weights = Weights {
    fts: 0.20,
    sim: 0.15,
    structure: 0.20,
    use_: 0.20,
    scope: 0.15,
    recency: 0.10,
};

const WITHOUT_SIM: Weights = Weights {
    fts: 0.20 + 0.15,
    sim: 0.0,
    structure: 0.20,
    use_: 0.20,
    scope: 0.15,
    recency: 0.10,
};

fn weights(query_embedded: bool) -> &'static Weights {
    if query_embedded {
        &WITH_SIM
    } else {
        &WITHOUT_SIM
    }
}

/// Score every candidate once. Each signal and their one weighted score are on the unit scale; the
/// caller follows that score only with stable vocabulary and id tie-breakers.
fn scored_candidates<'a>(known: &'a [Known], context: &Context) -> Vec<(&'a Known, Scored)> {
    let utility_if_absent = median_measured_utility(known);
    let recencies = normalised_recencies(known);

    known
        .iter()
        .enumerate()
        .filter(|(_, row)| Layer::parse(&row.layer).is_some())
        .filter(|(_, row)| Kind::parse(&row.kind).is_some())
        .filter_map(|(index, row)| {
            let mut scored = Scored {
                knowledge_id: row.id,
                shown: false,
                // `brief::of` sets it from SQLite's bm25, min-max normalised per pass
                // (`brief::normalise_fts`); the clamp keeps out-of-contract input from
                // outweighing the rest (spec 5.5).
                s_fts: finite_or_zero(row.s_fts).clamp(0.0, 1.0),
                s_sim: finite_or_zero(row.s_sim).clamp(0.0, 1.0),
                s_scope: scope_specificity(row, context)?,
                s_structure: structural_overlap(row, context),
                s_recency: recencies[index],
                // `outcome_count == 0` is absence, not failure. Give it the median measured
                // utility from this pass, floored at `NEUTRAL_UTILITY`, or `NEUTRAL_UTILITY` when
                // nothing is measured.
                s_use: if row.outcome_count == 0 {
                    utility_if_absent
                } else {
                    row.green_count as f64 / row.outcome_count as f64
                },
                score: 0.0,
            };
            scored.score = scored.weighted(context.query_embedded);
            Some((row, scored))
        })
        .collect()
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn scope_specificity(row: &Known, context: &Context) -> Option<f64> {
    let row_scope = Scope::parse(&row.scope_kind, row.scope_id.as_deref())?;
    context
        .chain
        .iter()
        .position(|scope| scope.columns() == row_scope.columns())
        .map(|index| (index + 1) as f64 / context.chain.len() as f64)
        // A recognised but out-of-chain candidate is not entitled to inherit into this context.
        .or(Some(0.0))
}

fn structural_overlap(row: &Known, context: &Context) -> f64 {
    let possible = context.files.len() + context.communities.len();
    if possible == 0 {
        return 0.0;
    }
    let Some(points_at) = row.points_at.as_deref() else {
        return 0.0;
    };
    let references = structural_references(points_at);
    context
        .files
        .iter()
        .chain(&context.communities)
        .filter(|candidate| {
            let candidate = normalise_reference(candidate);
            references.iter().any(|reference| reference == &candidate)
        })
        .count() as f64
        / possible as f64
}

fn structural_references(raw: &str) -> Vec<String> {
    fn collect(value: &serde_json::Value, into: &mut Vec<String>) {
        match value {
            serde_json::Value::String(value) => into.push(normalise_reference(value)),
            serde_json::Value::Array(values) => {
                for value in values {
                    collect(value, into);
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    collect(value, into);
                }
            }
            _ => {}
        }
    }

    let mut references = Vec::new();
    if let Ok(value) = serde_json::from_str(raw) {
        collect(&value, &mut references);
    } else {
        references.extend(
            raw.split([',', ';', '\n'])
                .map(normalise_reference)
                .filter(|reference| !reference.is_empty()),
        );
    }
    references.sort();
    references.dedup();
    references
}

fn normalise_reference(reference: &str) -> String {
    reference
        .trim()
        .trim_matches(['"', '\'', '[', ']', '{', '}'])
        .replace('\\', "/")
}

fn normalised_recencies(known: &[Known]) -> Vec<f64> {
    let timestamps: Vec<Option<f64>> = known
        .iter()
        .map(|row| {
            row.last_shown_at
                .as_deref()
                .and_then(|shown| chrono::DateTime::parse_from_rfc3339(shown).ok())
                .map(|shown| shown.timestamp_micros() as f64)
        })
        .collect();
    let shown: Vec<f64> = timestamps.iter().flatten().copied().collect();
    let Some(min) = shown.iter().copied().min_by(f64::total_cmp) else {
        return vec![NEUTRAL_RECENCY; known.len()];
    };
    let max = shown
        .iter()
        .copied()
        .max_by(f64::total_cmp)
        .expect("a minimum means there is also a maximum");
    let normalise = |timestamp: f64| {
        if max == min {
            NEUTRAL_RECENCY
        } else {
            (timestamp - min) / (max - min)
        }
    };
    let absent = median(shown.into_iter().map(normalise).collect()).unwrap_or(NEUTRAL_RECENCY);
    timestamps
        .into_iter()
        .map(|timestamp| timestamp.map(&normalise).unwrap_or(absent))
        .collect()
}

fn median_measured_utility(known: &[Known]) -> f64 {
    let measured: Vec<f64> = known
        .iter()
        .filter(|row| row.outcome_count > 0)
        .map(|row| row.green_count as f64 / row.outcome_count as f64)
        .collect();
    // An unmeasured row is never scored as worse than even (owner, 2026-09-24).
    median(measured)
        .map(|m| m.max(NEUTRAL_UTILITY))
        .unwrap_or(NEUTRAL_UTILITY)
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        // The even median is the arithmetic mean of the two middle values, not the lower middle.
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

/// The candidates in `(score desc, layer, kind, id asc)` order for the selector to consider.
fn ordered_candidates<'a>(known: &'a [Known], context: &Context) -> Vec<&'a Known> {
    let mut candidates: Vec<(Layer, Kind, &Known, Scored)> = scored_candidates(known, context)
        .into_iter()
        .filter_map(|(row, scored)| {
            Some((
                Layer::parse(&row.layer)?,
                Kind::parse(&row.kind)?,
                row,
                scored,
            ))
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .3
            .score
            .total_cmp(&left.3.score)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.id.cmp(&right.2.id))
    });
    candidates.into_iter().map(|(_, _, row, _)| row).collect()
}

struct Selected<'a> {
    row: &'a Known,
    width: usize,
    chars: usize,
}

struct Selection<'a> {
    items: Vec<Selected<'a>>,
    omitted: Vec<usize>,
}

fn same_scope(row: &Known, scope: &Scope) -> bool {
    Scope::parse(&row.scope_kind, row.scope_id.as_deref())
        .is_some_and(|row_scope| row_scope.columns() == scope.columns())
}

fn scope_heading(scope: &Scope) -> String {
    match scope {
        Scope::Machine => "\n\nHouse-wide knowledge:".into(),
        Scope::Project(id) => format!("\n\nKnowledge about project {id}:"),
        Scope::Job { id, .. } => format!("\n\nKnowledge about job {id}:"),
    }
}

fn cut_notice(omitted: usize) -> String {
    format!(
        "\n\n({omitted} further admitted {} in this scope {} not shown here, to leave room for the work.)",
        if omitted == 1 { "note" } else { "notes" },
        if omitted == 1 { "is" } else { "are" }
    )
}

fn item_piece(row: &Known, width: usize) -> String {
    let layer = Layer::parse(&row.layer).expect("selection only carries recognised layers");
    let provenance = match layer {
        Layer::Working => "(said by a run of this job, not approved) ",
        _ => "",
    };
    format!(
        "\n- [{}] {}{}: {}",
        layer.as_str(),
        provenance,
        row.title,
        clip(&row.body, width)
    )
}

fn char_count(value: &str) -> usize {
    value.chars().count()
}

fn select_pass<'a>(
    candidates: &[&'a Known],
    groups: &[Scope],
    budget: &Budget,
    initial_notices: &[bool],
    reserve_new_notices: bool,
) -> Selection<'a> {
    let totals: Vec<usize> = groups
        .iter()
        .map(|scope| {
            candidates
                .iter()
                .filter(|row| same_scope(row, scope))
                .count()
        })
        .collect();
    let mut notices = initial_notices.to_vec();
    let mut used = char_count(PREAMBLE)
        + groups
            .iter()
            .map(|scope| char_count(&scope_heading(scope)))
            .sum::<usize>()
        + notices
            .iter()
            .enumerate()
            .filter(|(_, reserved)| **reserved)
            .map(|(index, _)| char_count(&cut_notice(totals[index])))
            .sum::<usize>();

    let floor_counts = [
        (Layer::Semantic, 2usize),
        (Layer::Episodic, 1usize),
        (Layer::Procedural, 1usize),
        (Layer::Working, 1usize),
    ];
    let mut floor_ids = Vec::new();
    let mut priority = Vec::new();
    for (layer, count) in floor_counts {
        for row in candidates
            .iter()
            .copied()
            .filter(|row| Layer::parse(&row.layer) == Some(layer))
            .take(count)
        {
            floor_ids.push(row.id);
            priority.push((row, budget.floor_item_chars));
        }
    }
    priority.extend(
        candidates
            .iter()
            .copied()
            .filter(|row| !floor_ids.contains(&row.id))
            .map(|row| (row, budget.per_item_chars)),
    );

    let mut selected: Vec<Selected<'a>> = Vec::new();
    for (row, width) in priority {
        let group = groups
            .iter()
            .position(|scope| same_scope(row, scope))
            .expect("candidate belongs to one present scope group");
        let chars = char_count(&item_piece(row, width));
        if used.saturating_add(chars) <= budget.render_chars {
            used += chars;
            selected.push(Selected { row, width, chars });
            continue;
        }

        if !reserve_new_notices || notices[group] {
            continue;
        }
        notices[group] = true;
        used = used.saturating_add(char_count(&cut_notice(totals[group])));
        while used > budget.render_chars {
            let Some(removed) = selected.pop() else {
                break;
            };
            used -= removed.chars;
            let removed_group = groups
                .iter()
                .position(|scope| same_scope(removed.row, scope))
                .expect("selected row belongs to one present scope group");
            if !notices[removed_group] {
                notices[removed_group] = true;
                used = used.saturating_add(char_count(&cut_notice(totals[removed_group])));
            }
        }
    }

    let omitted = groups
        .iter()
        .enumerate()
        .map(|(index, scope)| {
            totals[index]
                - selected
                    .iter()
                    .filter(|item| same_scope(item.row, scope))
                    .count()
        })
        .collect();
    Selection {
        items: selected,
        omitted,
    }
}

/// Whether a row carries the approval required outside the automatic briefing.
///
/// Consolidator measurements count only when they have both their episodic shape and a measured
/// observation count. `recall` shares this rule with briefing admission rather than inventing a
/// second meaning for `active`. A third exception is opened deliberately, by owner decision D3: a
/// distilled episode (`source = 'distiller'`, active on trial) reaches a node unapproved, spec
/// `.ai/specs/2026-10-05-destilador-design.md` section 4.6. This function must NOT be narrowed to
/// say so: a distilled semantic or procedural row a person approved is `active` with
/// `source = 'distiller'` too, and it is approved. A proposed one never gets here. The other named
/// exception is not approval: `admitted` separately requires a same-job working row with
/// well-tagged evidence.
pub fn approved(row: &Known) -> bool {
    match row.status.as_str() {
        "active" if row.source == "consolidator" => {
            row.layer == "episodic" && row.observations.is_some()
        }
        "active" => true,
        _ => false,
    }
}

// Only what a person approved. Filtered here rather than trusted from the caller's query:
// `select` is the last thing between a `proposed` row and a node's prompt, and something that
// reaches a prompt unapproved makes the approval decorative, which is the entire mechanism.
// The named exceptions are a measured consolidator observation, a working fact with well-tagged
// evidence read inside the same job, and -- opened deliberately by owner decision D3, spec
// `.ai/specs/2026-10-05-destilador-design.md` section 4.6 -- a distilled episode (`source =
// 'distiller'`) active on trial, which `approved` already lets through; spelling their complete
// shapes here keeps a fourth one out.
fn admitted(row: &Known, context: &Context) -> bool {
    match row.status.as_str() {
        "active" => approved(row),
        "live" => {
            row.source == "run"
                && row.layer == "working"
                && row.evidence.as_deref().is_some_and(evidence_is_tagged)
                && context
                    .chain
                    .iter()
                    .any(|scope| matches!(scope, Scope::Job { .. }) && same_scope(row, scope))
        }
        _ => false,
    }
}

/// Select a bounded briefing without doing I/O.
///
/// Admission applies spec §4.5 first: approved rows plus the two named exceptions, measured
/// consolidator observations and same-job working facts with well-tagged evidence. Structure then
/// owns its bytes first. Populated layers claim their item floors, and candidates left over compete in
/// [`ordered_candidates`] order. The first pass discovers which scope groups are cut; the second
/// reserves those notices before choosing rows and accounts for any cut it exposes at the
/// boundary. The trace covers every admitted candidate.
pub fn select(known: &[Known], context: &Context, budget: &Budget) -> Brief {
    let admitted: Vec<Known> = known
        .iter()
        .filter(|row| admitted(row, context))
        .cloned()
        .collect();
    let candidates: Vec<&Known> = ordered_candidates(&admitted, context)
        .into_iter()
        .filter(|row| context.chain.iter().any(|scope| same_scope(row, scope)))
        .collect();
    if candidates.is_empty() {
        return Brief {
            block: None,
            trace: scored_candidates(&admitted, context)
                .into_iter()
                .map(|(_, scored)| scored)
                .collect(),
        };
    }

    let groups: Vec<Scope> = context
        .chain
        .iter()
        .filter(|scope| candidates.iter().any(|row| same_scope(row, scope)))
        .cloned()
        .collect();
    let first = select_pass(
        &candidates,
        &groups,
        budget,
        &vec![false; groups.len()],
        false,
    );
    let reserved: Vec<bool> = first.omitted.iter().map(|omitted| *omitted > 0).collect();
    let selected = select_pass(&candidates, &groups, budget, &reserved, true);
    let shown: Vec<i64> = selected.items.iter().map(|item| item.row.id).collect();

    let mut block = String::from(PREAMBLE);
    for (index, scope) in groups.iter().enumerate() {
        block.push_str(&scope_heading(scope));
        for item in selected
            .items
            .iter()
            .filter(|item| same_scope(item.row, scope))
        {
            block.push_str(&item_piece(item.row, item.width));
        }
        if selected.omitted[index] > 0 {
            block.push_str(&cut_notice(selected.omitted[index]));
        }
    }
    let block = (char_count(&block) <= budget.render_chars).then_some(block);
    let trace = scored_candidates(&admitted, context)
        .into_iter()
        .map(|(_, mut scored)| {
            scored.shown = block.is_some() && shown.contains(&scored.knowledge_id);
            scored
        })
        .collect();
    Brief { block, trace }
}

/// Every column of the store, in the order the migration declares them.
///
/// One constant and not five copies: [`FromRow`] matches by name, so a query that forgets a column
/// fails at runtime on the row rather than at the call site, and the five readers below would each
/// have to be corrected separately every time the table grows.
const COLUMNS: &str = "id, layer, scope_kind, scope_id, source, generator, evidence, observations,
                       fingerprint, points_at, expires_after_runs, last_confirmed_at, shown_count,
                       outcome_count, green_count, last_shown_at, kind, title, body, status,
                       proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at";

/// PURE: the block a node's brief gains because of what earlier runs learned.
///
/// Appended to the brief and never replacing it, exactly as `notes::render` is — a node handed a
/// standing instruction instead of its task does the standing instruction.
/// It trusts [`select`] for admission and holds no second copy of the rule.
#[cfg_attr(not(test), allow(dead_code))] // Tests only since Task 3.4; production goes through brief::of.
pub fn render(known: &[Known], context: &Context) -> Option<String> {
    select(known, context, &Budget::default()).block
}

/// One row's share of the room.
///
/// By chars and not bytes: a slice through a UTF-8 boundary panics on exactly the inputs nobody
/// writes tests with, and a body is free text somebody wrote.
fn clip(body: &str, width: usize) -> String {
    if body.chars().count() <= width {
        return body.to_owned();
    }
    let mut cut: String = body.chars().take(width).collect();
    cut.push('…');
    cut
}

/// The stable identity and short human label of one failed gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureSignature {
    pub fingerprint: String,
    pub headline: String,
}

/// Turns the volatile output of a failed gate into the failure it describes.
pub fn failure_signature(output: &str) -> Option<FailureSignature> {
    if output.trim().is_empty() {
        return None;
    }

    let normalised: Vec<String> = output
        .lines()
        .map(normalise_line)
        .filter(|line| !line.is_empty())
        .collect();
    if normalised.is_empty() {
        return None;
    }

    let mut decisive = Vec::new();
    for line in &normalised {
        let lower = line.to_lowercase();
        if [
            "test result:",
            "running ",
            "finished ",
            "compiling ",
            "…[output truncated",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            continue;
        }
        if [
            "error",
            "failed",
            "panicked",
            "denied",
            "acesso negado",
            "cannot",
            "could not",
            "not found",
            "assert",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            decisive.push(line.as_str());
            if decisive.len() == 3 {
                break;
            }
        }
    }
    if decisive.is_empty() {
        decisive.push(normalised.last().expect("non-empty output has one line"));
    }

    let joined = decisive.join("\n");
    Some(FailureSignature {
        fingerprint: hex16(&Sha256::digest(joined.as_bytes())),
        headline: clip(decisive[0], 160),
    })
}

fn normalise_line(line: &str) -> String {
    let line = replace_worktree_paths(line);
    let line = replace_numbered_ids(&line);
    let line = replace_line_columns(&line);
    let line = replace_durations(&line);
    collapse_horizontal_space(&line)
}

fn replace_worktree_paths(line: &str) -> String {
    let mut normalised = line.to_owned();
    let mut cursor = 0;
    loop {
        let bytes = normalised.as_bytes();
        let mut found = None;
        let mut at = cursor;
        while at < bytes.len() {
            let numbered = bytes[at..].starts_with(b"run-") || bytes[at..].starts_with(b"job-");
            if numbered && at > 0 && matches!(bytes[at - 1], b'/' | b'\\') {
                let mut end = at + 4;
                let digit_start = end;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > digit_start && end < bytes.len() && matches!(bytes[end], b'/' | b'\\') {
                    found = Some((at, end));
                    break;
                }
            }
            at += 1;
        }
        let Some((segment, end)) = found else {
            break;
        };

        let mut start = segment;
        while start > 0 && !is_path_token_delimiter(normalised.as_bytes()[start - 1]) {
            start -= 1;
        }
        normalised.replace_range(start..end, "<worktree>");
        let replacement_end = start + "<worktree>".len();
        let mut token_end = replacement_end;
        while token_end < normalised.len()
            && !is_path_token_delimiter(normalised.as_bytes()[token_end])
        {
            token_end += 1;
        }
        let canonical_suffix = normalised[replacement_end..token_end].replace('\\', "/");
        normalised.replace_range(replacement_end..token_end, &canonical_suffix);
        cursor = replacement_end + canonical_suffix.len();
    }
    normalised
}

fn is_path_token_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(byte, b'"' | b'\'' | b'(' | b')' | b'<' | b'>' | b'=' | b',')
}

fn replace_numbered_ids(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalised = String::with_capacity(line.len());
    let mut copied_until = 0;
    let mut at = 0;
    while at < bytes.len() {
        let label = if bytes[at..].starts_with(b"run-") {
            Some("run-<n>")
        } else if bytes[at..].starts_with(b"job-") {
            Some("job-<n>")
        } else {
            None
        };
        let Some(label) = label else {
            at += 1;
            continue;
        };
        let mut end = at + 4;
        let digit_start = end;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == digit_start {
            at += 4;
            continue;
        }
        normalised.push_str(&line[copied_until..at]);
        normalised.push_str(label);
        copied_until = end;
        at = end;
    }
    normalised.push_str(&line[copied_until..]);
    normalised
}

fn replace_line_columns(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalised = String::with_capacity(line.len());
    let mut copied_until = 0;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b':' {
            at += 1;
            continue;
        }
        let mut middle = at + 1;
        while middle < bytes.len() && bytes[middle].is_ascii_digit() {
            middle += 1;
        }
        if middle == at + 1 || middle == bytes.len() || bytes[middle] != b':' {
            at += 1;
            continue;
        }
        let mut end = middle + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == middle + 1 {
            at += 1;
            continue;
        }
        normalised.push_str(&line[copied_until..at]);
        normalised.push_str(":<l>:<c>");
        copied_until = end;
        at = end;
    }
    normalised.push_str(&line[copied_until..]);
    normalised
}

fn replace_durations(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalised = String::with_capacity(line.len());
    let mut copied_until = 0;
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let mut end = at;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end < bytes.len()
            && bytes[end] == b'.'
            && end + 1 < bytes.len()
            && bytes[end + 1].is_ascii_digit()
        {
            end += 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
        }
        let suffix_end = if bytes[end..].starts_with(b"ms") {
            end + 2
        } else if bytes[end..].starts_with(b"s") {
            end + 1
        } else {
            at = end;
            continue;
        };
        if line[suffix_end..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
        {
            at = suffix_end;
            continue;
        }
        normalised.push_str(&line[copied_until..at]);
        normalised.push_str("<t>");
        copied_until = suffix_end;
        at = suffix_end;
    }
    normalised.push_str(&line[copied_until..]);
    normalised
}

fn collapse_horizontal_space(line: &str) -> String {
    let mut normalised = String::with_capacity(line.len());
    let mut separating = false;
    for character in line.trim().chars() {
        if matches!(character, ' ' | '\t') {
            separating = !normalised.is_empty();
        } else {
            if separating {
                normalised.push(' ');
                separating = false;
            }
            normalised.push(character);
        }
    }
    normalised
}

fn hex16(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(16);
    for byte in bytes.iter().take(8) {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Closes every live working finding owned by one job, preserving the transition as history.
pub async fn close_working_for_job(pool: &SqlitePool, job_id: i64) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let at = chrono::Utc::now().to_rfc3339();
    let scope_id = job_id.to_string();
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         SELECT id, 'live', 'closed', 'the job ended', ?
           FROM knowledge
          WHERE layer = 'working' AND scope_kind = 'job' AND scope_id = ? AND status = 'live'",
    )
    .bind(&at)
    .bind(&scope_id)
    .execute(&mut *tx)
    .await?;
    let closed = sqlx::query(
        "UPDATE knowledge SET status = 'closed', ended_at = ?
          WHERE layer = 'working' AND scope_kind = 'job' AND scope_id = ? AND status = 'live'",
    )
    .bind(&at)
    .bind(&scope_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(closed)
}

/// Closes live working findings whose job has ended or no longer exists.
pub async fn close_orphaned_working(pool: &SqlitePool) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let at = chrono::Utc::now().to_rfc3339();
    let placeholders = vec!["?"; crate::job::TERMINAL_STATUSES.len()].join(", ");
    let predicate = format!(
        "layer = 'working' AND scope_kind = 'job' AND status = 'live'
         AND NOT EXISTS (
             SELECT 1 FROM jobs j
              WHERE CAST(j.id AS TEXT) = knowledge.scope_id
                AND j.status NOT IN ({placeholders})
         )"
    );

    // `AssertSqlSafe`, audited: the only interpolation is one `?` per status in the fixed
    // `TERMINAL_STATUSES` list. Every status is bound below.
    let mut events = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         SELECT id, 'live', 'closed', 'the job ended', ? FROM knowledge WHERE {predicate}"
    )))
    .bind(&at);
    for status in crate::job::TERMINAL_STATUSES {
        events = events.bind(status);
    }
    events.execute(&mut *tx).await?;

    // Same audit as above; `ended_at` is bound before the fixed status list.
    let mut update = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE knowledge SET status = 'closed', ended_at = ? WHERE {predicate}"
    )))
    .bind(&at);
    for status in crate::job::TERMINAL_STATUSES {
        update = update.bind(status);
    }
    let closed = update.execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(closed)
}

/// What a node in this scope is entitled to be told.
///
/// The chain comes too, and that is the whole reason this takes a [`Scope`] rather than a project
/// id: machine-wide rows are not hidden by the scoping rule, which is about not letting one
/// project's lesson become another's lie. `scope_id IS ?` rather than `=`, because the machine
/// scope's id is NULL and `= NULL` is never true — the bug would be a briefing silently missing
/// everything known about the house.
///
/// Approved rows are fetched across the whole chain. A `live` row is fetched only when its stored
/// shape is a job-scoped working finding; [`select`] and its [`admitted`] gate still decide whether
/// that row belongs to this exact job and is evidenced enough to reach the prompt.
///
/// Ordered here as well as in [`render`], so a caller that skips the renderer still gets a stable
/// list, and so the LIMIT below cuts the tail rather than an arbitrary middle.
pub async fn for_scope(pool: &SqlitePool, scope: &Scope) -> sqlx::Result<Vec<Known>> {
    let chain = scope.chain();
    let terms: Vec<&str> = chain
        .iter()
        .map(|_| "(scope_kind = ? AND scope_id IS ?)")
        .collect();
    let sql = format!(
        "SELECT {COLUMNS}
           FROM knowledge
          WHERE (status = 'active'
                 OR (status = 'live' AND layer = 'working' AND scope_kind = 'job'))
            AND ({})
          ORDER BY id
          LIMIT ?",
        terms.join(" OR ")
    );
    // `AssertSqlSafe` because sqlx 0.9 takes only `&'static str` otherwise, and the audit it asks
    // for is short: the interpolated parts are [`COLUMNS`], which is a literal, and one
    // `(scope_kind = ? AND scope_id IS ?)` per link of the chain, which is also a literal. Every
    // value the caller supplies is bound below.
    let mut query = sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(sql));
    for (kind, id) in &chain {
        query = query.bind(*kind).bind(id.clone());
    }
    query
        // A ceiling on the QUERY as well as on the rendering, because the two protect different
        // things: `RENDER_CHARS` keeps a prompt affordable, and this keeps a project that has
        // approved ten thousand rows from reading all of them into memory to render forty.
        .bind(MAX_READ as i64)
        .fetch_all(pool)
        .await
}

/// How many approved rows are read before rendering ever begins.
pub const MAX_READ: usize = 200;

/// Everything the store holds, in every status, newest first.
///
/// Not filtered to `active`, deliberately: the reviewable history IS the feature, and a screen that
/// showed only what is in force could not answer "what did it try to learn that I said no to".
///
/// Here rather than written out at the one call site it has, which is where it used to live. The
/// column list is [`COLUMNS`] and nothing else, so a reader outside this module cannot fall behind
/// the table by naming twelve of its twenty-six columns — which is precisely how the handler that
/// used to hold this query would have survived `0143` by silently returning nothing.
pub async fn all(pool: &SqlitePool) -> sqlx::Result<Vec<Known>> {
    // `AssertSqlSafe`, audited: the only interpolation is [`COLUMNS`], a literal.
    sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge ORDER BY id DESC LIMIT 500"
    )))
    .fetch_all(pool)
    .await
}

/// What somebody is asking the store to learn.
///
/// A struct and not eight positional arguments: the call site of the earlier shape was four string
/// literals in a row, where swapping `title` and `body` compiles, passes, and is found by a person
/// reading a strange prompt a week later.
pub struct Declaration<'a> {
    pub project_id: Option<&'a str>,
    pub origin_run_id: Option<i64>,
    pub kind: Kind,
    pub title: &'a str,
    pub body: &'a str,
    pub reasoning: &'a str,
    /// The row this one replaces — ended if and when THIS one is approved, never before.
    pub supersedes: Option<i64>,
}

/// The two things a declaration can say that the store must refuse.
#[derive(Debug)]
pub enum ProposeError {
    Db(sqlx::Error),
    /// `supersedes` names a row that is not there — a chain nobody could read back.
    UnknownPredecessor(i64),
    /// `supersedes` names a row in another scope. Refused because it is the one column that writes
    /// across the boundary the rest of this module exists to hold.
    ForeignPredecessor(i64),
}

impl std::fmt::Display for ProposeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposeError::Db(error) => write!(formatter, "{error}"),
            ProposeError::UnknownPredecessor(id) => {
                write!(formatter, "there is nothing known with id {id} to replace")
            }
            ProposeError::ForeignPredecessor(id) => {
                write!(
                    formatter,
                    "what is known with id {id} belongs to another scope"
                )
            }
        }
    }
}

impl std::error::Error for ProposeError {}

impl From<sqlx::Error> for ProposeError {
    fn from(error: sqlx::Error) -> Self {
        ProposeError::Db(error)
    }
}

/// A run declaring something it thinks the next run should know.
///
/// Writes the row `proposed` AND the proposal that asks about it, in one transaction — the two are
/// one act, and a crash between them would leave either a lesson nobody can approve or a question
/// about a lesson that is not there.
///
/// The row is written now rather than on approval, unlike `create_calendar_event`'s shape, and the
/// difference is deliberate: a rejected event is nothing, but a rejected LESSON is a record worth
/// keeping — it is how somebody later sees what the agent kept trying to learn and was told no to.
///
/// **The three new columns are derived here and not asked for**, by exactly the translation
/// `0143_knowledge.sql` applies to the rows that came before: the layer from the kind, the scope
/// from the project id, and the source from whether a run is behind the request. It is what keeps
/// this phase a move rather than a change — the door writes what it wrote. Deriving the scope from
/// the run instead of from the caller is a later decision, with its own reasons.
/// The fingerprint is derived from the body with the consolidator's own function, so
/// `consolidate::blocked_outcome` sees a pending, rejected, or reverted proposal.
pub async fn propose(
    pool: &SqlitePool,
    declaration: Declaration<'_>,
) -> Result<(i64, i64), ProposeError> {
    let mut tx = pool.begin().await?;
    let ids = propose_in(&mut tx, declaration).await?;
    tx.commit().await?;
    crate::embed::nudge();
    Ok(ids)
}

/// `propose`, inside a transaction the caller owns — for a caller whose own writes must land or
/// vanish together with the lesson and its question. Nothing is committed here.
pub async fn propose_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    declaration: Declaration<'_>,
) -> Result<(i64, i64), ProposeError> {
    propose_in_with(tx, declaration, &Provenance::default()).await
}

/// Where a learning came from, when somebody other than a run or the owner is writing it.
///
/// Kept beside [`Declaration`] and not inside it: a struct literal cannot gain fields without
/// editing every literal, and the callers that exist must stay as they are. `None` everywhere is
/// today's behaviour - each field overrides exactly one value the door would otherwise derive.
#[derive(Default)]
pub struct Provenance<'a> {
    /// Replaces the `run`/`owner` derivation (the distiller writes `distiller`).
    pub source: Option<&'a str>,
    /// Which cause queued the distillation this row came out of.
    pub distill_cause: Option<&'a str>,
    /// Tagged evidence, stored as given.
    pub evidence: Option<&'a str>,
    pub points_at: Option<&'a str>,
    /// Replaces the `gate:` signature derived from the body.
    pub fingerprint: Option<&'a str>,
    /// Replaces the layer derived from the kind.
    pub layer: Option<Layer>,
}

/// `propose_in` with the provenance a caller other than a run or the owner brings. Nothing is
/// committed here.
pub async fn propose_in_with(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    declaration: Declaration<'_>,
    provenance: &Provenance<'_>,
) -> Result<(i64, i64), ProposeError> {
    let Declaration {
        project_id,
        origin_run_id,
        kind,
        title,
        body,
        reasoning,
        supersedes,
    } = declaration;

    let scope = Scope::of_project(project_id);
    let (scope_kind, scope_id) = scope.columns();
    let derived_fingerprint =
        failure_signature(body).map(|signature| format!("gate:{}", signature.fingerprint));
    let fingerprint = provenance
        .fingerprint
        .map(str::to_owned)
        .or(derived_fingerprint);

    // Checked before anything is written, and checked here rather than left to the foreign key:
    // SQLite would accept a link to another scope's row without a word, and the failure would
    // surface as one repository's history quietly containing another's.
    if let Some(predecessor) = supersedes {
        let owner: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT scope_kind, scope_id FROM knowledge WHERE id = ?")
                .bind(predecessor)
                .fetch_optional(&mut **tx)
                .await?;
        let owner = owner.ok_or(ProposeError::UnknownPredecessor(predecessor))?;
        if owner.0 != scope_kind || owner.1.as_deref() != scope_id.as_deref() {
            return Err(ProposeError::ForeignPredecessor(predecessor));
        }
    }

    let now = chrono::Utc::now().to_rfc3339();

    let knowledge_id = sqlx::query(
        "INSERT INTO knowledge
           (layer, scope_kind, scope_id, source, kind, title, body, fingerprint, status, supersedes,
            origin_run_id, created_at, evidence, points_at, distill_cause)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'proposed', ?, ?, ?, ?, ?, ?)",
    )
    .bind(provenance.layer.unwrap_or_else(|| kind.layer()).as_str())
    .bind(scope_kind)
    .bind(scope_id.as_deref())
    // Never read from the body: who knocked at the door is a fact the door has and the text does
    // not.
    .bind(provenance.source.unwrap_or(if origin_run_id.is_some() {
        "run"
    } else {
        "owner"
    }))
    .bind(kind.as_str())
    .bind(title)
    .bind(body)
    .bind(fingerprint.as_deref())
    .bind(supersedes)
    .bind(origin_run_id)
    .bind(&now)
    .bind(provenance.evidence)
    .bind(provenance.points_at)
    .bind(provenance.distill_cause)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'proposed', ?, ?)",
    )
    .bind(knowledge_id)
    .bind(if provenance.source == Some("distiller") {
        "distilled from a job"
    } else if origin_run_id.is_some() {
        "declared by a run"
    } else {
        "declared by the owner"
    })
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    // `tool_input` carries the id and nothing a reader would have to join to understand the
    // question. A proposal a person cannot answer without opening another screen is a proposal that
    // waits until morning and then gets approved unread.
    //
    // **`refinement_id` keeps its name here, and `kind = 'refinement'` below keeps its value.**
    // Both are data already written to disk, in rows this migration does not rewrite — renaming
    // either would orphan every question still waiting for an answer, which is a worse thing than
    // an old word in a JSON payload.
    let tool_input = serde_json::json!({
        "refinement_id": knowledge_id,
        "kind": kind.as_str(),
        "title": title,
        "body": body,
        // Carried so the question reads as what it is. "Approve this note" and "approve this note
        // INSTEAD of the one you approved in March" are different decisions, and only one of them
        // costs you something you already have.
        "supersedes": supersedes,
    })
    .to_string();

    let proposal_id = sqlx::query(
        "INSERT INTO proposals
           (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input,
            created_at, decided_at)
         VALUES ('refinement', 'pending', ?, NULL, ?, NULL, ?, ?, ?, NULL)",
    )
    .bind(origin_run_id)
    .bind(project_id)
    .bind(reasoning)
    .bind(&tool_input)
    .bind(&now)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'pending', 'created', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    sqlx::query("UPDATE knowledge SET proposal_id = ? WHERE id = ?")
        .bind(proposal_id)
        .bind(knowledge_id)
        .execute(&mut **tx)
        .await?;

    Ok((knowledge_id, proposal_id))
}

/// How many runs a distilled episode may go unconfirmed before it expires - the consolidator's
/// trial, spec `destilador` section 3.
pub const DISTILLED_TRIAL_RUNS: i64 = 50;

/// The note of an event that renews a row without changing its status.
pub const NOTE_RECONFIRMED: &str = "reconfirmed";

/// The identity of a learning for deduplication: its title lowercased, whitespace collapsed and
/// final punctuation stripped. `None` when nothing is left to identify it by.
pub fn title_fingerprint(title: &str) -> Option<String> {
    let lowered = title.to_lowercase();
    let collapsed = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_end_matches(|c: char| ".!?:;,\u{2026}".contains(c) || c.is_whitespace())
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("title:{trimmed}"))
    }
}

/// Record an episode a job taught: believed from the start, on trial.
///
/// Active, episodic and the distiller's, expiring unless somebody reconfirms it. `observations`
/// and `generator` stay NULL - they are the consolidator's measurement and a distilled row has
/// none. Nothing is committed here.
pub async fn record_distilled(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    declaration: &Declaration<'_>,
    provenance: &Provenance<'_>,
) -> sqlx::Result<i64> {
    let scope = Scope::of_project(declaration.project_id);
    let (scope_kind, scope_id) = scope.columns();
    let now = chrono::Utc::now().to_rfc3339();

    let id = sqlx::query(
        "INSERT INTO knowledge
           (layer, scope_kind, scope_id, source, kind, title, body, fingerprint, evidence,
            points_at, distill_cause, expires_after_runs, last_confirmed_at, status,
            created_at, activated_at)
         VALUES ('episodic', ?, ?, 'distiller', ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(scope_kind)
    .bind(scope_id.as_deref())
    .bind(declaration.kind.as_str())
    .bind(declaration.title)
    .bind(declaration.body)
    .bind(provenance.fingerprint)
    .bind(provenance.evidence)
    .bind(provenance.points_at)
    .bind(provenance.distill_cause)
    .bind(DISTILLED_TRIAL_RUNS)
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'active', 'created', ?)",
    )
    .bind(id)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    Ok(id)
}

/// A learning seen again: merge the new evidence into the project's newest live row with the same
/// fingerprint, renew it when it is an active episode, and say so in a same-status event.
///
/// `None` when the project has no such row. Project-scoped on purpose - the same title in another
/// project is another learning. Nothing is committed here.
pub async fn reconfirm_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    project_id: &str,
    fingerprint: &str,
    evidence: &str,
) -> sqlx::Result<Option<i64>> {
    let row: Option<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, layer, status, evidence FROM knowledge
         WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint = ?
           AND status IN ('active', 'proposed')
         ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(fingerprint)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((id, layer, status, stored)) = row else {
        return Ok(None);
    };
    reconfirm_row_in(tx, id, &layer, &status, stored, evidence).await?;
    Ok(Some(id))
}

/// `reconfirm_in` for a row already identified by its id (a near-identical learning found by
/// similarity rather than by fingerprint). `false` when no live row has that id.
pub async fn reconfirm_id_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    evidence: &str,
) -> sqlx::Result<bool> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT layer, status, evidence FROM knowledge
         WHERE id = ? AND status IN ('active', 'proposed')",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((layer, status, stored)) = row else {
        return Ok(false);
    };
    reconfirm_row_in(tx, id, &layer, &status, stored, evidence).await?;
    Ok(true)
}

/// The note of an event that says a new learning resembles an older one without being it.
pub(crate) const NOTE_NEAR_DUPLICATE: &str = "near_duplicate:";

/// Say, in a same-status event, that `knowledge_id` is a near-duplicate of `of_id`.
pub async fn note_near_duplicate_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    knowledge_id: i64,
    of_id: i64,
) -> sqlx::Result<()> {
    let status: String = sqlx::query_scalar("SELECT status FROM knowledge WHERE id = ?")
        .bind(knowledge_id)
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(knowledge_id)
    .bind(&status)
    .bind(&status)
    .bind(format!("{NOTE_NEAR_DUPLICATE}{of_id}"))
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Merge evidence into a live row, renew it when it is an active episode, and log the event.
async fn reconfirm_row_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    layer: &str,
    status: &str,
    stored: Option<String>,
    evidence: &str,
) -> sqlx::Result<()> {
    let elements = |raw: Option<&str>| -> Vec<serde_json::Value> {
        raw.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(known_element)
            .collect()
    };
    let mut merged = elements(stored.as_deref());
    for element in elements(Some(evidence)) {
        let key = |value: &serde_json::Value| (value.get("t").cloned(), value.get("id").cloned());
        if !merged.iter().any(|kept| key(kept) == key(&element)) {
            merged.push(element);
        }
    }
    let merged = tagged_evidence(&serde_json::Value::Array(merged));

    let now = chrono::Utc::now().to_rfc3339();
    let renews = layer == "episodic" && status == "active";
    sqlx::query(
        "UPDATE knowledge SET evidence = ?,
           last_confirmed_at = CASE WHEN ? THEN ? ELSE last_confirmed_at END
         WHERE id = ?",
    )
    .bind(merged.as_deref())
    .bind(renews)
    .bind(&now)
    .bind(id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(status)
    .bind(status)
    .bind(NOTE_RECONFIRMED)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecisionError {
    NotFound,
    NotPending,
    Malformed,
}

/// Which row a pending proposal is asking about, or why it is not answerable.
///
/// Shared by both answers deliberately: a yes and a no must agree about what counts as a question,
/// or the pair drifts into a proposal that can be approved and not refused — which is exactly what
/// this layer shipped with, `proposals::reject_proposal` taking `action-approval` alone.
async fn pending_knowledge(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let row: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT kind, status, tool_input FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| DecisionError::NotFound)?;
    let (kind, status, tool_input) = row.ok_or(DecisionError::NotFound)?;
    if kind != "refinement" {
        return Err(DecisionError::NotFound);
    }
    if status != "pending" {
        return Err(DecisionError::NotPending);
    }
    tool_input
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| {
            value
                .get("refinement_id")
                .and_then(serde_json::Value::as_i64)
        })
        .ok_or(DecisionError::Malformed)
}

/// A person said yes, and only now does anything reach a prompt.
///
/// One transaction, like the calendar's: a dropped request must not leave the proposal and the
/// store disagreeing about whether the agent was allowed to learn something.
pub async fn approve(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let knowledge_id = pending_knowledge(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    // Guarded on `proposed`, so a second approval of the same row is a no-op rather than a second
    // activation stamp over the first.
    let activated = sqlx::query(
        "UPDATE knowledge SET status = 'active', activated_at = ?
          WHERE id = ? AND status = 'proposed'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;
    if activated.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'active', 'approved by the owner', ?)",
    )
    .bind(knowledge_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    // The chain moves here and nowhere else. A successor that ended its predecessor when it was
    // merely *declared* would let a question nobody answered delete the answer already in force,
    // so the old text stands until the moment somebody chooses the new one over it.
    let predecessor: Option<i64> =
        sqlx::query_scalar("SELECT supersedes FROM knowledge WHERE id = ?")
            .bind(knowledge_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| DecisionError::NotFound)?;
    if let Some(predecessor) = predecessor {
        let ended = sqlx::query(
            "UPDATE knowledge SET status = 'superseded', ended_at = ?
              WHERE id = ? AND status = 'active'",
        )
        .bind(&now)
        .bind(predecessor)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;
        // Not an error when it matches nothing: the predecessor may have been reverted while this
        // successor waited for an answer, and what the person just approved is still approved.
        if ended.rows_affected() == 1 {
            sqlx::query(
                "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                 VALUES (?, 'active', 'superseded', ?, ?)",
            )
            .bind(predecessor)
            .bind(format!("replaced by {knowledge_id}"))
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|_| DecisionError::NotFound)?;
        }
    }

    sqlx::query("UPDATE proposals SET status = 'approved', decided_at = ? WHERE id = ?")
        .bind(&now)
        .bind(proposal_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', 'approved', 'refinement activated', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    tx.commit().await.map_err(|_| DecisionError::NotFound)?;
    Ok(knowledge_id)
}

/// A person said no, and the refusal is kept.
///
/// The layer shipped without this and it was not a missing nicety: `proposals::reject_proposal`
/// answers `action-approval` alone, so a refinement proposal had one button. A queue you can only
/// say yes to is a queue where everything is eventually approved — and the thing being approved
/// here is what every later run is told.
///
/// `rejected` and not deleted, for the reason the module's own doc gives: what the agent kept
/// trying to learn and was told no to is a record worth having.
pub async fn reject(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let knowledge_id = pending_knowledge(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    let refused = sqlx::query(
        "UPDATE knowledge SET status = 'rejected', ended_at = ?
          WHERE id = ? AND status = 'proposed'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;
    if refused.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'rejected', 'refused by the owner', ?)",
    )
    .bind(knowledge_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    sqlx::query("UPDATE proposals SET status = 'rejected', decided_at = ? WHERE id = ?")
        .bind(&now)
        .bind(proposal_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', 'rejected', 'refinement refused', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    tx.commit().await.map_err(|_| DecisionError::NotFound)?;
    Ok(knowledge_id)
}

/// Taking one back, which is the half that makes approving safe to do.
///
/// `reverted` and not deleted: the history is the feature. A store somebody can only add to is one
/// nobody dares add to.
pub async fn revert(pool: &SqlitePool, knowledge_id: i64, note: &str) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE knowledge SET status = 'reverted', ended_at = ? WHERE id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'active', 'reverted', ?, ?)",
    )
    .bind(knowledge_id)
    .bind(note)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// One decision in a row's life, as a person reads it back.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Event {
    pub id: i64,
    pub from_status: Option<String>,
    pub to_status: String,
    pub note: Option<String>,
    pub at: String,
}

/// What this says, what it said before, and what replaced it — the reviewable history the whole
/// store is for, in one answer.
///
/// One call and not three, because the three are only useful together: "revert this" is a decision
/// a person makes by reading the text that would come back, and a screen that made them fetch it
/// separately is a screen where they revert without having read it.
#[derive(Debug, Serialize)]
pub struct History {
    pub known: Known,
    pub events: Vec<Event>,
    /// Newest first: what this one replaced, then what THAT replaced, back to the first text.
    pub replaced: Vec<Known>,
    /// What replaced this one, if a person has approved a successor.
    pub replaced_by: Option<Known>,
}

/// How far back a chain is read before the walk stops and says no more.
const MAX_CHAIN: usize = 50;

/// Read one row, its own decisions, and the chain on both sides of it.
pub async fn history(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<History>> {
    let Some(known) = fetch(pool, id).await? else {
        return Ok(None);
    };

    let events = sqlx::query_as::<_, Event>(
        "SELECT id, from_status, to_status, note, at
           FROM knowledge_events WHERE knowledge_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let mut replaced: Vec<Known> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::from([id]);
    let mut next = known.supersedes;
    while let Some(previous) = next {
        // No path through this module can write a cycle — a predecessor must already exist, so
        // links only ever point backwards — but this is a loop over data, and a loop over data that
        // trusts it terminates until the day the data is wrong, and then it hangs the daemon
        // holding the connection instead of returning a poor answer.
        if !seen.insert(previous) || replaced.len() >= MAX_CHAIN {
            break;
        }
        let Some(row) = fetch(pool, previous).await? else {
            break;
        };
        next = row.supersedes;
        replaced.push(row);
    }

    // The newest successor, because a chain forked by two proposals approved out of order is a
    // thing SQLite will happily store and a person should still be able to read.
    let replaced_by = sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge WHERE supersedes = ? ORDER BY id DESC LIMIT 1"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(Some(History {
        known,
        events,
        replaced,
        replaced_by,
    }))
}

async fn fetch(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<Known>> {
    sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// The complete vocabulary a run may use to point at the source of a finding.
pub const EVIDENCE_TAGS: [&str; 8] = [
    "job",
    "run",
    "job_item",
    "proposal",
    "knowledge",
    "project",
    "command",
    "gate",
];

fn known_element(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(tag) = object.get("t").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !EVIDENCE_TAGS.contains(&tag) {
        return false;
    }
    match object.get("id") {
        Some(serde_json::Value::Number(id)) => id.as_u64().is_some_and(|id| id > 0),
        Some(serde_json::Value::String(id)) => !id.is_empty() && id.chars().count() <= 120,
        _ => false,
    }
}

/// Keep only shaped, known referents and serialise them in the daemon's own JSON form.
pub fn tagged_evidence(raw: &serde_json::Value) -> Option<String> {
    struct Tagged<'a>(&'a serde_json::Map<String, serde_json::Value>);

    impl Serialize for Tagged<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            use serde::ser::SerializeMap;

            let mut clean = serializer.serialize_map(Some(self.0.len()))?;
            clean.serialize_entry("t", self.0.get("t").expect("known element has a tag"))?;
            clean.serialize_entry("id", self.0.get("id").expect("known element has an id"))?;
            for (key, value) in self.0 {
                if key != "t" && key != "id" {
                    clean.serialize_entry(key, value)?;
                }
            }
            clean.end()
        }
    }

    let kept: Vec<_> = raw
        .as_array()?
        .iter()
        .filter(|element| known_element(element))
        .filter_map(|element| {
            let object = element.as_object()?;
            Some(Tagged(object))
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    let normalised = serde_json::to_string(&kept).ok()?;
    debug_assert!(evidence_is_tagged(&normalised));
    Some(normalised)
}

/// Whether stored evidence still has at least one shaped, known referent.
pub fn evidence_is_tagged(stored: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stored)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .is_some_and(|elements| elements.iter().any(known_element))
}

/// Why a run's attempted working-layer finding was refused.
#[derive(Debug)]
pub enum FindingError {
    EmptyFact,
    FactTooLong,
    NoEvidence,
    EvidenceTooLong,
    NoJob,
    JobEnded,
    TooMany,
    Db(sqlx::Error),
}

impl std::fmt::Display for FindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingError::EmptyFact => write!(formatter, "a finding needs a fact"),
            FindingError::FactTooLong => write!(
                formatter,
                "a finding may be at most {PER_ITEM_CHARS} characters"
            ),
            FindingError::NoEvidence => write!(formatter, "a finding needs tagged evidence"),
            FindingError::EvidenceTooLong => write!(
                formatter,
                "a finding's evidence may be at most {MAX_EVIDENCE_CHARS} characters"
            ),
            FindingError::NoJob => write!(formatter, "the run does not belong to a job"),
            FindingError::JobEnded => write!(formatter, "the run's job has ended"),
            FindingError::TooMany => write!(
                formatter,
                "the job already has {MAX_LIVE_FINDINGS_PER_JOB} live findings"
            ),
            FindingError::Db(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for FindingError {}

impl From<sqlx::Error> for FindingError {
    fn from(error: sqlx::Error) -> Self {
        FindingError::Db(error)
    }
}

const MAX_LIVE_FINDINGS_PER_JOB: i64 = 20;
/// About 3.3x the 600-character fact cap, leaving room for dozens of
/// `{"t":"run","id":N}` referents whose ids are already capped at 120 characters. Measured on
/// the normalised form because that is what is stored.
pub const MAX_EVIDENCE_CHARS: usize = 2_000;

/// Leave one evidenced fact for the next node of the caller's own live job.
pub async fn note_finding(
    pool: &SqlitePool,
    run_id: i64,
    fact: &str,
    evidence: &serde_json::Value,
) -> Result<i64, FindingError> {
    if fact.trim().is_empty() {
        return Err(FindingError::EmptyFact);
    }
    if fact.chars().count() > PER_ITEM_CHARS {
        return Err(FindingError::FactTooLong);
    }
    let evidence = tagged_evidence(evidence).ok_or(FindingError::NoEvidence)?;
    if evidence.chars().count() > MAX_EVIDENCE_CHARS {
        return Err(FindingError::EvidenceTooLong);
    }

    let Some((job_id, _project_id)) = sqlx::query_as::<_, (Option<i64>, Option<String>)>(
        "SELECT job_id, project_id FROM runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?
    else {
        return Err(FindingError::NoJob);
    };
    let Some(job_id) = job_id else {
        return Err(FindingError::NoJob);
    };
    let Some(job_status) = sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?
    else {
        return Err(FindingError::JobEnded);
    };
    if crate::job::TERMINAL_STATUSES.contains(&job_status.as_str()) {
        return Err(FindingError::JobEnded);
    }

    let title = clip(fact.lines().next().unwrap_or_default(), 80);
    let now = chrono::Utc::now().to_rfc3339();
    let job_scope = job_id.to_string();
    let inserted = sqlx::query(
        "INSERT INTO knowledge
           (layer, status, scope_kind, scope_id, source, generator, evidence, observations,
            fingerprint, kind, title, body, proposal_id, origin_run_id, created_at, activated_at)
         SELECT
           'working', 'live', 'job', ?, 'run', NULL, ?, NULL,
           NULL, 'memory', ?, ?, NULL, ?, ?, NULL
          WHERE (SELECT COUNT(*) FROM knowledge
                  WHERE scope_kind = 'job' AND scope_id = ? AND status = 'live') < ?",
    )
    .bind(&job_scope)
    .bind(evidence)
    .bind(title)
    .bind(fact)
    .bind(run_id)
    .bind(now)
    .bind(&job_scope)
    .bind(MAX_LIVE_FINDINGS_PER_JOB)
    .execute(pool)
    .await?;
    if inserted.rows_affected() == 0 {
        Err(FindingError::TooMany)
    } else {
        Ok(inserted.last_insert_rowid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards that checkout location and worktree generation do not split one failure identity.
    #[test]
    fn the_same_failure_in_two_worktree_layouts_normalises_to_one_signature() {
        let legacy = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos-worktrees\nucleos\run-900372\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");
        let nested = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos\.nucleos\worktrees\run-900446\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");
        let git_bash = failure_signature(
            r#"error: failed to remove file `/c/Projects/nucleos/.nucleos/worktrees/run-900451/core/target/debug/nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");

        assert_eq!(legacy.fingerprint, nested.fingerprint);
        assert_eq!(legacy.fingerprint, git_bash.fingerprint);
    }

    /// Guards that stable normalisation preserves the facts that distinguish real failures.
    #[test]
    fn two_genuinely_different_failures_do_not_collapse_into_one_signature() {
        let access_denied = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos\.nucleos\worktrees\run-900446\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");
        let file_in_use = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos\.nucleos\worktrees\run-900446\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 32)"#,
        )
        .expect("a failure has a signature");
        let panic = failure_signature("thread 'x' panicked at src/a.rs:10:5:")
            .expect("a failure has a signature");

        assert_ne!(access_denied.fingerprint, file_in_use.fingerprint);
        assert_ne!(access_denied.fingerprint, panic.fingerprint);
        assert_ne!(file_in_use.fingerprint, panic.fingerprint);
    }

    /// Guards that volatile locations, timings, and run names do not change failure identity.
    #[test]
    fn a_failure_that_differs_only_in_line_numbers_durations_and_run_ids_is_the_same_failure() {
        let first = failure_signature(
            "thread 'a' panicked at src/a.rs:10:5: after 12.34s\nerror: branch nucleos/run-900372\nfinished in 12.34s",
        )
        .expect("a failure has a signature");
        let second = failure_signature(
            "thread 'a' panicked at src/a.rs:99:7: after 350ms\nerror: branch nucleos/run-900999\nfinished in 350ms",
        )
        .expect("a failure has a signature");

        assert_eq!(first.fingerprint, second.fingerprint);
    }

    /// Guards that silence has no identity while non-empty unclassified output keeps a fallback.
    #[test]
    fn an_output_with_nothing_in_it_has_no_signature() {
        assert!(failure_signature("").is_none());
        assert!(failure_signature("   \n\t").is_none());
        assert!(failure_signature("test result: ok. 3 passed").is_some());
    }

    /// The last version of the schema that still had `refinements` in it.
    ///
    /// Named rather than written as a literal in five places: what these tests are about is the
    /// boundary between before and after, and a bare `142` at a call site says nothing about which
    /// side of it the caller means to be on.
    const BEFORE: i64 = 142;

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
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    async fn insert_job(pool: &SqlitePool, project_id: &str, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO jobs
               (project_id, project_root, rule_name, prompt, status, max_items, gate_each, review,
                gate_retries, head_sha, max_rounds, budget_usd, created_at, team_id)
             VALUES (?, ?, NULL, 'test job', ?, 1, 1, 0, 0, NULL, NULL, NULL,
                     '2026-09-20T00:00:00+00:00', NULL)",
        )
        .bind(project_id)
        .bind(format!("/project/{project_id}"))
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_job_knowledge(
        pool: &SqlitePool,
        job_id: i64,
        layer: &str,
        status: &str,
        title: &str,
    ) -> i64 {
        sqlx::query(
            r#"INSERT INTO knowledge
                 (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
               VALUES (?, 'job', ?, 'run', '[{"t":"run","id":1}]', 'memory', ?, 'b', ?,
                       '2026-09-20T00:00:00+00:00')"#,
        )
        .bind(layer)
        .bind(job_id.to_string())
        .bind(title)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn note_finding_refuses_evidence_over_the_cap() {
        let pool = test_pool().await;
        let job_id = insert_job(&pool, "p", "running").await;
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO runs (project_id, prompt, status, job_id, created_at)
             VALUES ('p', 'test run', 'running', ?, '2026-10-01T00:00:00+00:00')
             RETURNING id",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let evidence = serde_json::Value::Array(
            (1..=200)
                .map(|id| serde_json::json!({"t": "run", "id": id}))
                .collect(),
        );

        assert!(matches!(
            note_finding(&pool, run_id, "too much evidence", &evidence).await,
            Err(FindingError::EvidenceTooLong)
        ));
        let written: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE layer = 'working'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(written, 0);

        assert!(
            note_finding(
                &pool,
                run_id,
                "small evidence",
                &serde_json::json!([{"t": "run", "id": run_id}]),
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_finding_closes_with_its_job_through_the_one_place_that_ends_a_job() {
        assert_eq!(
            crate::job::TERMINAL_STATUSES.len(),
            8,
            "a ninth ending needs a deliberate finding-lifetime decision"
        );
        let pool = test_pool().await;

        for (index, terminal) in crate::job::TERMINAL_STATUSES.iter().enumerate() {
            let job_id = insert_job(&pool, &format!("retired-{index}"), "running").await;
            let other_job_id = insert_job(&pool, &format!("running-{index}"), "running").await;
            let finding = insert_job_knowledge(
                &pool,
                job_id,
                "working",
                "live",
                &format!("finding-{index}"),
            )
            .await;
            let other = insert_job_knowledge(
                &pool,
                other_job_id,
                "working",
                "live",
                &format!("other-{index}"),
            )
            .await;
            let approved = insert_job_knowledge(
                &pool,
                job_id,
                "semantic",
                "active",
                &format!("approved-{index}"),
            )
            .await;

            crate::job::retire(&pool, job_id, terminal).await.unwrap();

            let (status, ended_at): (String, Option<String>) =
                sqlx::query_as("SELECT status, ended_at FROM knowledge WHERE id = ?")
                    .bind(finding)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                status, "closed",
                "the `{terminal}` ending left its finding live"
            );
            assert!(ended_at.is_some(), "the closed finding has no ending time");
            let event: (String, String, String) = sqlx::query_as(
                "SELECT from_status, to_status, note FROM knowledge_events WHERE knowledge_id = ?",
            )
            .bind(finding)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                event,
                ("live".into(), "closed".into(), "the job ended".into())
            );
            let event_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ?")
                    .bind(finding)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(event_count, 1, "retiring wrote more than one closing event");

            let other_status: String =
                sqlx::query_scalar("SELECT status FROM knowledge WHERE id = ?")
                    .bind(other)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(other_status, "live", "another running job lost its finding");
            let approved_row: (String, Option<String>) =
                sqlx::query_as("SELECT status, ended_at FROM knowledge WHERE id = ?")
                    .bind(approved)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(approved_row, ("active".into(), None));
        }
    }

    #[tokio::test]
    async fn an_orphaned_finding_is_closed_by_the_hourly_pass() {
        let pool = test_pool().await;
        let ended_job = insert_job(&pool, "ended", "running").await;
        sqlx::query("UPDATE jobs SET status = 'failed' WHERE id = ?")
            .bind(ended_job)
            .execute(&pool)
            .await
            .unwrap();
        let running_job = insert_job(&pool, "running", "running").await;
        let ended = insert_job_knowledge(&pool, ended_job, "working", "live", "ended").await;
        let missing = insert_job_knowledge(&pool, 9_999_999, "working", "live", "missing").await;
        let running = insert_job_knowledge(&pool, running_job, "working", "live", "running").await;

        assert_eq!(close_orphaned_working(&pool).await.unwrap(), 2);
        let statuses: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, status FROM knowledge WHERE id IN (?, ?, ?) ORDER BY id")
                .bind(ended)
                .bind(missing)
                .bind(running)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            statuses,
            vec![
                (ended, "closed".into()),
                (missing, "closed".into()),
                (running, "live".into()),
            ]
        );
        assert_eq!(close_orphaned_working(&pool).await.unwrap(), 0);
    }

    #[test]
    fn the_consolidator_has_no_path_to_the_working_layer() {
        let production = include_str!("consolidate.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the consolidator has a test boundary")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !production.contains("working"),
            "the consolidator became a second writer for the working layer"
        );
    }

    /// One row of 0088's table, written while that table still exists.
    ///
    /// The translation is a property of the migration, so the only honest way to test it is to put
    /// rows where the migration will find them — which means stopping the chain at [`BEFORE`]
    /// rather than asking `#[sqlx::test]` for a database where `refinements` is already gone.
    async fn seed_refinement(
        pool: &sqlx::SqlitePool,
        project: Option<&str>,
        kind: &str,
        status: &str,
        title: &str,
        origin_run_id: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO refinements
               (project_id, kind, title, body, status, origin_run_id, created_at)
             VALUES (?, ?, ?, 'body', ?, ?, '2026-08-19T00:00:00+00:00')",
        )
        .bind(project)
        .bind(kind)
        .bind(title)
        .bind(status)
        .bind(origin_run_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed(pool: &sqlx::SqlitePool, project: Option<&str>, status: &str, title: &str) {
        let scope = Scope::of_project(project);
        let (scope_kind, scope_id) = scope.columns();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', ?, ?, 'owner', 'memory', ?, 'body', ?,
                     '2026-08-19T00:00:00+00:00')",
        )
        .bind(scope_kind)
        .bind(scope_id)
        .bind(title)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    /// `episodic` and `working` are empty the moment the store exists, and that is the claim: 0088
    /// had nothing that could translate into either, so a row in one of them right after the
    /// migration would mean the translation invented knowledge.
    #[tokio::test]
    async fn the_measured_and_the_working_layers_are_empty_the_moment_the_store_is_created() {
        let pool = crate::testdb::pool_migrated_through(BEFORE).await;
        for kind in ["prompt", "memory", "skill", "subagent"] {
            seed_refinement(&pool, Some("mine"), kind, "active", kind, None).await;
        }
        crate::testdb::apply_migrations_after(&pool, BEFORE).await;

        let counted = |layer: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM knowledge WHERE layer = ?")
                    .bind(layer)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };

        assert_eq!(
            counted("episodic").await,
            0,
            "the translation invented a measurement nobody measured"
        );
        assert_eq!(
            counted("working").await,
            0,
            "the translation invented a job's working knowledge out of a table with no jobs in it"
        );
        // And nothing was dropped on the way: the four kinds landed in the two layers that existed.
        assert_eq!(counted("semantic").await, 1);
        assert_eq!(counted("procedural").await, 3);
    }

    /// The translation of one row, asserted rather than assumed. 0088 had no `source` column, so the
    /// criterion is the only one available: a run is the only thing that could have written a row
    /// without a person.
    #[tokio::test]
    async fn a_lesson_a_person_wrote_migrates_as_the_owners_and_one_a_run_wrote_as_a_runs() {
        let pool = crate::testdb::pool_migrated_through(BEFORE).await;
        seed_refinement(&pool, Some("mine"), "memory", "active", "by hand", None).await;
        seed_refinement(&pool, None, "prompt", "active", "by a run", Some(900_001)).await;
        crate::testdb::apply_migrations_after(&pool, BEFORE).await;

        let source = |title: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, String, Option<String>)>(
                    "SELECT source, scope_kind, scope_id FROM knowledge WHERE title = ?",
                )
                .bind(title)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };

        assert_eq!(
            source("by hand").await,
            (
                "owner".to_owned(),
                "project".to_owned(),
                Some("mine".to_owned())
            ),
            "a row with no run behind it did not migrate as the owner's"
        );
        assert_eq!(
            source("by a run").await,
            ("run".to_owned(), "machine".to_owned(), None),
            "a row a run wrote did not migrate as a run's, or lost its machine scope"
        );
    }

    /// Unknown vocabulary is an invisible row, never a counted one (D10). Written by direct SQL,
    /// which is the only writer that skips the Rust constants.
    #[tokio::test]
    async fn a_row_written_by_direct_sql_with_an_unknown_layer_reaches_no_brief() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('astrology', 'project', 'mine', 'owner', 'memory', 'mercury is retrograde',
                     'so the build is flaky', 'active', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        seed(&pool, Some("mine"), "active", "a layer this binary knows").await;

        let read = for_scope(&pool, &Scope::Project("mine".into()))
            .await
            .unwrap();
        assert_eq!(
            read.len(),
            2,
            "the read is where the filtering happens, and it is not: {read:?}"
        );

        // The row exists, is `active`, is in scope, and still reaches nothing. That is the whole of
        // what "no CHECK constraints" costs and the whole of what the Rust constants buy.
        // Rendering now groups by the reading context; the assertion remains about the unknown
        // layer, so it supplies the project context without changing what it proves.
        let block =
            render(&read, &project_context_for("mine")).expect("the known layer still renders");
        assert!(
            !block.contains("mercury is retrograde"),
            "a row this binary cannot name reached a node's prompt: {block}"
        );
        assert!(
            block.contains("a layer this binary knows"),
            "the unknown row took the known one down with it: {block}"
        );
    }

    /// The mirror carries the corpus that predates it. The assertion the spec did not ask for:
    /// without the backfill the first of the five signals is dead on every row that existed before
    /// today.
    #[tokio::test]
    async fn a_row_that_existed_before_the_mirror_is_still_findable_by_its_words() {
        let pool = crate::testdb::pool_migrated_through(BEFORE).await;
        sqlx::query(
            "INSERT INTO refinements (project_id, kind, title, body, status, created_at)
             VALUES ('mine', 'memory', 'the estuary at dawn', 'herons stand in the shallows',
                     'active', '2026-08-19T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::testdb::apply_migrations_after(&pool, BEFORE).await;

        let found: Vec<i64> =
            sqlx::query_scalar("SELECT rowid FROM knowledge_fts WHERE knowledge_fts MATCH ?")
                .bind("herons")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            found.len(),
            1,
            "the corpus that predates the mirror is invisible to it, and silently"
        );
    }

    /// And the mirror does not outlive what it indexes.
    #[tokio::test]
    async fn a_deleted_row_leaves_no_terms_behind() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (id, layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES (7, 'semantic', 'project', 'mine', 'owner', 'memory', 'the estuary at dawn',
                     'herons stand in the shallows', 'active', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let matching = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM knowledge_fts WHERE knowledge_fts MATCH ?",
                )
                .bind("herons")
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(matching().await, 1, "the insert trigger wrote no terms");

        sqlx::query("DELETE FROM knowledge WHERE id = 7")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            matching().await,
            0,
            "the index kept the terms of a row that is gone, which is the bug 0057 was written for"
        );
    }

    /// The scoping rule the migration argues for, asserted in both directions at once. A lesson
    /// about one repository's build is a lie about another's — and a lesson about the house is not
    /// hidden by that rule, which is the half a `scope_id = ?` alone would get wrong.
    #[tokio::test]
    async fn a_node_reads_its_own_projects_lessons_and_the_houses_and_no_others() {
        let pool = test_pool().await;
        seed(&pool, Some("mine"), "active", "mine-active").await;
        seed(&pool, None, "active", "house-wide").await;
        seed(&pool, Some("other"), "active", "someone-elses").await;
        seed(&pool, Some("mine"), "proposed", "mine-unapproved").await;

        let read = for_scope(&pool, &Scope::Project("mine".into()))
            .await
            .unwrap();
        let titles: Vec<&str> = read.iter().map(|r| r.title.as_str()).collect();

        assert!(
            titles.contains(&"mine-active"),
            "own project's lesson missing: {titles:?}"
        );
        assert!(
            titles.contains(&"house-wide"),
            "machine-wide lesson missing: {titles:?}"
        );
        assert!(
            !titles.contains(&"someone-elses"),
            "another project's lesson leaked in: {titles:?}"
        );
        assert!(
            !titles.contains(&"mine-unapproved"),
            "something nobody approved was read for a prompt: {titles:?}"
        );
    }

    /// A job reads all three links of the chain: `machine` -> `project` -> `job`.
    ///
    /// The project arrives beside the job id rather than being looked up, because `scope_id` is a
    /// polymorphic TEXT column with no foreign key — there is nothing for a join to follow, and a
    /// reader that guessed would be guessing which project a job belongs to.
    #[tokio::test]
    async fn a_job_reads_the_house_its_project_and_its_own() {
        let pool = test_pool().await;
        seed(&pool, None, "active", "house-wide").await;
        seed(&pool, Some("mine"), "active", "the project's").await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('working', 'job', '41', 'run', 'memory', 'this job''s own', 'body', 'active',
                     '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Another job's, which the chain must not reach: a job id is the most specific link there
        // is, and inheritance goes downwards only.
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('working', 'job', '42', 'run', 'memory', 'another job''s', 'body', 'active',
                     '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let scope = Scope::Job {
            id: 41,
            project: Some("mine".into()),
        };
        let titles: Vec<String> = for_scope(&pool, &scope)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.title)
            .collect();
        assert_eq!(
            titles,
            vec!["house-wide", "the project's", "this job's own"],
            "a job did not read its whole chain, or read past the end of it: {titles:?}"
        );
    }

    #[tokio::test]
    async fn a_live_finding_is_read_by_its_own_job_and_no_other_job() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'forty-one finding', 'body', 'live', '2026-09-20T00:00:00+00:00'),
               ('working', 'job', '42', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'forty-two finding', 'body', 'live', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let scope = Scope::Job {
            id: 41,
            project: Some("p".into()),
        };
        let rows = for_scope(&pool, &scope).await.unwrap();
        assert!(rows.iter().any(|row| row.title == "forty-one finding"));
        assert!(!rows.iter().any(|row| row.title == "forty-two finding"));

        let brief = crate::brief::of(&pool, &job_context(41), "q")
            .await
            .unwrap();
        let block = brief.block.expect("the finding reaches its own job");
        assert!(block.contains("forty-one finding"));
        assert!(!block.contains("forty-two finding"));
    }

    #[tokio::test]
    async fn a_live_row_outside_the_job_scope_is_never_fetched() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'project', 'p', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'project live row', 'body', 'live', '2026-09-20T00:00:00+00:00'),
               ('semantic', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'semantic live row', 'body', 'live', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = for_scope(
            &pool,
            &Scope::Job {
                id: 41,
                project: Some("p".into()),
            },
        )
        .await
        .unwrap();
        assert!(
            rows.is_empty(),
            "ineligible live rows were fetched: {rows:?}"
        );
    }

    #[tokio::test]
    async fn a_live_finding_is_labelled_as_said_by_a_run_and_not_approved() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'forty-one finding', 'body', 'live', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let brief = crate::brief::of(&pool, &job_context(41), "q")
            .await
            .unwrap();
        let block = brief.block.expect("the finding renders");
        assert!(block.contains("said by a run of this job, not approved"));

        let approved = select(
            &[one(1, "memory", "approved semantic", "body")],
            &project_context(),
            &Budget::default(),
        )
        .block
        .expect("the approved semantic row renders");
        assert!(!approved.contains("said by a run of this job, not approved"));
    }

    #[tokio::test]
    async fn recall_never_answers_a_live_row() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'live finding', 'the recallword is here', 'live',
                '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let recalled = crate::brief::recall(
            &pool,
            &Scope::Job {
                id: 41,
                project: Some("p".into()),
            },
            "recallword",
            None,
        )
        .await
        .unwrap();
        assert!(recalled.is_empty(), "recall answered a live row");
    }

    /// No run behind the request means the owner made it, and both the row and its first event say
    /// so — an event note claiming a run nobody started would be a history that lies.
    #[tokio::test]
    async fn an_owner_declaration_is_recorded_as_the_owners() {
        let pool = test_pool().await;
        let (knowledge_id, _) = propose(
            &pool,
            Declaration {
                project_id: None,
                origin_run_id: None,
                kind: Kind::Memory,
                title: "Owner's lesson",
                body: "Written by hand.",
                reasoning: "the owner said so",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        let source: String = sqlx::query_scalar("SELECT source FROM knowledge WHERE id = ?")
            .bind(knowledge_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(source, "owner");
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'proposed'",
        )
        .bind(knowledge_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(note, "declared by the owner");
    }

    /// `propose_in` commits nothing itself: a caller that abandons its transaction leaves neither
    /// the lesson nor the question about it behind.
    #[tokio::test]
    async fn propose_in_rolls_back_with_its_transaction() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        propose_in(
            &mut tx,
            Declaration {
                project_id: Some("mine"),
                origin_run_id: None,
                kind: Kind::Memory,
                title: "Never landed",
                body: "Rolled back.",
                reasoning: "test",
                supersedes: None,
            },
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();

        let knowledge: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&pool)
            .await
            .unwrap();
        let proposals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proposals")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((knowledge, proposals), (0, 0));
    }

    fn distilled<'a>(title: &'a str, body: &'a str, kind: Kind) -> Declaration<'a> {
        Declaration {
            project_id: Some("p"),
            origin_run_id: None,
            kind,
            title,
            body,
            reasoning: "distilled from a closed job",
            supersedes: None,
        }
    }

    /// The same sentence typed twice must be one learning: case, spacing and a final full stop are
    /// the differences a model makes from one answer to the next.
    #[test]
    fn a_title_fingerprint_ignores_case_spacing_and_final_punctuation() {
        let canonical = title_fingerprint("Run cargo fmt before the gate").unwrap();
        assert_eq!(canonical, "title:run cargo fmt before the gate");
        for variant in [
            "RUN CARGO FMT BEFORE THE GATE",
            "  Run   cargo\tfmt\nbefore  the gate  ",
            "Run cargo fmt before the gate.",
            "Run cargo fmt before the gate!?",
            "Run cargo fmt before the gate ...",
            "Run cargo fmt before the gate:",
        ] {
            assert_eq!(
                title_fingerprint(variant).as_deref(),
                Some(canonical.as_str())
            );
        }
        assert_ne!(
            title_fingerprint("Run cargo clippy before the gate").unwrap(),
            canonical
        );
        assert_eq!(title_fingerprint(""), None);
        assert_eq!(title_fingerprint("   \n\t "), None);
        assert_eq!(title_fingerprint(" ... "), None);
    }

    /// An episode a job taught is believed from the start, on trial: active, episodic, the
    /// distiller's, expiring unless somebody reconfirms it, with its evidence and its cause. It
    /// leaves the consolidator's two columns alone.
    #[tokio::test]
    async fn a_distilled_episode_is_recorded_active_on_trial() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let id = record_distilled(
            &mut tx,
            &distilled(
                "The linker lock",
                "The gate failed on a locked exe.",
                Kind::Memory,
            ),
            &Provenance {
                distill_cause: Some("gate_recovered"),
                evidence: Some(r#"[{"t":"job","id":7},{"t":"run","id":3}]"#),
                points_at: Some("core/src/main.rs"),
                fingerprint: Some("title:the linker lock"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let row = fetch(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.layer, "episodic");
        assert_eq!(row.source, "distiller");
        assert_eq!(row.status, "active");
        assert_eq!(row.kind, "memory");
        assert_eq!(row.scope_kind, "project");
        assert_eq!(row.scope_id.as_deref(), Some("p"));
        assert_eq!(row.expires_after_runs, Some(DISTILLED_TRIAL_RUNS));
        assert_eq!(DISTILLED_TRIAL_RUNS, 50);
        assert_eq!(row.fingerprint.as_deref(), Some("title:the linker lock"));
        assert_eq!(
            row.evidence.as_deref(),
            Some(r#"[{"t":"job","id":7},{"t":"run","id":3}]"#)
        );
        assert_eq!(row.points_at.as_deref(), Some("core/src/main.rs"));
        assert!(row.last_confirmed_at.is_some());
        assert!(row.activated_at.is_some());
        assert_eq!(row.observations, None);
        assert_eq!(row.generator, None);
        assert_eq!(row.proposal_id, None);
        assert_eq!(row.origin_run_id, None);
        let cause: Option<String> =
            sqlx::query_scalar("SELECT distill_cause FROM knowledge WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cause.as_deref(), Some("gate_recovered"));

        let events: Vec<(Option<String>, String, Option<String>)> = sqlx::query_as(
            "SELECT from_status, to_status, note FROM knowledge_events WHERE knowledge_id = ?",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, None);
        assert_eq!(events[0].1, "active");
        assert_eq!(events[0].2.as_deref(), Some("created"));
    }

    /// A rule a job taught is a question for a person, not a belief: it goes through the same door
    /// as any declaration and carries where it came from.
    #[tokio::test]
    async fn a_distilled_rule_is_proposed_with_its_provenance() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let (id, proposal_id) = propose_in_with(
            &mut tx,
            distilled("Format before gating", "Run fmt first.", Kind::Memory),
            &Provenance {
                source: Some("distiller"),
                distill_cause: Some("job_failed"),
                evidence: Some(r#"[{"t":"job","id":9}]"#),
                points_at: Some("scripts/gates.sh"),
                fingerprint: Some("title:format before gating"),
                layer: Some(Layer::Procedural),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let row = fetch(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "proposed");
        assert_eq!(row.source, "distiller");
        assert_eq!(row.layer, "procedural");
        assert_eq!(
            row.fingerprint.as_deref(),
            Some("title:format before gating")
        );
        assert_eq!(row.evidence.as_deref(), Some(r#"[{"t":"job","id":9}]"#));
        assert_eq!(row.points_at.as_deref(), Some("scripts/gates.sh"));
        assert_eq!(row.proposal_id, Some(proposal_id));
        assert_eq!(row.observations, None);
        assert_eq!(row.generator, None);
        let cause: Option<String> =
            sqlx::query_scalar("SELECT distill_cause FROM knowledge WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cause.as_deref(), Some("job_failed"));
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'proposed'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(note, "distilled from a job");
        let kind: String = sqlx::query_scalar("SELECT kind FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind, "refinement");
    }

    /// `propose_in` is `propose_in_with` and a default provenance: what the door wrote before this
    /// change, byte for byte, for a declaration that names no provenance.
    #[tokio::test]
    async fn a_declaration_without_provenance_writes_what_it_always_wrote() {
        let pool = test_pool().await;
        let gate_body = "error: build failed because the linker refused output.exe";
        let expected = failure_signature(gate_body)
            .map(|signature| format!("gate:{}", signature.fingerprint))
            .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let (by_owner, _) = propose_in(&mut tx, distilled("A", gate_body, Kind::Memory))
            .await
            .unwrap();
        let (plain_id, _) = propose_in(&mut tx, distilled("B", "plain words", Kind::Skill))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let owner = fetch(&pool, by_owner).await.unwrap().unwrap();
        assert_eq!(owner.source, "owner");
        assert_eq!(owner.layer, "semantic");
        assert_eq!(owner.status, "proposed");
        assert_eq!(owner.fingerprint.as_deref(), Some(expected.as_str()));
        assert_eq!(owner.evidence, None);
        assert_eq!(owner.points_at, None);
        let cause: Option<String> =
            sqlx::query_scalar("SELECT distill_cause FROM knowledge WHERE id = ?")
                .bind(by_owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cause, None);
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'proposed'",
        )
        .bind(by_owner)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(note, "declared by the owner");

        let plain = fetch(&pool, plain_id).await.unwrap().unwrap();
        let plain_expected = failure_signature("plain words")
            .map(|signature| format!("gate:{}", signature.fingerprint))
            .unwrap();
        assert_eq!(plain.fingerprint.as_deref(), Some(plain_expected.as_str()));
        assert_eq!(plain.layer, "procedural");
    }

    /// The same learning twice is one row that was seen again: an active episode is renewed, a
    /// proposed rule is not (nobody has believed it yet), and both gain the new evidence and a
    /// same-status event that says so.
    #[tokio::test]
    async fn the_same_title_is_reconfirmed_not_repeated() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let episode = record_distilled(
            &mut tx,
            &distilled("Seen twice", "An episode.", Kind::Memory),
            &Provenance {
                distill_cause: Some("job_landed"),
                evidence: Some(r#"[{"t":"job","id":7},{"t":"run","id":3}]"#),
                fingerprint: Some("title:seen twice"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (rule, _) = propose_in_with(
            &mut tx,
            distilled("A pending rule", "A rule.", Kind::Memory),
            &Provenance {
                source: Some("distiller"),
                distill_cause: Some("job_landed"),
                evidence: Some(r#"[{"t":"job","id":7}]"#),
                fingerprint: Some("title:a pending rule"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        sqlx::query(
            "UPDATE knowledge SET last_confirmed_at = '2020-01-01T00:00:00+00:00' WHERE id = ?",
        )
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let again = reconfirm_in(
            &mut tx,
            "p",
            "title:seen twice",
            r#"[{"t":"run","id":3},{"t":"run","id":4},{"t":"vibes","id":1}]"#,
        )
        .await
        .unwrap();
        let again_rule = reconfirm_in(
            &mut tx,
            "p",
            "title:a pending rule",
            r#"[{"t":"job","id":8}]"#,
        )
        .await
        .unwrap();
        let nothing = reconfirm_in(&mut tx, "p", "title:never written", "[]")
            .await
            .unwrap();
        let other_project = reconfirm_in(&mut tx, "q", "title:seen twice", "[]")
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(again, Some(episode));
        assert_eq!(again_rule, Some(rule));
        assert_eq!(nothing, None);
        assert_eq!(other_project, None);

        let renewed = fetch(&pool, episode).await.unwrap().unwrap();
        assert_ne!(
            renewed.last_confirmed_at.as_deref(),
            Some("2020-01-01T00:00:00+00:00"),
            "an active episode seen again must be renewed"
        );
        let merged: serde_json::Value =
            serde_json::from_str(renewed.evidence.as_deref().unwrap()).unwrap();
        assert_eq!(
            merged,
            serde_json::json!([
                {"t":"job","id":7},
                {"t":"run","id":3},
                {"t":"run","id":4},
            ])
        );
        let reconfirmed: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT from_status, to_status, note FROM knowledge_events
             WHERE knowledge_id = ? AND note = ?",
        )
        .bind(episode)
        .bind(NOTE_RECONFIRMED)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(reconfirmed.len(), 1);
        assert_eq!(reconfirmed[0].0, "active");
        assert_eq!(reconfirmed[0].1, "active");

        let proposed = fetch(&pool, rule).await.unwrap().unwrap();
        assert_eq!(proposed.status, "proposed");
        assert_eq!(
            proposed.last_confirmed_at, None,
            "a proposed row is not renewed"
        );
        let merged: serde_json::Value =
            serde_json::from_str(proposed.evidence.as_deref().unwrap()).unwrap();
        assert_eq!(
            merged,
            serde_json::json!([{"t":"job","id":7},{"t":"job","id":8}])
        );
        let events: Vec<(String, String)> = sqlx::query_as(
            "SELECT from_status, to_status FROM knowledge_events
             WHERE knowledge_id = ? AND note = ?",
        )
        .bind(rule)
        .bind(NOTE_RECONFIRMED)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            events,
            vec![("proposed".to_string(), "proposed".to_string())]
        );

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 2, "reconfirming must never insert");
    }

    /// A closed job is a thing a finding can point at.
    #[test]
    fn a_job_is_evidence() {
        assert!(EVIDENCE_TAGS.contains(&"job"));
        assert!(known_element(&serde_json::json!({"t": "job", "id": 7})));
        assert!(known_element(&serde_json::json!({"t": "job", "id": "7"})));
        assert!(!known_element(&serde_json::json!({"t": "job", "id": 0})));
        assert_eq!(
            tagged_evidence(&serde_json::json!([{"t": "job", "id": 7}])).as_deref(),
            Some(r#"[{"t":"job","id":7}]"#)
        );
    }

    /// Spec 4.6, in the four cases that matter: what the distiller wrote reaches a node exactly
    /// when it is `active` -- an episode on trial, or a rule a person approved -- and never while a
    /// rule is `proposed` or once an episode has expired.
    #[test]
    fn distilled_rows_reach_a_node_only_as_the_amendment_allows() {
        let context = project_context();
        let reaches = |layer: &str, status: &str, proposal_id: Option<i64>| {
            let mut row = one(1, "memory", "distilled", "learned from a job");
            row.source = "distiller".into();
            row.layer = layer.into();
            row.status = status.into();
            row.proposal_id = proposal_id;
            row.observations = None;
            row.generator = None;
            select(std::slice::from_ref(&row), &context, &Budget::default())
                .block
                .is_some()
        };

        assert!(
            reaches("episodic", "active", None),
            "an active distilled episode is the owner's D3 exception"
        );
        assert!(
            !reaches("semantic", "proposed", Some(1)),
            "a proposed distilled rule reached a node"
        );
        assert!(
            !reaches("procedural", "proposed", Some(1)),
            "a proposed distilled procedure reached a node"
        );
        assert!(
            reaches("semantic", "active", Some(1)),
            "an approved distilled rule must be read"
        );
        assert!(
            reaches("procedural", "active", Some(1)),
            "an approved distilled procedure must be read"
        );
        assert!(
            !reaches("episodic", "expired", None),
            "an expired distilled episode reached a node"
        );
    }

    /// The whole mechanism, end to end and in the order it happens: a run declares, nothing reaches
    /// a prompt, a person says yes, and only then does it. The middle assertion is the one that
    /// matters — it is what "the agent declares and the core activates" means when it is true.
    #[tokio::test]
    async fn a_declared_lesson_reaches_no_prompt_until_a_person_approves_it() {
        let pool = test_pool().await;
        let mine = Scope::Project("mine".into());
        let (knowledge_id, proposal_id) = propose(
            &pool,
            Declaration {
                project_id: Some("mine"),
                origin_run_id: Some(900_001),
                kind: Kind::Memory,
                title: "The suite needs Git's usr/bin on PATH",
                body: "Nine tests spawn echo as a program and Windows has no real echo.exe but \
                       Git's.",
                reasoning: "learned it the hard way in this run",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        assert!(
            for_scope(&pool, &mine).await.unwrap().is_empty(),
            "a lesson nobody approved was already reaching prompts"
        );

        assert_eq!(approve(&pool, proposal_id).await.unwrap(), knowledge_id);
        let after = for_scope(&pool, &mine).await.unwrap();
        assert_eq!(after.len(), 1, "approving did not activate the lesson");
        // Rendering now needs the scope chain that the store read represented; approval remains
        // the assertion under test rather than the new grouping.
        assert!(
            render(&after, &project_context_for("mine")).is_some(),
            "an active lesson renders nothing"
        );

        // The door derives what 0088 could not say, and derives it the way the migration does.
        assert_eq!(
            after[0].layer, "semantic",
            "a fact landed in the wrong layer"
        );
        assert_eq!(
            after[0].source, "run",
            "a run's lesson is not marked as one"
        );

        // Second approval is a no-op rather than a second activation stamp.
        assert_eq!(
            approve(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        );

        assert!(
            revert(&pool, knowledge_id, "made things worse")
                .await
                .unwrap()
        );
        assert!(
            for_scope(&pool, &mine).await.unwrap().is_empty(),
            "a reverted lesson still reaches prompts"
        );

        // The history is the feature: three rows, not a deleted row.
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ?")
                .bind(knowledge_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events, 3, "the row's history is not reviewable");
    }

    fn one(id: i64, kind: &str, title: &str, body: &str) -> Known {
        Known {
            id,
            layer: Kind::parse(kind).unwrap().layer().as_str().into(),
            scope_kind: "project".into(),
            scope_id: Some("p".into()),
            source: "run".into(),
            generator: None,
            evidence: None,
            observations: None,
            fingerprint: None,
            points_at: None,
            expires_after_runs: None,
            last_confirmed_at: None,
            shown_count: 0,
            outcome_count: 0,
            green_count: 0,
            last_shown_at: None,
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
            s_fts: 0.0,
            s_sim: 0.0,
            status: "active".into(),
            proposal_id: Some(1),
            supersedes: None,
            origin_run_id: Some(900_001),
            created_at: "2026-08-19T00:00:00+00:00".into(),
            activated_at: Some("2026-08-19T00:00:00+00:00".into()),
            ended_at: None,
        }
    }

    fn project_context() -> Context {
        project_context_for("p")
    }

    fn project_context_for(id: &str) -> Context {
        Context {
            chain: vec![Scope::Machine, Scope::Project(id.into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        }
    }

    fn job_context(id: i64) -> Context {
        Context {
            chain: vec![
                Scope::Machine,
                Scope::Project("p".into()),
                Scope::Job {
                    id,
                    project: Some("p".into()),
                },
            ],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        }
    }

    /// `prompt_budget.rs`, `budget.rs` and `token_efficiency.rs` exist to measure this: a briefing that
    /// changes at every node BREAKS THE PROMPT CACHE. So machine and project scope are written first —
    /// they do not change between nodes of the same job — and what the task chose comes last.
    #[test]
    fn two_nodes_of_the_same_job_read_the_same_bytes_before_the_bytes_that_are_theirs() {
        let mut machine = one(1, "memory", "house", "shared house fact");
        machine.scope_kind = "machine".into();
        machine.scope_id = None;
        let project = one(2, "memory", "project", "shared project fact");
        let mut left = one(3, "memory", "left", "left-node fact");
        left.scope_kind = "job".into();
        left.scope_id = Some("77".into());
        left.points_at = Some("core/src/left.rs".into());
        let mut right = one(4, "memory", "right", "right-node fact");
        right.scope_kind = "job".into();
        right.scope_id = Some("77".into());
        right.points_at = Some("core/src/right.rs".into());
        let known = [machine, project, left, right];
        let context = |file: &str| Context {
            chain: vec![
                Scope::Machine,
                Scope::Project("p".into()),
                Scope::Job {
                    id: 77,
                    project: Some("p".into()),
                },
            ],
            files: vec![file.into()],
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };

        let left = render(&known, &context("core/src/left.rs")).expect("left node is briefed");
        let right = render(&known, &context("core/src/right.rs")).expect("right node is briefed");
        let job_heading = "\n\nKnowledge about job 77:";
        let left_job = left
            .find(job_heading)
            .expect("left brief has its job group");
        let right_job = right
            .find(job_heading)
            .expect("right brief has its job group");

        assert_eq!(&left[..left_job], &right[..right_job]);
        assert_ne!(&left[left_job..], &right[right_job..]);
    }

    /// The layer is an item tag and not a heading, and that is what the reader needs to see: the tag is
    /// what distinguishes a MEASUREMENT from something a person approved.
    #[test]
    fn every_item_says_which_layer_it_came_from() {
        let semantic = one(1, "memory", "fact", "approved fact");
        let mut episodic = one(2, "memory", "measurement", "measured result");
        episodic.layer = Layer::Episodic.as_str().into();
        episodic.source = "consolidator".into();
        episodic.observations = Some(3);
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };

        let block = render(&[semantic, episodic], &context).expect("both items render");
        assert!(block.contains("\n- [semantic] fact: approved fact"));
        assert!(block.contains("\n- [episodic] measurement: measured result"));
        assert!(!block.contains("\n\nSemantic:"));
        assert!(!block.contains("\n\nEpisodic:"));
    }

    /// A project that has learned nothing must cost nothing. The block is appended to every node of
    /// every job, so an empty store that still wrote a heading would tax every run for ever.
    #[test]
    fn a_project_with_nothing_learned_adds_nothing_to_the_brief() {
        // Scope headings are written only for present groups, so the new grouped block must still
        // cost nothing when there is no knowledge.
        assert!(render(&[], &project_context()).is_none());
    }

    /// The failure this wording exists to prevent: a node handed a standing instruction INSTEAD of
    /// its task does the standing instruction. `notes::render` had to say the same thing, and its
    /// test asserts the item's own brief is still there beside it.
    #[test]
    fn the_block_adds_to_the_brief_rather_than_replacing_it() {
        // Scope groups replace kind headings, but the preamble still has to preserve the node's
        // own brief as the governing instruction.
        let block = render(
            &[one(
                1,
                "prompt",
                "Run fmt",
                "Always run cargo fmt before finishing.",
            )],
            &project_context(),
        )
        .expect("one active row renders");
        assert!(block.contains("Run fmt"), "the title is missing: {block}");
        assert!(
            block.contains("Always run cargo fmt before finishing."),
            "the body is missing: {block}"
        );
        assert!(
            block.to_lowercase().contains("still"),
            "nothing tells the node its own brief still stands: {block}"
        );
    }

    /// Scope groups deliberately replace the old per-kind headings: stable machine and project
    /// bytes precede the job's own bytes, whatever order the rows happened to arrive in.
    #[test]
    fn the_kinds_arrive_in_a_fixed_order_whatever_order_the_rows_do() {
        let mut job = one(4, "subagent", "job item", "s");
        job.scope_kind = "job".into();
        job.scope_id = Some("77".into());
        let project = one(3, "skill", "project item", "k");
        let mut machine = one(2, "memory", "machine item", "m");
        machine.scope_kind = "machine".into();
        machine.scope_id = None;
        let block = render(&[job, project, machine], &job_context(77)).expect("renders");
        let at = |needle: &str| {
            block
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        assert!(
            at("machine item") < at("project item"),
            "machine scope must come before project scope: {block}"
        );
        assert!(
            at("project item") < at("job item"),
            "project scope must come before job scope: {block}"
        );
        assert!(
            !block.contains("Standing instructions:"),
            "the removed kind heading came back: {block}"
        );
    }

    /// D6 is this test and nothing else. Without the `id` tail, two rows of the same scope with the same
    /// files and rank 0 tie, two distributions become indistinguishable, and "deterministic" is a word.
    /// `render`'s `sort_by_key` carries the reason in the house's own words: "what a node reads first
    /// is a property of the store, never of the order rows happened to come back from SQLite."
    #[test]
    fn two_candidates_that_tie_on_every_signal_are_still_ordered_the_same_way_every_time() {
        let mut higher_score = one(90, "subagent", "higher-score", "s");
        higher_score.layer = Layer::Working.as_str().into();
        higher_score.s_fts = 1.0;

        let tied_second = one(20, "memory", "tied-second", "m2");
        let tied_first = one(10, "memory", "tied-first", "m1");
        let prompt = one(80, "prompt", "prompt", "p");
        let skill = one(70, "skill", "skill", "k");

        // The tied pair shares project scope `p`, this one selection context (and therefore its files),
        // and rank 0.0. The other rows make each earlier key observable before the id tail is asserted.
        let first = vec![
            higher_score.clone(),
            tied_second.clone(),
            prompt.clone(),
            tied_first.clone(),
            skill.clone(),
        ];
        let second = vec![skill, tied_first, prompt, tied_second, higher_score];
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let ids = |rows: &[Known]| {
            ordered_candidates(rows, &context)
                .into_iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };

        let first_ids = ids(&first);
        let second_ids = ids(&second);
        assert_eq!(first_ids.as_bytes(), second_ids.as_bytes());
        assert_eq!(first_ids, "90,10,20,80,70");
    }

    #[test]
    fn the_order_is_score_then_layer_then_kind_then_id() {
        let mut highest_score = one(50, "subagent", "highest score", "working");
        highest_score.layer = Layer::Working.as_str().into();
        highest_score.s_fts = 0.5;
        let memory = one(40, "memory", "memory", "semantic");
        let prompt = one(30, "prompt", "prompt", "procedural");
        let later_skill = one(20, "skill", "later skill", "procedural");
        let earlier_skill = one(10, "skill", "earlier skill", "procedural");
        let rows = vec![highest_score, memory, prompt, later_skill, earlier_skill];
        let ids = |rows: &[Known]| {
            ordered_candidates(rows, &project_context())
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(ids(&rows), vec![50, 40, 30, 10, 20]);
        let reversed = rows.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(ids(&reversed), vec![50, 40, 30, 10, 20]);
    }

    #[test]
    fn every_signal_and_the_score_are_on_the_unit_scale() {
        let context = Context {
            chain: vec![
                Scope::Machine,
                Scope::Project("p".into()),
                Scope::Job {
                    id: 77,
                    project: Some("p".into()),
                },
            ],
            files: vec!["a.rs".into(), "b.rs".into()],
            communities: vec!["core".into()],
            node: None,
            gate: None,
            query_embedded: false,
        };
        let mut r1 = one(1, "memory", "best", "job");
        r1.scope_kind = "job".into();
        r1.scope_id = Some("77".into());
        r1.points_at = Some(r#"["a.rs","b.rs","core"]"#.into());
        r1.s_fts = 7.0;
        r1.shown_count = 4;
        r1.outcome_count = 4;
        r1.green_count = 3;
        r1.last_shown_at = Some("2026-09-21T00:00:00+00:00".into());
        let mut r2 = one(2, "memory", "failed", "machine");
        r2.scope_kind = "machine".into();
        r2.scope_id = None;
        r2.s_fts = -2.0;
        r2.shown_count = 2;
        r2.outcome_count = 2;
        r2.last_shown_at = Some("2026-09-01T00:00:00+00:00".into());
        let mut r3 = one(3, "memory", "unknown", "project");
        r3.s_fts = f64::NAN;
        let rows = vec![r1, r2, r3];

        let scored = scored_candidates(&rows, &context);
        for (row, candidate) in &scored {
            for (name, value) in [
                ("s_fts", candidate.s_fts),
                ("s_scope", candidate.s_scope),
                ("s_structure", candidate.s_structure),
                ("s_recency", candidate.s_recency),
                ("s_use", candidate.s_use),
                ("score", candidate.score),
            ] {
                assert!(
                    (0.0..=1.0).contains(&value),
                    "row {} has {name} outside the unit scale: {value}",
                    row.id
                );
            }
        }
        let fts = |id| {
            scored
                .iter()
                .find(|(row, _)| row.id == id)
                .map(|(_, candidate)| candidate.s_fts)
                .expect("row is scored")
        };
        assert_eq!(fts(1), 1.0);
        assert_eq!(fts(2), 0.0);
        assert_eq!(fts(3), 0.0);
        assert_eq!(ordered_candidates(&rows, &context)[0].id, 1);

        let expected = scored
            .iter()
            .find(|(row, _)| row.id == 1)
            .map(|(_, candidate)| candidate.score)
            .expect("row 1 is scored");
        let traced = select(&rows, &context, &Budget::default())
            .trace
            .into_iter()
            .find(|candidate| candidate.knowledge_id == 1)
            .map(|candidate| candidate.score)
            .expect("row 1 reaches the trace");
        assert_eq!(traced, expected);
    }

    /// Spec 5.2: the six weights (with similarity) and today's five (without) are two tables, and each
    /// must sum to 1.0 so a score stays on the unit scale. Without similarity the FTS weight absorbs
    /// the similarity share, so every score a briefing without a query vector produced stays as it was.
    #[test]
    fn both_weight_tables_sum_to_one_and_the_absent_one_is_todays_five() {
        let sum = |w: &Weights| w.fts + w.sim + w.structure + w.use_ + w.scope + w.recency;
        assert!(
            (sum(&WITH_SIM) - 1.0).abs() < 1e-9,
            "WITH_SIM does not sum to 1.0"
        );
        assert!(
            (sum(&WITHOUT_SIM) - 1.0).abs() < 1e-9,
            "WITHOUT_SIM does not sum to 1.0"
        );

        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(WITHOUT_SIM.fts, 0.35));
        assert!(near(WITHOUT_SIM.sim, 0.0));
        assert!(near(WITHOUT_SIM.structure, 0.20));
        assert!(near(WITHOUT_SIM.use_, 0.20));
        assert!(near(WITHOUT_SIM.scope, 0.15));
        assert!(near(WITHOUT_SIM.recency, 0.10));

        assert!(near(WITH_SIM.fts, 0.20));
        assert!(near(WITH_SIM.sim, 0.15));
        assert!(near(WITH_SIM.structure, 0.20));
        assert!(near(WITH_SIM.use_, 0.20));
        assert!(near(WITH_SIM.scope, 0.15));
        assert!(near(WITH_SIM.recency, 0.10));

        // The guarantee the use weight documents must hold in both tables.
        for (name, w) in [("WITH_SIM", &WITH_SIM), ("WITHOUT_SIM", &WITHOUT_SIM)] {
            assert!(
                w.use_ * NEUTRAL_UTILITY >= w.recency,
                "{name}: a never-shown row could score below a failed row"
            );
        }
    }

    /// When the briefing has a query vector, the row closer to it wins, and the trace says by how much.
    #[test]
    fn with_a_query_vector_the_row_closer_to_it_scores_higher_and_the_trace_carries_both() {
        let mut far = one(1, "memory", "far", "same body");
        far.s_sim = 0.1;
        let mut near = one(2, "memory", "near", "same body");
        near.s_sim = 0.9;
        let rows = vec![far, near];
        let mut context = project_context();
        context.query_embedded = true;

        let brief = select(&rows, &context, &Budget::default());
        let by_id = |id: i64| {
            brief
                .trace
                .iter()
                .find(|row| row.knowledge_id == id)
                .expect("row reaches the trace")
        };
        assert_eq!(by_id(1).s_sim, 0.1);
        assert_eq!(by_id(2).s_sim, 0.9);
        assert!(
            by_id(2).score > by_id(1).score,
            "the closer row does not score higher: {} vs {}",
            by_id(2).score,
            by_id(1).score
        );
        let block = brief.block.expect("both rows are shown");
        assert!(
            block.find("near").expect("near is shown") < block.find("far").expect("far is shown"),
            "the closer row is not shown first: {block}"
        );
    }

    /// Absence is per briefing, not per row (spec 5.2): without a query vector the similarity column
    /// weighs nothing, so rows that differ only in it score alike.
    #[test]
    fn without_a_query_vector_similarity_weighs_nothing() {
        let mut far = one(1, "memory", "far", "same body");
        far.s_sim = 0.1;
        let mut near = one(2, "memory", "near", "same body");
        near.s_sim = 0.9;
        let rows = vec![far, near];
        let mut context = project_context();
        context.query_embedded = false;

        let brief = select(&rows, &context, &Budget::default());
        let score = |id: i64| {
            brief
                .trace
                .iter()
                .find(|row| row.knowledge_id == id)
                .map(|row| row.score)
                .expect("row reaches the trace")
        };
        assert_eq!(
            score(1),
            score(2),
            "similarity leaked into a briefing without a query vector"
        );
    }

    /// A new row does not compete from the bottom as if it had failed: the selection treats absence as
    /// absence, and what that is worth as a NUMBER is decided here and tested in a table. Without this,
    /// whoever implements it picks a value by taste and D6's table tests that taste.
    ///
    /// The utility component of a row with no outcome is the MEDIAN of the candidates that have one,
    /// floored at the neutral constant, and the neutral constant when none of them does.
    #[test]
    fn a_row_nobody_has_measured_scores_like_the_middle_of_the_ones_somebody_has() {
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let cases = [
            ("odd", vec![(1, 5), (3, 5), (5, 5)], 0.6),
            ("even", vec![(1, 5), (4, 5)], 0.5),
            ("single", vec![(3, 4)], 0.75),
            ("mostly failed", vec![(0, 5), (0, 5), (5, 5)], 0.5),
            ("low", vec![(1, 5), (1, 5)], 0.5),
            ("none", Vec::new(), NEUTRAL_UTILITY),
        ];

        for (name, measured, expected) in cases {
            let mut rows = vec![one(100, "memory", "not measured", "new")];
            for (offset, (green_count, outcome_count)) in measured.into_iter().enumerate() {
                let mut row = one(offset as i64 + 1, "memory", "measured", "old");
                row.green_count = green_count;
                row.outcome_count = outcome_count;
                rows.push(row);
            }

            let actual = scored_candidates(&rows, &context)
                .into_iter()
                .find(|(row, _)| row.id == 100)
                .map(|(_, scored)| scored.s_use)
                .expect("the unmeasured row is scored");
            assert!(
                (actual - expected).abs() < f64::EPSILON,
                "{name}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn a_never_shown_row_never_scores_below_a_recent_failure() {
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let mut recent_failure = one(1, "memory", "recent failure", "bad");
        recent_failure.shown_count = 4;
        recent_failure.outcome_count = 4;
        recent_failure.last_shown_at = Some("2026-09-20T00:00:00+00:00".into());
        let mut old_failure = one(2, "memory", "old failure", "bad");
        old_failure.shown_count = 4;
        old_failure.outcome_count = 4;
        old_failure.last_shown_at = Some("2026-09-01T00:00:00+00:00".into());
        let mut old_green = one(3, "memory", "old green", "good");
        old_green.shown_count = 4;
        old_green.outcome_count = 4;
        old_green.green_count = 4;
        old_green.last_shown_at = Some("2026-09-01T00:00:00+00:00".into());
        let never_shown = one(4, "memory", "never shown", "new");
        let rows = vec![recent_failure, old_failure, old_green, never_shown];

        let scored = scored_candidates(&rows, &context);
        let score = |id| {
            scored
                .iter()
                .find(|(row, _)| row.id == id)
                .map(|(_, scored)| scored)
                .expect("row is scored")
        };
        assert!(
            score(4).score >= score(1).score,
            "a never-shown row must not score below a recent measured failure"
        );
        assert_eq!(score(4).s_use, NEUTRAL_UTILITY);
        assert_eq!(score(4).s_recency, 0.0);
    }

    /// This is the realistic fixture: every signal except utility is equal, because the pass that
    /// records an outcome also records the show at the same instant. The never-shown row's recency
    /// is absence, not zero, so it must not order like an old measured failure.
    #[test]
    fn not_measured_does_not_order_like_measured_and_failed() {
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let not_measured = one(1, "memory", "not measured", "new");
        let mut failed = one(2, "memory", "failed", "bad");
        failed.shown_count = 4;
        failed.outcome_count = 4;
        failed.last_shown_at = Some("2026-09-20T00:00:00+00:00".into());
        let mut green = one(3, "memory", "green", "good");
        green.shown_count = 4;
        green.outcome_count = 4;
        green.green_count = 4;
        green.last_shown_at = Some("2026-09-20T00:00:00+00:00".into());
        let rows = vec![failed, not_measured, green];

        let scored = scored_candidates(&rows, &context);
        let utility = |id| {
            scored
                .iter()
                .find(|(row, _)| row.id == id)
                .map(|(_, scored)| scored.s_use)
                .expect("row is scored")
        };
        assert_ne!(utility(1), utility(2));
        assert_eq!(
            ordered_candidates(&rows, &context)
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![3, 1, 2]
        );
    }

    /// A store that grows for a year would take the context the work needs, and the failure mode is
    /// silent: a prompt does not get slower, it gets emptier of room. So it is bounded, and it says
    /// what it left out rather than trimming in silence.
    #[test]
    fn the_block_is_bounded_and_says_what_it_left_out() {
        let many: Vec<Known> = (1..=60)
            .map(|i| one(i, "memory", &format!("fact {i}"), &"x".repeat(300)))
            .collect();
        // The new selector accounts for each scope heading and its own cut notice, so the old loose
        // byte bound becomes the exact character ceiling the grouped block promises.
        let block = render(&many, &project_context()).expect("renders");
        assert!(
            block.chars().count() <= RENDER_CHARS,
            "unbounded: {} chars from {} rows",
            block.chars().count(),
            many.len()
        );
        assert!(
            block.contains("not shown") || block.contains("more"),
            "trimmed in silence, which is the one way this may not fail: {block}"
        );
    }

    /// The test `the_block_is_bounded_and_says_what_it_left_out` cannot write today — it only
    /// asserts `<= RENDER_CHARS * 2`, because the cut notice is appended AFTER the loop. Two passes
    /// make the real ceiling assertable.
    #[test]
    fn the_block_never_passes_the_ceiling_however_much_is_on_offer() {
        let many: Vec<Known> = (1..=60)
            .map(|i| one(i, "memory", &format!("fact {i}"), &"x".repeat(1_000)))
            .collect();
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let budget = Budget {
            render_chars: RENDER_CHARS,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&many, &context, &budget);
        let block = brief.block.expect("some of sixty active rows render");
        assert!(
            block.chars().count() <= RENDER_CHARS,
            "the block passed its ceiling: {} > {RENDER_CHARS}",
            block.chars().count()
        );
        assert!(block.contains("not shown"), "the cut was silent: {block}");
    }

    /// A floor reserved for a layer with no candidate is dead budget, and two layers are born empty.
    #[test]
    fn a_layer_with_nothing_in_it_reserves_no_floor() {
        let mut first = one(1, "memory", "first", &"a".repeat(301));
        first.s_fts = 0.9;
        let mut second = one(2, "memory", "second", &"b".repeat(301));
        second.s_fts = 0.6;
        let mut third = one(3, "memory", "third", "small scored remainder");
        third.s_fts = 0.3;
        let known = [first, second, third];
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let budget = Budget {
            render_chars: 1_100,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&known, &context, &budget);
        assert_eq!(
            brief.trace.iter().filter(|row| row.shown).count(),
            3,
            "empty episodic, procedural, and working layers consumed room: {:?}",
            brief
                .trace
                .iter()
                .map(|row| (row.knowledge_id, row.shown))
                .collect::<Vec<_>>()
        );
    }

    /// Structural first: the current preamble, one heading per PRESENT SCOPE GROUP, and one cut
    /// notice per group that was actually cut. Groups of scope and not of layer — a block ordered
    /// by scope interleaves layers, so a per-layer heading has nowhere to sit.
    #[test]
    fn the_headings_and_the_notices_come_off_the_top_before_any_floor_is_reserved() {
        // The preamble no longer claims every item was approved because §4.5 names two exceptions;
        // the structural reservation still measures those exact bytes before choosing any item.
        let preamble = PREAMBLE;
        assert!(preamble.contains("two named admission rules"));
        let machine_heading = "\n\nHouse-wide knowledge:";
        let project_heading = "\n\nKnowledge about project p:";
        let notice = "\n\n(1 further admitted note in this scope is not shown here, to leave room for the work.)";
        let structural_chars = [preamble, machine_heading, project_heading, notice, notice]
            .into_iter()
            .map(|part| part.chars().count())
            .sum();

        let mut machine = one(1, "memory", "machine item", &"m".repeat(301));
        machine.scope_kind = "machine".into();
        machine.scope_id = None;
        let project = one(2, "memory", "project item", &"p".repeat(301));
        let known = [machine, project];
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let budget = Budget {
            render_chars: structural_chars,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&known, &context, &budget);
        let block = brief
            .block
            .expect("present groups still render their structure");
        assert_eq!(block.chars().count(), structural_chars);
        assert!(block.contains(machine_heading));
        assert!(block.contains(project_heading));
        assert_eq!(block.matches(notice).count(), 2);
        assert!(!block.contains("machine item"));
        assert!(!block.contains("project item"));
        assert!(brief.trace.iter().all(|row| !row.shown));
    }

    /// The two clips are different numbers and the test says which is which.
    #[test]
    fn a_floor_item_is_cut_at_three_hundred_and_one_that_won_on_score_at_six_hundred() {
        let mut floor = one(1, "memory", "floor", &"f".repeat(301));
        floor.s_fts = 0.9;
        let mut other_floor = one(2, "memory", "other floor", "short");
        other_floor.s_fts = 0.6;
        let mut scored = one(3, "memory", "score", &"s".repeat(601));
        scored.s_fts = 0.3;
        let known = [floor, other_floor, scored];
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
        };
        let budget = Budget {
            render_chars: RENDER_CHARS,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&known, &context, &budget);
        let block = brief.block.expect("all three rows fit");
        assert!(
            block.contains(&format!("{}…", "f".repeat(FLOOR_ITEM_CHARS))),
            "the floor item was not clipped at {FLOOR_ITEM_CHARS}: {block}"
        );
        assert!(
            block.contains(&format!("{}…", "s".repeat(PER_ITEM_CHARS))),
            "the score winner was not clipped at {PER_ITEM_CHARS}: {block}"
        );
        assert_eq!(brief.trace.iter().filter(|row| row.shown).count(), 3);
    }

    /// The admission rule is asserted on `select`, the seam every caller passes through. Active
    /// rows still represent approval. The only rows that may bypass it are (a) an `active`
    /// consolidator `episodic` row with observations and (b) a `live` run `working` row with
    /// well-tagged evidence, read only inside the job that wrote it. Enumerating the surrounding
    /// status/source/layer space makes a third exception fail here rather than reach a prompt.
    #[test]
    fn nothing_that_a_person_has_not_approved_reaches_a_node() {
        let context = job_context(77);
        let statuses = ["active", "proposed", "live", "rejected", "reverted"];
        let sources = ["owner", "run", "consolidator", "distiller"];
        let layers = ["semantic", "episodic", "procedural", "working"];
        let mut id = 0;

        for status in statuses {
            for source in sources {
                for layer in layers {
                    id += 1;
                    let title = format!("{status}/{source}/{layer}");
                    let mut row = one(id, "memory", &title, "candidate");
                    row.status = status.into();
                    row.source = source.into();
                    row.layer = layer.into();
                    row.proposal_id =
                        (status == "active" && source != "consolidator").then_some(id);
                    if source == "consolidator" {
                        row.observations = Some(1);
                    }
                    if status == "live" {
                        row.scope_kind = "job".into();
                        row.scope_id = Some("77".into());
                        row.evidence = Some(r#"[{"t":"run","id":900001}]"#.into());
                    }

                    let measured_exception = status == "active"
                        && source == "consolidator"
                        && layer == "episodic"
                        && row.observations.is_some();
                    let working_exception = status == "live"
                        && source == "run"
                        && layer == "working"
                        && row.evidence.as_deref().is_some_and(evidence_is_tagged);
                    let approved = status == "active" && source != "consolidator";
                    assert_eq!(
                        select(std::slice::from_ref(&row), &context, &Budget::default(),)
                            .block
                            .is_some(),
                        approved || measured_exception || working_exception,
                        "the admission rule was wrong for {title}"
                    );
                    assert_eq!(
                        select(std::slice::from_ref(&row), &context, &Budget::default(),)
                            .trace
                            .iter()
                            .any(|scored| scored.knowledge_id == id),
                        approved || measured_exception || working_exception,
                        "a row the admission rule refuses was scored as a candidate for {title}"
                    );
                }
            }
        }

        let mut measurement_without_observations = one(100, "memory", "unmeasured", "body");
        measurement_without_observations.source = "consolidator".into();
        measurement_without_observations.layer = "episodic".into();
        measurement_without_observations.proposal_id = None;
        measurement_without_observations.observations = None;

        let mut working_without_evidence = one(101, "memory", "unsupported", "body");
        working_without_evidence.status = "live".into();
        working_without_evidence.source = "run".into();
        working_without_evidence.layer = "working".into();
        working_without_evidence.scope_kind = "job".into();
        working_without_evidence.scope_id = Some("77".into());
        working_without_evidence.evidence = Some("   ".into());

        let mut another_jobs_working_fact = working_without_evidence.clone();
        another_jobs_working_fact.id = 102;
        another_jobs_working_fact.title = "another job's".into();
        another_jobs_working_fact.scope_id = Some("78".into());
        another_jobs_working_fact.evidence = Some(r#"[{"t":"run","id":900002}]"#.into());

        let mut working_with_unknown_tag = working_without_evidence.clone();
        working_with_unknown_tag.id = 103;
        working_with_unknown_tag.title = "unknown evidence tag".into();
        working_with_unknown_tag.evidence = Some(r#"[{"t":"vibes","id":1}]"#.into());

        let mut working_with_untagged_evidence = working_without_evidence.clone();
        working_with_untagged_evidence.id = 104;
        working_with_untagged_evidence.title = "untagged evidence".into();
        working_with_untagged_evidence.evidence = Some("run:900001".into());

        assert!(
            select(
                &[
                    measurement_without_observations,
                    working_without_evidence,
                    another_jobs_working_fact,
                    working_with_unknown_tag,
                    working_with_untagged_evidence,
                ],
                &context,
                &Budget::default(),
            )
            .block
            .is_none(),
            "an incomplete or cross-job exception reached a node's prompt"
        );
    }

    /// Nothing a person has not approved reaches a node, except (a) an `active` consolidator
    /// `episodic` row with measured observations (including a merge successor), and (b) a `live`
    /// run `working` row with well-tagged evidence read only by the job that wrote it.
    ///
    /// This test FAILS if a third appears: an `active` row with no `proposal_id` that is not
    /// consolidator-episodic-measured, a `live` row reaching a run of another job, or a `live` row
    /// whose `evidence` is empty or badly tagged. This door checks only that evidence exists and is
    /// tagged; it does not validate that the sentence is true.
    #[tokio::test]
    async fn nothing_a_person_has_not_approved_reaches_a_node_but_the_two_named_exceptions() {
        let pool = test_pool().await;

        seed(&pool, Some("p"), "active", "owner approved").await;
        let owner_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'owner approved'")
                .fetch_one(&pool)
                .await
                .unwrap();

        let (approved_id, approved_proposal) = propose(
            &pool,
            Declaration {
                project_id: Some("p"),
                origin_run_id: Some(900_001),
                kind: Kind::Prompt,
                title: "run lesson approved",
                body: "approved body",
                reasoning: "the run declared it",
                supersedes: None,
            },
        )
        .await
        .unwrap();
        approve(&pool, approved_proposal).await.unwrap();
        let (proposed_id, _) = propose(
            &pool,
            Declaration {
                project_id: Some("p"),
                origin_run_id: Some(900_002),
                kind: Kind::Prompt,
                title: "run lesson proposed",
                body: "not approved",
                reasoning: "still waiting",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        // There is no public one-row consolidator writer: its production writer is per-pass
        // machinery. These rows therefore reproduce that writer's persisted columns directly.
        sqlx::query(
            r#"INSERT INTO knowledge
                 (layer, scope_kind, scope_id, source, generator, evidence, observations, kind,
                  title, body, status, proposal_id, supersedes, created_at)
               VALUES
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 4, 'memory', 'measured episode', 'measured',
                  'active', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', NULL, 'memory', 'unmeasured episode', 'unmeasured',
                  'active', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('semantic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 4, 'memory', 'semantic consolidation', 'semantic',
                  'active', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 4, 'memory', 'proposed measurement', 'proposed',
                  'proposed', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 8, 'memory', 'measured merge successor', 'merged',
                  'active', NULL, 4, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', NULL, 'memory', 'unmeasured merge successor',
                  'not measured', 'active', NULL, 4, '2026-10-01T00:00:00+00:00')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let measured_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'measured episode'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let unmeasured_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'unmeasured episode'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let semantic_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'semantic consolidation'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let proposed_measurement_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'proposed measurement'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let measured_successor_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'measured merge successor'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let unmeasured_successor_id: i64 = sqlx::query_scalar(
            "SELECT id FROM knowledge WHERE title = 'unmeasured merge successor'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        for job_id in [77_i64, 78] {
            sqlx::query(
                "INSERT INTO jobs
                   (id, project_id, project_root, rule_name, prompt, status, max_items, gate_each,
                    review, gate_retries, head_sha, max_rounds, budget_usd, created_at, team_id)
                 VALUES (?, 'p', '/project/p', NULL, 'test job', 'running', 1, 1, 0, 0, NULL,
                         NULL, NULL, '2026-10-01T00:00:00+00:00', NULL)",
            )
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO runs (id, project_id, prompt, status, job_id, created_at)
                 VALUES (?, 'p', 'test run', 'running', ?, '2026-10-01T00:00:00+00:00')",
            )
            .bind(9_000 + job_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        }
        let finding_77_id = note_finding(
            &pool,
            9_077,
            "finding for job 77",
            &serde_json::json!([{"t": "run", "id": 9_077}]),
        )
        .await
        .unwrap();
        let finding_78_id = note_finding(
            &pool,
            9_078,
            "finding for job 78",
            &serde_json::json!([{"t": "run", "id": 9_078}]),
        )
        .await
        .unwrap();

        sqlx::query(
            r#"INSERT INTO knowledge
                 (layer, scope_kind, scope_id, source, evidence, kind, title, body, status,
                  created_at)
               VALUES
                 ('working', 'job', '77', 'run', '[{"t":"vibes","id":1}]', 'memory',
                  'unknown tag', 'bad tag', 'live', '2026-10-01T00:00:00+00:00'),
                 ('working', 'job', '77', 'run', 'run:1', 'memory',
                  'untagged evidence', 'not JSON', 'live', '2026-10-01T00:00:00+00:00'),
                 ('working', 'job', '77', 'run', '[]', 'memory',
                  'empty evidence', 'empty array', 'live', '2026-10-01T00:00:00+00:00'),
                 ('working', 'project', 'p', 'run', '[{"t":"run","id":1}]', 'memory',
                  'project live row', 'wrong scope', 'live', '2026-10-01T00:00:00+00:00')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let bad_live_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM knowledge WHERE title IN
             ('unknown tag', 'untagged evidence', 'empty evidence', 'project live row')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        let brief_77 = crate::brief::of(&pool, &job_context(77), "query words")
            .await
            .unwrap();
        let traced_77: std::collections::BTreeSet<i64> =
            brief_77.trace.iter().map(|row| row.knowledge_id).collect();
        let admitted_77 = std::collections::BTreeSet::from([
            owner_id,
            approved_id,
            measured_id,
            measured_successor_id,
            finding_77_id,
        ]);
        assert_eq!(
            traced_77, admitted_77,
            "a third exception entered the trace"
        );
        assert!(
            brief_77.trace.iter().all(|row| row.shown),
            "the admitted rows did not all reach the block"
        );
        let block_77 = brief_77.block.expect("the admitted set builds a briefing");
        for admitted_title in [
            "owner approved",
            "run lesson approved",
            "measured episode",
            "measured merge successor",
            "finding for job 77",
        ] {
            assert!(
                block_77.contains(admitted_title),
                "the block omitted admitted row {admitted_title}"
            );
        }
        for refused_title in [
            "run lesson proposed",
            "unmeasured episode",
            "semantic consolidation",
            "proposed measurement",
            "unmeasured merge successor",
            "finding for job 78",
            "unknown tag",
            "untagged evidence",
            "empty evidence",
            "project live row",
        ] {
            assert!(
                !block_77.contains(refused_title),
                "the block included refused row {refused_title}"
            );
        }
        for refused_id in [
            proposed_id,
            unmeasured_id,
            semantic_id,
            proposed_measurement_id,
            unmeasured_successor_id,
            finding_78_id,
        ]
        .into_iter()
        .chain(bad_live_ids.iter().copied())
        {
            assert!(
                !brief_77
                    .trace
                    .iter()
                    .any(|row| row.knowledge_id == refused_id),
                "refused row {refused_id} entered job 77's trace"
            );
        }

        let brief_78 = crate::brief::of(&pool, &job_context(78), "query words")
            .await
            .unwrap();
        assert!(
            brief_78
                .trace
                .iter()
                .any(|row| row.knowledge_id == finding_78_id),
            "job 78 did not receive its own finding"
        );
        assert!(
            !brief_78
                .trace
                .iter()
                .any(|row| row.knowledge_id == finding_77_id),
            "job 78 received job 77's finding"
        );

        let unapproved_active_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
              WHERE source = 'run' AND status = 'active' AND proposal_id IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unapproved_active_runs, 0);
        let wrongly_shaped_live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
              WHERE status = 'live'
                AND NOT (layer = 'working' AND scope_kind = 'job' AND source = 'run')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            wrongly_shaped_live, 1,
            "a writer produced a stray live row beyond the deliberate project-scope fixture"
        );
    }

    /// A short way to say "somebody declared this", so the tests below read as the sequence they
    /// assert rather than as seven fields of noise repeated four times.
    async fn declare(
        pool: &sqlx::SqlitePool,
        project: Option<&str>,
        title: &str,
        replaces: Option<i64>,
    ) -> Result<(i64, i64), ProposeError> {
        propose(
            pool,
            Declaration {
                project_id: project,
                origin_run_id: None,
                kind: Kind::Prompt,
                title,
                body: "body",
                reasoning: "because",
                supersedes: replaces,
            },
        )
        .await
    }

    /// The column the migration argued for, asserted at the moment it means anything: **approving
    /// the successor** is what ends the predecessor, and nothing before that does.
    ///
    /// The middle assertion is the one worth the test. A successor that ended the old text the
    /// moment it was *declared* would let a rejected proposal delete what it failed to replace —
    /// the store would lose a lesson by way of a question nobody said yes to.
    #[tokio::test]
    async fn approving_a_successor_is_what_ends_the_one_it_replaces() {
        let pool = test_pool().await;
        let mine = Scope::Project("mine".into());
        let (first, first_proposal) = declare(&pool, Some("mine"), "old text", None)
            .await
            .unwrap();
        approve(&pool, first_proposal).await.unwrap();

        let (second, second_proposal) = declare(&pool, Some("mine"), "new text", Some(first))
            .await
            .unwrap();
        let live: Vec<i64> = for_scope(&pool, &mine)
            .await
            .unwrap()
            .iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(
            live,
            vec![first],
            "an unapproved successor already ended the text it wants to replace"
        );

        approve(&pool, second_proposal).await.unwrap();
        let live: Vec<i64> = for_scope(&pool, &mine)
            .await
            .unwrap()
            .iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(
            live,
            vec![second],
            "both texts are in force at once, which is the pile the chain exists to prevent"
        );

        let (status, ended): (String, Option<String>) =
            sqlx::query_as("SELECT status, ended_at FROM knowledge WHERE id = ?")
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "superseded", "the predecessor kept a wrong status");
        assert!(ended.is_some(), "the predecessor ended at no time at all");

        // Named, not merely ended: "this stopped applying" and "this was replaced by that" are
        // different things to read six months later, and only one of them can be acted on.
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'superseded'",
        )
        .bind(first)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            note.contains(&second.to_string()),
            "the history does not say what replaced it: {note}"
        );
    }

    /// Two refusals that protect the same rule the scoping does. A successor naming nothing would
    /// leave a dangling chain nobody can read back; a successor naming ANOTHER scope's row would
    /// let one repository end another's — the exact poisoning the scope columns exist to stop,
    /// arriving through the one column that writes across the boundary.
    #[tokio::test]
    async fn a_successor_may_not_name_nothing_nor_another_scopes_lesson() {
        let pool = test_pool().await;
        let (theirs, _) = declare(&pool, Some("theirs"), "their lesson", None)
            .await
            .unwrap();

        assert!(
            matches!(
                declare(&pool, Some("mine"), "replaces a ghost", Some(4242)).await,
                Err(ProposeError::UnknownPredecessor(4242))
            ),
            "something was allowed to replace a row that does not exist"
        );
        assert!(
            matches!(
                declare(&pool, Some("mine"), "reaches across", Some(theirs)).await,
                Err(ProposeError::ForeignPredecessor(_))
            ),
            "one project was allowed to end another project's lesson"
        );
    }

    /// Saying no, kept as a refusal rather than as an absence.
    ///
    /// Written after finding the layer shipped able to approve and unable to refuse — a queue with
    /// one button is a queue where everything is eventually approved, and what is being approved
    /// here is what every later run is told.
    #[tokio::test]
    async fn a_refused_lesson_is_kept_as_a_refusal_rather_than_deleted() {
        let pool = test_pool().await;
        let (knowledge_id, proposal_id) = declare(&pool, Some("mine"), "not this one", None)
            .await
            .unwrap();

        assert_eq!(reject(&pool, proposal_id).await.unwrap(), knowledge_id);
        let refused = fetch(&pool, knowledge_id)
            .await
            .unwrap()
            .expect("the refusal was deleted rather than recorded");
        assert_eq!(refused.status, "rejected");
        assert!(refused.ended_at.is_some(), "a refusal with no time on it");
        assert!(
            for_scope(&pool, &Scope::Project("mine".into()))
                .await
                .unwrap()
                .is_empty(),
            "a refused lesson is reaching prompts"
        );

        // Answered once: a second refusal is a conflict, not a second decision written over the
        // first. Same guard the approval carries, and asserted here because the two must agree.
        assert_eq!(
            reject(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        );
        assert_eq!(
            approve(&pool, proposal_id).await,
            Err(DecisionError::NotPending),
            "a refused proposal could still be approved afterwards"
        );

        let decided: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(decided, "rejected", "the question is still in the queue");
    }

    /// What the history screen reads: the chain back through every text this one replaced, and the
    /// successor that replaced it, oldest question first — "what did this say before I changed it".
    ///
    /// The cycle at the end is forced with a raw UPDATE because no path in this module can create
    /// one (a predecessor must already exist, so links only ever point backwards). It is asserted
    /// anyway: a walk that trusts its data terminates until the day the data is wrong, and then it
    /// hangs the daemon instead of returning a bad answer.
    #[tokio::test]
    async fn the_history_reads_back_through_everything_a_row_replaced() {
        let pool = test_pool().await;
        let (first, first_proposal) = declare(&pool, Some("mine"), "first text", None)
            .await
            .unwrap();
        approve(&pool, first_proposal).await.unwrap();
        let (second, second_proposal) = declare(&pool, Some("mine"), "second text", Some(first))
            .await
            .unwrap();
        approve(&pool, second_proposal).await.unwrap();
        let (third, third_proposal) = declare(&pool, Some("mine"), "third text", Some(second))
            .await
            .unwrap();
        approve(&pool, third_proposal).await.unwrap();

        // `middle` and not `history`: a local of the same name shadows the function, and the next
        // call in this test reads as calling a struct.
        let middle = history(&pool, second)
            .await
            .unwrap()
            .expect("a row that exists has a history");
        assert_eq!(middle.known.id, second);
        assert_eq!(
            middle.replaced.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![first],
            "the text this one replaced is not readable from it"
        );
        assert_eq!(
            middle.replaced_by.as_ref().map(|r| r.id),
            Some(third),
            "the text that replaced this one is not readable from it"
        );
        // proposed → active → superseded, all three still there.
        assert_eq!(
            middle.events.len(),
            3,
            "the row's own history is incomplete: {:?}",
            middle.events
        );

        assert!(
            history(&pool, 4242).await.unwrap().is_none(),
            "a row that does not exist reported a history"
        );

        sqlx::query("UPDATE knowledge SET supersedes = ? WHERE id = ?")
            .bind(third)
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        let looped = history(&pool, third)
            .await
            .unwrap()
            .expect("the walk returned rather than hanging");
        let mut ids: Vec<i64> = looped.replaced.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            looped.replaced.len(),
            "the walk went round the cycle and read a row twice"
        );
    }
}
