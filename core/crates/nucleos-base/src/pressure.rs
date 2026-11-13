//! How much window each agent cost, per round, and what that says about the shape of the team.
//!
//! Pure reading: this module writes nothing, anywhere. It lives apart from `runs.rs` because
//! that one is about WRITING a run's lifecycle and this one is about READING what was left behind —
//! and because `runs.rs` already runs past five thousand lines.
//!
//! What it deliberately does NOT do: it is not a live signal (the decision it informs is taken
//! between jobs, not during one), it does not split agents, it does not change teams. It gives the
//! evidence; the decision is the owner's.

use sqlx::SqlitePool;

/// What is being measured.
pub enum Scope {
    /// The most recent job of this team, by name.
    Team(String),
    /// One specific job, by `team_runs.id`.
    Job(String),
}

/// Internally tagged so that `never_ran` reaches the reader as an `outcome` value, and
/// not as the absence of fields. An empty report that reads as approval is worse than none at
/// all, and the route is where that distinction is most easily flattened.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Report {
    /// The team exists and has never run anything. Not the same as running and having no problems.
    NeverRan {
        team: String,
    },
    Measured(Measured),
}

#[derive(Debug, serde::Serialize)]
pub struct Measured {
    pub team: String,
    pub job: String,
    pub rounds: i64,
    pub items_measured: usize,
    /// Planned items that never got a run.
    pub items_without_run: usize,
    /// Items with a run but neither `tools_used` nor `num_turns` — they count towards occupancy,
    /// stay out of the fit.
    pub items_without_steps: usize,
    pub rollups: Vec<AgentRollup>,
}

/// One row per planned item, with what its run spent — or nothing, if it never ran.
#[derive(Debug)]
pub struct Item {
    pub round: i64,
    pub agent_id: String,
    pub agent_name: String,
    /// `None` when the item was planned and never got a run.
    pub run_id: Option<i64>,
    pub context_peak: Option<i64>,
    pub compacted: bool,
    pub tools_used: Option<String>,
    pub num_turns: Option<i64>,
}

/// One row of the harvest, as SQLite returns it. Named because clippy counts the arms of the
/// tuple, and eight is more than it tolerates anonymous.
type HarvestedRow = (
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    i64,
    Option<String>,
    Option<i64>,
);

/// `LEFT JOIN` and not `JOIN`: an item may not have got to run, and that is a real state. A
/// `JOIN` dropped it silently and the report said the layer had less work than it
/// did.
const HARVEST_SQL: &str = "\
SELECT ti.round, ti.agent_id, a.name AS agent_name, r.id AS run_id,
       r.context_peak, COALESCE(r.compacted, 0) AS compacted, r.tools_used, r.num_turns
FROM team_items ti
JOIN agents a ON a.id = ti.agent_id
LEFT JOIN runs r ON r.id = ti.run_id
WHERE ti.team_run_id = ?
ORDER BY ti.round, ti.ordinal";

async fn harvest(pool: &SqlitePool, job: &str) -> sqlx::Result<Vec<Item>> {
    let rows: Vec<HarvestedRow> = sqlx::query_as(HARVEST_SQL)
        .bind(job)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let (
                round,
                agent_id,
                agent_name,
                run_id,
                context_peak,
                compacted,
                tools_used,
                num_turns,
            ) = row;
            Item {
                round,
                agent_id,
                agent_name,
                run_id,
                context_peak,
                compacted: compacted != 0,
                tools_used,
                num_turns,
            }
        })
        .collect())
}

