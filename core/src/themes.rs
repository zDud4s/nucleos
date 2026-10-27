//! Themes above the entities (spec 2026-10-22 section 6). Proposes, never decides; the synthesis (B6,
//! Phase 6) is owner-only text and never enters a prompt, the feed, the outbox or a log.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
