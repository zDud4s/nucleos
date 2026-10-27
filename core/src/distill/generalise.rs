//! The daily generalisation pass: groups look-alike `source = 'distiller'` rows by embedding and asks the
//! distiller's model for ONE general lesson per group that rests on three distinct subjects (spec 5).
//!
//! It reads no owner text (the notes seam, [`notes_near`], is Phase 6's) and writes only `proposed` rows: the
//! owner approves a general lesson or it never reaches a prompt. State lives in `schema_meta` keys, no table.

mod plan;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

use self::plan::{
    CLUSTER_THRESHOLD, Candidate, GroupMemory, MAX_EVIDENCE, MAX_GROUP_SIZE,
    MAX_PASS_GROUPS_PER_DAY, Member, Outcome, Placement, Reach, Seal, Subject, due, members_hash,
    memory_key, order_groups, scope_for, seal_start, settle, stale_keys, subjects_of,
};
use super::extract;
use crate::embed::Embedder;
use crate::knowledge::Layer;
use crate::map::intent::Extractor;

const SEAL_AT_KEY: &str = "distiller.generalised_at";
const SEAL_ATTEMPTS_KEY: &str = "distiller.generalised_attempts";

/// How far back a row still counts as material for a lesson.
const WINDOW_DAYS: i64 = 90;

/// A set of look-alike rows with what they are seen across.
pub(crate) struct Group {
    pub members: Vec<Member>,
    pub subjects: BTreeSet<Subject>,
    pub centroid: Vec<f32>,
    pub min_id: i64,
}

/// Live distiller rows that do not quote the owner, grouped by vector. A row without a vector under `model` is left out
/// (the backfill embeds it later); lessons never feed lessons.
pub(crate) async fn form_groups(
    pool: &SqlitePool,
    model: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<Group>> {
    let rows = sqlx::query(
        "SELECT k.id, k.scope_kind, k.scope_id, k.evidence, k.title, k.body FROM knowledge k
          WHERE k.source = 'distiller' AND k.created_at >= ? AND k.scope_kind <> 'machine'
            AND (k.status IN ('active', 'proposed')
                 OR (k.scope_kind = 'observation' AND k.status = 'observed'))
            AND COALESCE(k.distill_cause, '') <> 'generalised'
            AND NOT EXISTS (SELECT 1 FROM knowledge_events e
                             WHERE e.knowledge_id = k.id AND e.note = 'quoted_owner_text')
          ORDER BY k.id",
    )
    .bind((now - chrono::Duration::days(WINDOW_DAYS)).to_rfc3339())
    .fetch_all(pool)
    .await?;
    let members: Vec<Member> = rows
        .iter()
        .map(|row| Member {
            id: row.get("id"),
            scope_kind: row.get("scope_kind"),
            scope_id: row.get("scope_id"),
            evidence: row.get("evidence"),
            title: row.get("title"),
            body: row.get("body"),
        })
        .collect();
    let ids: Vec<i64> = members.iter().map(|m| m.id).collect();
    let vectors = crate::embed::vectors_for(pool, &ids, model).await?;
    let items: Vec<(i64, &[f32])> = members
        .iter()
        .filter_map(|m| vectors.get(&m.id).map(|v| (m.id, v.as_slice())))
        .collect();
    let by_id: HashMap<i64, &Member> = members.iter().map(|m| (m.id, m)).collect();
    let clusters = crate::embed::cluster::cluster(
        &items,
        crate::embed::cluster::Params {
            threshold: CLUSTER_THRESHOLD,
            min_size: 1,
            max_size: MAX_GROUP_SIZE,
        },
    );
    let mut groups = Vec::new();
    for cluster in clusters {
        let group_members: Vec<Member> = cluster
            .iter()
            .filter_map(|id| by_id.get(id).map(|m| (*m).clone()))
            .collect();
        let mut centroid: Vec<f32> = Vec::new();
        for id in &cluster {
            let Some(vector) = vectors.get(id) else {
                continue;
            };
            if centroid.is_empty() {
                centroid = vec![0.0; vector.len()];
            }
            if centroid.len() == vector.len() {
                for (sum, x) in centroid.iter_mut().zip(vector) {
                    *sum += x;
                }
            }
        }
        let Some(&min_id) = cluster.first() else {
            continue;
        };
        groups.push(Group {
            subjects: subjects_of(&group_members),
            members: group_members,
            centroid,
            min_id,
        });
    }
    Ok(groups)
}

/// A note close to a group: only its id and cosine. The id is the distinct subject `note:<id>` (it counts for
/// `MIN_SUBJECTS` and proves `about`, owner decision P5). Texts are NOT here: Phase 6 fetches them itself
/// with `owner_notes::texts_for_ids` as CONTEXT for the model only, never quoted and never stored.
pub(crate) struct NearNote {
    pub id: i64,
    #[allow(dead_code)] // Phase 6 reads it when it fills the seam.
    pub cosine: f32,
}

/// Notes close to a group: Phase 6's seam. Phase 6 implements it on `owner_notes::nearest_to_vector`
/// (`group.centroid`, `model`, `CLUSTER_THRESHOLD`, `MAX_NOTES_PER_GROUP`) and adds the sanctioned calls to the
/// boundary sweep (spec section 10). Returns empty until then; this module names no owner-notes API.
pub(crate) async fn notes_near(
    _pool: &SqlitePool,
    _group: &Group,
    _model: &str,
) -> Vec<NearNote> {
    Vec::new()
}

const PASS_STANDING: &str = "You read several look-alike notes an assistant wrote down at different times, in \
different projects, conversations or weeks. Answer with a JSON array and nothing else: either one general lesson \
or `[]`. A lesson is an object with `layer` (semantic or procedural), `title` (one line), `body` (short), \
`about` (`owner` when it describes the person the assistant works for, otherwise `work`) and `files` (repository \
paths it concerns). Write only what the rows share; never invent. Never quote the owner context. `[]` is a good \
answer when the rows only look alike.";

/// The JSON schema of the answer: at most one lesson.
fn lesson_format() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "maxItems": 1,
        "items": {
            "type": "object",
            "properties": {
                "layer": { "type": "string", "enum": ["semantic", "procedural"] },
                "title": { "type": "string" },
                "body": { "type": "string" },
                "about": { "type": "string", "enum": ["owner", "work"] },
                "files": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["layer", "title", "body"]
        }
    })
}

