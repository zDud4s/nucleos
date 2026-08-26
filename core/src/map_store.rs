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
/// Six of the nine are `TEXT` in one tuple, so a `SELECT` that reordered two of them would still
/// typecheck and the mistake would surface as a decision whose section is somehow the name of a
/// brain. This alias and the `SELECT`s below are one thing written three times; changing any of
/// them without the others is what it exists to make visible. The house shape — see `ErrandRow` in
/// `errands.rs` and `Row` in `project_commands.rs`, both a row of this size read the same way.
///
/// `approved_at` is the one column a reorder cannot swallow, being the only nullable one and so the
/// only `Option<String>` in the tuple. That is luck rather than design, and it is worth saying
/// because the same column is the one this row got wrong for longest — see [`from_row`].
type DecisionRow = (
    i64,
    String,
    String,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
);

/// The single place a row becomes a [`Decision`].
///
/// `None` for a `kind` the CHECK should have refused. Dropped rather than defaulted: the same
/// argument `Kind::from_wire` makes, and a row that reaches here unreadable is a row nobody can act
/// on either way.
///
/// **`approved_at` is read off the row and is no longer asserted here.** It used to be hardcoded to
/// `None`, which was true of every row [`pending`] can return — its own `WHERE` says
/// `approved_at IS NULL` — and was therefore a fact that query already stated. Restating it in the
/// mapping quietly turned it into a property of *the type* instead of a property of *that query*,
/// so the next reader would have inherited a field that is permanently `None` whatever the table
/// holds. A caller filtering on it — the junction is built from approved decisions and nothing
/// else — would then have produced an empty map for every project, with a header reading zeros:
/// silently wrong, on the one screen that exists to stop exactly that. [`pending`] still answers
/// `None` here, now because the row says so rather than because this function does.
fn from_row(
    (id, spec_slug, section, ordinal, text, kind, brain, extracted_at, approved_at): DecisionRow,
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
        approved_at,
    })
}

