//! The plan a plan node writes: `plan.json`, its items and the dependency graph between them.

use super::*;

/// Why a plan node produced no usable queue.
///
/// `Absent` and an empty `items` list are different outcomes and must stay that way: the first says
/// the plan node failed to produce anything, the second says it looked and found no work. Only the
/// first is a failure.
#[derive(Debug)]
pub enum PlanError {
    Absent,
    Unreadable(String),
    /// The plan parsed and the graph in it does not hold up.
    ///
    /// A THIRD variant and not an `Unreadable` with a different string, because the two are
    /// different failures with different owners: unreadable is a node that wrote something that is
    /// not JSON, and this is a node that answered the wrong thing. The message names the item, since
    /// the whole plan is refused for one bad row and a reader otherwise cannot tell which.
    Rejected(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Absent => write!(formatter, "the plan node wrote no plan.json"),
            PlanError::Unreadable(reason) => write!(formatter, "plan.json is unreadable: {reason}"),
            PlanError::Rejected(reason) => write!(formatter, "the plan was refused: {reason}"),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct PlanFile {
    /// Absent when a replan node wrote `{"done": true}`. A plan node always writes it, so the
    /// default is what makes ONE shape read both files — and what makes a plan node that forgot the
    /// key indistinguishable from one that found nothing, which is the reading `parse_plan`'s caller
    /// already treats as a successful empty night.
    #[serde(default)]
    pub(super) items: Vec<PlanItem>,
    /// A replan node saying the work is over. `#[serde(default)]` so a plan node's file, which never
    /// carries it, reads as `false` rather than as a parse failure.
    #[serde(default)]
    pub(super) done: bool,
    /// Why it is over, in the node's own words. Kept for the feed: "job 12 finished after 3 rounds"
    /// is a fact, and the reason it stopped asking is the part a person can disagree with.
    #[serde(default)]
    pub(super) why: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlanItem {
    /// The unfinished items this one takes over, numbered as the replan prompt lists them
    /// (one-based). Only a replan is shown any, so only a replan's items carry this.
    #[serde(default)]
    pub(super) replaces: Vec<i64>,
    pub(super) description: String,
    /// `Option`, and the distinction it carries is the whole of decision 10: **an absent key is not
    /// an empty list**.
    ///
    /// Without a team the two mean the same thing and both mean nothing — the field is younger than
    /// the plans that have to keep parsing, and any planner may take the prompt's offer to decline.
    /// With a team they are opposites. An empty list is a director saying "this item touches
    /// nothing anyone else will"; an absent key is a director that did not answer, which is the most
    /// likely thing an LLM does with a key it has not seen before. Read as empty, that answer
    /// degenerates the job into something SLOWER than the sequential one it replaced: nothing can be
    /// told apart, so nothing may run beside anything.
    ///
    /// `#[serde(default)]` on the `Option` keeps every plan written before this parsing, which is
    /// what makes the strictness affordable.
    #[serde(default)]
    pub(super) files: Option<Vec<String>>,
    /// Which earlier items this one may not start before, by global ordinal.
    ///
    /// `Option` for the same reason as `files`, and the stakes are higher: read as empty, an absent
    /// key is a director saying every item is independent, and the queue would start work on top of
    /// work that has not landed.
    #[serde(default)]
    pub(super) depends_on: Option<Vec<i64>>,
    /// Which member of the team works this item.
    ///
    /// `Option` for a different reason from the other two. Absent and empty are not opposites here
    /// — an empty agent id is not an answer anybody could act on — it is that a job WITHOUT a team
    /// has nobody to name, so the key is required with one and never read without one.
    ///
    /// There is no fallback member and there must not be. Choosing for a director that did not
    /// choose would put the assignment in two places — its judgement and a rule here — and the rule
    /// would win silently on exactly the plans where the director had least to say.
    #[serde(default)]
    pub(super) agent_id: Option<String>,
}

/// One item of the queue: what to do, and where the planner guessed it lives.
#[derive(Debug, PartialEq, Eq)]
pub struct PlannedItem {
    pub description: String,
    /// Empty when the planner named nothing. A hint and not a boundary — it is where the implement
    /// node starts looking, never the extent of what it may touch.
    pub files: Vec<String>,
    /// The global ordinals this item may not start before. Empty for a plan with no graph in it,
    /// which is every plan a job without a team produces.
    pub depends_on: Vec<i64>,
    /// Who works it, checked against the team's roster. `None` for every plan of every job without
    /// a team, which is the whole of what makes those jobs read here exactly as they always did.
    pub agent_id: Option<String>,
}

/// A validated work queue, plus however much of it did not fit.
#[derive(Debug, PartialEq, Eq)]
pub struct PlannedItems {
    /// Every unfinished item the plan says it takes over, as zero-based ordinals.
    pub replaces: Vec<usize>,
    pub items: Vec<PlannedItem>,
    /// Items the daemon's ceiling cut. Carried rather than discarded so the feed can say what was
    /// left out — a queue silently trimmed reads downstream as the whole of what the planner found.
    pub dropped: usize,
    /// A replan node declaring the work over, and why.
    ///
    /// Distinct from an empty `items` on purpose, and the distinction is the whole of ending #1
    /// versus ending #3: "there is nothing more to do" is a claim the node is making, while an empty
    /// queue is a round that happened to produce nothing and might not be the last. One ends the job
    /// now; the other feeds a counter that ends it after two.
    pub done: Option<String>,
}

/// A dependency list on its way into the row, or NULL when there is nothing to say.
///
/// **NULL for an empty list, and here that is right where the parsing's opposite rule is also
/// right.** Coming IN, absent and empty are opposites: one is a director that did not answer and the
/// other is one that answered "nothing". By the time a list has been checked, both survivors mean
/// the same thing to every reader downstream — this item waits for nobody — and NULL is how this
/// schema has spelled that since `files` (0056).
pub(super) fn edges(depends_on: &[i64]) -> Option<String> {
    (!depends_on.is_empty())
        .then(|| serde_json::to_string(depends_on).ok())
        .flatten()
}

/// The graph rules this job's next plan is held to, or `None` for a job with no team.
///
/// Reads the facts the rules need at the moment the plan is being read, and not earlier. The first
/// ordinal in particular has to be current: it is what a `depends_on` is range-checked against, and
/// it moves every time a round is ingested. The roster has to be current for the same kind of
/// reason — somebody removed from the team between the plan and the replan may not be given work by
/// the second one.
pub(super) async fn graph_rules(pool: &SqlitePool, job: &JobRow) -> Option<Graph> {
    let team: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT team_id FROM jobs WHERE id = ?")
            .bind(job.id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten();
    let team = team?;

    // The same expression `ingest_replan` uses to hand out ordinals, asked here so the number the
    // plan is checked against is the number the plan will be given.
    let first_ordinal: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(ordinal) + 1, 0) FROM job_items WHERE job_id = ?")
            .bind(job.id)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
    Some(Graph {
        first_ordinal: first_ordinal.max(0) as usize,
        roster: roster_of(pool, &team).await,
    })
}

/// Everyone this team's director may give work to, with the line it chooses them by.
///
/// **The director is on its own roster**, and that is a decision rather than an oversight. A team of
/// one — a director and no members — is an ordinary way to ask for parallel work under a single
/// persona, and leaving it out would make every such team refuse every plan it ever wrote, for a
/// reason no message could usefully explain. `team.rs` keeps its departments' directors off the
/// roster because there a director hands work to specialists and does none itself; here the roster
/// is "who may write code in this job", and the director qualifies.
///
/// Ordered and de-duplicated by the `UNION`, so the prompt lists the same people in the same order
/// every round — a roster that reshuffles between rounds is a director being asked the same question
/// twice in two different words.
pub(super) async fn roster_of(pool: &SqlitePool, team_id: &str) -> Vec<TeamMate> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT agents.id, agents.speciality FROM agents
          WHERE agents.id IN (
                SELECT agent_id FROM team_members WHERE team_id = ?
                UNION
                SELECT director_agent_id FROM teams WHERE id = ?)
          ORDER BY agents.id",
    )
    .bind(team_id)
    .bind(team_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(id, speciality)| TeamMate { id, speciality })
    .collect()
}

/// One name a director may put on an item, and the line it decides by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMate {
    pub id: String,
    /// One line, and load-bearing rather than decoration — `agent.rs:11` says so. It is the whole of
    /// what the director is shown about each member, and the whole of what it chooses on.
    pub speciality: String,
}