/// The one lesson a group produced, already checked.
pub(crate) struct Lesson {
    pub layer: Layer,
    pub title: String,
    pub body: String,
    pub owner: bool,
    pub files: Vec<String>,
}

/// An answer is `[]` or one semantic or procedural lesson; anything else in the array is dropped.
pub(crate) fn parse_lessons(answer: &str) -> Result<Vec<Lesson>, extract::ParseError> {
    let values = serde_json::from_str::<Vec<serde_json::Value>>(extract::strip_fence(answer))
        .map_err(|_| extract::ParseError::NotAnArray)?;
    Ok(values
        .iter()
        .filter_map(|v| {
            let o = v.as_object()?;
            let layer = Layer::parse(o.get("layer")?.as_str()?)
                .filter(|l| matches!(l, Layer::Semantic | Layer::Procedural))?;
            let (title, body) = (o.get("title")?.as_str()?.trim(), o.get("body")?.as_str()?.trim());
            if title.is_empty() || body.is_empty() {
                return None;
            }
            let files = o
                .get("files")
                .and_then(|f| f.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            Some(Lesson {
                layer,
                title: title.into(),
                body: body.into(),
                owner: o.get("about").and_then(|a| a.as_str()) == Some("owner"),
                files,
            })
        })
        .take(1)
        .collect())
}

/// The dossier of one group: its look-alike rows, scrubbed, and the owner context when a note is near.
fn dossier(group: &Group, note_texts: &[String]) -> String {
    let mut out = String::from("--- LOOK-ALIKE ROWS ---\n");
    for member in &group.members {
        out.push_str(&format!(
            "- {}\n  {}\n",
            extract::scrub(&member.title),
            extract::scrub(&extract::clip_head(&member.body, 600))
        ));
    }
    if !note_texts.is_empty() {
        out.push_str("--- OWNER CONTEXT (do not quote) ---\n");
        for text in note_texts {
            out.push_str(&format!("{}\n", extract::scrub(text)));
        }
    }
    out
}

/// What the write did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Written {
    #[allow(dead_code)] // the new row's id: the pass only needs to know it was written.
    Proposed(i64),
    Reconfirmed,
    /// The owner already refused it, or it has nowhere to live: nothing was written.
    Fenced,
}

