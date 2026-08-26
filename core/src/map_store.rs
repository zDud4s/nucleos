//! Where the intention layer's rows live.
//!
//! Separate from `map_intent.rs` for the same reason that module knows no SQL: the prompt and the
//! parse are the part worth testing without a database, and this is the part worth testing without
//! a model. Slices 4 and 5 add `map_stamps` and `map_triage` beside this table, and the SQL of the
//! three wants to be together and far from `http.rs`, which is already 19,000 lines.

use crate::chats::Brain;
use crate::map_intent::{Extracted, Kind};
use crate::map_stamp::Verdict;
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
/// `retired_at IS NULL` excludes exactly ONE thing: a line the owner rejected when it was put in
/// front of them, which was never approved and never entered the map (§4). Nothing else.
///
/// **Corrected 2026-08-26, when slice 4 landed.** This said the clause excluded two things, the
/// second being a decision withdrawn under §5.2's *mudei de ideias* — approved once, later stood
/// down, and held out here so it would stop driving the map. It does not do that and must not,
/// because §5.2 spells out what withdrawing is: *"Fica retirada, com o spec marcado por actualizar.
/// Pára de te chatear sem desaparecer em silêncio."* Setting `retired_at` drops the row out of this
/// query, so [`crate::map_join::join`] never sees it, so the map never mentions it again — which is
/// disappearing in silence, the one outcome that sentence forbids. And *com o spec marcado por
/// actualizar* needs a row somebody still reads: a decision that has left every reader cannot mark
/// its own document as claiming something abandoned.
///
/// So the two mechanisms stay apart, and each keeps one meaning. `retired_at` is **no at approval
/// time** — [`decide`] with `approved: false`, which is the only thing 0117's header describes.
/// `map_stamp::Verdict::Withdrawn` is **yes, and then a change of mind**: `approved_at` stays set,
/// `retired_at` stays NULL, the row stays here and in the junction, and its derived standing says
/// the document still claims something its owner abandoned.
///
/// The cost of the wrong reading, had a handler implemented withdrawal the way this comment
/// described: the withdrawn decision leaves this query, so it leaves [`stamps`] too — which repeats
/// this filter on purpose, so the two readers cannot disagree — and `Verdict::Withdrawn` becomes a
/// variant no read can ever return, with its count permanently zero. §5.2 spends a paragraph on why
/// the third verdict is not a convenience; the map would have deleted it silently and gone on
/// nagging forever about work its owner had explicitly abandoned.
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

/// One stamp, as it sits in the table.
///
/// No `id` and no `project_id`, and neither is an omission. The id is never needed by a caller,
/// because §9.2 makes the current state "the last row" and nothing addresses an individual stamp;
/// exposing it would invite the update this table exists to refuse. The project is a fact about the
/// decision rather than about the stamp, and a second copy of it here would be a second place for
/// it to be wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stamp {
    pub decision_id: i64,
    pub verdict: Verdict,
    pub stamped_at: String,
    /// The anchor code as it stood when this was written, in `map_stamp`'s canonical form.
    ///
    /// Three states, and flattening any two of them is the bug this field exists to prevent.
    /// `None` is *nobody could compute one* — **git is there and would not answer**, which is a fact
    /// about this daemon at that moment and is transient. `Some("")` is *computed, and there is
    /// nothing here to watch*, which is a fact about the decision or about its project, is
    /// permanent, and is what makes a stamp that can never expire. `Some(text)` is the digest. An
    /// `unwrap_or_default()` anywhere downstream turns the first into the second and mints a green
    /// that never comes back to ask; `0118`'s CHECK stops `settled` reaching the table as `None` at
    /// all, and this type is what keeps the other two apart afterwards.
    ///
    /// **A project with no git repository writes `Some("")` and not `None`**, and the reason is
    /// §11: such a project is not broken, and with no repository there is genuinely nothing that
    /// could ever move — so refusing the stamp, or storing it as *could not compute* and telling its
    /// owner to try again for ever, would both be answers about a fault that does not exist. Which
    /// of the silences a `Some("")` came from is deliberately not recorded here:
    /// [`crate::map_stamp::standing`] re-derives it on every read from what the project looks like
    /// now, and a copy in this column would be a second place for it to be wrong — and the one that
    /// goes stale, because a project can gain a repository and this row cannot notice.
    ///
    /// **`0118`'s own header still lists *no repository* among the things NULL means, and it is
    /// wrong; this paragraph is the correction.** It is not fixed in the SQL because `sqlx::migrate!`
    /// checksums that file byte for byte and a migration that has already run somewhere would then
    /// panic with `Migrate(VersionMismatch)` — the trap `.gitattributes` and `0115`'s header both
    /// describe. Correcting a comment is not worth a daemon that will not start, so the correction
    /// lives here, where the type that enforces it is.
    pub code_digest: Option<String>,
    pub note: Option<String>,
}