/// The rules a plan is held to when a team wrote it, and the facts those rules need.
///
/// Present only for a job a team directs. Its absence is not laxity for its own sake: without a team
/// the items share one checkout and run one at a time, so none of the keys buys anything and
/// demanding them would fail plans that were right.
///
/// No longer `Copy`, because the roster is a `Vec`. It travels by reference from here on; the three
/// functions that take it never keep it.
#[derive(Debug, Clone)]
pub struct Graph {
    /// The global ordinal the FIRST item of this round will be given.
    ///
    /// **The number without which the whole scheme silently mis-reads itself.** Ordinals continue
    /// across rounds, so round 2's first item might be ordinal 5. A director writing
    /// `"depends_on": [0]` meaning "the first item I just listed" would be naming an item from
    /// round 1 — a plan that is wrong in a way nothing here can see, because ordinal 0 exists and is
    /// finished. Range-checking against this makes the mistake visible instead, and the prompt is
    /// told the same number so the director can be right in the first place.
    pub first_ordinal: usize,
    /// Who may be given work, and what each of them is for.
    ///
    /// Empty only when the team's every agent has been deleted, which `foreign_keys` makes very
    /// nearly impossible. An empty roster refuses every plan, and the message says the roster is
    /// empty rather than that the name was wrong — the two are different problems and only one of
    /// them is the director's.
    pub roster: Vec<TeamMate>,
}

