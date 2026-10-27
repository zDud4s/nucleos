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

/// The only grouping call. Items are `(position, vector)`; the result is deterministic, lists
/// positions, and is ordered by each group's smallest position.
///
/// Greedy seeding: the lowest unassigned position seeds a group made of every unassigned item whose
/// cosine to the seed reaches the threshold (closest first, capped at `MAX_MEMBERS`); a seed that
/// gathers fewer than `MIN_MEMBERS` stays unassigned. The shared grouper of the chat-observation
/// phase (`embed::cluster`) is not on this branch; if it lands with the same shape, only this
/// adapter changes.
fn cluster_groups(items: &[(i64, Vec<f32>)]) -> Vec<Vec<i64>> {
    let mut assigned = vec![false; items.len()];
    let mut groups: Vec<Vec<i64>> = Vec::new();
    for seed in 0..items.len() {
        if assigned[seed] {
            continue;
        }
        let mut near: Vec<(usize, f32)> = (0..items.len())
            .filter(|i| *i != seed && !assigned[*i])
            .filter_map(|i| {
                crate::embed::cosine(&items[seed].1, &items[i].1)
                    .filter(|c| *c >= CLUSTER_THRESHOLD)
                    .map(|c| (i, c))
            })
            .collect();
        near.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        near.truncate(MAX_MEMBERS - 1);
        if near.len() + 1 < MIN_MEMBERS {
            continue;
        }
        let members: Vec<usize> = std::iter::once(seed)
            .chain(near.iter().map(|(i, _)| *i))
            .collect();
        let mut group: Vec<i64> = members.iter().map(|i| items[*i].0).collect();
        for member in members {
            assigned[member] = true;
        }
        group.sort_unstable();
        groups.push(group);
    }
    groups.sort_by_key(|g| g[0]);
    groups
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

/// One clustering pass: absorb close members into `accepted` themes, recompute centroids, then propose
/// at most `MAX_NEW_THEMES_PER_RUN` new themes. Returns how many new themes it proposed.
pub async fn cluster_pass(
    pool: &SqlitePool,
    model: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let found = candidates(pool, model).await?;
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