/// Append one stamp, and say whether it landed on anything.
///
/// **Never an UPDATE.** §9.2: re-carimbar acrescenta uma linha. A writer that replaced the previous
/// row would erase the evidence that this decision had once been settled, and that evidence is
/// precisely what the owner comes looking for when the doubt returns.
///
/// The ownership check is inside the INSERT rather than beside it, and that is the point of the
/// `INSERT ... SELECT`. The alternative — read the decision, check three things in Rust, then write
/// — is two statements that agree only as long as somebody keeps them agreeing, and it is the same
/// check-in-the-handler that [`decide`] argues against. Here the row is written *from* the decision
/// it is a verdict on, so a stamp for a decision that fails the `WHERE` cannot be constructed at
/// all.
///
/// `code_digest` is `None` only when git was there and would not answer, and `Some("")` when the
/// anchor set came back empty — including in a project with no repository at all. The table refuses
/// the first for `settled`: §7.1 makes *está como quero* the
/// only verdict the code moving can falsify, so it is the only one that may not be recorded without
/// knowing what it is anchored to. A caller that has no digest for a green must fail the request —
/// `503`, because it is this machine that is unable, not the owner who is wrong — rather than store
/// a stamp nothing will ever expire. Amber and a withdrawal take `None` without complaint, neither
/// having any expiry the code can reach.
///
/// `false` means no row was written, and it covers four things that are one answer to whoever
/// asked: an id that names nothing, a decision belonging to another project, a line still waiting in
/// the pile (§4 — nothing reaches the map unapproved), and a line already retired. None of them is a
/// failure of this daemon. An `Err`, by contrast, is the table refusing the row itself — an amber
/// with no note is the case that exists today — and that one is a bug in the caller, so it is not
/// flattened into `false` where it would look like a missing decision.
pub async fn stamp(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    decision_id: i64,
    verdict: Verdict,
    code_digest: Option<&str>,
    note: Option<&str>,
) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO map_stamps (decision_id, verdict, stamped_at, code_digest, note)
         SELECT id, ?, ?, ?, ? FROM map_decisions
          WHERE id = ? AND project_id = ? AND approved_at IS NOT NULL AND retired_at IS NULL",
    )
    .bind(verdict.as_str())
    .bind(&now)
    .bind(code_digest)
    .bind(note)
    .bind(decision_id)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// The columns every read of `map_stamps` selects, named once for the reason [`DecisionRow`] gives:
/// the verdict and the timestamp are both `String` and the digest and the note are both
/// `Option<String>`, so a `SELECT` that swapped either pair would still typecheck and would surface
/// as a verdict that is somehow an instant, or a note that is somehow a list of blob hashes.
type StampRow = (i64, String, String, Option<String>, Option<String>);

/// The single place a row becomes a [`Stamp`].
///
/// `None` for a verdict this module cannot read, which drops the stamp and so leaves the decision
/// among the ones nobody has looked at. Unreachable while the `CHECK` in `0118` stands — it admits
/// exactly the three [`Verdict::from_wire`] accepts — and written anyway, because the alternative to
/// dropping is defaulting, and every default here is a sentence put in the owner's mouth. Visible
/// debt is the honest failure; a green nobody gave is the one this map exists to prevent.
fn stamp_from_row(
    (decision_id, verdict, stamped_at, code_digest, note): StampRow,
) -> Option<Stamp> {
    Some(Stamp {
        decision_id,
        verdict: Verdict::from_wire(&verdict)?,
        stamped_at,
        code_digest,
        note,
    })
}