/// The module's only door. Resolves the scope, harvests, and builds the report.
///
/// `Scope::Team` resolves to that team's most recent `team_run`. A team that exists and has
/// none gives `NeverRan`; a name that does not exist gives an error, because they are different
/// things and whoever misspelled the name has a right to know.
pub async fn measure(pool: &SqlitePool, scope: Scope) -> sqlx::Result<Report> {
    let (team, job) = match scope {
        Scope::Team(name) => {
            let team_id: String = sqlx::query_scalar("SELECT id FROM teams WHERE name = ?")
                .bind(&name)
                .fetch_one(pool)
                .await?;
            let latest: Option<String> = sqlx::query_scalar(
                "SELECT id FROM team_runs WHERE team_id = ?
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            )
            .bind(&team_id)
            .fetch_optional(pool)
            .await?;
            match latest {
                Some(job) => (name, job),
                None => return Ok(Report::NeverRan { team: name }),
            }
        }
        Scope::Job(job) => {
            let team: String = sqlx::query_scalar(
                "SELECT t.name FROM team_runs tr JOIN teams t ON t.id = tr.team_id WHERE tr.id = ?",
            )
            .bind(&job)
            .fetch_one(pool)
            .await?;
            (team, job)
        }
    };

    let items = harvest(pool, &job).await?;
    let mut rounds: Vec<i64> = items.iter().map(|item| item.round).collect();
    rounds.sort_unstable();
    rounds.dedup();

    Ok(Report::Measured(Measured {
        team,
        job,
        rounds: rounds.len() as i64,
        items_measured: items.iter().filter(|item| item.run_id.is_some()).count(),
        items_without_run: items.iter().filter(|item| item.run_id.is_none()).count(),
        items_without_steps: items
            .iter()
            .filter(|item| {
                item.run_id.is_some()
                    && steps_of(item.tools_used.as_deref(), item.num_turns).is_none()
            })
            .count(),
        rollups: {
            let mut rollups = roll_up(&items);
            judge(&mut rollups);
            rollups
        },
    }))
}