/// The scopes a lesson is compared with: its own, and for `machine` also its members' projects.
fn fence_scopes(reach: &Reach, projects: &BTreeSet<String>) -> Vec<(&'static str, Option<String>)> {
    match reach {
        Reach::Nowhere => vec![],
        Reach::Project(p) => vec![("project", Some(p.clone()))],
        Reach::Machine => std::iter::once(("machine", None))
            .chain(projects.iter().map(|p| ("project", Some(p.clone()))))
            .collect(),
    }
}

const FENCE_STATUSES: [&str; 4] = ["proposed", "active", "rejected", "reverted"];

/// Write the lesson as a `proposed` row behind the refusal fence, in one transaction. The group's own members are
/// always excluded from the fence: a member at 0.95 is the evidence, not the duplicate.
pub(crate) async fn write_lesson(
    pool: &SqlitePool,
    embedder: &dyn Embedder,
    group: &Group,
    lesson: &Lesson,
    placement: &Placement,
    note_texts: &[String],
) -> sqlx::Result<Written> {
    if placement.reach == Reach::Nowhere {
        return Ok(Written::Fenced);
    }
    let vector = embedder
        .embed(&crate::embed::text_of(&lesson.title, &lesson.body))
        .await
        .ok();
    let model = embedder.model();
    let member_ids: Vec<i64> = group.members.iter().map(|m| m.id).collect();
    let projects: BTreeSet<String> = group
        .members
        .iter()
        .filter_map(|m| m.project().map(str::to_owned))
        .collect();
    let fingerprint = crate::knowledge::title_fingerprint(&lesson.title);
    let evidence = serde_json::Value::Array(
        member_ids
            .iter()
            .take(MAX_EVIDENCE)
            .map(|id| serde_json::json!({"t": "knowledge", "id": id}))
            .collect(),
    )
    .to_string();

    let mut tx = pool.begin().await?;
    let mut best: Option<(i64, f32)> = None;
    for (scope_kind, scope_id) in fence_scopes(&placement.reach, &projects) {
        let mut hit: Option<i64> = None;
        if let Some(fingerprint) = fingerprint.as_deref() {
            let same: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM knowledge
                  WHERE fingerprint = ? AND scope_kind = ? AND scope_id IS ?
                    AND status IN ('proposed', 'active', 'rejected', 'reverted') ORDER BY id",
            )
            .bind(fingerprint)
            .bind(scope_kind)
            .bind(scope_id.as_deref())
            .fetch_all(&mut *tx)
            .await?;
            hit = same.into_iter().find(|id| !member_ids.contains(id));
        }
        if hit.is_none()
            && let Some(vector) = vector.as_deref()
            && let Some((id, similarity)) = crate::embed::nearest_in_scope(
                &mut tx,
                scope_kind,
                scope_id.as_deref(),
                &FENCE_STATUSES,
                model,
                vector,
                &member_ids,
            )
            .await?
        {
            if similarity >= super::SIM_SAME {
                hit = Some(id);
            } else if similarity >= super::SIM_NEAR
                && best.is_none_or(|(_, top)| similarity > top)
            {
                best = Some((id, similarity));
            }
        }
        if let Some(hit) = hit {
            let status: String = sqlx::query_scalar("SELECT status FROM knowledge WHERE id = ?")
                .bind(hit)
                .fetch_one(&mut *tx)
                .await?;
            // A refused or reverted lesson is the owner's no: it is not reconfirmed and not asked again
            // (precedent: consolidate.rs). A live one is renewed by this new evidence.
            let written = if matches!(status.as_str(), "proposed" | "active") {
                crate::knowledge::reconfirm_id_in(&mut tx, hit, &evidence).await?;
                Written::Reconfirmed
            } else {
                Written::Fenced
            };
            tx.commit().await?;
            return Ok(written);
        }
    }

    let (kind, _) = extract::door(lesson.layer);
    let points_at = if lesson.files.is_empty() {
        None
    } else {
        Some(serde_json::json!(lesson.files).to_string())
    };
    let reasoning = format!("seen across {} look-alike rows", group.members.len());
    let project_id = match &placement.reach {
        Reach::Project(p) => Some(p.as_str()),
        _ => None,
    };
    let scope = match &placement.reach {
        Reach::Machine => Some(crate::knowledge::Scope::Machine),
        _ => None,
    };
    let (new_id, _) = crate::knowledge::propose_in_with(
        &mut tx,
        crate::knowledge::Declaration {
            project_id,
            origin_run_id: None,
            kind,
            title: &lesson.title,
            body: &lesson.body,
            reasoning: &reasoning,
            supersedes: None,
        },
        &crate::knowledge::Provenance {
            source: Some("distiller"),
            distill_cause: Some("generalised"),
            evidence: Some(&evidence),
            points_at: points_at.as_deref(),
            fingerprint: fingerprint.as_deref(),
            layer: Some(lesson.layer),
            about: placement.about,
            scope,
        },
    )
    .await
    .map_err(|e| match e {
        crate::knowledge::ProposeError::Db(db) => db,
        _ => sqlx::Error::Protocol("a general lesson was refused".into()),
    })?;
    let sources: Vec<&str> = note_texts.iter().map(String::as_str).collect();
    if crate::governance::quote_guard::quotes(
        &format!("{}\n{}", lesson.title, lesson.body),
        &sources,
    ) {
        crate::knowledge::note_quoted_owner_text_in(&mut tx, new_id).await?;
    }
    if let Some(vector) = vector.as_deref() {
        crate::embed::store_in(&mut tx, new_id, model, vector).await?;
        if let Some((of_id, _)) = best {
            crate::knowledge::note_near_duplicate_in(&mut tx, new_id, of_id).await?;
        }
    }
    tx.commit().await?;
    nucleos_base::wake::bump(nucleos_base::wake::Topic::Proposals);
    Ok(Written::Proposed(new_id))
}