/// The current verdict on each of one project's approved decisions.
///
/// One row per decision at most, and decisions nobody has stamped are simply absent — the caller
/// pairs this against [`approved`] and what is missing is §5.3's `K nunca vistas`, which is debt
/// and is meant to be large on day one. Returning a placeholder for them would make the absence
/// something a reader has to interpret instead of something they can count.
///
/// The JOIN is load-bearing rather than decorative. `map_stamps` carries no `project_id`, so it
/// reaches one only through its decision, and this is where one owner's pile is kept out of
/// another's. `approved_at IS NOT NULL AND retired_at IS NULL` repeats [`approved`]'s filter
/// verbatim, and the repetition is the point: the two readers are paired against each other by every
/// caller — a decision here and not there, or there and not here, is a count that does not
/// reconcile. What it holds out is a line the owner rejected when it was proposed, and only that.
/// A decision withdrawn under §5.2 keeps `retired_at` NULL and is still returned by both, carrying
/// `Verdict::Withdrawn`; [`approved`]'s doc comment says why, and says what the other reading would
/// have cost.
///
/// **The subquery picks a row id and not a maximum timestamp, and the tie-break is why.**
/// `stamped_at` comes from `chrono::Utc::now().to_rfc3339()`, which is a clock and not a counter:
/// two stamps written close enough together share an instant, and a test that stamps twice in a row
/// does it easily. `MAX(stamped_at)` would then match both rows, and which one came back would be
/// whichever plan SQLite happened to choose — so a decision's verdict could change between two reads
/// with nothing having happened, which is the portrait this map refuses to be. `id DESC` breaks it
/// on insertion order, which is the order the owner actually stamped in.
pub async fn stamps(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<Vec<Stamp>> {
    let rows = sqlx::query_as::<_, StampRow>(
        "SELECT s.decision_id, s.verdict, s.stamped_at, s.code_digest, s.note
           FROM map_stamps s
           JOIN map_decisions d ON d.id = s.decision_id
          WHERE d.project_id = ?
            AND d.approved_at IS NOT NULL
            AND d.retired_at IS NULL
            AND s.id = (SELECT latest.id
                          FROM map_stamps latest
                         WHERE latest.decision_id = s.decision_id
                         ORDER BY latest.stamped_at DESC, latest.id DESC
                         LIMIT 1)
          ORDER BY s.decision_id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().filter_map(stamp_from_row).collect())
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

    /// One of `two_decisions`, approved — the only state a stamp is allowed to land on.
    async fn an_approved_decision(pool: &sqlx::SqlitePool, project_id: &str) -> i64 {
        record(pool, project_id, "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let id = pending(pool, project_id).await.unwrap()[0].id;
        assert!(decide(pool, project_id, id, true).await.unwrap());
        id
    }

    /// Every verdict ever stamped on one decision, oldest first, straight off the table.
    ///
    /// [`stamps`] deliberately answers with the last one only, so the history it is hiding can be
    /// asserted from nowhere else.
    async fn every_stamp(pool: &sqlx::SqlitePool, decision_id: i64) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT verdict FROM map_stamps WHERE decision_id = ? ORDER BY id",
        )
        .bind(decision_id)
        .fetch_all(pool)
        .await
        .unwrap()
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

    #[tokio::test]
    async fn re_stamping_adds_a_row_and_the_last_one_is_what_is_read() {
        // Append-only is §9.2, and it is not an implementation detail: a *mudei de ideias* that
        // overwrote would erase the proof that this decision had once been settled, which is
        // exactly what somebody wants to see when the doubt comes back. Both halves are asserted,
        // because only the pair is discriminating — a table that kept everything and a reader that
        // showed everything would be a map with two answers for one decision.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        assert!(
            stamp(
                &pool,
                "alpha",
                id,
                Verdict::Settled,
                Some("a1b2 core/src/x.rs"),
                None
            )
            .await
            .unwrap()
        );
        assert!(
            stamp(&pool, "alpha", id, Verdict::Withdrawn, None, None)
                .await
                .unwrap()
        );

        assert_eq!(
            every_stamp(&pool, id).await,
            vec!["settled".to_string(), "withdrawn".to_string()],
            "the settled one is still there to be shown"
        );
        let latest = stamps(&pool, "alpha").await.unwrap();
        assert_eq!(
            latest.len(),
            1,
            "one answer per decision, not one per stamp"
        );
        assert_eq!(latest[0].verdict, Verdict::Withdrawn);

        // The tie, and why `MAX(stamped_at)` alone would not settle it. This row is given the
        // timestamp of the one before it, which is what two stamps written inside one tick of the
        // clock look like — `stamp` reads `Utc::now()` itself and so cannot be asked to collide on
        // purpose. Without the `id DESC` tie-break the winner here is whichever row SQLite reached
        // first, and a decision whose verdict changes between two reads for no reason anybody can
        // see is the portrait this map refuses to be.
        let when = latest[0].stamped_at.clone();
        sqlx::query(
            "INSERT INTO map_stamps (decision_id, verdict, stamped_at, code_digest, note)
             VALUES (?, 'settled', ?, '', NULL)",
        )
        .bind(id)
        .bind(&when)
        .execute(&pool)
        .await
        .unwrap();

        let latest = stamps(&pool, "alpha").await.unwrap();
        assert_eq!(
            latest[0].verdict,
            Verdict::Settled,
            "of two stamps sharing an instant, the one written later is the current one"
        );
    }

    #[tokio::test]
    async fn an_amber_stamp_without_a_note_is_refused_by_the_table() {
        // §5.2 makes the note the entire point of amber — *falta migrar as páginas de pilar* is
        // worth more than the colour is — so an empty *a meio* is a row that should not exist
        // rather than a row a handler remembers to reject. Refused by the CHECK, because a handler
        // is a check the second caller forgets: the argument `decide` already makes about
        // `project_id`, owed here for the same reason.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        // A tab is whitespace too, and bare `trim` in SQLite strips spaces and nothing else, so
        // the CHECK in `0118` names the characters it must actually see through.
        for note in [None, Some(""), Some("   "), Some("\t"), Some("\n \r")] {
            assert!(
                stamp(&pool, "alpha", id, Verdict::Partial, None, note)
                    .await
                    .is_err(),
                "amber carrying {note:?} for a note"
            );
        }
        assert!(
            every_stamp(&pool, id).await.is_empty(),
            "and none of the three left a row behind"
        );
    }

    #[tokio::test]
    async fn a_settled_stamp_needs_no_note_and_neither_does_a_withdrawal() {
        // The CHECK guards one verdict and must not spread to the other two. §7 says the note is
        // obligatory on *a meio* and optional on the others, and a constraint that asked for one
        // everywhere would make the cheapest verdict — *está como quero*, which should be a single
        // click — cost a sentence nobody has to write.
        let pool = test_pool().await;
        let alfa = an_approved_decision(&pool, "alpha").await;
        let beta = pending(&pool, "alpha").await.unwrap()[0].id;
        assert!(decide(&pool, "alpha", beta, true).await.unwrap());

        assert!(
            stamp(
                &pool,
                "alpha",
                alfa,
                Verdict::Settled,
                Some("a1b2 core/src/x.rs"),
                None
            )
            .await
            .unwrap()
        );
        assert!(
            stamp(&pool, "alpha", beta, Verdict::Withdrawn, None, None)
                .await
                .unwrap()
        );

        let latest = stamps(&pool, "alpha").await.unwrap();
        assert_eq!(latest.len(), 2);
        assert!(
            latest.iter().all(|row| row.note.is_none()),
            "a note nobody wrote comes back as None and not as an empty sentence"
        );

        // Optional and not forbidden. Withdrawing is an assertion rather than a forgetting (§5.2),
        // and the owner is allowed to say what they changed their mind about.
        assert!(
            stamp(
                &pool,
                "alpha",
                beta,
                Verdict::Withdrawn,
                None,
                Some("o spec está velho")
            )
            .await
            .unwrap()
        );
        let latest = stamps(&pool, "alpha").await.unwrap();
        assert_eq!(
            latest
                .iter()
                .find(|row| row.decision_id == beta)
                .expect("the withdrawn decision")
                .note
                .as_deref(),
            Some("o spec está velho")
        );
    }

    #[tokio::test]
    async fn one_project_s_stamps_never_reach_another_project_s_map() {
        // `map_stamps` has no `project_id`; it reaches one only through `decision_id`, so the JOIN
        // is the whole of what stands between two owners' piles. The id is a global integer, and a
        // check that lived in the HTTP handler is a check the second caller forgets — the argument
        // `decide` makes, owed by both the reader and the writer here.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;
        assert!(
            stamp(
                &pool,
                "alpha",
                id,
                Verdict::Settled,
                Some("a1b2 core/src/x.rs"),
                None
            )
            .await
            .unwrap()
        );

        assert_eq!(stamps(&pool, "alpha").await.unwrap().len(), 1);
        assert!(stamps(&pool, "beta").await.unwrap().is_empty());

        assert!(
            !stamp(&pool, "beta", id, Verdict::Withdrawn, None, None)
                .await
                .unwrap(),
            "and a stamp aimed at another project's decision lands nowhere"
        );
        assert_eq!(every_stamp(&pool, id).await, vec!["settled".to_string()]);
    }

    #[tokio::test]
    async fn a_decision_nobody_approved_cannot_be_stamped() {
        // The other two clauses of the same `WHERE`, and neither is ceremony. A stamp on a line
        // still waiting in the pile would be a verdict on something the owner never agreed exists
        // (§4), which is the model being handed back the authority §6 took from it. A stamp on a
        // retired one would put a verdict on a decision somebody explicitly stood down.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let waiting = pending(&pool, "alpha").await.unwrap();

        assert!(
            !stamp(
                &pool,
                "alpha",
                waiting[0].id,
                Verdict::Settled,
                Some(""),
                None
            )
            .await
            .unwrap(),
            "a line still waiting to be read"
        );
        assert!(decide(&pool, "alpha", waiting[1].id, false).await.unwrap());
        assert!(
            !stamp(
                &pool,
                "alpha",
                waiting[1].id,
                Verdict::Settled,
                Some(""),
                None
            )
            .await
            .unwrap(),
            "a line the owner said no to"
        );
        assert!(
            !stamp(&pool, "alpha", 9_999, Verdict::Settled, Some(""), None)
                .await
                .unwrap(),
            "an id that names nothing at all"
        );
        assert!(stamps(&pool, "alpha").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_settled_stamp_whose_digest_could_not_be_computed_is_refused_by_the_table() {
        // Name the row this refuses, because it is the whole reason the column is nullable: a green
        // on a decision with perfectly good anchor files, recorded at a moment when `git` did not
        // answer, and therefore anchored to nothing anybody can compare against. Nothing would ever
        // expire it. Nobody would ever learn why. It is §1's false confidence manufactured by the
        // feature built to cure it, and it would have looked exactly like a stamp that was working.
        //
        // The table and not the handler, for the reason the note CHECK gives one screen up. The
        // route answers `503` — this machine is unable, the owner is not wrong — and this is what
        // keeps that true for the second caller.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        assert!(
            stamp(&pool, "alpha", id, Verdict::Settled, None, None)
                .await
                .is_err()
        );
        assert!(
            every_stamp(&pool, id).await.is_empty(),
            "and it left no row behind"
        );

        // The same green with a digest that was computed and came back empty is allowed. That is a
        // decision with no readable anchor, which is a true thing about the decision and is the one
        // §7 requires be SHOWN rather than refused — slice 6 is what starts giving these anchors.
        assert!(
            stamp(&pool, "alpha", id, Verdict::Settled, Some(""), None)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn an_amber_stamp_records_no_digest_without_complaint() {
        // §7.1: neither *a meio* nor *mudei de ideias* expires by the code moving — one expires by
        // time and the other never — so a digest nobody could compute costs them nothing, and a
        // CHECK that demanded one would refuse two honest rows to guard a rule that does not apply
        // to them. The constraint is narrow on purpose, and this is the half of it that says so.
        let pool = test_pool().await;
        let alfa = an_approved_decision(&pool, "alpha").await;
        let beta = pending(&pool, "alpha").await.unwrap()[0].id;
        assert!(decide(&pool, "alpha", beta, true).await.unwrap());

        assert!(
            stamp(
                &pool,
                "alpha",
                alfa,
                Verdict::Partial,
                None,
                Some("falta o resto")
            )
            .await
            .unwrap()
        );
        assert!(
            stamp(&pool, "alpha", beta, Verdict::Withdrawn, None, None)
                .await
                .unwrap()
        );
        assert_eq!(stamps(&pool, "alpha").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_digest_that_came_back_empty_is_not_one_nobody_could_read() {
        // The distinction the whole amendment is for, pinned at the only place it can be pinned:
        // through the column and back. `Some("")` is *computed, and this decision has no readable
        // anchor* — permanent, a property of the decision, and correctly a stamp that never
        // expires. `None` is *this daemon could not compute one* — transient, a property of the
        // moment, and no statement about the code at all.
        //
        // A single `unwrap_or_default()` between here and the panel collapses the second into the
        // first and turns every failed `git` call into a permanent green. It would break nothing
        // that compiles and no other test, which is why this one asserts the two values rather than
        // asserting that both rows merely exist.
        let pool = test_pool().await;
        let empty = an_approved_decision(&pool, "alpha").await;
        let unknown = pending(&pool, "alpha").await.unwrap()[0].id;
        assert!(decide(&pool, "alpha", unknown, true).await.unwrap());

        assert!(
            stamp(&pool, "alpha", empty, Verdict::Settled, Some(""), None)
                .await
                .unwrap()
        );
        assert!(
            stamp(&pool, "alpha", unknown, Verdict::Withdrawn, None, None)
                .await
                .unwrap()
        );

        let latest = stamps(&pool, "alpha").await.unwrap();
        let digest_of = |id: i64| {
            latest
                .iter()
                .find(|row| row.decision_id == id)
                .expect("a stamp for this decision")
                .code_digest
                .clone()
        };
        assert_eq!(
            digest_of(empty),
            Some(String::new()),
            "computed, and there was nothing readable to watch"
        );
        assert_eq!(digest_of(unknown), None, "nobody could compute one");
    }
}