/// How many steps this item took. `tools_used` first, `num_turns` as a fallback.
///
/// The order is not arbitrary: a turn with twelve tools and a turn with one count the same in
/// `num_turns`. Falling back to it loses resolution, and `Measured::items_without_steps` says how
/// many times that was needed.
///
/// A malformed list counts as absent and never as zero: garbage is not the statement «used no
/// tools», and treating it as such put a false point in the fit.
fn steps_of(tools_used: Option<&str>, num_turns: Option<i64>) -> Option<i64> {
    if let Some(raw) = tools_used
        && let Ok(serde_json::Value::Array(calls)) = serde_json::from_str::<serde_json::Value>(raw)
    {
        return Some(calls.len() as i64);
    }
    num_turns
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fit {
    /// Fewer than three points, or all at the same x. There is no line that can be asserted.
    ///
    /// Returning zeros would assert «startup 0, slope 0» — a reading, and a false one.
    Insufficient,
    Line {
        intercept: f64,
        slope: f64,
        r2: f64,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentRollup {
    pub round: i64,
    pub agent_id: String,
    pub agent_name: String,
    pub items: usize,
    pub items_with_steps: usize,
    pub compacted_items: usize,
    pub peak_p50: Option<i64>,
    pub peak_p90: Option<i64>,
    pub steps_median: Option<i64>,
    pub fit: Fit,
    /// Filled in by `judge`, never by `roll_up`.
    pub verdicts: Vec<Verdict>,
}

/// One row per `(round, agent_id)`, in round order.
///
/// Arithmetic and nothing else — it does not judge. Two of the four verdicts are not the property
/// of a single rollup: one compares the agent with the others of the same round, another compares
/// it with itself in other rounds. Whoever judges receives the whole slice, and comes next.
fn roll_up(items: &[Item]) -> Vec<AgentRollup> {
    let mut order: Vec<(i64, String)> = Vec::new();
    for item in items {
        let key = (item.round, item.agent_id.clone());
        if !order.contains(&key) {
            order.push(key);
        }
    }
    order
        .into_iter()
        .map(|(round, agent_id)| {
            let group: Vec<&Item> = items
                .iter()
                .filter(|item| item.round == round && item.agent_id == agent_id)
                .collect();
            let mut peaks: Vec<i64> = group.iter().filter_map(|item| item.context_peak).collect();
            peaks.sort_unstable();
            let mut steps: Vec<i64> = group
                .iter()
                .filter_map(|item| steps_of(item.tools_used.as_deref(), item.num_turns))
                .collect();
            steps.sort_unstable();
            // Only items that have BOTH things enter the fit: a point with a missing axis is not
            // half a point, it is none.
            let points: Vec<(f64, f64)> = group
                .iter()
                .filter_map(|item| {
                    match (
                        steps_of(item.tools_used.as_deref(), item.num_turns),
                        item.context_peak,
                    ) {
                        (Some(steps), Some(peak)) => Some((steps as f64, peak as f64)),
                        _ => None,
                    }
                })
                .collect();
            AgentRollup {
                round,
                agent_name: group
                    .first()
                    .map(|item| item.agent_name.clone())
                    .unwrap_or_default(),
                agent_id,
                items: group.len(),
                items_with_steps: steps.len(),
                compacted_items: group.iter().filter(|item| item.compacted).count(),
                peak_p50: percentile(&peaks, 0.5),
                peak_p90: percentile(&peaks, 0.9),
                steps_median: percentile(&steps, 0.5),
                fit: fit(&points),
                verdicts: Vec::new(),
            }
        })
        .collect()
}

/// Calibration of the readings. First guesses, deliberately conservative.
///
/// None of these numbers was measured: `team_runs` was empty when this was written, so there was
/// nothing to calibrate against. They are here together and named so they can be revised after the
/// first real jobs, and THAT is what is expected to happen -- not that they stay.
mod calibration {
    /// How many times above the median of the round's peers counts as carrying the round alone.
    pub const IMBALANCE: f64 = 2.0;
    /// Below this, "more items than the peers" is noise and not signal.
    pub const MIN_ITEMS: usize = 3;
    /// Rise from the first to the last round, as a fraction, for the handoff to count as heavy.
    pub const ROUND_CLIMB: f64 = 0.25;
    /// What share of typical occupancy is already there before the agent does anything.
    pub const DOMINANT_STARTUP: f64 = 0.5;
    /// Above this the startup amortises and stops being the problem.
    pub const AMORTIZING_STEPS: i64 = 10;
}

/// What the evidence suggests doing to the shape of the team. Never done automatically.
///
/// They are not exclusive: an agent can fire `SplitSpeciality` and `TrimPrompt` at the same time,
/// and that is information -- it fills up early AND does little work, so the problem is almost all
/// in the briefing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// This agent's items compacted: the speciality covers too much surface.
    SplitSpeciality,
    /// Carries the round almost alone: this layer is short of people.
    MoreAgents,
    /// Context climbs from round to round: the handoff between layers carries too much.
    MoreLayers,
    /// Born expensive and does little work. Splitting here makes it WORSE -- each half pays the
    /// startup again.
    TrimPrompt,
}

/// Fills in `verdicts` on every rollup of the slice.
///
/// Takes the whole slice and not one rollup because half of the readings are comparisons:
/// `MoreAgents` measures an agent against the others of the same round, `MoreLayers` measures it
/// against itself in other rounds. A single-rollup judge would have no way to see them.
///
/// **None of the four uses an absolute token threshold.** The model's window is not known per
/// run -- `runner.rs::window_env` only passes it when the question brings it, and an agent run
/// does not -- so all of them compare the agent with itself or with its peers.
fn judge(rollups: &mut [AgentRollup]) {
    // A copy of the three fields the comparisons read, taken before writing to any
    // rollup: a judgement has to see the slice as it arrived, not half-written.
    let seen: Vec<(i64, String, usize, Option<i64>)> = rollups
        .iter()
        .map(|rollup| {
            (
                rollup.round,
                rollup.agent_id.clone(),
                rollup.items,
                rollup.peak_p50,
            )
        })
        .collect();

    for rollup in rollups.iter_mut() {
        let mut verdicts = Vec::new();

        // Ground truth from the CLI itself: it did not fit. Once is enough.
        if rollup.compacted_items >= 1 {
            verdicts.push(Verdict::SplitSpeciality);
        }

        let peers: Vec<i64> = seen
            .iter()
            .filter(|(round, agent, ..)| *round == rollup.round && *agent != rollup.agent_id)
            .map(|(_, _, items, _)| *items as i64)
            .collect();
        if !peers.is_empty() && rollup.items >= calibration::MIN_ITEMS {
            let mut sorted = peers;
            sorted.sort_unstable();
            // The floor of 1 stops a layer where the peers did nothing from dividing by zero and
            // declaring infinite imbalance.
            let baseline = percentile(&sorted, 0.5).unwrap_or(1).max(1) as f64;
            if rollup.items as f64 >= calibration::IMBALANCE * baseline {
                verdicts.push(Verdict::MoreAgents);
            }
        }

        let mut mine: Vec<(i64, Option<i64>)> = seen
            .iter()
            .filter(|(_, agent, ..)| *agent == rollup.agent_id)
            .map(|(round, _, _, p50)| (*round, *p50))
            .collect();
        mine.sort_by_key(|(round, _)| *round);
        // Anchored to the LAST round in which the agent appears, which is where the rise is already
        // fully visible.
        if mine.len() >= 2
            && mine.last().map(|(round, _)| *round) == Some(rollup.round)
            && let Some(peaks) = mine
                .iter()
                .map(|(_, p50)| *p50)
                .collect::<Option<Vec<i64>>>()
            && peaks.windows(2).all(|pair| pair[1] > pair[0])
            && peaks[0] > 0
            && (peaks[peaks.len() - 1] - peaks[0]) as f64 / peaks[0] as f64
                >= calibration::ROUND_CLIMB
        {
            verdicts.push(Verdict::MoreLayers);
        }

        if let Fit::Line { intercept, .. } = &rollup.fit
            && let Some(p50) = rollup.peak_p50
            && let Some(steps) = rollup.steps_median
            && *intercept >= calibration::DOMINANT_STARTUP * p50 as f64
            && steps <= calibration::AMORTIZING_STEPS
        {
            verdicts.push(Verdict::TrimPrompt);
        }

        rollup.verdicts = verdicts;
    }
}

/// Percentile by linear interpolation over the values present, sorted.
///
/// Written by hand and not pulled from a dependency: it is seven lines and the repository has no
/// statistics crate. With two values the p50 is the mean of the two, and not the lower — both
/// answers are defensible and this is the one that is here.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        // No value is not the value zero.
        return None;
    }
    let rank = p * (sorted.len() - 1) as f64;
    let low = rank.floor() as usize;
    let high = rank.ceil() as usize;
    if low == high {
        return Some(sorted[low]);
    }
    let frac = rank - low as f64;
    Some((sorted[low] as f64 + (sorted[high] - sorted[low]) as f64 * frac).round() as i64)
}

