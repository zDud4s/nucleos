//! Prompt assembly for directors, specialists, and delivery.

use super::*;

// ---------------------------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------------------------

/// What is in the folder, as the director reads it: the item that produced each file, who wrote it,
/// and its first line.
///
/// The first line and not the body, because the index goes into every director prompt and the
/// bodies are what `read_team_file` is for.
async fn folder_index(state: &AppState, run: &TeamRun) -> String {
    let items: Vec<TeamItem> = sqlx::query_as(
        "SELECT ordinal, round, agent_id, description, state, run_id, output_path
         FROM team_items WHERE team_run_id = ? ORDER BY ordinal",
    )
    .bind(&run.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    if items.is_empty() {
        return "The folder is empty; nothing has been done yet.".to_owned();
    }

    let root = files_root(state).ok();
    let mut out = String::new();
    for item in items {
        let first_line = item
            .output_path
            .as_ref()
            .zip(root.as_ref())
            .and_then(|(name, root)| {
                let folder = crate::files::resolve_within(root, &run.workspace).ok()?;
                let path = crate::files::resolve_within(&folder, name).ok()?;
                let body = std::fs::read_to_string(path).ok()?;
                body.lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "(no answer)".to_owned());
        out.push_str(&format!(
            "- round {} item {} [{}] {} — {}: {}\n",
            item.round,
            item.ordinal,
            item.state,
            item.agent_id,
            item.output_path.as_deref().unwrap_or("(no file)"),
            first_line
        ));
    }
    out
}

/// The address book one specialist is shown: everybody it may write to, and not itself.
///
/// Separate from `roster_lines` because the two answer different questions. That one is what the
/// DIRECTOR is shown — who it may hand work to — and a director does not hand work to itself. This
/// one is who a specialist may TELL something, which includes the director and excludes the reader.
///
/// The director is marked rather than merely listed. A specialist choosing between two names should
/// know which of them decides what the next round contains, because that is usually where a finding
/// belongs, and a bare id says nothing about it.
async fn colleagues_lines(state: &AppState, team_id: &str, excluding: &str) -> String {
    let director: Option<String> =
        sqlx::query_scalar("SELECT director_agent_id FROM teams WHERE id = ?")
            .bind(team_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    let mut out = String::new();
    for id in addressable(&state.pool, team_id).await.unwrap_or_default() {
        if id == excluding {
            continue;
        }
        if let Ok(Some(agent)) = crate::agent::get(&state.pool, &id).await {
            let role = match director.as_deref() == Some(id.as_str()) {
                true => " (directs this department and plans the next round)",
                false => "",
            };
            out.push_str(&format!("- {}: {}{role}\n", agent.id, agent.speciality));
        }
    }
    out
}

async fn roster_lines(state: &AppState, team_id: &str) -> String {
    let mut out = String::new();
    for id in roster(&state.pool, team_id).await.unwrap_or_default() {
        if let Ok(Some(agent)) = crate::agent::get(&state.pool, &id).await {
            out.push_str(&format!("- {}: {}\n", agent.id, agent.speciality));
        }
    }
    out
}

/// The director's prompt, for planning and replanning alike.
///
/// The roster carries each member's `speciality` and NOT their `prompt`: a specialist's
/// instructions are its own, and putting them in the director's context would make every round pay
/// for text that only changes what somebody else does.
pub(super) async fn director_prompt(state: &AppState, run: &TeamRun, team: &Team) -> String {
    format!(
        "You direct the {} department. Its mission: {}\n\n\
         The request you are working on:\n{}\n\n\
         Your specialists, and what each is for:\n{}\n\
         What is in the department's folder so far:\n{}\n\
         Reply with JSON and nothing else, in this shape:\n\
         {{\"items\": [{{\"agent_id\": \"...\", \"description\": \"...\"}}], \"done\": false, \"why\": \"...\"}}\n\n\
         Each item is one piece of work for one specialist, described well enough to be done \
         without asking you anything. Queue at most {} of them. Set \"done\" to true when the \
         folder holds enough to answer the request — everything queued beside a true \"done\" is \
         discarded, so do not do both. \"why\" is one sentence for the person who asked.{}",
        team.name,
        team.mission,
        run.request,
        roster_lines(state, &team.id).await,
        folder_index(state, run).await,
        MAX_ITEMS_PER_ROUND,
        pending_recruits_line(state, run).await,
    )
}

/// What the director has already asked the owner for, so it does not ask again.
///
/// `propose_teammate` refuses a duplicate, and that refusal alone is not enough: a director replans
/// every round with the same prompt and the same gap in front of it, so without this it spends a
/// turn per round walking into the same closed door. `calendar_proposal_pending_for` exists for the
/// identical reason and its doc says so.
///
/// Empty string when there is nothing pending, so the prompt of a department that never asked for
/// anybody is byte for byte what it was.
async fn pending_recruits_line(state: &AppState, run: &TeamRun) -> String {
    let pending = crate::proposals::list_pending_recruits(&state.pool, Some(&run.id))
        .await
        .unwrap_or_default();
    if pending.is_empty() {
        return String::new();
    }
    let named: Vec<String> = pending
        .iter()
        .map(|proposal| {
            let speciality = proposal
                .tool_input
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|payload| {
                    payload
                        .get("speciality")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            match proposal.tool_name.as_deref() {
                Some(id) if !speciality.is_empty() => format!("{id} ({speciality})"),
                Some(id) => id.to_owned(),
                None => "somebody".to_owned(),
            }
        })
        .collect();
    format!(
        "\n\nYou have already asked the owner for: {}. They are waiting to be decided and will not \
         join this run. Do not ask again — work with who you have, and say in the delivery what \
         was left thin because of it.",
        named.join("; ")
    )
}

pub(super) async fn delivery_prompt(state: &AppState, run: &TeamRun, team: &Team) -> String {
    format!(
        "You direct the {} department. Its mission: {}\n\n\
         The request:\n{}\n\n\
         What is in the department's folder:\n{}\n\
         Write the department's answer to that request. Read any file you need with the \
         read_team_file tool, naming it exactly as it appears above. Reply with the finished \
         document itself — markdown, no preamble, no JSON. It is what the person who asked will \
         open, and it is the only thing they will read.{}",
        team.name,
        team.mission,
        run.request,
        folder_index(state, run).await,
        // Cheap, and it is the difference between a bad delivery and an honest one. Without it the
        // person reading a thin section has no way to know the department knew it was thin.
        match pending_recruits_line(state, run).await.is_empty() {
            true => String::new(),
            false =>
                " Say plainly, at the end, which part is thin and why — you asked for somebody \
                      this department does not have and did not get them in time."
                    .to_owned(),
        },
    ) + &undelivered_line(state, run).await
}

/// What one member of this department told another that nobody was ever given, ready to append.
///
/// The ordinary way for a note to end up here is not a failure: the round ended, the plan did not
/// queue that colleague again, and the words stayed in the queue. But the delivery is the last
/// moment anybody looks, and a queue quietly emptied by the run ending reads downstream as the whole
/// of what the department found. `job::PlannedItems::dropped` is the precedent and the sentence is
/// its.
///
/// Appended to the DELIVERY and not to every replan, because the director already gets its own mail
/// on every node it runs — what this covers is specifically the messages addressed to somebody else
/// that nobody will now ever read.
async fn undelivered_line(state: &AppState, run: &TeamRun) -> String {
    let left = crate::team_notes::undelivered(&state.pool, &run.id)
        .await
        .unwrap_or_default();
    if left.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nSome of this department's members left notes for colleagues who were never started \
         again, so nobody read them. They are below, and they are findings this department paid for. \
         Use what is useful and leave what is not:",
    );
    for note in &left {
        out.push_str(&format!(
            "\n\n{} wrote to {}:\n\n{}",
            note.from_agent_id, note.to_agent_id, note.body
        ));
    }
    out
}

/// A specialist is told its own instructions, its own task, the mission, the roster and the index —
/// and deliberately not the rest of the plan. What the others are DOING is not its context.
///
/// **The roster is, and it was the thing missing.** A specialist could not name a colleague, so
/// there was nobody for it to address even once there was a channel: `roster_lines` prints
/// `- <agents.id>: <speciality>`, which is exactly what `send_team_note` takes. It carries each
/// member's speciality and not their prompt, for `director_prompt`'s reason — a specialist's
/// instructions are its own, and putting them here would make every node pay for text that only
/// changes what somebody else does.
pub(super) async fn specialist_prompt(
    state: &AppState,
    run: &TeamRun,
    mission: &str,
    item: &TeamItem,
) -> String {
    let own = crate::agent::get(&state.pool, &item.agent_id)
        .await
        .ok()
        .flatten()
        .map(|agent| agent.prompt)
        .unwrap_or_default();
    format!(
        "{own}\n\n\
         You are working inside a department whose mission is: {mission}\n\
         The department was asked to: {}\n\n\
         Your task:\n{}\n\n\
         Who else is in this department:\n{}\n\
         What is already in the department's folder:\n{}\n\
         Answer with your work itself. Do not write it to a file — your reply IS the deliverable, \
         and it is filed for you. If you find something that changes what one of the people above \
         should be doing, tell them with the send_team_note tool, addressing them by the id on the \
         left. They will not answer you — they are a separate run — so say it in a way they can act \
         on alone, and put it in your own answer as well.",
        run.request,
        item.description,
        colleagues_lines(state, &run.team_id, &item.agent_id).await,
        folder_index(state, run).await,
    )
}
