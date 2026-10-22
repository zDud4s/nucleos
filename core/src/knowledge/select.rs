//! Pure knowledge scoring, selection, and rendering.

use super::*;

/// The midpoint of the bounded utility range: without any outcomes, there is evidence for neither
/// success nor failure, so the neutral value must favour neither end.
pub(super) const NEUTRAL_UTILITY: f64 = 0.5;

/// The midpoint of the bounded recency range: without a show timestamp, there is evidence for
/// neither end, so the neutral value must favour neither end.
pub(super) const NEUTRAL_RECENCY: f64 = 0.5;

/// The weight of each signal. Two tables, each summing to 1.0: one for a briefing that has a query
/// vector and one for a briefing that has none (spec 5.2).
pub(super) struct Weights {
    /// Confirmed by owner 2026-09-23. The text match against this very work is the most direct evidence of relevance, but it is 0.0 for every row on day one, so it cannot be the whole score.
    pub(super) fts: f64,
    /// SIM: confirmed by owner 2026-10-06 (destilador spec 5.2); taken from FTS because both measure relevance to the task.
    pub(super) sim: f64,
    /// Confirmed by owner 2026-09-23. Files and map communities overlap: the signal this project has for free because it already builds the map (spec §5.1).
    pub(super) structure: f64,
    /// Confirmed by owner 2026-09-23. Measured outcomes. When the other signals are equal,
    /// `use_ * NEUTRAL_UTILITY >= recency` guarantees that a never-shown row never scores below a
    /// failed row. At the extreme they tie, and `(layer, kind, id)` breaks the tie (spec §5.4).
    pub(super) use_: f64,
    /// Confirmed by owner 2026-09-23. The chain already decides entitlement; specificity only tips a contradiction towards the most specific scope (spec §3.4).
    pub(super) scope: f64,
    /// Confirmed by owner 2026-09-23. Decay (spec §8.1) measures less than it seems — run-less contexts leave no trace to refresh it (D15) — so it weighs least.
    pub(super) recency: f64,
}

pub(super) const WITH_SIM: Weights = Weights {
    fts: 0.20,
    sim: 0.15,
    structure: 0.20,
    use_: 0.20,
    scope: 0.15,
    recency: 0.10,
};

pub(super) const WITHOUT_SIM: Weights = Weights {
    fts: 0.20 + 0.15,
    sim: 0.0,
    structure: 0.20,
    use_: 0.20,
    scope: 0.15,
    recency: 0.10,
};

pub(super) fn weights(query_embedded: bool) -> &'static Weights {
    if query_embedded {
        &WITH_SIM
    } else {
        &WITHOUT_SIM
    }
}

