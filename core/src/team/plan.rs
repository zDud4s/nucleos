//! Team planning data structures and pure plan-transition helpers.

use super::*;

/// One item a director asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedItem {
    pub agent_id: String,
    pub description: String,
}

/// What one director node produced, after the roster and the ceiling have been applied.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedItems {
    pub items: Vec<PlannedItem>,
    /// What was cut, and why — reported rather than discarded, because a silently truncated queue
    /// reads downstream as the whole of what the planner found.
    pub dropped: Vec<String>,
    pub done: bool,
    pub why: Option<String>,
}

/// PURE: the plan a director wrote, or `None` if there is no plan in what it wrote.
///
/// `None` is the signal the round machine acts on — a relaunch in round 0, a dry round after — so
/// it must mean "unreadable" and never "readable and empty". A readable plan with no items is
/// `Some` with an empty list, and those two travel to different places.
///
/// The JSON is looked for inside the answer rather than expected to be the whole of it, because a
/// model asked for JSON writes a sentence around it perhaps a third of the time. An `agent_id`
/// outside the roster drops THAT ITEM and not the plan: a director that misspells one name in five
/// should not cost the round.
pub fn parse_team_plan(text: &str, roster: &[String], ceiling: usize) -> Option<PlannedItems> {
    let value = extract_json(text)?;

    // `done: true` wins over any items beside it, for the reason `job::parse_plan` gives: the model
    // said the work is finished, and queueing work it also mentioned would be obeying the half of
    // the answer that costs money.
    let done = value.get("done").and_then(serde_json::Value::as_bool) == Some(true);
    let why = value
        .get("why")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let mut items = Vec::new();
    let mut dropped = Vec::new();
    if !done {
        let raw = value
            .get("items")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in raw {
            let agent_id = entry.get("agent_id").and_then(serde_json::Value::as_str);
            let description = entry.get("description").and_then(serde_json::Value::as_str);
            let (Some(agent_id), Some(description)) = (agent_id, description) else {
                dropped.push("an item named no agent or no work".to_owned());
                continue;
            };
            if !roster.iter().any(|member| member == agent_id) {
                dropped.push(format!("{agent_id} is not on this team"));
                continue;
            }
            if description.trim().is_empty() {
                dropped.push(format!("{agent_id} was given no work to do"));
                continue;
            }
            if items.len() == ceiling {
                dropped.push(format!(
                    "{agent_id} was cut: a round queues at most {ceiling} items"
                ));
                continue;
            }
            items.push(PlannedItem {
                agent_id: agent_id.to_owned(),
                description: description.trim().to_owned(),
            });
        }
    }

    Some(PlannedItems {
        items,
        dropped,
        done,
        why,
    })
}

/// The outermost JSON object in a string, or `None`.
///
/// Whole-string first, because that is what a well-behaved answer is and parsing it directly is
/// what keeps a `}` inside a string literal from confusing the fallback. The fallback spans the
/// first `{` to the last `}`, which is the shape a model produces when it wraps the object in prose
/// or a code fence.
fn extract_json(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && value.is_object()
    {
        return Some(value);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&trimmed[start..=end])
        .ok()
        .filter(serde_json::Value::is_object)
}

/// Where a run goes when a director node lands.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub state: &'static str,
    pub round: i64,
    pub dry_rounds: i64,
    pub plan_retries: i64,
    /// Whether the plan's items should be queued. `false` whenever the run is leaving `working`,
    /// so that items are never written for a round that will not run.
    pub queue_items: bool,
    pub why: Option<String>,
}

/// PURE: the whole round machine, in one function.
///
/// Every transition of the design's §3 table that a director node can cause is here, and the two
/// call sites — the tick and the reconciliation — share it rather than each implementing the half
/// it needs. `plan: None` means the node produced nothing readable.
///
/// **The order is: increment the round, then check the ceiling.** A team with `max_rounds = 3` runs
/// rounds 0, 1 and 2, and the check catches it as round 3 opens. Written down because it is exactly
/// the kind of detail an implementer guesses, and guesses wrong half the time.
pub fn next_after_director(
    state: &str,
    plan: Option<&PlannedItems>,
    round: i64,
    dry_rounds: i64,
    plan_retries: i64,
    max_rounds: i64,
) -> Progress {
    let unchanged = Progress {
        state: "working",
        round,
        dry_rounds,
        plan_retries,
        queue_items: false,
        why: None,
    };

    if state == "planning" {
        return match plan {
            // Round 0 has no partial work to hand over, so an unreadable plan is worth one more
            // attempt and then nothing.
            None if plan_retries < PLAN_RETRY_LIMIT => Progress {
                state: "planning",
                plan_retries: plan_retries + 1,
                ..unchanged
            },
            None => Progress {
                state: "failed",
                why: Some("the director's plan could not be read, twice".to_owned()),
                ..unchanged
            },
            Some(plan) if plan.done => Progress {
                state: "delivering",
                why: plan.why.clone(),
                ..unchanged
            },
            // Readable, not done, and nothing to delegate. Not a dry round — there has been no
            // round — and not a failure either: the director read the request and found nothing to
            // hand out, which is an answer the delivery node should write down.
            Some(plan) if plan.items.is_empty() => Progress {
                state: "delivering",
                why: plan
                    .why
                    .clone()
                    .or_else(|| Some("the director queued no work".to_owned())),
                ..unchanged
            },
            Some(_) => Progress {
                state: "working",
                queue_items: true,
                ..unchanged
            },
        };
    }

    // A round ended. It counts whether or not the replan could be read — an unreadable plan is a
    // dry round, because there is work in the folder either way.
    let round = round + 1;
    let produced = plan.is_some_and(|plan| !plan.done && !plan.items.is_empty());
    let dry_rounds = if produced { 0 } else { dry_rounds + 1 };
    let done = plan.is_some_and(|plan| plan.done);

    if done || dry_rounds >= DRY_ROUNDS_TO_STOP || round >= max_rounds {
        return Progress {
            state: "delivering",
            round,
            dry_rounds,
            plan_retries,
            queue_items: false,
            why: plan.and_then(|plan| plan.why.clone()).or_else(|| {
                Some(if done {
                    "the director called the work finished".to_owned()
                } else if round >= max_rounds {
                    format!("the team reached its ceiling of {max_rounds} rounds")
                } else {
                    "two rounds in a row added nothing".to_owned()
                })
            }),
        };
    }

    Progress {
        state: "working",
        round,
        dry_rounds,
        plan_retries,
        queue_items: produced,
        why: None,
    }
}