async fn meta(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
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

async fn read_seal(pool: &SqlitePool) -> Seal {
    let at = meta(pool, SEAL_AT_KEY)
        .await
        .and_then(|raw| DateTime::parse_from_rfc3339(&raw).ok())
        .map(|at| at.with_timezone(&Utc));
    let attempts = meta(pool, SEAL_ATTEMPTS_KEY)
        .await
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(0);
    Seal { at, attempts }
}

async fn write_seal(pool: &SqlitePool, seal: &Seal) -> sqlx::Result<()> {
    if let Some(at) = seal.at {
        set_meta(pool, SEAL_AT_KEY, &at.to_rfc3339()).await?;
    }
    set_meta(pool, SEAL_ATTEMPTS_KEY, &seal.attempts.to_string()).await
}

/// Forget the memory of groups that no longer exist and of the oldest beyond the ceiling.
async fn prune_memory(pool: &SqlitePool, groups: &[Group]) {
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT key FROM schema_meta WHERE key LIKE 'generalise.group.%'")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let live: BTreeSet<i64> = groups.iter().map(|g| g.min_id).collect();
    for key in stale_keys(&keys, &live) {
        let _ = sqlx::query("DELETE FROM schema_meta WHERE key = ?")
            .bind(key)
            .execute(pool)
            .await;
    }
}

/// A group with the context the pass gathered for it.
struct Prepared<'a> {
    group: &'a Group,
    near: Vec<NearNote>,
    note_texts: Vec<String>,
    candidate: Candidate,
}