impl Graph {
    /// PURE: the three keys of one planned item, checked, or the reason the whole plan is refused.
    ///
    /// **One bad row refuses the plan rather than dropping the row**, and that is the decision worth
    /// arguing with. Dropping it would leave a queue that looks complete and is missing the work
    /// somebody asked for; refusing costs a round and says why. A job whose director cannot answer
    /// the three questions is a job that should stop being run in parallel, not one that should be
    /// run in parallel badly.
    pub(super) fn check(
        &self,
        index: usize,
        description: &str,
        files: Option<Vec<String>>,
        depends_on: Option<Vec<i64>>,
        agent_id: Option<String>,
    ) -> Result<(Vec<String>, Vec<i64>, String), PlanError> {
        let named = || description.chars().take(60).collect::<String>();
        let Some(files) = files else {
            return Err(PlanError::Rejected(format!(
                "item {} (`{}`) names no `files`. With a team every item has to say what it will                  touch, because that is the only thing that can tell two items apart before either                  of them has run",
                index + 1,
                named()
            )));
        };
        if files.is_empty() {
            return Err(PlanError::Rejected(format!(
                "item {} (`{}`) declares an empty `files`. An item that touches nothing overlaps                  with everything, so the queue would run it alone — which is slower than not                  splitting the work at all",
                index + 1,
                named()
            )));
        }
        let Some(depends_on) = depends_on else {
            return Err(PlanError::Rejected(format!(
                "item {} (`{}`) names no `depends_on`. An absent list is not an empty one: read as                  empty it would say this item may start beside everything, and work would be built                  on work that has not landed. Write `[]` to say it depends on nothing",
                index + 1,
                named()
            )));
        };

        // **Every dependency must be an EARLIER ordinal**, which is a range check and a cycle check
        // at once. A graph whose edges only ever point backwards cannot contain a cycle, so there is
        // nothing here to walk and nothing to get wrong — where a general cycle detector is a second
        // algorithm that has to agree with this one about what an edge is.
        //
        // The cost is that a director has to list items in dependency order. That costs it nothing
        // and the prompt says so; the alternative buys the freedom to write a plan whose order is
        // misleading to the person who reads it afterwards.
        let own = (self.first_ordinal + index) as i64;
        for dependency in &depends_on {
            if *dependency < 0 || *dependency >= own {
                return Err(PlanError::Rejected(format!(
                    "item {} (`{}`) is ordinal {own} and depends on {dependency}, which is not an                      earlier item of this job. Dependencies point backwards only — list an item                      after everything it needs — and the ordinals of this round start at {}",
                    index + 1,
                    named(),
                    self.first_ordinal
                )));
            }
        }

        // Named last of the three because it is the one a director can get wrong while meaning
        // well: the other two are shapes, and this is a name that has to exist. The message carries
        // the roster, because "no such member" without saying who there IS costs the next round to
        // discover.
        let Some(agent_id) = agent_id else {
            return Err(PlanError::Rejected(format!(
                "item {} (`{}`) names no `agent_id`. With a team every item is given to somebody:                  {}",
                index + 1,
                named(),
                self.who()
            )));
        };
        if !self.roster.iter().any(|mate| mate.id == agent_id) {
            return Err(PlanError::Rejected(format!(
                "item {} (`{}`) is given to `{agent_id}`, who is not on this team: {}",
                index + 1,
                named(),
                self.who()
            )));
        }
        Ok((files, depends_on, agent_id))
    }