/// What is waiting for the owner in this project.
///
/// Not approved, not retired, oldest extraction first — a pile read in the order it arrived is a
/// pile that ends, and one ordered by anything else is a pile that never does.
pub async fn pending(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<Vec<Decision>> {
    let rows = sqlx::query_as::<_, DecisionRow>(
        "SELECT id, spec_slug, section, ordinal, text, kind, brain, extracted_at, approved_at
           FROM map_decisions
          WHERE project_id = ? AND approved_at IS NULL AND retired_at IS NULL
          ORDER BY extracted_at, spec_slug, ordinal",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().filter_map(from_row).collect())
}

/// What the owner said yes to, in this project.
///
/// The mirror of [`pending`], and the two must never become one query with a flag. §4 says an
/// extraction nobody has approved is a pile apart, counted apart from the real decisions, and one
/// query with a boolean is how those two counts come to share a call site and then a number. They
/// also answer different questions: `pending` is a queue somebody works through and orders by
/// arrival, this is the material the map is made of and orders by document.
///
/// `retired_at IS NULL` excludes two different things with one clause, and both belong out. A line
/// the owner rejected was never approved (§4). A decision later withdrawn — §5.2's *mudei de
/// ideias* — was, and the whole point of withdrawing it is that it stops driving the map without
/// disappearing from the table; a reader that kept it would go on reporting a decision somebody
/// explicitly stood down.
///
/// Ordered `spec_slug, ordinal, id`, which is the order [`crate::map_join::join`] sorts into
/// anyway. Stated here rather than left to the caller because `(spec_slug, ordinal)` is not a total
/// order: `UNIQUE (project_id, spec_slug, ordinal, extracted_at)` lets two extractions of one spec
/// both hold ordinal 1 and both be approved. Without the third key the order is whatever plan
/// SQLite chose, and a map that changes shape between two reads for no reason anybody can see is
/// the portrait decision 1 refuses.
pub async fn approved(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<Vec<Decision>> {
    let rows = sqlx::query_as::<_, DecisionRow>(
        "SELECT id, spec_slug, section, ordinal, text, kind, brain, extracted_at, approved_at
           FROM map_decisions
          WHERE project_id = ? AND approved_at IS NOT NULL AND retired_at IS NULL
          ORDER BY spec_slug, ordinal, id",
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
    async fn an_approved_decision_comes_back_and_a_pending_one_does_not() {
        // The two readers are mirrors, and the pile the owner has not read must never be counted
        // among the decisions the map is made of (§4).
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        assert!(
            approved(&pool, "alpha").await.unwrap().is_empty(),
            "nothing arrives approved"
        );

        let waiting = pending(&pool, "alpha").await.unwrap();
        assert!(decide(&pool, "alpha", waiting[0].id, true).await.unwrap());

        let said_yes = approved(&pool, "alpha").await.unwrap();
        assert_eq!(said_yes.len(), 1);
        assert_eq!(said_yes[0].ordinal, 1);
        assert_eq!(
            pending(&pool, "alpha").await.unwrap().len(),
            1,
            "the other line is still waiting to be read"
        );
    }

    #[tokio::test]
    async fn a_rejected_decision_never_comes_back() {
        // A `no` is retired rather than deleted, so the row is still there and must be invisible to
        // both readers: it is not waiting to be answered and it is not something the map is made
        // of. The same clause also holds out §5.2's *mudei de ideias*, which is a decision the owner
        // stood down after approving — kept in the table precisely so it stops mattering without
        // vanishing.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let waiting = pending(&pool, "alpha").await.unwrap();

        assert!(decide(&pool, "alpha", waiting[0].id, false).await.unwrap());
        assert!(decide(&pool, "alpha", waiting[1].id, true).await.unwrap());

        // Both halves asserted, because only the pair is discriminating: a reader that answered
        // nothing at all would satisfy the absence on its own, and the failure this guards against
        // is a `WHERE` that keeps every answered line rather than one that keeps none.
        let said_yes = approved(&pool, "alpha").await.unwrap();
        assert_eq!(said_yes.len(), 1, "the yes landed and the no did not");
        assert_eq!(said_yes[0].text, "Beta.");
        assert_eq!(
            retired_texts(&pool, "alpha").await,
            vec!["Alfa.".to_string()]
        );
    }

    #[tokio::test]
    async fn another_project_s_approvals_are_not_mine() {
        // `decide` puts `project_id` in its own `WHERE` rather than trusting a handler to check it,
        // and a reader owes the same guarantee for the same reason: the id is a global integer, so
        // the project is the only thing between one owner and another owner's decisions.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let id = pending(&pool, "alpha").await.unwrap()[0].id;
        assert!(decide(&pool, "alpha", id, true).await.unwrap());

        assert_eq!(approved(&pool, "alpha").await.unwrap().len(), 1);
        assert!(approved(&pool, "beta").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_approved_decision_carries_the_moment_it_was_approved() {
        // The test that would have caught it. `from_row` hardcoded `approved_at: None` — true of
        // every row `pending` can return, since its own `WHERE` says so, and a lie the moment a
        // second reader existed. Nothing asserted the field because nothing could: the only reader
        // was the one whose answer was `None` either way. A caller filtering on it — the junction
        // is built from approved decisions and nothing else — would have seen an empty map for
        // every project and a header of zeros, which is silently wrong on the one screen built to
        // stop exactly that.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let waiting = pending(&pool, "alpha").await.unwrap();
        assert!(waiting[0].approved_at.is_none(), "nothing arrives approved");

        assert!(decide(&pool, "alpha", waiting[0].id, true).await.unwrap());

        let said_yes = approved(&pool, "alpha").await.unwrap();
        let moment = said_yes[0]
            .approved_at
            .as_deref()
            .expect("the moment the owner said yes");
        assert!(
            chrono::DateTime::parse_from_rfc3339(moment).is_ok(),
            "an instant the owner can be shown, not whatever a default would have been: {moment}"
        );
        assert!(
            pending(&pool, "alpha")
                .await
                .unwrap()
                .iter()
                .all(|row| row.approved_at.is_none()),
            "and the pile still answers None, now because the row says so"
        );
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
