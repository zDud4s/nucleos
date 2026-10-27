//! Themes above the entities (spec 2026-10-22 section 6). Proposes, never decides; the synthesis (B6,
//! Phase 6) is owner-only text and never enters a prompt, the feed, the outbox or a log.
use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

pub const STATES: [&str; 4] = ["proposed", "accepted", "refused", "merged"];
pub const ORIGINS: [&str; 2] = ["system", "owner"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MemberKind {
    Note,
    Knowledge,
}

impl MemberKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberKind::Note => "note",
            MemberKind::Knowledge => "knowledge",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "note" => Some(Self::Note),
            "knowledge" => Some(Self::Knowledge),
            _ => None,
        }
    }
}

pub type MemberKey = (MemberKind, i64);

pub const CLUSTER_THRESHOLD: f32 = 0.70; // guess until there is data (spec 6.2)
pub const MIN_MEMBERS: usize = 3;
pub const MAX_MEMBERS: usize = 40;
pub const MAX_NEW_THEMES_PER_RUN: usize = 3;
pub const BLOCKING_OVERLAP: f64 = 0.5;

pub struct Candidate {
    pub key: MemberKey,
    pub vector: Vec<f32>,
}

pub struct NameRow<'a> {
    pub title: &'a str,
    pub quoted: bool,
    pub cosine: f32,
}

/// The only grouping call: the shared `embed::cluster` (spec B12) at the themes' threshold. Items are
/// `(position, vector)`; the result lists positions, ordered by each group's smallest position.
fn cluster_groups(items: &[(i64, Vec<f32>)]) -> Vec<Vec<i64>> {
    let refs: Vec<(i64, &[f32])> = items.iter().map(|(i, v)| (*i, v.as_slice())).collect();
    crate::embed::cluster::cluster(
        &refs,
        crate::embed::cluster::Params {
            threshold: CLUSTER_THRESHOLD,
            min_size: MIN_MEMBERS,
            max_size: MAX_MEMBERS,
        },
    )
}

fn jaccard(a: &HashSet<MemberKey>, b: &HashSet<MemberKey>) -> f64 {
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

/// New themes for this run. `blocking` holds the member sets of every `refused` theme (spec 6.2) and,
/// by this plan's reading, of every live `proposed`/`accepted` one too, so a week does not re-propose
/// what the owner already has or declined. A member joins at most one NEW theme per run.
pub fn plan_new_themes(
    candidates: &[Candidate],
    blocking: &[HashSet<MemberKey>],
) -> Vec<Vec<MemberKey>> {
    let mut sorted: Vec<&Candidate> = candidates.iter().collect();
    sorted.sort_by_key(|c| c.key);
    let items: Vec<(i64, Vec<f32>)> = sorted
        .iter()
        .enumerate()
        .map(|(i, c)| (i as i64, c.vector.clone()))
        .collect();
    let mut taken: HashSet<MemberKey> = HashSet::new();
    let mut out = Vec::new();
    for group in cluster_groups(&items) {
        let keys: Vec<MemberKey> = group
            .iter()
            .map(|i| sorted[*i as usize].key)
            .filter(|k| !taken.contains(k))
            .collect();
        let set: HashSet<MemberKey> = keys.iter().copied().collect();
        if keys.len() < MIN_MEMBERS
            || blocking.iter().any(|b| jaccard(&set, b) >= BLOCKING_OVERLAP)
        {
            continue;
        }
        taken.extend(keys.iter().copied());
        out.push(keys);
        if out.len() == MAX_NEW_THEMES_PER_RUN {
            break;
        }
    }
    out
}

/// Title of the closest knowledge member that did NOT quote the owner (that title may hold note text),
/// else "Untitled theme". No note text ever reaches `themes.name`.
pub fn provisional_name(rows: &[NameRow]) -> String {
    rows.iter()
        .filter(|r| !r.quoted)
        .max_by(|a, b| a.cosine.total_cmp(&b.cosine))
        .map(|r| r.title.trim().chars().take(80).collect::<String>())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled theme".to_owned())
}

pub const MAX_PASS_ATTEMPTS: i64 = 3;
const PERIOD: chrono::Duration = chrono::Duration::days(7);
const SEAL_KEY: &str = "themes.clustered_at";
const ATTEMPTS_KEY: &str = "themes.clustered_attempts";

/// Seal at the START of an attempt (spec 5.1): a failing pass is not retried every 30 s tick.
pub fn pass_due(now: DateTime<Utc>, sealed: Option<DateTime<Utc>>, attempts: i64) -> bool {
    let Some(sealed) = sealed else { return true };
    if attempts > 0 && attempts < MAX_PASS_ATTEMPTS {
        return crate::distill::extract::retry_at(attempts, sealed).is_some_and(|at| now >= at);
    }
    now - sealed >= PERIOD
}

/// Mean of equally long vectors; vectors of another length than the first are skipped.
pub fn centroid(vectors: &[Vec<f32>]) -> Vec<f32> {
    let Some(first) = vectors.first() else {
        return Vec::new();
    };
    let dim = first.len();
    let mut sum = vec![0.0_f64; dim];
    let mut n = 0usize;
    for v in vectors.iter().filter(|v| v.len() == dim) {
        for (s, x) in sum.iter_mut().zip(v) {
            *s += f64::from(*x);
        }
        n += 1;
    }
    sum.into_iter().map(|s| (s / n as f64) as f32).collect()
}

/// The vectors the grouping runs over: active notes (ids and vectors only) and the live knowledge rows
/// the distiller or the owner wrote. The consolidator's counter-observations are not themes material.
async fn candidates(pool: &SqlitePool, model: &str) -> sqlx::Result<Vec<Candidate>> {
    let mut out: Vec<Candidate> = crate::owner_notes::vectors_for_clustering(pool, model)
        .await?
        .into_iter()
        .map(|(id, vector)| Candidate {
            key: (MemberKind::Note, id),
            vector,
        })
        .collect();
    let rows: Vec<(i64, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT e.knowledge_id, e.dim, e.vector
           FROM knowledge_embeddings e JOIN knowledge k ON k.id = e.knowledge_id
          WHERE e.model = ? AND k.status IN ('active', 'proposed', 'observed')
            AND k.source IN ('distiller', 'owner')
          ORDER BY e.knowledge_id",
    )
    .bind(model)
    .fetch_all(pool)
    .await?;
    out.extend(rows.into_iter().filter_map(|(id, dim, blob)| {
        crate::embed::decode(&blob, dim).map(|vector| Candidate {
            key: (MemberKind::Knowledge, id),
            vector,
        })
    }));
    Ok(out)
}

/// Member sets that block a new proposal: every `refused`, `proposed` and `accepted` theme, minus the
/// members the owner took out.
async fn blocking_sets(pool: &SqlitePool) -> sqlx::Result<Vec<HashSet<MemberKey>>> {
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT m.theme_id, m.member_kind, m.member_id
           FROM theme_members m JOIN themes t ON t.id = m.theme_id
          WHERE t.state IN ('refused', 'proposed', 'accepted') AND m.excluded = 0
          ORDER BY m.theme_id",
    )
    .fetch_all(pool)
    .await?;
    let mut by_theme: std::collections::BTreeMap<i64, HashSet<MemberKey>> = Default::default();
    for (theme_id, kind, id) in rows {
        if let Some(kind) = MemberKind::parse(&kind) {
            by_theme.entry(theme_id).or_default().insert((kind, id));
        }
    }
    Ok(by_theme.into_values().collect())
}