/// Score every candidate once. Each signal and their one weighted score are on the unit scale; the
/// caller follows that score only with stable vocabulary and id tie-breakers.
pub(super) fn scored_candidates<'a>(
    known: &'a [Known],
    context: &Context,
) -> Vec<(&'a Known, Scored)> {
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
                // `brief::of` sets it from SQLite's bm25, and it has two meanings. With a query
                // vector (embedder path) it is the plain pass-normalised share of the best match
                // (`brief::normalise_fts` anchors the floor at 0, so an unmatched row is 0 and
                // the best match is 1). Without one (no-embedder path) `brief::lift_fts` lifts
                // it onto `[MIN_RELEVANCE, 1.0]` for rows at or above its floor, and leaves a
                // weaker row at its raw share, which the gate then drops. The clamp keeps
                // out-of-contract input from outweighing the rest (spec 5.5).
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

pub(super) fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

pub(super) fn scope_specificity(row: &Known, context: &Context) -> Option<f64> {
    let row_scope = Scope::parse(&row.scope_kind, row.scope_id.as_deref())?;
    let scopes = context.scopes();
    scopes
        .iter()
        .position(|scope| scope.columns() == row_scope.columns())
        .map(|index| (index + 1) as f64 / scopes.len() as f64)
        // A recognised but out-of-chain candidate is not entitled to inherit into this context.
        .or(Some(0.0))
}

pub(super) fn structural_overlap(row: &Known, context: &Context) -> f64 {
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

pub(super) fn structural_references(raw: &str) -> Vec<String> {
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

pub(super) fn normalise_reference(reference: &str) -> String {
    reference
        .trim()
        .trim_matches(['"', '\'', '[', ']', '{', '}'])
        .replace('\\', "/")
}

pub(super) fn normalised_recencies(known: &[Known]) -> Vec<f64> {
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

pub(super) fn median_measured_utility(known: &[Known]) -> f64 {
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

pub(super) fn median(mut values: Vec<f64>) -> Option<f64> {
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
pub(super) fn ordered_candidates<'a>(known: &'a [Known], context: &Context) -> Vec<&'a Known> {
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

pub(super) struct Selected<'a> {
    pub(super) row: &'a Known,
    pub(super) width: usize,
    pub(super) chars: usize,
}

pub(super) struct Selection<'a> {
    pub(super) items: Vec<Selected<'a>>,
    pub(super) omitted: Vec<usize>,
}

pub(super) fn same_scope(row: &Known, scope: &Scope) -> bool {
    Scope::parse(&row.scope_kind, row.scope_id.as_deref())
        .is_some_and(|row_scope| row_scope.columns() == scope.columns())
}

pub(super) fn scope_heading(scope: &Scope) -> String {
    match scope {
        Scope::Machine => "\n\nHouse-wide knowledge:".into(),
        Scope::Project(id) => format!("\n\nKnowledge about project {id}:"),
        Scope::Team(id) => format!("\n\nKnowledge for team {id}:"),
        Scope::Agent(id) => format!("\n\nKnowledge for agent {id}:"),
        Scope::Job { id, .. } => format!("\n\nKnowledge about job {id}:"),
    }
}

pub(super) fn cut_notice(omitted: usize) -> String {
    format!(
        "\n\n({omitted} further admitted {} in this scope {} not shown here, to leave room for the work.)",
        if omitted == 1 { "note" } else { "notes" },
        if omitted == 1 { "is" } else { "are" }
    )
}

pub(super) fn item_piece(row: &Known, width: usize) -> String {
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

pub(super) fn char_count(value: &str) -> usize {
    value.chars().count()
}

pub(super) fn select_pass<'a>(
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
pub(super) fn admitted(row: &Known, context: &Context) -> bool {
    match row.status.as_str() {
        "active" => approved(row),
        "live" => {
            row.source == "run"
                && row.layer == "working"
                && row.evidence.as_deref().is_some_and(evidence_is_tagged)
                && context
                    .scopes()
                    .iter()
                    .any(|scope| matches!(scope, Scope::Job { .. }) && same_scope(row, scope))
        }
        _ => false,
    }
}

/// The least textual relevance a row needs to be briefed at all.
///
/// `s_fts` is bm25 over the best match of the pass, and it means two things. On the embedder path
/// it is the plain pass-normalised share of the best: an unmatched row is `0.0` and a sole match
/// is `1.0`, so `0.1` drops the non-matches and the tail matches under a tenth of the best (one
/// common term). On the no-embedder path `brief::lift_fts` lifts every row above its small floor
/// onto `[MIN_RELEVANCE, 1.0]`, so a real selective match clears this gate even when the best
/// row is far stronger; a row below that floor (a stopword-only or near-noise match, about 1e-6 of
/// the best) is not lifted, keeps its raw share and falls under the gate. The textual signal is
/// `s_fts`, or the larger of `s_fts` and `s_sim` when the briefing has a query vector
/// ([`Context::query_embedded`]). For `s_sim` `0.1` is lenient, because the cosine of unrelated
/// texts depends on the configured embedder; calibrate it on real vectors later.
pub const MIN_RELEVANCE: f64 = 0.1;

/// How well a row's text answers the briefing's query, on the unit scale.
///
/// `s_sim` counts only when the briefing computed a query vector; otherwise it is whatever a reader
/// left in the field, and says nothing.
pub(super) fn textual_relevance(row: &Known, context: &Context) -> f64 {
    let s_fts = finite_or_zero(row.s_fts).clamp(0.0, 1.0);
    if context.query_embedded {
        s_fts.max(finite_or_zero(row.s_sim).clamp(0.0, 1.0))
    } else {
        s_fts
    }
}

/// Select a bounded briefing without doing I/O.
///
/// Admission applies spec §4.5 first: approved rows plus the two named exceptions, measured
/// consolidator observations and same-job working facts with well-tagged evidence. Structure then
/// owns its bytes first. Populated layers claim their item floors, and candidates left over compete in
/// [`ordered_candidates`] order. The first pass discovers which scope groups are cut; the second
/// reserves those notices before choosing rows and accounts for any cut it exposes at the
/// boundary. Candidates under [`MIN_RELEVANCE`] are left out before either pass, whatever room is
/// left. The trace covers every admitted candidate.
pub fn select(known: &[Known], context: &Context, budget: &Budget) -> Brief {
    let admitted: Vec<Known> = known
        .iter()
        .filter(|row| admitted(row, context))
        .cloned()
        .collect();
    let scopes = context.scopes();
    // Relevance is decided HERE, before the groups and `select_pass`: the layer floors then only
    // ever see survivors, and a floor can no longer be spent on a row that matched nothing. The
    // trace below still carries every admitted row, `shown = false` for the ones dropped.
    let candidates: Vec<&Known> = ordered_candidates(&admitted, context)
        .into_iter()
        .filter(|row| scopes.iter().any(|scope| same_scope(row, scope)))
        .filter(|row| textual_relevance(row, context) >= MIN_RELEVANCE)
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

    let groups: Vec<Scope> = scopes
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
pub(super) fn clip(body: &str, width: usize) -> String {
    if body.chars().count() <= width {
        return body.to_owned();
    }
    let mut cut: String = body.chars().take(width).collect();
    cut.push('…');
    cut
}