    /// The roster as a sentence for a prompt, one member per line.
    ///
    /// One per line rather than run together, because this is a list to choose FROM: a director
    /// reading a comma-separated paragraph picks the first name whose speciality matches the word
    /// it was thinking of, and the ones after the second comma stop being read. The refusals use
    /// `who()` instead, which runs them together — there the roster is context for a mistake, not a
    /// menu.
    pub(super) fn introduce(&self) -> String {
        if self.roster.is_empty() {
            // Reachable only if the team's every agent has been deleted, which `foreign_keys` makes
            // very nearly impossible. Said plainly anyway: a director told "the team is:" followed
            // by nothing invents a name, and the plan is refused for a reason that reads as its
            // fault.
            return "This team has no members on record, so there is nobody to give the work to \
                    and any plan will be refused. Say so instead of guessing at a name."
                .to_owned();
        }
        format!(
            "The team is:\n{}\n",
            self.roster
                .iter()
                .map(|mate| format!("  {} — {}", mate.id, mate.speciality))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    /// The roster as a sentence, for the two refusals that have to name it.
    ///
    /// An empty roster gets a different sentence, because it is a different problem: the director
    /// answered as well as it could and there was nobody to answer with, which is the owner's to fix
    /// and not the director's. Told as "nobody is on this team" it costs one reading; told as "`x`
    /// is not on this team" it costs a round of guessing at names.
    pub(super) fn who(&self) -> String {
        if self.roster.is_empty() {
            return "this team has no members and no director, so there is nobody to give it to"
                .to_owned();
        }
        format!(
            "the team is {}",
            self.roster
                .iter()
                .map(|mate| format!("`{}` ({})", mate.id, mate.speciality))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Reads the queue a plan node produced.
///
/// Takes bytes rather than a path so the decisions here stay testable without a filesystem, and so
/// "the file was missing" is expressed by the caller as `None` rather than inferred from an IO error
/// that could equally mean a permissions problem.
pub fn parse_plan(
    contents: Option<&[u8]>,
    max_items: usize,
    graph: Option<&Graph>,
) -> Result<PlannedItems, PlanError> {
    let bytes = contents.ok_or(PlanError::Absent)?;
    let parsed: PlanFile =
        serde_json::from_slice(bytes).map_err(|error| PlanError::Unreadable(error.to_string()))?;

    let total = parsed.items.len();
    let mut items = Vec::with_capacity(total.min(max_items));
    let mut replaced: Vec<usize> = Vec::new();
    for (index, item) in parsed.items.into_iter().take(max_items).enumerate() {
        let PlanItem {
            description,
            replaces,
            files,
            depends_on,
            agent_id,
        } = item;
        // A zero or a negative names no item the prompt listed, so it names nothing.
        replaced.extend(
            replaces
                .into_iter()
                .filter(|number| *number >= 1)
                .map(|number| (number - 1) as usize),
        );
        let (files, depends_on, agent_id) = match graph {
            Some(graph) => {
                let (files, depends_on, agent_id) =
                    graph.check(index, &description, files, depends_on, agent_id)?;
                (files, depends_on, Some(agent_id))
            }
            // No team, no graph. None of the keys means anything here and an absent one is not an
            // answer anybody was owed — which is what every plan written before this parsing relies
            // on.
            None => (files.unwrap_or_default(), Vec::new(), None),
        };
        items.push(PlannedItem {
            description,
            files,
            depends_on,
            agent_id,
        });
    }

    replaced.sort_unstable();
    replaced.dedup();
    Ok(PlannedItems {
        replaces: replaced,
        dropped: total - items.len(),
        items,
        // `done` wins over any items alongside it, and the alternative would be worse in both
        // directions: honouring the items would queue work the node just said was unnecessary, and
        // treating the pair as malformed would fail a job over a node being redundant.
        done: parsed
            .done
            .then(|| parsed.why.unwrap_or_else(|| "no reason given".to_owned())),
    })
}