/// The theme the planner proposed, written in one transaction. The name comes from the closest
/// knowledge member that did not quote the owner.
async fn insert_theme(
    pool: &SqlitePool,
    model: &str,
    keys: &[MemberKey],
    by_key: &std::collections::HashMap<MemberKey, &Vec<f32>>,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let vectors: Vec<Vec<f32>> = keys
        .iter()
        .filter_map(|k| by_key.get(k).map(|v| (*v).clone()))
        .collect();
    let center = centroid(&vectors);
    let mut titled: Vec<(String, bool, f32)> = Vec::new();
    for (kind, id) in keys {
        if *kind != MemberKind::Knowledge {
            continue;
        }
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT k.title,
                    EXISTS (SELECT 1 FROM knowledge_events e
                             WHERE e.knowledge_id = k.id AND e.note = 'quoted_owner_text')
               FROM knowledge k WHERE k.id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        if let Some((title, quoted)) = row {
            let cosine = by_key
                .get(&(*kind, *id))
                .and_then(|v| crate::embed::cosine(v, &center))
                .unwrap_or(0.0);
            titled.push((title, quoted != 0, cosine));
        }
    }
    let rows: Vec<NameRow> = titled
        .iter()
        .map(|(title, quoted, cosine)| NameRow {
            title,
            quoted: *quoted,
            cosine: *cosine,
        })
        .collect();
    let name = provisional_name(&rows);
    let stamp = now.to_rfc3339();
    let mut tx = pool.begin().await?;
    let theme_id: i64 = sqlx::query_scalar(
        "INSERT INTO themes (name, state, model, dim, centroid, created_at)
         VALUES (?, 'proposed', ?, ?, ?, ?) RETURNING id",
    )
    .bind(name)
    .bind(model)
    .bind(center.len() as i64)
    .bind(crate::embed::encode(&center))
    .bind(&stamp)
    .fetch_one(&mut *tx)
    .await?;
    for (kind, id) in keys {
        sqlx::query(
            "INSERT INTO theme_members (theme_id, member_kind, member_id, origin, added_at)
             VALUES (?, ?, ?, 'system', ?)",
        )
        .bind(theme_id)
        .bind(kind.as_str())
        .bind(id)
        .bind(&stamp)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// For every live theme of `model`: an `accepted` one takes in each candidate close to its centroid
/// that is not already a member (an `excluded` member stays out), and every `proposed`/`accepted` one
/// gets its centroid recomputed from the live vectors of its members (the excluded and the vanished
/// do not count). A theme with a NULL centroid is recomputed here, which is how it recovers.
async fn absorb_and_recompute(
    pool: &SqlitePool,
    model: &str,
    found: &[Candidate],
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let by_key: std::collections::HashMap<MemberKey, &Vec<f32>> =
        found.iter().map(|c| (c.key, &c.vector)).collect();
    let themes: Vec<(i64, String, Option<Vec<u8>>, i64)> = sqlx::query_as(
        "SELECT id, state, centroid, dim FROM themes
          WHERE model = ? AND state IN ('proposed', 'accepted') ORDER BY id",
    )
    .bind(model)
    .fetch_all(pool)
    .await?;
    for (theme_id, state, blob, dim) in themes {
        let known: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT member_kind, member_id, excluded FROM theme_members WHERE theme_id = ?",
        )
        .bind(theme_id)
        .fetch_all(pool)
        .await?;
        let mut members: std::collections::HashMap<MemberKey, bool> = Default::default();
        for (kind, id, excluded) in known {
            if let Some(kind) = MemberKind::parse(&kind) {
                members.insert((kind, id), excluded != 0);
            }
        }
        let stamp = now.to_rfc3339();
        if state == "accepted"
            && let Some(center) = blob.as_deref().and_then(|b| crate::embed::decode(b, dim))
        {
            for candidate in found {
                let close = crate::embed::cosine(&candidate.vector, &center)
                    .is_some_and(|c| c >= CLUSTER_THRESHOLD);
                if close && !members.contains_key(&candidate.key) {
                    sqlx::query(
                        "INSERT OR IGNORE INTO theme_members
                           (theme_id, member_kind, member_id, origin, added_at)
                         VALUES (?, ?, ?, 'system', ?)",
                    )
                    .bind(theme_id)
                    .bind(candidate.key.0.as_str())
                    .bind(candidate.key.1)
                    .bind(&stamp)
                    .execute(pool)
                    .await?;
                    members.insert(candidate.key, false);
                }
            }
        }
        let live: Vec<Vec<f32>> = {
            let mut keys: Vec<&MemberKey> = members
                .iter()
                .filter(|(_, excluded)| !**excluded)
                .map(|(key, _)| key)
                .collect();
            keys.sort();
            keys.into_iter()
                .filter_map(|key| by_key.get(key).map(|v| (*v).clone()))
                .collect()
        };
        if live.is_empty() {
            continue;
        }
        let fresh = centroid(&live);
        sqlx::query("UPDATE themes SET centroid = ?, dim = ? WHERE id = ?")
            .bind(crate::embed::encode(&fresh))
            .bind(fresh.len() as i64)
            .bind(theme_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// One clustering pass: absorb close members into `accepted` themes, recompute centroids, then propose
/// at most `MAX_NEW_THEMES_PER_RUN` new themes. Returns how many new themes it proposed.
pub async fn cluster_pass(
    pool: &SqlitePool,
    model: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let found = candidates(pool, model).await?;
    absorb_and_recompute(pool, model, &found, now).await?;
    let blocking = blocking_sets(pool).await?;
    let planned = plan_new_themes(&found, &blocking);
    let by_key: std::collections::HashMap<MemberKey, &Vec<f32>> =
        found.iter().map(|c| (c.key, &c.vector)).collect();
    for keys in &planned {
        insert_theme(pool, model, keys, &by_key, now).await?;
    }
    Ok(planned.len())
}

async fn meta(pool: &SqlitePool, key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
}

async fn set_meta(pool: &SqlitePool, key: &str, value: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO schema_meta (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// The weekly step the distiller's tick calls. Seals at the start of an attempt, retries a failure with
/// the distiller's backoff up to `MAX_PASS_ATTEMPTS`, then waits the normal week. A failure logs the
/// SQL error text and nothing else.
pub async fn run_weekly(pool: &SqlitePool, model: &str, now: DateTime<Utc>) {
    let sealed = match meta(pool, SEAL_KEY).await {
        Ok(value) => value
            .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
            .map(|t| t.with_timezone(&Utc)),
        Err(error) => {
            tracing::warn!("themes: could not read its seal: {error}");
            return;
        }
    };
    let attempts: i64 = meta(pool, ATTEMPTS_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if !pass_due(now, sealed, attempts) {
        return;
    }
    // A pass that starts after the attempts were spent begins a fresh round.
    let next = if attempts >= MAX_PASS_ATTEMPTS { 1 } else { attempts + 1 };
    if set_meta(pool, SEAL_KEY, &now.to_rfc3339()).await.is_err()
        || set_meta(pool, ATTEMPTS_KEY, &next.to_string()).await.is_err()
    {
        tracing::warn!("themes: could not seal the pass");
        return;
    }
    match cluster_pass(pool, model, now).await {
        Ok(_) => {
            let _ = set_meta(pool, ATTEMPTS_KEY, "0").await;
        }
        Err(error) => tracing::warn!("themes: clustering failed: {error}"),
    }
}

// ---- Phase 6: the synthesis of an accepted theme (spec 2026-10-22 section 6.3, section 10) ----

/// The standing instruction of a synthesis. The note lines are owner context: summarised, never copied.
pub const SYNTHESIS_STANDING: &str = "You write a short plain-text synthesis (at most 2000 characters) \
of a theme in the owner's second brain. Lines marked 'owner context, do not quote' are the owner's \
private notes: summarise what they say about the theme, never copy their wording.";

pub const MAX_SYNTHESIS_CHARS: usize = 2000;
const REFRESH_AFTER: chrono::Duration = chrono::Duration::days(7);
const FORCE_EVERY: chrono::Duration = chrono::Duration::hours(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SynthesisError {
    /// The theme is not `accepted` (or does not exist): only an accepted theme is synthesised.
    NotAllowed,
    /// The model or the database failed; the previous text, if any, is untouched.
    Failed,
}

pub struct KnowledgeText<'a> {
    pub title: &'a str,
    pub body: &'a str,
}

/// Only live knowledge feeds a synthesis: `active` and `observed`.
pub fn feeds_synthesis(status: &str) -> bool {
    matches!(status, "active" | "observed")
}

/// At most `MAX_SYNTHESIS_CHARS` characters, cut on a character boundary.
pub fn clip_synthesis(text: &str) -> String {
    text.trim().chars().take(MAX_SYNTHESIS_CHARS).collect()
}

/// The model input: every text is scrubbed, and the note text sits under the do-not-quote marker.
pub fn synthesis_prompt(knowledge: &[KnowledgeText<'_>], notes: &[(i64, String)]) -> String {
    let mut out = String::from("## Knowledge rows of the theme\n");
    for row in knowledge {
        out.push_str(&format!(
            "- {}: {}\n",
            crate::distill::scrub(row.title),
            crate::distill::scrub(row.body)
        ));
    }
    if !notes.is_empty() {
        out.push_str("\n## Notes (owner context, do not quote)\n");
        for (_, text) in notes {
            out.push_str(&format!("- {}\n", crate::distill::scrub(text)));
        }
    }
    out.push_str("\nWrite the synthesis now.");
    out
}

/// Whether a stored synthesis is stale: the basis changed by 3 members or 20% (an entry plus an exit
/// counts as one swap) AND a week passed since it was written. Never written: due.
pub fn refresh_due(
    basis: &[String],
    current: &[String],
    synthesis_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    let Some(at) = synthesis_at else {
        return true;
    };
    let before: HashSet<&String> = basis.iter().collect();
    let after: HashSet<&String> = current.iter().collect();
    let changes = before.symmetric_difference(&after).count() / 2;
    changes > 0
        && (changes >= 3 || changes * 5 >= basis.len().max(1))
        && now - at >= REFRESH_AFTER
}

/// The owner's "force" route is limited to one synthesis an hour.
pub fn force_allowed(synthesis_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    synthesis_at.is_none_or(|at| now - at >= FORCE_EVERY)
}

fn model_label(asked: &crate::map::intent::Extractor<'_>) -> String {
    match asked {
        crate::map::intent::Extractor::Cli(_) => "hosted".to_owned(),
        crate::map::intent::Extractor::Loopback { model, .. } => (*model).to_owned(),
    }
}

/// One plain-text question to the distiller's brain (no schema: the answer is prose).
async fn ask_plain(
    asked: &crate::map::intent::Extractor<'_>,
    prompt: String,
) -> std::io::Result<String> {
    match asked {
        crate::map::intent::Extractor::Cli(runner) => {
            crate::map::intent::ask_once(*runner, prompt, "distillation", Some(SYNTHESIS_STANDING))
                .await
        }
        crate::map::intent::Extractor::Loopback {
            client,
            base_url,
            model,
        } => {
            crate::execution::runner::ollama_chat(
                client,
                base_url,
                model,
                &format!("{SYNTHESIS_STANDING}\n\n{prompt}"),
                serde_json::json!({"num_ctx": 32_768, "temperature": 0}),
                None,
                false,
            )
            .await
        }
    }
}

/// The non-excluded members of a theme, as `(kind, id)`.
async fn live_members(pool: &SqlitePool, theme: i64) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT member_kind, member_id FROM theme_members
          WHERE theme_id = ? AND excluded = 0 ORDER BY member_kind, member_id",
    )
    .bind(theme)
    .fetch_all(pool)
    .await
}

/// Knowledge members that feed a synthesis, as `(id, title, body)`.
async fn feeding_knowledge(
    pool: &SqlitePool,
    members: &[(String, i64)],
) -> sqlx::Result<Vec<(i64, String, String)>> {
    let mut out = Vec::new();
    for (_, id) in members.iter().filter(|(k, _)| k == "knowledge") {
        let row: Option<(String, String, String)> =
            sqlx::query_as("SELECT title, body, status FROM knowledge WHERE id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        if let Some((title, body, _)) = row.filter(|(_, _, st)| feeds_synthesis(st)) {
            out.push((*id, title, body));
        }
    }
    Ok(out)
}

/// The basis a synthesis is written against: the fed knowledge rows and every note member, sorted.
fn basis_of(knowledge: &[(i64, String, String)], members: &[(String, i64)]) -> Vec<String> {
    let mut basis: Vec<String> = knowledge
        .iter()
        .map(|(id, _, _)| format!("knowledge:{id}"))
        .chain(
            members
                .iter()
                .filter(|(k, _)| k == "note")
                .map(|(_, id)| format!("note:{id}")),
        )
        .collect();
    basis.sort();
    basis
}

/// Synthesise an `accepted` theme and store the text with its basis. Owner-only: the text goes to the
/// theme row and nowhere else (never knowledge, the feed, the outbox, a channel or a log). A failure
/// logs the error kind only and keeps the previous text.
pub async fn synthesise(
    pool: &SqlitePool,
    asked: crate::map::intent::Extractor<'_>,
    id: i64,
    now: DateTime<Utc>,
) -> Result<(), SynthesisError> {
    let state: Option<String> = sqlx::query_scalar("SELECT state FROM themes WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|_| SynthesisError::Failed)?;
    if state.as_deref() != Some("accepted") {
        return Err(SynthesisError::NotAllowed);
    }
    let fail = |kind: &str| {
        tracing::warn!("themes: synthesis failed: {kind}");
        SynthesisError::Failed
    };
    let members = live_members(pool, id).await.map_err(|_| fail("sql"))?;
    let knowledge = feeding_knowledge(pool, &members)
        .await
        .map_err(|_| fail("sql"))?;
    let note_ids: Vec<i64> = members
        .iter()
        .filter(|(k, _)| k == "note")
        .map(|(_, id)| *id)
        .collect();
    let notes = crate::owner_notes::texts_for_ids(pool, &note_ids)
        .await
        .map_err(|_| fail("sql"))?;
    let rows: Vec<KnowledgeText<'_>> = knowledge
        .iter()
        .map(|(_, title, body)| KnowledgeText { title, body })
        .collect();
    let prompt = synthesis_prompt(&rows, &notes);
    let model = model_label(&asked);
    let answer = ask_plain(&asked, prompt)
        .await
        .map_err(|e| fail(&format!("{:?}", e.kind())))?;
    let text = clip_synthesis(&answer);
    if text.is_empty() {
        return Err(fail("empty"));
    }
    let basis = serde_json::to_string(&basis_of(&knowledge, &members)).map_err(|_| fail("json"))?;
    sqlx::query(
        "UPDATE themes SET synthesis = ?, synthesis_at = ?, synthesis_model = ?, synthesis_basis = ?
          WHERE id = ? AND state = 'accepted'",
    )
    .bind(&text)
    .bind(now.to_rfc3339())
    .bind(&model)
    .bind(&basis)
    .bind(id)
    .execute(pool)
    .await
    .map_err(|_| fail("sql"))?;
    Ok(())
}

fn reborrow<'a>(asked: &'a crate::map::intent::Extractor<'_>) -> crate::map::intent::Extractor<'a> {
    match asked {
        crate::map::intent::Extractor::Cli(runner) => crate::map::intent::Extractor::Cli(*runner),
        crate::map::intent::Extractor::Loopback {
            client,
            base_url,
            model,
        } => crate::map::intent::Extractor::Loopback {
            client,
            base_url,
            model,
        },
    }
}

/// The weekly refresh: every accepted theme whose synthesis is due is synthesised again. Called by
/// the distiller's tick after `run_weekly` (integration point: pass the tick's `asked`).
pub async fn refresh_syntheses(
    pool: &SqlitePool,
    asked: crate::map::intent::Extractor<'_>,
    now: DateTime<Utc>,
) {
    let accepted: Vec<(i64, Option<String>, Option<String>)> = match sqlx::query_as(
        "SELECT id, synthesis_basis, synthesis_at FROM themes WHERE state = 'accepted' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!("themes: could not list accepted themes: {error}");
            return;
        }
    };
    for (id, basis, at) in accepted {
        let Ok(members) = live_members(pool, id).await else {
            continue;
        };
        let Ok(knowledge) = feeding_knowledge(pool, &members).await else {
            continue;
        };
        let current = basis_of(&knowledge, &members);
        let before: Vec<String> = basis
            .and_then(|b| serde_json::from_str(&b).ok())
            .unwrap_or_default();
        let at = at
            .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| t.with_timezone(&Utc));
        if refresh_due(&before, &current, at, now) {
            let _ = synthesise(pool, reborrow(&asked), id, now).await;
        }
    }
}

// ---- Phase 5 A7-A9: the owner's actions on a theme ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Accept,
    Refuse,
    Merge,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ThemeError {
    NotAllowed,
    BadName,
    BadTarget,
    NotFound,
    Db(String),
}

impl From<sqlx::Error> for ThemeError {
    /// Only the driver's error text, never a row.
    fn from(error: sqlx::Error) -> Self {
        ThemeError::Db(error.to_string())
    }
}

/// The table of spec 6.3: which action moves which state where.
pub fn transition(from: &str, action: Action) -> Result<&'static str, ThemeError> {
    match (from, action) {
        ("proposed", Action::Accept) | ("refused", Action::Accept) => Ok("accepted"),
        ("proposed", Action::Refuse) | ("accepted", Action::Refuse) => Ok("refused"),
        ("proposed", Action::Merge) | ("accepted", Action::Merge) => Ok("merged"),
        _ => Err(ThemeError::NotAllowed),
    }
}

const NAME_CHARS: usize = 80;

fn clean_name(name: &str) -> Result<&str, ThemeError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > NAME_CHARS {
        return Err(ThemeError::BadName);
    }
    Ok(name)
}

async fn state_of(pool: &SqlitePool, id: i64) -> Result<String, ThemeError> {
    sqlx::query_scalar("SELECT state FROM themes WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(ThemeError::NotFound)
}

/// Accept a `proposed` or `refused` theme, optionally renaming it in the same step.
pub async fn accept(pool: &SqlitePool, id: i64, name: Option<&str>) -> Result<(), ThemeError> {
    let to = transition(&state_of(pool, id).await?, Action::Accept)?;
    let name = name.map(clean_name).transpose()?;
    sqlx::query(
        "UPDATE themes SET state = ?, decided_at = ?, name = COALESCE(?, name) WHERE id = ?",
    )
    .bind(to)
    .bind(Utc::now().to_rfc3339())
    .bind(name)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Refuse a theme. The synthesis it may hold is derived from the members of a theme the owner no
/// longer stands behind, so it is deleted with the refusal.
pub async fn refuse(pool: &SqlitePool, id: i64) -> Result<(), ThemeError> {
    let to = transition(&state_of(pool, id).await?, Action::Refuse)?;
    sqlx::query(
        "UPDATE themes SET state = ?, decided_at = ?, synthesis = NULL, synthesis_at = NULL,
                synthesis_model = NULL, synthesis_basis = NULL
          WHERE id = ?",
    )
    .bind(to)
    .bind(Utc::now().to_rfc3339())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn rename(pool: &SqlitePool, id: i64, name: &str) -> Result<(), ThemeError> {
    let name = clean_name(name)?;
    let done = sqlx::query("UPDATE themes SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    if done.rows_affected() == 0 {
        return Err(ThemeError::NotFound);
    }
    Ok(())
}

/// Merge `from` into the live theme `into`. Members move over as `owner`-origin rows, `from` becomes
/// `merged`, and every theme that pointed at `from` now points at `into`, so `merged_into` never
/// forms a chain.
pub async fn merge(pool: &SqlitePool, from: i64, into: i64) -> Result<(), ThemeError> {
    if from == into {
        return Err(ThemeError::BadTarget);
    }
    let mut tx = pool.begin().await?;
    let from_state: String = sqlx::query_scalar("SELECT state FROM themes WHERE id = ?")
        .bind(from)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ThemeError::NotFound)?;
    transition(&from_state, Action::Merge)?;
    let into_state: String = sqlx::query_scalar("SELECT state FROM themes WHERE id = ?")
        .bind(into)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ThemeError::NotFound)?;
    if !matches!(into_state.as_str(), "proposed" | "accepted") {
        return Err(ThemeError::BadTarget);
    }
    let stamp = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT OR IGNORE INTO theme_members
           (theme_id, member_kind, member_id, origin, excluded, added_at)
         SELECT ?, member_kind, member_id, 'owner', excluded, ? FROM theme_members WHERE theme_id = ?",
    )
    .bind(into)
    .bind(&stamp)
    .bind(from)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM theme_members WHERE theme_id = ?")
        .bind(from)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE themes SET state = 'merged', merged_into = ?, decided_at = ?, synthesis = NULL,
                synthesis_at = NULL, synthesis_model = NULL, synthesis_basis = NULL
          WHERE id = ?",
    )
    .bind(into)
    .bind(&stamp)
    .bind(from)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE themes SET merged_into = ? WHERE merged_into = ?")
        .bind(into)
        .bind(from)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn set_member(
    pool: &SqlitePool,
    theme_id: i64,
    kind: MemberKind,
    member_id: i64,
    excluded: bool,
) -> Result<(), ThemeError> {
    if state_of(pool, theme_id).await? == "merged" {
        return Err(ThemeError::NotAllowed);
    }
    sqlx::query(
        "INSERT INTO theme_members (theme_id, member_kind, member_id, origin, excluded, added_at)
         VALUES (?, ?, ?, 'owner', ?, ?)
         ON CONFLICT(theme_id, member_kind, member_id)
         DO UPDATE SET excluded = ?, origin = 'owner'",
    )
    .bind(theme_id)
    .bind(kind.as_str())
    .bind(member_id)
    .bind(i64::from(excluded))
    .bind(Utc::now().to_rfc3339())
    .bind(i64::from(excluded))
    .execute(pool)
    .await?;
    Ok(())
}

/// The owner puts a member into a theme (or takes back one they had excluded).
pub async fn add_member(
    pool: &SqlitePool,
    theme_id: i64,
    kind: MemberKind,
    member_id: i64,
) -> Result<(), ThemeError> {
    set_member(pool, theme_id, kind, member_id, false).await
}

/// The owner takes a member out; the row stays, marked `excluded`, so the pass does not bring it back.
pub async fn exclude_member(
    pool: &SqlitePool,
    theme_id: i64,
    kind: MemberKind,
    member_id: i64,
) -> Result<(), ThemeError> {
    set_member(pool, theme_id, kind, member_id, true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cand(kind: MemberKind, id: i64, v: [f32; 3]) -> Candidate {
        Candidate {
            key: (kind, id),
            vector: v.to_vec(),
        }
    }
    fn near_x(kind: MemberKind, id: i64, e: f32) -> Candidate {
        cand(kind, id, [1.0, e, 0.0])
    }
    fn near_y(kind: MemberKind, id: i64, e: f32) -> Candidate {
        cand(kind, id, [e, 1.0, 0.0])
    }

    #[test]
    fn two_separate_groups_become_two_themes_of_mixed_kinds() {
        let c = vec![
            near_x(MemberKind::Note, 1, 0.00),
            near_x(MemberKind::Knowledge, 2, 0.05),
            near_x(MemberKind::Knowledge, 3, 0.10),
            near_y(MemberKind::Note, 4, 0.00),
            near_y(MemberKind::Note, 5, 0.05),
            near_y(MemberKind::Knowledge, 6, 0.10),
        ];
        let themes = plan_new_themes(&c, &[]);
        assert_eq!(themes.len(), 2);
        assert!(themes.iter().all(|t| t.len() == 3));
    }

    #[test]
    fn a_group_that_overlaps_a_blocking_theme_by_half_is_not_proposed_again() {
        let c: Vec<_> = (1..=4)
            .map(|i| near_x(MemberKind::Knowledge, i, 0.01 * i as f32))
            .collect();
        let refused: HashSet<MemberKey> = [(MemberKind::Knowledge, 1), (MemberKind::Knowledge, 2)]
            .into_iter()
            .collect();
        // Jaccard {1,2}/{1,2,3,4} = 0.5: blocked. A refused set of one member is 0.25: not blocked.
        assert!(plan_new_themes(&c, &[refused]).is_empty());
        let weak: HashSet<MemberKey> = [(MemberKind::Knowledge, 1)].into_iter().collect();
        assert_eq!(plan_new_themes(&c, &[weak]).len(), 1);
    }

    #[test]
    fn at_most_three_new_themes_per_run_and_a_member_joins_one() {
        let mut c = Vec::new();
        for g in 0..5 {
            for i in 0..3 {
                let mut v = vec![0.0_f32; 5];
                v[g] = 1.0;
                v[(g + 1) % 5] = 0.01 * i as f32;
                c.push(Candidate {
                    key: (MemberKind::Knowledge, (g * 10 + i) as i64),
                    vector: v,
                });
            }
        }
        let themes = plan_new_themes(&c, &[]);
        assert_eq!(themes.len(), MAX_NEW_THEMES_PER_RUN);
        let all: Vec<_> = themes.iter().flatten().collect();
        assert_eq!(all.len(), all.iter().collect::<HashSet<_>>().len());
    }

    #[test]
    fn the_provisional_name_is_never_the_title_of_a_row_that_quoted_the_owner() {
        let rows = [
            NameRow {
                title: "Quoted title with note text",
                quoted: true,
                cosine: 0.99,
            },
            NameRow {
                title: "Plain title",
                quoted: false,
                cosine: 0.80,
            },
        ];
        assert_eq!(provisional_name(&rows), "Plain title");
        let only_quoted = [NameRow {
            title: "x",
            quoted: true,
            cosine: 0.9,
        }];
        assert_eq!(provisional_name(&only_quoted), "Untitled theme");
        assert_eq!(provisional_name(&[]), "Untitled theme");
        let long = "é".repeat(120);
        let rows = [NameRow {
            title: &long,
            quoted: false,
            cosine: 0.5,
        }];
        assert_eq!(provisional_name(&rows).chars().count(), 80);
    }

    #[tokio::test]
    async fn migration_0244_creates_the_theme_tables_without_foreign_keys() {
        let pool = crate::testdb::fresh_pool().await;
        let theme_cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('themes') ORDER BY cid")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            theme_cols,
            [
                "id",
                "name",
                "state",
                "merged_into",
                "model",
                "dim",
                "centroid",
                "synthesis",
                "synthesis_at",
                "synthesis_model",
                "synthesis_basis",
                "created_at",
                "decided_at"
            ]
        );
        let fks: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_foreign_key_list('theme_members')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(fks, 0, "a member that vanished must read `missing`, not block a delete");
        // The primary key is (theme_id, member_kind, member_id): the same member twice is refused.
        let insert = "INSERT INTO theme_members (theme_id, member_kind, member_id, added_at) \
                      VALUES (1, 'note', 7, 't')";
        sqlx::query(insert).execute(&pool).await.unwrap();
        assert!(sqlx::query(insert).execute(&pool).await.is_err());
    }

    #[test]
    fn the_vocabularies_are_the_ones_of_the_spec() {
        assert_eq!(STATES, ["proposed", "accepted", "refused", "merged"]);
        assert_eq!(MemberKind::Note.as_str(), "note");
        assert_eq!(MemberKind::Knowledge.as_str(), "knowledge");
        assert_eq!(ORIGINS, ["system", "owner"]);
    }

    /// Only `themes.rs`, its routes and the project-forget statement name the theme tables (spec 10).
    #[test]
    fn only_themes_its_routes_and_project_exit_name_the_theme_tables() {
        let mut scanned = 0;
        for path in crate::source_scan::rust_files() {
            scanned += 1;
            let module = crate::source_scan::module_path(&path);
            if matches!(
                module.as_str(),
                "themes.rs" | "http/themes.rs" | "project_exit.rs"
            ) {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let production = crate::source_scan::production_code(&source);
            for table in ["theme_members", "FROM themes", "INTO themes", "UPDATE themes"] {
                assert!(!production.contains(table), "{module} names `{table}`");
            }
        }
        assert!(scanned > 20);
    }

    #[test]
    fn themes_rs_cannot_write_knowledge_the_feed_or_the_outbox() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/themes.rs"),
        )
        .unwrap();
        let production = crate::source_scan::production_code(&source);
        for banned in ["INSERT INTO knowledge", "feed::append", "outbox"] {
            assert!(!production.contains(banned), "themes.rs names `{banned}`");
        }
    }

    #[test]
    fn the_pass_is_due_weekly_and_retries_with_backoff_until_spent() {
        let now = chrono::Utc::now();
        let wk = chrono::Duration::days(7);
        assert!(pass_due(now, None, 0));
        assert!(
            !pass_due(now, Some(now - chrono::Duration::days(3)), 0),
            "sealed for the week"
        );
        assert!(pass_due(now, Some(now - wk - chrono::Duration::minutes(1)), 0));
        // One failed attempt just now: not yet; after the backoff: yes.
        assert!(!pass_due(now, Some(now), 1));
        assert!(pass_due(now, Some(now - chrono::Duration::hours(24)), 1));
        // Attempts spent: wait the normal week.
        assert!(!pass_due(
            now,
            Some(now - chrono::Duration::days(2)),
            MAX_PASS_ATTEMPTS
        ));
    }

    #[test]
    fn the_centroid_is_the_mean() {
        assert_eq!(
            centroid(&[vec![1.0, 0.0], vec![3.0, 2.0]]),
            vec![2.0_f32, 1.0]
        );
        assert!(centroid(&[]).is_empty());
    }

    use super::tests_support::knowledge_row;

    #[tokio::test]
    async fn the_pass_proposes_a_theme_from_distiller_and_owner_rows_but_not_the_consolidator() {
        let pool = crate::testdb::fresh_pool().await;
        for (i, (src, st)) in [
            ("distiller", "active"),
            ("owner", "proposed"),
            ("distiller", "observed"),
        ]
        .into_iter()
        .enumerate()
        {
            knowledge_row(&pool, &format!("t{i}"), src, st, [1.0, 0.01 * i as f32, 0.0]).await;
        }
        knowledge_row(&pool, "counter", "consolidator", "active", [1.0, 0.0, 0.0]).await;
        knowledge_row(&pool, "old", "distiller", "archived", [1.0, 0.0, 0.0]).await;
        let made = cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap();
        assert_eq!(made, 1);
        let (state, name, members): (String, String, i64) = sqlx::query_as(
            "SELECT t.state, t.name, (SELECT COUNT(*) FROM theme_members m WHERE m.theme_id = t.id)
               FROM themes t",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((state.as_str(), members), ("proposed", 3));
        assert!(name.starts_with('t'), "named after a member title, got {name}");
        // A second run on the same data proposes nothing (the blocking sets cover the group).
        assert_eq!(
            cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_refused_theme_does_not_come_back() {
        let pool = crate::testdb::fresh_pool().await;
        for i in 0..3 {
            knowledge_row(&pool, "x", "distiller", "active", [1.0, 0.01 * i as f32, 0.0]).await;
        }
        cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap();
        sqlx::query("UPDATE themes SET state = 'refused'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn an_accepted_theme_absorbs_a_new_close_member_and_recomputes_its_centroid() {
        let pool = crate::testdb::fresh_pool().await;
        for i in 0..3 {
            knowledge_row(&pool, "x", "distiller", "active", [1.0, 0.01 * i as f32, 0.0]).await;
        }
        cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap();
        sqlx::query("UPDATE themes SET state = 'accepted'")
            .execute(&pool)
            .await
            .unwrap();
        let near = knowledge_row(&pool, "y", "distiller", "active", [1.0, 0.04, 0.0]).await;
        let far = knowledge_row(&pool, "z", "distiller", "active", [0.0, 1.0, 0.0]).await;
        cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap();
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT member_id FROM theme_members WHERE member_kind = 'knowledge' ORDER BY 1",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(ids.contains(&near) && !ids.contains(&far));
        let blob: Vec<u8> = sqlx::query_scalar("SELECT centroid FROM themes")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(
            blob,
            crate::embed::encode(&[1.0, 0.01, 0.0]),
            "the centroid moved with the new member"
        );
    }

    #[tokio::test]
    async fn an_excluded_member_does_not_come_back_and_a_null_centroid_is_recomputed() {
        let pool = crate::testdb::fresh_pool().await;
        let ids = [
            knowledge_row(&pool, "a", "distiller", "active", [1.0, 0.0, 0.0]).await,
            knowledge_row(&pool, "b", "distiller", "active", [1.0, 0.02, 0.0]).await,
            knowledge_row(&pool, "c", "distiller", "active", [1.0, 0.04, 0.0]).await,
        ];
        cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap();
        sqlx::query("UPDATE themes SET state = 'accepted', centroid = NULL")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE theme_members SET excluded = 1 WHERE member_id = ?")
            .bind(ids[2])
            .execute(&pool)
            .await
            .unwrap();
        cluster_pass(&pool, "m", chrono::Utc::now()).await.unwrap();
        let excluded: i64 =
            sqlx::query_scalar("SELECT excluded FROM theme_members WHERE member_id = ?")
                .bind(ids[2])
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(excluded, 1);
        let null: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM themes WHERE centroid IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(null, 0);
    }

    #[test]
    fn the_transition_table_allows_exactly_the_cells_of_section_6_3() {
        use Action::*;
        let allowed = [
            ("proposed", Accept, "accepted"),
            ("proposed", Refuse, "refused"),
            ("proposed", Merge, "merged"),
            ("accepted", Refuse, "refused"),
            ("accepted", Merge, "merged"),
            ("refused", Accept, "accepted"),
        ];
        for from in STATES {
            for action in [Accept, Refuse, Merge] {
                let expected = allowed
                    .iter()
                    .find(|(f, a, _)| *f == from && *a == action)
                    .map(|c| c.2);
                assert_eq!(transition(from, action).ok(), expected, "{from} {action:?}");
            }
        }
        assert!(transition("merged", Accept).is_err(), "merged is terminal");
        assert!(transition("proposed", Accept).is_ok() && transition("accepted", Accept).is_err());
    }

    async fn theme(pool: &sqlx::SqlitePool, state: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO themes (name, state, model, dim, created_at) VALUES ('t', ?, 'm', 3, 't') RETURNING id",
        )
        .bind(state)
        .fetch_one(pool)
        .await
        .unwrap()
    }
    async fn member(pool: &sqlx::SqlitePool, t: i64, kind: &str, id: i64) {
        sqlx::query(
            "INSERT INTO theme_members (theme_id, member_kind, member_id, added_at) VALUES (?, ?, ?, 't')",
        )
        .bind(t)
        .bind(kind)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn merging_moves_members_as_owner_origin_and_repoints_without_chains() {
        let pool = crate::testdb::fresh_pool().await;
        let (a, b, c) = (
            theme(&pool, "proposed").await,
            theme(&pool, "accepted").await,
            theme(&pool, "accepted").await,
        );
        member(&pool, a, "note", 1).await;
        merge(&pool, a, b).await.unwrap();
        let origin: String = sqlx::query_scalar(
            "SELECT origin FROM theme_members WHERE theme_id = ? AND member_id = 1",
        )
        .bind(b)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(origin, "owner");
        merge(&pool, b, c).await.unwrap();
        let into: Vec<(i64, Option<i64>)> =
            sqlx::query_as("SELECT id, merged_into FROM themes ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            into,
            vec![(a, Some(c)), (b, Some(c)), (c, None)],
            "a points at the live theme"
        );
        // Never into a refused, merged or itself.
        let r = theme(&pool, "refused").await;
        assert!(
            merge(&pool, c, r).await.is_err()
                && merge(&pool, c, a).await.is_err()
                && merge(&pool, c, c).await.is_err()
        );
    }

    #[tokio::test]
    async fn rename_is_non_empty_and_at_most_80_chars_and_members_obey_the_state() {
        let pool = crate::testdb::fresh_pool().await;
        let t = theme(&pool, "proposed").await;
        assert!(rename(&pool, t, "  ").await.is_err());
        assert!(rename(&pool, t, &"x".repeat(81)).await.is_err());
        rename(&pool, t, "Sleep").await.unwrap();
        add_member(&pool, t, MemberKind::Note, 5).await.unwrap();
        exclude_member(&pool, t, MemberKind::Note, 5).await.unwrap();
        let merged = theme(&pool, "merged").await;
        assert!(add_member(&pool, merged, MemberKind::Note, 5).await.is_err());
    }

    #[tokio::test]
    async fn refusing_an_accepted_theme_deletes_its_synthesis() {
        let pool = crate::testdb::fresh_pool().await;
        let t = theme(&pool, "accepted").await;
        sqlx::query("UPDATE themes SET synthesis = 's', synthesis_basis = '[1]', synthesis_at = 't', synthesis_model = 'm' WHERE id = ?")
            .bind(t)
            .execute(&pool)
            .await
            .unwrap();
        refuse(&pool, t).await.unwrap();
        let s: Option<String> = sqlx::query_scalar("SELECT synthesis FROM themes WHERE id = ?")
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(s, None);
    }

    // ---- Phase 6: synthesis ----

    fn canned(stdout: &str) -> crate::execution::runner::FakeCommandRunner {
        crate::execution::runner::FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(crate::execution::runner::RunOutcome {
                exit_code: 0,
                stdout: stdout.to_owned(),
                stderr: String::new(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
                model_requested: false,
            })),
            ..Default::default()
        }
    }

    fn canned_err() -> crate::execution::runner::FakeCommandRunner {
        crate::execution::runner::FakeCommandRunner {
            fail_times: std::sync::Mutex::new(1),
            ..Default::default()
        }
    }

    async fn synth_theme(pool: &SqlitePool, state: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO themes (name, state, model, dim, created_at) VALUES ('t', ?, 'm', 3, 't')
             RETURNING id",
        )
        .bind(state)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn synth_member(pool: &SqlitePool, theme: i64, kind: &str, id: i64) {
        sqlx::query(
            "INSERT INTO theme_members (theme_id, member_kind, member_id, added_at) VALUES (?, ?, ?, 't')",
        )
        .bind(theme)
        .bind(kind)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn the_synthesis_input_scrubs_and_marks_note_text_as_owner_context() {
        let input = synthesis_prompt(
            &[KnowledgeText {
                title: "Sleep rule",
                body: "token sk-ABCDEF1234567890XYZ here",
            }],
            &[(4, "my note body".to_owned())],
        );
        assert!(!input.contains("sk-ABCDEF1234567890XYZ"), "scrubbed");
        assert!(input.contains("owner context, do not quote"));
        assert!(input.contains("my note body"));
        assert!(input.contains("Sleep rule"));
    }

    #[test]
    fn only_active_or_observed_knowledge_feeds_a_synthesis() {
        for (status, feeds) in [
            ("active", true),
            ("observed", true),
            ("archived", false),
            ("reverted", false),
            ("rejected", false),
            ("expired", false),
        ] {
            assert_eq!(feeds_synthesis(status), feeds, "{status}");
        }
    }

    #[tokio::test]
    async fn synthesise_runs_only_for_accepted_themes_and_stores_text_with_its_basis() {
        use crate::map::intent::Extractor;
        let pool = crate::testdb::fresh_pool().await;
        let now = chrono::Utc::now();
        let proposed = synth_theme(&pool, "proposed").await;
        let runner = canned("A summary.");
        assert_eq!(
            synthesise(&pool, Extractor::Cli(&runner), proposed, now).await,
            Err(SynthesisError::NotAllowed)
        );
        let t = synth_theme(&pool, "accepted").await;
        let k = knowledge_row(&pool, "Row", "distiller", "active", [1.0, 0.0, 0.0]).await;
        synth_member(&pool, t, "knowledge", k).await;
        synthesise(&pool, Extractor::Cli(&runner), t, now)
            .await
            .unwrap();
        let (s, basis, model): (String, String, String) = sqlx::query_as(
            "SELECT synthesis, synthesis_basis, synthesis_model FROM themes WHERE id = ?",
        )
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(s, "A summary.");
        assert_eq!(basis, format!("[\"knowledge:{k}\"]"));
        assert!(!model.is_empty());
    }

    #[tokio::test]
    async fn a_failed_synthesis_keeps_the_previous_text() {
        use crate::map::intent::Extractor;
        let pool = crate::testdb::fresh_pool().await;
        let t = synth_theme(&pool, "accepted").await;
        sqlx::query("UPDATE themes SET synthesis = 'old' WHERE id = ?")
            .bind(t)
            .execute(&pool)
            .await
            .unwrap();
        let runner = canned_err();
        assert_eq!(
            synthesise(&pool, Extractor::Cli(&runner), t, chrono::Utc::now()).await,
            Err(SynthesisError::Failed)
        );
        let kept: String = sqlx::query_scalar("SELECT synthesis FROM themes WHERE id = ?")
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, "old");
    }

    #[test]
    fn the_output_is_cut_at_2000_chars_on_a_char_boundary() {
        assert_eq!(clip_synthesis(&"\u{e9}".repeat(2500)).chars().count(), 2000);
    }

    #[test]
    fn a_synthesis_is_refreshed_after_3_member_changes_or_20_percent_and_a_week() {
        let week = chrono::Duration::days(7);
        let now = chrono::Utc::now();
        let old = Some(now - week - chrono::Duration::hours(1));
        let basis: Vec<String> = (1..=30).map(|i| format!("knowledge:{i}")).collect();
        let mut now_members = basis.clone();
        now_members[0] = "knowledge:100".into();
        now_members[1] = "knowledge:101".into();
        assert!(!refresh_due(&basis, &now_members, old, now));
        now_members[2] = "knowledge:102".into();
        assert!(refresh_due(&basis, &now_members, old, now));
        assert!(!refresh_due(
            &basis,
            &now_members,
            Some(now - chrono::Duration::days(6)),
            now
        ));
        let small: Vec<String> = (1..=5).map(|i| format!("note:{i}")).collect();
        let mut changed = small.clone();
        changed[0] = "note:50".into();
        assert!(refresh_due(&small, &changed, old, now));
        assert!(!refresh_due(&small, &small, old, now), "no change, no refresh");
        assert!(refresh_due(&[], &small, None, now));
    }

    #[test]
    fn the_force_route_is_limited_to_one_synthesis_per_hour() {
        let now = chrono::Utc::now();
        assert!(!force_allowed(Some(now - chrono::Duration::minutes(10)), now));
        assert!(force_allowed(Some(now - chrono::Duration::minutes(61)), now));
        assert!(force_allowed(None, now));
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// A knowledge row of `source` and `status` with a stored vector for model `m`.
    pub(crate) async fn knowledge_row(
        pool: &sqlx::SqlitePool,
        title: &str,
        source: &str,
        status: &str,
        v: [f32; 3],
    ) -> i64 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO knowledge (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'machine', NULL, ?, 'memory', ?, 'b', ?, 't') RETURNING id",
        )
        .bind(source)
        .bind(title)
        .bind(status)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        crate::embed::store_in(&mut conn, id, "m", &v).await.unwrap();
        id
    }
}
