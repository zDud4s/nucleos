//! Themes above the entities (spec 2026-10-22 section 6). Proposes, never decides; the synthesis (B6,
//! Phase 6) is owner-only text and never enters a prompt, the feed, the outbox or a log.
use std::collections::HashSet;

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
}