/// Least squares in one variable, with R² alongside.
///
/// R² comes out together and not apart because, with one variable and heterogeneous tasks, this is
/// population diagnosis and not a predictor for one item. A startup without the R² alongside reads
/// with a confidence it does not have.
fn fit(points: &[(f64, f64)]) -> Fit {
    if points.len() < 3 {
        return Fit::Insufficient;
    }
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|(x, _)| (x - mx).powi(2)).sum();
    if sxx == 0.0 {
        return Fit::Insufficient;
    }
    let sxy: f64 = points.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
    let slope = sxy / sxx;
    let intercept = my - slope * mx;
    let sst: f64 = points.iter().map(|(_, y)| (y - my).powi(2)).sum();
    let sse: f64 = points
        .iter()
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum();
    Fit::Line {
        intercept,
        slope,
        // Without variation in y there is nothing left to explain, and the line explains it all.
        r2: if sst == 0.0 { 1.0 } else { 1.0 - sse / sst },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// An item to seed: in which round, of which agent, and with which run — or none.
    struct Seed {
        round: i64,
        agent: &'static str,
        run: Option<RunSeed>,
    }

    /// What that item's `runs` row will have. `Default` is a healthy run with nothing to flag.
    #[derive(Default)]
    struct RunSeed {
        context_peak: Option<i64>,
        compacted: bool,
        tools: usize,
    }

    /// Inserts an agent with the given name, if it does not exist yet.
    ///
    /// The speciality is a facade: nothing in this module reads it, and a test that invented one
    /// would be suggesting otherwise.
    async fn seed_agent(pool: &SqlitePool, name: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO agents (id, name, speciality, prompt, engine,
                                           capabilities, created_at, updated_at)
             VALUES (?, ?, 'a speciality', 'a prompt', 'claude', '[\"shell\"]',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(name)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Inserts team, director and members. Returns the `teams.id`.
    async fn seed_team(pool: &SqlitePool, name: &str, members: &[&str]) -> String {
        seed_agent(pool, "director").await;
        for member in members {
            seed_agent(pool, member).await;
        }
        let team_id = name.to_lowercase();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES (?, ?, 'a mission', 'director', 3, 3,
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(&team_id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
        for member in members {
            sqlx::query("INSERT INTO team_members (team_id, agent_id) VALUES (?, ?)")
                .bind(&team_id)
                .bind(member)
                .execute(pool)
                .await
                .unwrap();
        }
        team_id
    }

    /// Inserts a `team_run` over the `NucleOS` team and the requested items, creating the `runs`
    /// where there is a `RunSeed`. Returns the `team_runs.id`.
    async fn seed_job(pool: &SqlitePool, items: &[Seed]) -> String {
        let mut members: Vec<&str> = items.iter().map(|seed| seed.agent).collect();
        members.sort_unstable();
        members.dedup();
        let team_id = seed_team(pool, "NucleOS", &members).await;

        let job = "job-1".to_string();
        sqlx::query(
            "INSERT INTO team_runs (id, team_id, request, workspace, token, state,
                                    created_at, updated_at)
             VALUES (?, ?, 'a request', 'a workspace', 'a token', 'running',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(&job)
        .bind(&team_id)
        .execute(pool)
        .await
        .unwrap();

        for (ordinal, seed) in items.iter().enumerate() {
            let run_id: Option<i64> = match &seed.run {
                Some(run) => {
                    let tools = serde_json::to_string(&vec![
                        serde_json::json!({"name": "Bash"});
                        run.tools
                    ])
                    .unwrap();
                    let id: i64 = sqlx::query_scalar(
                        "INSERT INTO runs (prompt, status, mode, context_peak, compacted,
                                           tools_used, team_run_id, created_at)
                         VALUES ('an item', 'completed', 'worktree', ?, ?, ?, ?,
                                 '2026-08-26T00:00:00Z')
                         RETURNING id",
                    )
                    .bind(run.context_peak)
                    .bind(run.compacted)
                    .bind(&tools)
                    .bind(&job)
                    .fetch_one(pool)
                    .await
                    .unwrap();
                    Some(id)
                }
                None => None,
            };
            sqlx::query(
                "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description,
                                         state, run_id)
                 VALUES (?, ?, ?, ?, 'do the thing', 'done', ?)",
            )
            .bind(&job)
            .bind(ordinal as i64 + 1)
            .bind(seed.round)
            .bind(seed.agent)
            .bind(run_id)
            .execute(pool)
            .await
            .unwrap();
        }
        job
    }

    #[tokio::test]
    async fn a_team_that_never_ran_says_so_instead_of_showing_zeros() {
        let pool = test_pool().await;
        seed_team(&pool, "NucleOS", &["Nucleo", "Concha"]).await;

        // A green, empty report reads as approval. That is not what happened: nothing
        // happened.
        assert!(matches!(
            measure(&pool, Scope::Team("NucleOS".into())).await.unwrap(),
            Report::NeverRan { .. }
        ));
    }

    #[tokio::test]
    async fn an_item_that_never_ran_is_counted_apart_and_not_dropped() {
        let pool = test_pool().await;
        let job = seed_job(
            &pool,
            &[
                Seed {
                    round: 1,
                    agent: "Nucleo",
                    run: Some(RunSeed::default()),
                },
                // Planned, never ran.
                Seed {
                    round: 1,
                    agent: "Concha",
                    run: None,
                },
            ],
        )
        .await;

        let Report::Measured(m) = measure(&pool, Scope::Job(job)).await.unwrap() else {
            panic!("a seeded job is measurable")
        };
        assert_eq!(m.items_measured, 1);
        assert_eq!(
            m.items_without_run, 1,
            "items that never ran are counted; dropping them would be saying the layer had less work than it did"
        );
    }

    /// An `Item` with what the rollup tests need to say, and nothing more.
    fn item(round: i64, agent: &str, peak: Option<i64>, compacted: bool, tools: usize) -> Item {
        Item {
            round,
            agent_id: agent.to_string(),
            agent_name: agent.to_string(),
            run_id: Some(1),
            context_peak: peak,
            compacted,
            // A list with `tools` calls — the content does not matter, the length does.
            tools_used: Some(
                serde_json::to_string(&vec![serde_json::json!({"name": "Bash"}); tools]).unwrap(),
            ),
            num_turns: None,
        }
    }

    /// A rollup in the neutral state: nothing compacted, typical occupancy, amortising steps, no
    /// fit. Each test touches only the field it cares about, and the rest stays explicitly
    /// harmless.
    fn r(round: i64, agent: &str, items: usize) -> AgentRollup {
        AgentRollup {
            round,
            agent_id: agent.to_string(),
            agent_name: agent.to_string(),
            items,
            items_with_steps: items,
            compacted_items: 0,
            peak_p50: Some(100_000),
            peak_p90: Some(100_000),
            steps_median: Some(20),
            fit: Fit::Insufficient,
            verdicts: Vec::new(),
        }
    }

    /// Judges the slice and returns one agent's verdicts in one round.
    fn verdicts_of(rollups: &mut [AgentRollup], agent: &str, round: i64) -> Vec<Verdict> {
        judge(rollups);
        rollups
            .iter()
            .find(|x| x.agent_name == agent && x.round == round)
            .expect("the requested rollup exists")
            .verdicts
            .clone()
    }

    #[test]
    fn an_agent_whose_items_compact_is_told_to_split() {
        // One compaction is enough. It is not a threshold: it is the CLI saying it did not fit.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].compacted_items = 1;
        assert!(verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::SplitSpeciality));
    }

    #[test]
    fn a_high_peak_that_never_compacted_is_slack_and_not_pressure() {
        // 180k without compaction is slack. It is the whole distinction the verdict keeps, and the
        // reason the signal is `compacted` and not a token count.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].peak_p90 = Some(180_000);
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::SplitSpeciality));
    }

    #[test]
    fn an_agent_carrying_the_round_alone_wants_company() {
        // 4 items against peers with 1: 4 >= 2 x max(1, median 1).
        let mut rs = vec![r(1, "Nucleo", 4), r(1, "Concha", 1), r(1, "Portao", 1)];
        assert!(verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn an_even_round_wants_nothing() {
        let mut rs = vec![r(1, "Nucleo", 3), r(1, "Concha", 3)];
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn two_items_against_one_is_noise_and_not_imbalance() {
        // 2 >= 2 x 1 is true, but 2 < MIN_ITEMS. The floor exists exactly for this case.
        let mut rs = vec![r(1, "Nucleo", 2), r(1, "Concha", 1)];
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn an_agent_alone_in_its_round_is_not_called_imbalanced() {
        // Without peers there is no possible imbalance -- only a layer of one. Guards against
        // `max(1, ...)` turning "there is no median" into "the median is 1".
        let mut rs = vec![r(1, "Nucleo", 5)];
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::MoreAgents));
    }

    #[test]
    fn context_climbing_across_rounds_asks_for_a_layer() {
        // 100k -> 150k: +50%, above ROUND_CLIMB. The verdict anchors to the last round.
        let mut rs = vec![r(1, "Nucleo", 2), r(2, "Nucleo", 2)];
        rs[0].peak_p50 = Some(100_000);
        rs[1].peak_p50 = Some(150_000);
        assert!(verdicts_of(&mut rs, "Nucleo", 2).contains(&Verdict::MoreLayers));
    }

    #[test]
    fn context_that_holds_steady_does_not() {
        let mut rs = vec![r(1, "Nucleo", 2), r(2, "Nucleo", 2)];
        rs[0].peak_p50 = Some(100_000);
        // +5%, noise.
        rs[1].peak_p50 = Some(105_000);
        assert!(!verdicts_of(&mut rs, "Nucleo", 2).contains(&Verdict::MoreLayers));
    }

    #[test]
    fn a_high_start_with_little_work_is_a_prompt_problem() {
        // Startup of 60k in a typical occupancy of 100k, and a median of 4 steps: born expensive
        // and does little.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].fit = Fit::Line {
            intercept: 60_000.0,
            slope: 900.0,
            r2: 0.8,
        };
        rs[0].steps_median = Some(4);
        assert!(verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::TrimPrompt));
    }

    #[test]
    fn a_high_start_that_does_a_lot_of_work_is_not() {
        // The same startup, 40 steps: it amortises. Splitting here would pay it again, and that is
        // the mistake this verdict exists to prevent.
        let mut rs = vec![r(1, "Nucleo", 3)];
        rs[0].fit = Fit::Line {
            intercept: 60_000.0,
            slope: 900.0,
            r2: 0.8,
        };
        rs[0].steps_median = Some(40);
        assert!(!verdicts_of(&mut rs, "Nucleo", 1).contains(&Verdict::TrimPrompt));
    }

    #[test]
    fn a_rollup_keeps_rounds_apart() {
        // The same agent in two rounds gives two rows, never a sum. Summing would erase the rise
        // between rounds, which is precisely the third verdict.
        let rollups = roll_up(&[
            item(1, "Nucleo", Some(100_000), false, 10),
            item(2, "Nucleo", Some(150_000), false, 10),
        ]);
        assert_eq!(rollups.len(), 2);
        assert_eq!(rollups[0].round, 1);
        assert_eq!(rollups[1].round, 2);
    }

    #[test]
    fn percentiles_come_from_the_items_that_have_a_peak() {
        let rollups = roll_up(&[
            item(1, "Nucleo", Some(100_000), false, 5),
            // No peak: counts towards `items`, not towards the p50.
            item(1, "Nucleo", None, false, 5),
            item(1, "Nucleo", Some(200_000), false, 5),
        ]);
        let r = &rollups[0];
        assert_eq!(
            r.items, 3,
            "the item without a peak is still an item of the layer"
        );
        assert_eq!(r.peak_p50, Some(150_000), "median of [100k, 200k]");
    }

    #[test]
    fn an_agent_with_no_peaks_at_all_has_no_percentiles() {
        // No peak is not a zero peak. Zero would be a claim, and a false one.
        let rollups = roll_up(&[item(1, "Nucleo", None, false, 5)]);
        assert_eq!(rollups[0].peak_p50, None);
        assert_eq!(rollups[0].peak_p90, None);
    }

    #[test]
    fn compacted_items_are_counted_and_not_just_flagged() {
        // The first verdict counts them; knowing that «some» compacted is not enough for the
        // report to say how many out of how many.
        let rollups = roll_up(&[
            item(1, "Nucleo", Some(100_000), true, 5),
            item(1, "Nucleo", Some(100_000), false, 5),
        ]);
        assert_eq!(rollups[0].compacted_items, 1);
        assert_eq!(rollups[0].items, 2);
    }

    #[test]
    fn steps_prefer_tools_and_fall_back_to_turns() {
        assert_eq!(
            steps_of(Some(r#"[{"name":"Bash"},{"name":"Read"}]"#), Some(9)),
            Some(2)
        );
        assert_eq!(steps_of(None, Some(9)), Some(9));
        assert_eq!(steps_of(None, None), None);
    }

    #[test]
    fn an_empty_tool_list_is_zero_steps_and_not_a_fallback() {
        // `[]` is a claim: «used no tools». It is not absence.
        assert_eq!(steps_of(Some("[]"), Some(7)), Some(0));
    }

    #[test]
    fn a_malformed_tool_list_is_absent_and_never_zero() {
        // Garbage is not a claim. Fall back to the turns.
        assert_eq!(steps_of(Some("{this is not a list"), Some(4)), Some(4));
        assert_eq!(steps_of(Some("{this is not a list"), None), None);
    }

    #[test]
    fn the_fit_needs_spread_and_says_so_when_it_has_none() {
        // Three items all with 10 steps: there is no slope that can be asserted.
        assert!(matches!(
            fit(&[(10.0, 100_000.0), (10.0, 110_000.0), (10.0, 105_000.0)]),
            Fit::Insufficient
        ));
        // Two points are a trivial line, not a measurement.
        assert!(matches!(
            fit(&[(10.0, 100_000.0), (20.0, 120_000.0)]),
            Fit::Insufficient
        ));
    }

    #[test]
    fn the_fit_recovers_a_line_it_was_given() {
        // startup 50k, slope 1 400/step, exact.
        let Fit::Line {
            intercept,
            slope,
            r2,
        } = fit(&[(10.0, 64_000.0), (20.0, 78_000.0), (30.0, 92_000.0)])
        else {
            panic!("should have fitted")
        };
        assert!((intercept - 50_000.0).abs() < 1.0);
        assert!((slope - 1_400.0).abs() < 0.1);
        assert!(r2 > 0.99);
    }
}