/// One attempt. `Failed` only when at least one group was asked and every call failed.
pub(crate) async fn pass(
    pool: &SqlitePool,
    asked: Extractor<'_>,
    embedder: &dyn Embedder,
    now: DateTime<Utc>,
) -> sqlx::Result<Outcome> {
    // 5.5: cover what approved lessons already cover.
    crate::knowledge::tidy_covered(pool).await?;
    let groups = form_groups(pool, embedder.model(), now).await?;
    let mut prepared = Vec::new();
    for group in &groups {
        // Phase 6's seam; empty today.
        let near = notes_near(pool, group, embedder.model()).await;
        // Phase 6: owner_notes::texts_for_ids(near ids).
        let note_texts: Vec<String> = Vec::new();
        let mut subjects: BTreeSet<String> = group.subjects.iter().map(Subject::key).collect();
        subjects.extend(near.iter().map(|n| Subject::Note(n.id).key()));
        let ids: Vec<i64> = group.members.iter().map(|m| m.id).collect();
        let memory: Option<GroupMemory> = meta(pool, &memory_key(group.min_id))
            .await
            .and_then(|raw| serde_json::from_str(&raw).ok());
        prepared.push(Prepared {
            group,
            near,
            note_texts,
            candidate: Candidate {
                min_id: group.min_id,
                hash: members_hash(&ids),
                subjects,
                memory,
            },
        });
    }
    prune_memory(pool, &groups).await;
    let (mut called, mut succeeded) = (0, 0);
    let candidates = prepared.iter().map(|p| p.candidate.clone()).collect();
    for c in order_groups(candidates)
        .into_iter()
        .take(MAX_PASS_GROUPS_PER_DAY)
    {
        let Some(p) = prepared.iter().find(|p| p.candidate.min_id == c.min_id) else {
            continue;
        };
        called += 1;
        let prompt = format!("{PASS_STANDING}\n\n{}", dossier(p.group, &p.note_texts));
        let Ok(answer) = super::ask(asked, prompt, PASS_STANDING, lesson_format()).await else {
            continue;
        };
        // A failed group is skipped, the others go on.
        let Ok(lessons) = parse_lessons(&answer) else {
            continue;
        };
        if let Some(lesson) = lessons.first() {
            let projects = p
                .group
                .members
                .iter()
                .filter_map(|m| m.project().map(str::to_owned))
                .collect();
            let all: BTreeSet<Subject> = p
                .group
                .subjects
                .iter()
                .cloned()
                .chain(p.near.iter().map(|n| Subject::Note(n.id)))
                .collect();
            let placement = scope_for(&projects, &all, lesson.owner);
            if write_lesson(pool, embedder, p.group, lesson, &placement, &p.note_texts)
                .await
                .is_err()
            {
                continue;
            }
        }
        // Success includes `[]` and a fenced duplicate: remember it, or day two asks again.
        let memory = GroupMemory {
            h: c.hash.clone(),
            s: c.subjects.iter().cloned().collect(),
        };
        set_meta(
            pool,
            &memory_key(c.min_id),
            &serde_json::to_string(&memory).unwrap_or_default(),
        )
        .await?;
        succeeded += 1;
    }
    Ok(if called > 0 && succeeded == 0 {
        Outcome::Failed
    } else {
        Outcome::Done
    })
}

/// The door of the worker's `tick`: nothing without an embedder, nothing inside the day, and the seal is written BEFORE
/// the model is asked.
pub(crate) async fn run_if_due(
    pool: &SqlitePool,
    asked: Extractor<'_>,
    embedder: Option<&dyn Embedder>,
    now: DateTime<Utc>,
) {
    let Some(embedder) = embedder else {
        return;
    };
    let seal = read_seal(pool).await;
    if !due(&seal, now) {
        return;
    }
    let started = seal_start(&seal, now);
    if write_seal(pool, &started).await.is_err() {
        return;
    }
    let outcome = match pass(pool, asked, embedder, now).await {
        Ok(outcome) => outcome,
        Err(_) => {
            // The kind only: never a dossier, an answer or a title.
            tracing::warn!("generalisation: the pass hit a database error");
            Outcome::Failed
        }
    };
    if let Err(error) = write_seal(pool, &settle(&started, outcome, now)).await {
        tracing::warn!("generalisation: could not settle its seal: {error}");
    }
}
