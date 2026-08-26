//! Where the intention layer's rows live.
//!
//! Separate from `map_intent.rs` for the same reason that module knows no SQL: the prompt and the
//! parse are the part worth testing without a database, and this is the part worth testing without
//! a model. Slices 4 and 5 add `map_stamps` and `map_triage` beside this table, and the SQL of the
//! three wants to be together and far from `http.rs`, which is already 19,000 lines.

use crate::chats::Brain;
use crate::map_intent::{Extracted, Kind};
use serde::Serialize;

/// A decision as it sits in the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub id: i64,
    pub spec_slug: String,
    pub section: String,
    pub ordinal: i64,
    pub text: String,
    pub kind: Kind,
    pub brain: String,
    pub extracted_at: String,
    pub approved_at: Option<String>,
}

/// Write one extraction's worth of proposals, all unapproved.
///
/// One timestamp for the whole batch rather than one per row: they were proposed together, the
/// owner reads them together, and `UNIQUE (project_id, spec_slug, ordinal, extracted_at)` uses it
/// to keep two extractions of one spec from colliding on ordinal.
pub async fn record(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    spec_slug: &str,
    brain: Brain,
    decisions: &[Extracted],
) -> sqlx::Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut written = 0;
    for row in decisions {
        sqlx::query(
            "INSERT INTO map_decisions
               (project_id, spec_slug, section, ordinal, text, kind, brain, extracted_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(spec_slug)
        .bind(&row.section)
        .bind(row.ordinal)
        .bind(&row.text)
        .bind(row.kind.as_str())
        .bind(brain.as_str())
        .bind(&now)
        .execute(pool)
        .await?;
        written += 1;
    }
    Ok(written)
}

/// The columns every read of this table selects, named rather than written out at the binding.
///
/// Six of the eight are `TEXT` in one tuple, so a `SELECT` that reordered two of them would still
/// typecheck and the mistake would surface as a decision whose section is somehow the name of a
/// brain. This alias and the `SELECT` below are one thing written twice; changing either without
/// the other is what it exists to make visible. The house shape — see `ErrandRow` in `errands.rs`
/// and `Row` in `project_commands.rs`, both a row of this size read the same way.
type DecisionRow = (i64, String, String, i64, String, String, String, String);

/// The single place a row becomes a [`Decision`].
///
/// `None` for a `kind` the CHECK should have refused. Dropped rather than defaulted: the same
/// argument `Kind::from_wire` makes, and a row that reaches here unreadable is a row nobody can act
/// on either way.
fn from_row(
    (id, spec_slug, section, ordinal, text, kind, brain, extracted_at): DecisionRow,
) -> Option<Decision> {
    Some(Decision {
        id,
        spec_slug,
        section,
        ordinal,
        text,
        kind: Kind::from_wire(&kind)?,
        brain,
        extracted_at,
        approved_at: None,
    })
}

/// What is waiting for the owner in this project.
///
/// Not approved, not retired, oldest extraction first — a pile read in the order it arrived is a
/// pile that ends, and one ordered by anything else is a pile that never does.
pub async fn pending(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<Vec<Decision>> {
    let rows = sqlx::query_as::<_, DecisionRow>(
        "SELECT id, spec_slug, section, ordinal, text, kind, brain, extracted_at
           FROM map_decisions
          WHERE project_id = ? AND approved_at IS NULL AND retired_at IS NULL
          ORDER BY extracted_at, spec_slug, ordinal",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().filter_map(from_row).collect())
}

/// The owner's answer to one line, and whether it landed on anything.
///
/// Approving stamps `approved_at`; rejecting stamps `retired_at`, which is what stops the line
/// being proposed for that spec again and is also the only record that somebody looked and said no.
///
/// `project_id` is in the `WHERE` and not checked by the caller. The id is a global integer, so the
/// project is the only thing standing between one project's owner and another project's pile — and
/// a check that lives in a handler is a check the second caller forgets. `false` means no row
/// changed: a wrong project, an id that does not exist, or a line somebody already answered. All
/// three are the same answer to whoever asked, and none of them is a failure of this daemon.
///
/// `AssertSqlSafe` because sqlx 0.9 only trusts `&'static str` by default, and `column` is chosen
/// at runtime. Safe here by construction and not by review: `column` comes from a `bool` and from
/// nothing else, so it is always exactly one of the two literals below and never anything a caller
/// supplies. If `column` ever starts coming from outside this function, that guarantee is gone and
/// the two queries must separate.
pub async fn decide(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    id: i64,
    approved: bool,
) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let column = if approved {
        "approved_at"
    } else {
        "retired_at"
    };
    let result = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE map_decisions SET {column} = ?
          WHERE id = ? AND project_id = ? AND approved_at IS NULL AND retired_at IS NULL"
    )))
    .bind(&now)
    .bind(id)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_intent::{Extracted, Kind};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn two_decisions() -> Vec<Extracted> {
        vec![
            Extracted {
                section: "## 1. Alfa".to_owned(),
                ordinal: 1,
                text: "Alfa.".to_owned(),
                kind: Kind::Countable,
            },
            Extracted {
                section: "## 2. Beta".to_owned(),
                ordinal: 2,
                text: "Beta.".to_owned(),
                kind: Kind::Character,
            },
        ]
    }

    async fn retired_texts(pool: &sqlx::SqlitePool, project_id: &str) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT text FROM map_decisions
              WHERE project_id = ? AND retired_at IS NOT NULL
              ORDER BY ordinal",
        )
        .bind(project_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_extraction_lands_as_a_pile_nobody_has_read() {
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();

        let waiting = pending(&pool, "alpha").await.unwrap();
        assert_eq!(waiting.len(), 2);
        assert!(waiting[0].approved_at.is_none(), "nothing arrives approved");
        assert_eq!(
            waiting[0].ordinal, 1,
            "in the order the owner will read them"
        );
    }

    #[tokio::test]
    async fn approving_one_line_leaves_the_others_where_they_were() {
        // The list is approved line by line and never in one gesture. A button that took all of
        // them would be the thousand-line plan again, wearing a smaller shape.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let waiting = pending(&pool, "alpha").await.unwrap();

        assert!(decide(&pool, "alpha", waiting[0].id, true).await.unwrap());

        let left = pending(&pool, "alpha").await.unwrap();
        assert_eq!(left.len(), 1, "the approved one has left the pile");
        assert_eq!(left[0].ordinal, 2);
    }

    #[tokio::test]
    async fn a_rejected_line_is_retired_and_never_proposed_for_that_spec_again() {
        // §4: a rejected line does not come back. Retired rather than deleted, because a row that
        // is gone cannot say that somebody once looked at it and said no.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let waiting = pending(&pool, "alpha").await.unwrap();

        assert!(decide(&pool, "alpha", waiting[0].id, false).await.unwrap());

        assert_eq!(pending(&pool, "alpha").await.unwrap().len(), 1);
        assert_eq!(
            retired_texts(&pool, "alpha").await,
            vec!["Alfa.".to_string()]
        );
    }

    #[tokio::test]
    async fn one_projects_pile_is_never_another_projects() {
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        assert!(pending(&pool, "beta").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn answering_a_line_of_another_project_answers_nothing() {
        // The id is a global integer, so the project is the only thing standing between one
        // project's owner and another project's pile. A check that lived in the HTTP handler is a
        // check the second caller forgets, so it lives here.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let id = pending(&pool, "alpha").await.unwrap()[0].id;

        assert!(!decide(&pool, "beta", id, true).await.unwrap());
        assert_eq!(pending(&pool, "alpha").await.unwrap().len(), 2, "untouched");
    }

    #[tokio::test]
    async fn a_line_already_answered_is_not_answered_twice() {
        // Two clicks on the same row, or a stale list in a window somebody left open. The second
        // must change nothing and must say it changed nothing.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let id = pending(&pool, "alpha").await.unwrap()[0].id;

        assert!(decide(&pool, "alpha", id, true).await.unwrap());
        assert!(!decide(&pool, "alpha", id, false).await.unwrap());
        assert!(
            retired_texts(&pool, "alpha").await.is_empty(),
            "the approval stands"
        );
    }
}
