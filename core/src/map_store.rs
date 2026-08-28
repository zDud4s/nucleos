//! §spec mapa-do-projeto
//!
//! Where the intention layer's rows live.
//!
//! Separate from `map_intent.rs` for the same reason that module knows no SQL: the prompt and the
//! parse are the part worth testing without a database, and this is the part worth testing without
//! a model. Slices 4 and 5 add `map_stamps` and `map_triage` beside this table, and the SQL of the
//! three wants to be together and far from `http.rs`, which is already 19,000 lines.
//!
//! **Three tables, three axes, and this module is where they are kept from becoming one.** A
//! decision carries what the code says about it (`map_join`), what its owner said (`map_stamps`) and
//! what the triager thought (`map_triage`), and §5 forbids flattening them — *"achatá-las numa só
//! punha o triador e o dono a falar pela mesma boca"*. Nothing here joins the three into a single
//! state; each reader answers about one axis and the caller pairs them. That is deliberate, and it
//! is why [`stamps`] and [`judgements`] are two functions returning two shapes rather than one
//! returning a verdict.

use crate::chats::Brain;
use crate::map_intent::{Extracted, Kind};
use crate::map_stamp::Verdict;
use crate::map_triage::Judgement;
use serde::{Deserialize, Serialize};

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

/// Write one extraction's worth of proposals, all unapproved, and retire the pile the last
/// extraction of this spec left unread.
///
/// One timestamp for the whole batch rather than one per row: they were proposed together, the
/// owner reads them together, and `UNIQUE (project_id, spec_slug, ordinal, extracted_at)` uses it
/// to keep two extractions of one spec from colliding on ordinal. The same instant is what the
/// superseded rows are retired at, so the retirement and the extraction that caused it read as one
/// event rather than two that happened to be close.
///
/// **Superseding was promised by `0117` and never implemented, and the gap had a cost.** That
/// migration's `retired_at` column says in its own comment: *"Set when the owner says no, **or when
/// a later extraction supersedes this one**."* Only the first half existed, so re-extracting a spec
/// left both lists live, and approving the second put two copies of every decision into the map —
/// two model calls per line in triage, two rows against every count, and no way for the owner to
/// tell which copy they were reading. The `UPDATE` below is that sentence, finally written.
///
/// **It retires only what is still PENDING** — `approved_at IS NULL AND retired_at IS NULL` — and
/// the restriction is the important half of this function, not a caution:
///
/// - **An approved row is the owner's act, and this is a model's.** §4 and §6 reserve approval to a
///   human; an extractor that could take it back would be the model recovering, through a side door,
///   the one authority the whole design removes from it. That the taking-back would be well
///   intentioned is exactly why it has to be refused here rather than judged case by case.
/// - **`map_stamps.decision_id` and `map_triage.decision_id` point at those rows.** Retiring one
///   drops it out of [`approved`], so its stamps and its judgements go on existing in their tables
///   while vanishing from every reader — the owner's verdict erased by a re-extraction they would
///   never connect to it. Neither foreign key cascades, on purpose, so nothing would even error.
///
/// **So a duplicate can still arise, and this is a known gap rather than a solved problem:** extract,
/// approve, extract again, approve again, and the spec has two live copies of a line. The honest
/// repair is not here — it is for the extract route to tell the owner how many approved decisions
/// from an earlier extraction of that spec are still live and let them decide what becomes of them.
/// This function must not make that choice silently, and a later reader reaching for `OR approved_at
/// IS NOT NULL` to "finish the job" would be making it for them.
///
/// **It does not break §5.3's header**, and that is worth writing down so nobody repairs arithmetic
/// that is not broken. The duplicates are distinct `decision_id`s, each with exactly one standing and
/// at most one current judgement, so `map_stamp::StampCounts`' five categories still reconcile to the
/// number of decisions. What is wrong is the number of decisions itself — duplication on screen, not
/// a total that stops adding up.
pub async fn record(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    spec_slug: &str,
    brain: Brain,
    decisions: &[Extracted],
) -> sqlx::Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();

    // One `UPDATE` before the loop and inside this same function, so the retirement and the write
    // that supersedes cannot drift the way a check in a handler drifts from the write it guards —
    // the argument [`decide`] already makes about `project_id`. Scoped to one spec of one project:
    // a second document's pending pile has nothing to do with this one having been re-read.
    sqlx::query(
        "UPDATE map_decisions SET retired_at = ?
          WHERE project_id = ? AND spec_slug = ?
            AND approved_at IS NULL AND retired_at IS NULL",
    )
    .bind(&now)
    .bind(project_id)
    .bind(spec_slug)
    .execute(pool)
    .await?;

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

/// Every document this project has decided anything about, whatever is on disk today.
///
/// **The filesystem is not the register of which documents a project has, and treating it as one is
/// what makes this feature rot.** `map_intent::specs_in` answers *which files are in `.ai/specs`
/// right now* — a question about a folder the owner archives, renames and reorganises, and whose
/// contents are gitignored working material by standing policy. A decision row, by contrast, copied
/// its slug, its heading and its text at extraction and keeps them for as long as the row exists.
/// So the durable answer to *which documents does this project have* is here, in the table, and the
/// folder is a second source that can only ever add to it.
///
/// Concretely, the failure this replaces: `spec_slug` is a filename without its extension and these
/// filenames carry dates. Rename `2026-08-24-mapa-do-projeto-design.md` and every approved decision
/// keeps the old slug for ever, while a slug list read off the disk holds only the new one — so
/// `map_join::evidence` stops being able to tell one document from another, every anchor degrades,
/// and nothing on screen says why. Read from here, the rename costs nothing.
///
/// **Every row and not only the approved ones**, because the question is which documents exist and
/// not which decisions stand. A rejected line is still evidence that this project extracted from
/// that document, and `evidence` uses the list only to refuse a citation that names a document
/// other than the one being asked about — a use that wants the widest true list.
pub async fn slugs(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT spec_slug FROM map_decisions WHERE project_id = ? ORDER BY spec_slug",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(slug,)| slug).collect())
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
    /// **`0118`'s header briefly listed *no repository* among the things NULL means, which A6 made
    /// wrong; it says `''` now.** Editing an applied migration is normally forbidden — `sqlx::migrate!`
    /// checksums the file byte for byte and one that has already run panics with
    /// `Migrate(VersionMismatch)`, the trap `.gitattributes` and `0115`'s header both describe, and
    /// which this feature has already sprung twice on the numbering alone. It was safe here only
    /// because `0118` had **never been applied**: checked against
    /// `%LOCALAPPDATA%\nucleos\NucleOS\data\nucleos.db` on 2026-08-26, whose `_sqlx_migrations`
    /// stopped at `117 map decisions`. `0117` is applied and must never be touched. **Check before
    /// editing any migration; do not infer from this one that it is allowed.**
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

/// How a decision's anchor set came to be written down.
///
/// **Two, and they are not the same claim.** A stamp records whatever the `§` comments produced at
/// the moment the owner gave a verdict — which, while §8 is unfixed, is a set of guesses about
/// which document a bare `§` meant. The owner pointing at files is a choice. Storing them alike
/// would let a guess be read back as a decision, and that collapse is the disease this whole
/// feature treats.
///
/// Two wire forms read as two, exactly as [`crate::map_stamp::Verdict`] insists: [`Self::as_str`]
/// is the STORAGE form and what `map_anchors.source`'s `CHECK` admits; the derived `Serialize` is
/// the JSON form the window speaks. They spell alike today by accident of English and not by
/// guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorSource {
    /// Recorded as a side effect of a verdict, from what the comments said at that moment.
    Stamp,
    /// Pointed at deliberately. The only one of the two that is a choice about which files a
    /// decision's code is.
    Owner,
}

impl AnchorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stamp => "stamp",
            Self::Owner => "owner",
        }
    }

    /// The two, and nothing else.
    ///
    /// `Option` and not a fallback, following [`crate::map_stamp::Verdict::from_wire`] rather than
    /// [`crate::chats::Brain::from_wire`]. Defaulting to `Owner` would invent a deliberate choice
    /// nobody made; defaulting to `Stamp` would quietly demote one somebody did. A record that
    /// cannot be read is therefore no record, which lands the decision back among the ones whose
    /// anchor is only what the comments say — visible, and the one answer that claims nothing.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "stamp" => Some(Self::Stamp),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

/// Which files are one decision's, as somebody wrote them down.
///
/// **The whole point is that this survives the `§` comment being deleted.** Every other anchor in
/// this map is recomputed from the working tree on every read and has no memory; this one is the
/// memory. A comment that disappears afterwards stops being a silent fall into *declarado, sem
/// código* and becomes a named alarm: these files were this decision's, and nothing says so any
/// more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnchorRecord {
    /// Sorted, deduplicated, forward slashes — the spelling `project_map::structure` produces.
    ///
    /// **Empty is legal and is not the same as absent.** *These files were this decision's and now
    /// none are* is an assertion the owner can make about code that was genuinely removed; no
    /// record at all is the absence of any assertion. A caller that read the two alike would turn
    /// a deliberate withdrawal into an oversight.
    pub paths: Vec<String>,
    pub source: AnchorSource,
    pub recorded_at: String,
}

/// The columns every read of `map_anchors` selects, named once for [`DecisionRow`]'s reason: three
/// of the four fields are `String` and a `SELECT` that swapped `source` for `recorded_at` would
/// still typecheck.
type AnchorRow = (i64, String, String, String);

/// Write down which files are one decision's.
///
/// **Canonicalised here and not by the caller**, because the only reader that matters is a set
/// difference and two records of the same set must compare equal. Sorted and deduplicated, so a
/// caller handing the paths in the order a UI listed them cannot produce a record that looks
/// different from one that says exactly the same thing.
///
/// The guard is [`stamp`]'s verbatim — the decision must belong to this project and be approved and
/// not retired — and it lives in the `WHERE` rather than in a handler, which is the argument
/// [`decide`] makes about checks a second caller forgets. `false` means no row matched, and the
/// three reasons it can mean that are deliberately one answer for [`decide`]'s reason.
pub async fn record_anchor(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    decision_id: i64,
    paths: &[String],
    source: AnchorSource,
) -> sqlx::Result<bool> {
    let canonical: std::collections::BTreeSet<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|path| !path.is_empty())
        .collect();
    let written = canonical.into_iter().collect::<Vec<_>>().join("\n");
    let now = chrono::Utc::now().to_rfc3339();

    let result = sqlx::query(
        "INSERT INTO map_anchors (decision_id, paths, source, recorded_at)
         SELECT id, ?, ?, ? FROM map_decisions
          WHERE id = ? AND project_id = ? AND approved_at IS NOT NULL AND retired_at IS NULL",
    )
    .bind(&written)
    .bind(source.as_str())
    .bind(&now)
    .bind(decision_id)
    .bind(project_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// The current anchor record for each of one project's approved decisions.
///
/// One row per decision at most, and decisions nobody has recorded anything for are simply absent —
/// [`stamps`]' shape and for its reason: the absence is countable rather than something a reader has
/// to interpret. On day one that is every decision, which is the honest starting point.
///
/// **The subquery picks a row id and not a maximum timestamp**, which is [`stamps`]' argument
/// verbatim: `recorded_at` is a clock rather than a counter, two records written in the same second
/// tie, and a set that changed between two reads with nothing having happened is the portrait this
/// map refuses to be.
///
/// The JOIN is load-bearing exactly as [`stamps`]' is — `map_anchors` carries no `project_id` and
/// reaches one only through its decision.
pub async fn anchors(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> sqlx::Result<std::collections::BTreeMap<i64, AnchorRecord>> {
    let rows = sqlx::query_as::<_, AnchorRow>(
        "SELECT a.decision_id, a.paths, a.source, a.recorded_at
           FROM map_anchors a
           JOIN map_decisions d ON d.id = a.decision_id
          WHERE d.project_id = ?
            AND d.approved_at IS NOT NULL
            AND d.retired_at IS NULL
            AND a.id = (SELECT latest.id
                          FROM map_anchors latest
                         WHERE latest.decision_id = a.decision_id
                         ORDER BY latest.recorded_at DESC, latest.id DESC
                         LIMIT 1)
          ORDER BY a.decision_id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|(decision_id, paths, source, recorded_at)| {
            // A source nobody can read is no record, which is what `AnchorSource::from_wire` argues
            // for. The row stays in the table — it is history — and this read declines to present
            // it as an anchor rather than guessing which of the two claims it was making.
            let source = AnchorSource::from_wire(&source)?;
            Some((
                decision_id,
                AnchorRecord {
                    // `''` is an empty set and not one empty path, which `str::split` would give.
                    paths: paths.lines().map(str::to_owned).collect(),
                    source,
                    recorded_at,
                },
            ))
        })
        .collect())
}

/// One triage judgement, as it sits in the table.
///
/// No `id` and no `project_id`, for the two reasons [`Stamp`] gives: the current answer is the last
/// row, so nothing addresses an individual judgement, and the project is a fact about the decision
/// rather than about the judgement.
///
/// **`inputs_digest` is on the row and is not compared here**, which is the same split slice 4 made
/// with [`crate::map_stamp::standing`] and is worth saying out loud because the tempting shape is a
/// reader that answers only *current* judgements. Deciding staleness needs the digest of the inputs
/// **as they are now** — the decision's text, its anchor set, the anchor blobs — and computing that
/// means reading the repository, which is exactly what this module is kept away from so its SQL
/// stays exercisable with no git anywhere near it. So [`judgements`] returns the latest row whatever
/// its digest says, and whoever holds the current reading compares.
///
/// Dropping the field and returning only fresh rows would also have been wrong in a quieter way: a
/// judgement that went stale would become indistinguishable from one that never happened, and the
/// silenced pile §6.2 requires to be *sempre acessível* would lose exactly the rows most worth
/// looking at — the ones whose reason was written about code that has since moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Judged {
    pub decision_id: i64,
    /// Flagged or silenced, and never a third thing. See [`Judgement`], and `0119`'s CHECK, which is
    /// the copy of that rule a caller cannot go round.
    pub judgement: Judgement,
    /// Why, in the triager's own words. Never empty — the table refuses it — because §6.2 makes the
    /// readable reason the only mitigation §13 has for a triager that silences what it should have
    /// shown.
    pub reason: String,
    /// Which brain answered, as `Brain::as_str` spells it. The other half of §6.2: the repair for a
    /// triager that silences too much is to stop using that triager, and that is not a decision
    /// anybody can take about a pile that will not say who filled it.
    pub model: String,
    pub computed_at: String,
    /// What the triager looked at, hashed. Compared against a freshly computed digest by whoever has
    /// one; see this struct's own doc for why that comparison does not happen here.
    pub inputs_digest: String,
    /// This sentence was written by the daemon rather than by a model.
    ///
    /// **Not a column, and the one field here that is derived** — which is exactly why it belongs on
    /// the row rather than on whatever a caller happens to build out of it. [`Self::model`] names
    /// the brain that ANSWERED, and that stays true of an answer nobody could read: the only
    /// producer of such a sentence is [`crate::map_triage::unreadable_flag`], which records the
    /// failure as a flag so a confused model costs a look instead of disappearing. A client handed
    /// `reason` under `model` with nothing else has no way to tell a machine's note about a failure
    /// from a model's opinion about the code, and that attribution is the whole of §6.2.
    ///
    /// **Computed in [`judged_from_row`], which is the only place a `Judged` comes into existence**,
    /// so no reader of this table can be handed the sentence without the answer to *whose is it*.
    /// The alternative shipped for one slice and was backwards: the field sat on the silenced pile's
    /// row, where [`crate::map_triage::unreadable_flag`] makes it provably `false`, while the pile
    /// that can actually carry the mark had nothing — and the client duly re-spelled
    /// [`crate::map_triage::DAEMON_MARK`] in TypeScript to work around it, which is the second
    /// spelling of a convention that the constant exists to prevent.
    pub machine_written: bool,
}

/// Append one triage judgement, and say whether it landed on anything.
///
/// **Never an UPDATE**, for `map_stamps`' reason and for one of its own. §13 rates *o triador
/// silencia o que devia mostrar* a real residual risk whose only mitigation is that the pile stays
/// visible with its reasons — so the row proving a silence happened IS the mitigation, and a writer
/// that replaced the previous row would delete it. A triager that flagged something last week and
/// silences it today is exactly the case somebody will want to read back.
///
/// The ownership check is inside the INSERT rather than beside it, which is the whole point of the
/// `INSERT ... SELECT`: the row is written *from* the decision it is a judgement on, so a judgement
/// for a decision that fails the `WHERE` cannot be constructed at all. The alternative — read,
/// check three things in Rust, then write — is two statements that agree only while somebody keeps
/// them agreeing, and it is the check-in-the-handler [`decide`] already argues against.
///
/// `approved_at IS NOT NULL AND retired_at IS NULL` is [`approved`]'s filter verbatim, repeated for
/// the reason [`stamps`] repeats it: two readers of one pile that disagree produce counts that do
/// not reconcile. It is also §4's rule reappearing — triaging a line still waiting would have the
/// model pass judgement on something the owner never agreed exists, which is the authority §6 takes
/// away from it arriving back through a different door.
///
/// **`model` and `inputs_digest` are `&str` rather than typed**, and that is where this function is
/// weakest: `record` takes a `Brain` and writes `Brain::as_str()`, while this takes whatever the
/// caller has. The reason is that §6.2 asks for *o modelo*, which is not always the same thing as
/// the brain — a brain resolves to a model name, and the pile is more useful naming the one that
/// actually answered. `0119` refuses both columns blank, which is the floor the type would have
/// given for free and is why the looser signature costs nothing that matters.
///
/// `false` means no row was written, and it covers the four things [`stamp`] lists, which are one
/// answer to whoever asked. An `Err` is the table refusing the row itself — a blank reason, a blank
/// model, a blank digest — and that is a bug in the caller, so it is not flattened into `false`
/// where it would look like a missing decision.
pub async fn triage(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    decision_id: i64,
    judgement: Judgement,
    reason: &str,
    model: &str,
    inputs_digest: &str,
) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO map_triage (decision_id, verdict, reason, model, computed_at, inputs_digest)
         SELECT id, ?, ?, ?, ?, ? FROM map_decisions
          WHERE id = ? AND project_id = ? AND approved_at IS NOT NULL AND retired_at IS NULL",
    )
    .bind(judgement.as_str())
    .bind(reason)
    .bind(model)
    .bind(&now)
    .bind(inputs_digest)
    .bind(decision_id)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// The columns every read of `map_triage` selects, named once for the reason [`DecisionRow`] gives,
/// and with more cause here than either table above it: four of the six are `String`, so a `SELECT`
/// that swapped any pair of them would still typecheck and surface as a silenced pile whose reasons
/// are all the same timestamp, or whose model is somehow a hash.
type JudgedRow = (i64, String, String, String, String, String);

/// The single place a row becomes a [`Judged`].
///
/// `None` for a verdict this module cannot read, which drops the judgement and so leaves the
/// decision *not looked at*. Unreachable while `0119`'s CHECK stands — it admits exactly the two
/// [`Judgement::from_wire`] accepts — and written anyway, because the alternative to dropping is
/// defaulting, and both defaults are worse than the absence: silencing a row nobody could read would
/// clear a decision out of the owner's queue on the strength of a parse failure, and flagging it
/// would raise an alarm the reason column cannot explain.
fn judged_from_row(
    (decision_id, verdict, reason, model, computed_at, inputs_digest): JudgedRow,
) -> Option<Judged> {
    Some(Judged {
        decision_id,
        judgement: Judgement::from_wire(&verdict)?,
        // Read before `reason` is moved, and read HERE rather than by whoever draws the pile.
        // `crate::map_triage::written_by_the_daemon` is the one owner of the test and this is the
        // one place a row becomes a `Judged`, so the two facts a reader needs about a sentence —
        // what it says and whose it is — cannot arrive separately.
        machine_written: crate::map_triage::written_by_the_daemon(&reason),
        reason,
        model,
        computed_at,
        inputs_digest,
    })
}

/// The latest judgement per decision, for one project's approved decisions.
///
/// Decisions the triager has never looked at are simply absent, as unstamped ones are from
/// [`stamps`]: the caller pairs this against [`approved`], and the third pile — *not looked at* — is
/// what is missing from both. A placeholder would make the absence something a reader has to
/// interpret rather than something they can count, and §5.3 requires these categories to add up.
///
/// **Returned whether or not the judgement is still about the same thing.** `inputs_digest` comes
/// back on the row and is not compared here — see [`Judged`], which argues why the comparison
/// belongs to whoever holds the current reading of the repository, and what returning only fresh
/// rows would have cost the silenced pile §6.2 requires to be *sempre acessível*.
///
/// The JOIN is load-bearing rather than decorative. `map_triage` carries no `project_id`, so it
/// reaches one only through its decision, and this is what keeps one owner's pile out of another's.
///
/// **The subquery picks a row id and not a maximum timestamp, and the tie-break matters more here
/// than it did for stamps.** `computed_at` is a clock and not a counter, and a triage run sweeps a
/// project's whole *never seen* pile in one batch — so judgements sharing an instant are the
/// ordinary case rather than the contrived one. `MAX(computed_at)` would match several rows and let
/// SQLite's plan choose the winner, which is a decision that reads silenced on one refresh and
/// flagged on the next with nothing having happened. `id DESC` breaks it on insertion order, which
/// is the order the triager actually answered in.
///
/// **The reader of *what is true now*, and [`silencings`] is the reader of *what happened*.** This
/// one answers `GET /map`, which needs one judgement per decision so §5.3's numbers can reconcile;
/// that one answers §6.2's pile, which needs every row and drops none of the filters. Two readers
/// of one table, and the pair is deliberate — see [`silencings`], which argues the other side.
pub async fn judgements(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<Vec<Judged>> {
    let rows = sqlx::query_as::<_, JudgedRow>(
        "SELECT t.decision_id, t.verdict, t.reason, t.model, t.computed_at, t.inputs_digest
           FROM map_triage t
           JOIN map_decisions d ON d.id = t.decision_id
          WHERE d.project_id = ?
            AND d.approved_at IS NOT NULL
            AND d.retired_at IS NULL
            AND t.id = (SELECT latest.id
                          FROM map_triage latest
                         WHERE latest.decision_id = t.decision_id
                         ORDER BY latest.computed_at DESC, latest.id DESC
                         LIMIT 1)
          ORDER BY t.decision_id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().filter_map(judged_from_row).collect())
}

/// One silencing, with the decision it was about — the whole row §6.2's pile is made of.
///
/// **Carries the decision's own words and not merely its id**, because the pile is read by somebody
/// who does not have the map open beside it. §6.2 asks for the reason and the model; §1 says the
/// gesture the owner cannot perform is cross-referencing three hundred rows by hand, and a pile of
/// ids would ask for exactly that.
///
/// No `inputs_digest`. This type is the record of what the triager DID, and staleness is a question
/// about what is true now — [`Judged`] carries the digest for the reader that asks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Silencing {
    pub decision_id: i64,
    pub spec_slug: String,
    pub section: String,
    pub text: String,
    /// Why the triager silenced it, in its own words. Never empty — `0119` refuses it.
    pub reason: String,
    /// Which brain or model answered. §6.2 names it: the repair for a triager that silences too
    /// much is to stop using that triager, and that is not a decision anybody can take about a pile
    /// that will not say who filled it.
    pub model: String,
    pub computed_at: String,
    /// The decision has since been retired — the owner said no, or a later extraction superseded it.
    ///
    /// **On the row rather than filtered out of the query**, which is the whole point of [`silencings`]
    /// not repeating [`approved`]'s `retired_at IS NULL`. A reader auditing the triager does not
    /// care whether the decision survived; hiding the silencing because the decision was later
    /// withdrawn deletes exactly the evidence §13's mitigation rests on. What the flag buys is that
    /// the pile can say *this decision is gone* while still showing what was said about it.
    pub retired: bool,
}

/// When this project's triager last answered anything, or `None` if it never has.
///
/// **The one question the other three readers of this table cannot answer, and the panel was
/// reduced to guessing at it.** [`judgements`] returns what still describes the map and
/// [`silencings`] returns the silences; a run that flagged everything and whose answers have all
/// since gone stale leaves both empty, which is indistinguishable from a project nobody ever pressed
/// the button on. The panel had to hedge — *as far as this map can see, it has never run* — and a
/// hedge is what an honest surface writes when the daemon will not answer a question it could.
///
/// **Unfiltered by staleness and by standing, on purpose, because the question is whether a run
/// HAPPENED and not whether anything it produced is still true.** Filtering either way would make
/// `None` mean *nothing it said still stands*, which is a different sentence and one the panel
/// already says elsewhere with numbers.
///
/// `approved_at IS NOT NULL` is kept and `retired_at` is deliberately not, which is [`silencings`]'
/// split verbatim: §4 and §6 mean the triager never sees an unapproved decision, so a judgement
/// against one is a defect rather than a record — while a run that judged decisions the owner has
/// since retired is a run that happened.
///
/// `MAX` over an empty set is one row holding NULL rather than no rows at all, so this is a
/// `fetch_one` of an `Option` and never a `fetch_optional`: reading it the other way would have made
/// *no such project* and *never run* the same answer, which is the pair
/// [`get_project_map_decisions`] refuses to collapse one route over.
pub async fn last_triaged(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> sqlx::Result<Option<String>> {
    let (when,): (Option<String>,) = sqlx::query_as(
        "SELECT MAX(t.computed_at)
           FROM map_triage t
           JOIN map_decisions d ON d.id = t.decision_id
          WHERE d.project_id = ?
            AND d.approved_at IS NOT NULL",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    Ok(when)
}

/// How many silencings one read of §6.2's pile carries.
///
/// **§6.2 asks for *sempre acessível*, which is not *all at once*.** `map_triage` is append-only by
/// design — a triager that silenced something it should not have is a bug, and the row proving it
/// did IS §13's mitigation — so this table only ever grows, and it grows by up to
/// `MAX_TRIAGE_BATCH` rows every time somebody presses the button this feature exists to encourage.
/// The window reads that route on every open of the map mode. Uncapped, the payload of a read grows
/// without bound with the number of presses, which is the one axis nothing else here is bounded on:
/// the write side was capped at twenty and the read side at nothing.
///
/// **Two hundred, argued from what is on the other end of it rather than rounded.** A run silences
/// at most `MAX_TRIAGE_BATCH` = 20 decisions, so this is ten saturated runs of history — well past
/// the point where an older silencing is being read as evidence rather than skimmed as a list. The
/// panel draws twelve rows at a time and filters the rest against the judgements the map is still
/// holding, so the cap has to clear the whole currently-silenced set with room over it, and this
/// repository's approved population is ~80 decisions today against the ~350 §10 predicts. And a row
/// is not small: the decision's own text travels with it, deliberately, because a pile of ids would
/// ask for the cross-reference §1 says the owner cannot perform — call it ~400 bytes, so two hundred
/// rows is ~80 KB on a route the window performs per open, the same order as
/// [`crate::map_recency::WINDOW`]'s measured git walk on the same request.
///
/// **What the cap does NOT do is hide that it happened.** [`SilencedPile::total`] is counted over
/// the uncapped set, so the remainder is always available to be said out loud — a pile that quietly
/// stopped is the same defect as a batch that quietly truncated, which is the whole reason
/// `TriageReport::left_over` exists one route over.
pub const SILENCED_PAGE: usize = 200;

/// §6.2's pile as one read of it: the newest rows, and how many there are altogether.
///
/// **The total is the count of the uncapped set and never `rows.len()`.** That is the entire point
/// of returning a struct rather than a `Vec`: a capped list whose length is the only number
/// available reads as the whole pile, and its reader has no way to learn otherwise. Same shape and
/// same argument as `TriageReport::left_over`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SilencedPile {
    /// The newest [`SILENCED_PAGE`] of them, newest first.
    pub rows: Vec<Silencing>,
    /// Every silencing on record for this project, including the ones the cap left out.
    pub total: usize,
}

/// Every silencing this project's triager has ever written, newest first.
///
/// **Every row and not the latest judgement per decision, which is the difference from
/// [`judgements`] and the reason `map_triage` was made append-only in the first place.** §6.2 asks
/// for *"a razão de **cada** silenciamento e o modelo que o produziu"*, and a reader that returned
/// one row per decision would lose the case the requirement most obviously covers: a decision
/// silenced last week and flagged today has a silencing in the table and none in the pile. Nothing
/// was reading the history until this function; the table's append-only discipline was a promise
/// with no reader to keep it for.
///
/// **`retired_at` is deliberately NOT in the `WHERE`, and `approved_at IS NOT NULL` deliberately
/// is.** They look like one filter and are two different claims. A retired decision is out of the
/// map and its silencing is still a thing the triager did — §6.2's *sempre acessível* has no
/// exception for a decision somebody later withdrew, and the retirement is reported on the row as
/// [`Silencing::retired`] instead. An UNAPPROVED decision is different: §4 and §6 mean the triager
/// never sees one, so a judgement against one is not a record to preserve, it is a defect, and a
/// reader that quietly displayed it would be the place that defect went unnoticed.
///
/// **The verdict is compared against [`Judgement::Silenced`]'s own storage form** rather than the
/// literal `'silenced'`, so the word has one owner. `0119`'s CHECK admits exactly two values and
/// `Judgement::as_str` writes them; a third spelling here would be a filter that silently matched
/// nothing the day either changed.
///
/// **Newest first, tie-broken by `id DESC` and never by `decision_id`.** A sweep silences a batch
/// inside one instant — `computed_at` is a clock and not a counter — so ties are the ordinary case,
/// and insertion order is the order the triager actually answered in. Ordering by decision id would
/// interleave two runs of one project and make the last press impossible to read off the top of the
/// pile, which is what somebody opens this for.
pub async fn silencings(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<SilencedPile> {
    // `COUNT(*) OVER ()` rather than a second `SELECT COUNT(*)`, because a window function is
    // evaluated before `LIMIT` and inside the same statement: the total and the rows are then one
    // answer about one instant. Two statements would let a run land between them and report a
    // remainder that was never true — small, and exactly the kind of quietly wrong number this
    // feature exists against.
    let rows = sqlx::query_as::<_, SilencingRow>(
        "SELECT t.decision_id, d.spec_slug, d.section, d.text, t.reason, t.model, t.computed_at,
                CASE WHEN d.retired_at IS NULL THEN 0 ELSE 1 END,
                COUNT(*) OVER ()
           FROM map_triage t
           JOIN map_decisions d ON d.id = t.decision_id
          WHERE d.project_id = ?
            AND d.approved_at IS NOT NULL
            AND t.verdict = ?
          ORDER BY t.computed_at DESC, t.id DESC
          LIMIT ?",
    )
    .bind(project_id)
    .bind(Judgement::Silenced.as_str())
    .bind(SILENCED_PAGE as i64)
    .fetch_all(pool)
    .await?;

    // No rows means no silencings, so the one case where the window function has nowhere to put the
    // total is also the one case where the total is knowable without it.
    let total = rows.first().map_or(0, |row| row.8.max(0) as usize);

    Ok(SilencedPile {
        rows: rows
            .into_iter()
            .map(
                |(
                    decision_id,
                    spec_slug,
                    section,
                    text,
                    reason,
                    model,
                    computed_at,
                    retired,
                    _,
                )| {
                    Silencing {
                        decision_id,
                        spec_slug,
                        section,
                        text,
                        reason,
                        model,
                        computed_at,
                        retired: retired != 0,
                    }
                },
            )
            .collect(),
        total,
    })
}

/// The columns [`silencings`] selects, named once for [`JudgedRow`]'s reason and with the same
/// hazard: six of the nine are `String`, so a `SELECT` that swapped any pair would still typecheck
/// and surface as a pile whose reasons are all section headings.
///
/// The retirement flag is an `i64` and not a `bool` because the expression producing it is a SQL
/// `CASE`, and a `CASE` returning 0/1 is the portable spelling — `d.retired_at IS NOT NULL` decodes
/// too, and leaves a reader wondering which of SQLite's truthiness rules is in play.
///
/// The last is `COUNT(*) OVER ()`, repeated identically on every row of the answer: the size of the
/// pile before [`SILENCED_PAGE`] cut it. It rides on the row because that is what makes it one
/// answer with the rows rather than a second question asked a moment later.
type SilencingRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
);

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

    /// A record comes back canonical, whatever order it was handed in.
    ///
    /// Sorted and deduplicated by the writer and not by the caller, because the only reader that
    /// matters is a set difference: two records of the same set must compare equal, or a stamp
    /// would lapse over a list that was reordered by a UI.
    #[tokio::test]
    async fn a_record_comes_back_canonical_whatever_order_it_arrived_in() {
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        assert!(
            record_anchor(
                &pool,
                "alpha",
                id,
                &[
                    "core/src/b.rs".to_owned(),
                    "core/src/a.rs".to_owned(),
                    "core/src/b.rs".to_owned(),
                ],
                AnchorSource::Owner,
            )
            .await
            .unwrap()
        );

        let held = anchors(&pool, "alpha").await.unwrap();
        let record = held.get(&id).expect("the decision has a record");
        assert_eq!(record.paths, ["core/src/a.rs", "core/src/b.rs"]);
        assert_eq!(record.source, AnchorSource::Owner);
    }

    /// Append-only, and the last row is the answer (§9.2).
    ///
    /// Asserted through the reader rather than by counting rows, because the property that matters
    /// is which set the map anchors to — and the earlier rows staying in the table is what somebody
    /// reads when they are trying to work out whether a comment went on purpose.
    #[tokio::test]
    async fn the_latest_record_is_the_one_the_map_anchors_to() {
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        record_anchor(
            &pool,
            "alpha",
            id,
            &["core/src/a.rs".to_owned()],
            AnchorSource::Stamp,
        )
        .await
        .unwrap();
        record_anchor(
            &pool,
            "alpha",
            id,
            &["core/src/b.rs".to_owned()],
            AnchorSource::Owner,
        )
        .await
        .unwrap();

        let held = anchors(&pool, "alpha").await.unwrap();
        assert_eq!(held[&id].paths, ["core/src/b.rs"]);
        assert_eq!(held[&id].source, AnchorSource::Owner);

        let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM map_anchors WHERE decision_id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            rows.0, 2,
            "the first record is history, not something to overwrite"
        );
    }

    /// **An empty record is an assertion and never an absence**, and the two must not read alike.
    ///
    /// *These files were this decision's and now none are* is what somebody says about code they
    /// genuinely removed. *Nobody has written anything down* is the day-one state of every decision
    /// in the project. A reader that collapsed them would turn a deliberate withdrawal into an
    /// oversight, which is the silent wrong answer this whole feature refuses.
    #[tokio::test]
    async fn an_empty_record_is_an_assertion_and_not_an_absence() {
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        assert!(!anchors(&pool, "alpha").await.unwrap().contains_key(&id));

        record_anchor(&pool, "alpha", id, &[], AnchorSource::Owner)
            .await
            .unwrap();

        let held = anchors(&pool, "alpha").await.unwrap();
        let record = held.get(&id).expect("an empty record is still a record");
        assert!(record.paths.is_empty());
    }

    /// One project cannot write down anchors for another's decision.
    ///
    /// The guard is in the `WHERE` and not in the handler, which is [`decide`]'s argument about
    /// checks a second caller forgets — and this is the test that keeps it there.
    #[tokio::test]
    async fn a_neighbours_decision_is_not_ours_to_anchor() {
        let pool = test_pool().await;
        let mine = an_approved_decision(&pool, "alpha").await;

        assert!(
            !record_anchor(
                &pool,
                "beta",
                mine,
                &["core/src/a.rs".to_owned()],
                AnchorSource::Owner
            )
            .await
            .unwrap()
        );
        assert!(anchors(&pool, "alpha").await.unwrap().is_empty());
    }

    /// A decision nobody approved cannot be anchored either.
    ///
    /// [`stamp`]'s filter verbatim, and repeated for its reason: a line still sitting in the pile
    /// waiting to be answered is not in the map, and anchoring it would put a file on a map that
    /// does not have the decision it belongs to.
    #[tokio::test]
    async fn a_line_nobody_has_answered_yet_cannot_be_anchored() {
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let waiting = pending(&pool, "alpha").await.unwrap()[0].id;

        assert!(
            !record_anchor(
                &pool,
                "alpha",
                waiting,
                &["core/src/a.rs".to_owned()],
                AnchorSource::Owner,
            )
            .await
            .unwrap()
        );
    }

    /// The two, and nothing else.
    ///
    /// **A unit test and not a round trip through the table, because the `CHECK` makes the round
    /// trip unreachable** — `source` admits exactly `'stamp'` and `'owner'`, and `PRAGMA
    /// writable_schema` does not turn a constraint off. The branch in [`anchors`] that drops such a
    /// row is therefore defensive rather than exercised, and it stays: a restored backup, a
    /// hand-edited database and a later migration are all ways a third word arrives, and the
    /// alternative to dropping the row is guessing which of the two claims it was making.
    #[test]
    fn a_source_nobody_can_read_is_not_guessed_at() {
        assert_eq!(AnchorSource::from_wire("stamp"), Some(AnchorSource::Stamp));
        assert_eq!(AnchorSource::from_wire("owner"), Some(AnchorSource::Owner));
        assert_eq!(AnchorSource::from_wire("Owner"), None);
        assert_eq!(AnchorSource::from_wire(""), None);
        assert_eq!(AnchorSource::from_wire("whatever"), None);
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
    async fn a_second_extraction_supersedes_the_pile_nobody_had_read() {
        // `0117`'s `retired_at` column says it is set "when the owner says no, **or when a later
        // extraction supersedes this one**", and only the first half was ever written. Without the
        // second, re-reading a spec left both lists live and approving the new one put two copies of
        // every line into the map — two model calls per line in triage, and no way for the owner to
        // tell which copy they were looking at.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        assert_eq!(pending(&pool, "alpha").await.unwrap().len(), 2);

        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();

        let waiting = pending(&pool, "alpha").await.unwrap();
        assert_eq!(
            waiting.len(),
            2,
            "the owner reads one list per spec, not one per time it was read"
        );
        assert_eq!(
            retired_texts(&pool, "alpha").await,
            vec!["Alfa.".to_owned(), "Beta.".to_owned()],
            "superseded rather than deleted — a row that is gone cannot say it was once proposed"
        );
    }

    #[tokio::test]
    async fn a_second_extraction_leaves_an_approved_decision_alone() {
        // **The half that must never be "tidied up" into the half above**, and the two reasons are
        // the whole of why the `UPDATE` says `approved_at IS NULL`.
        //
        // First, approval is the owner's act and §4 and §6 reserve it to a human; an extractor that
        // could take it back would be the model recovering the one authority this design removes
        // from it, through a door nobody is watching.
        //
        // Second, and this is the one that would go unnoticed: `map_stamps.decision_id` and
        // `map_triage.decision_id` point at approved rows. Retiring one drops it out of `approved`,
        // so the owner's verdict and the triager's judgement go on existing in their tables while
        // vanishing from every reader — erased by a re-extraction nobody would connect to it. This
        // test stamps the decision first precisely so that is what it is asserting.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;
        assert!(
            stamp(&pool, "alpha", id, Verdict::Settled, Some("aaa a.rs"), None)
                .await
                .unwrap()
        );

        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();

        let live: Vec<i64> = approved(&pool, "alpha")
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(
            live,
            vec![id],
            "the approved decision is still in the map the stamp was made against"
        );
        assert_eq!(
            stamps(&pool, "alpha").await.unwrap().len(),
            1,
            "and its stamp is still reachable, which is what retiring it would have broken"
        );
        // The line that was still waiting when the re-read happened IS superseded — this test is
        // about the approved row and not about weakening the rule above it.
        assert_eq!(
            retired_texts(&pool, "alpha").await,
            vec!["Beta.".to_owned()]
        );
    }

    #[tokio::test]
    async fn superseding_is_scoped_to_one_spec_and_one_project() {
        // A second document being re-read has nothing to do with this one, and another owner's pile
        // has nothing to do with either. `map_decisions.id` is a global integer, so the project is
        // the only thing standing between two owners — the argument `decide` already makes, owed
        // again by every statement that writes without being handed an id.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        record(&pool, "alpha", "outra", Brain::Local, &two_decisions())
            .await
            .unwrap();
        record(&pool, "beta", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();

        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();

        assert_eq!(
            retired_texts(&pool, "alpha").await,
            vec!["Alfa.".to_owned(), "Beta.".to_owned()],
            "only the spec that was re-read loses its unread pile"
        );
        assert_eq!(
            pending(&pool, "alpha").await.unwrap().len(),
            4,
            "the other spec's two are untouched, beside this spec's fresh two"
        );
        assert!(
            retired_texts(&pool, "beta").await.is_empty(),
            "another project's identically named spec is not this project's business"
        );
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

    /// Every judgement ever recorded on one decision, oldest first, straight off the table.
    ///
    /// [`judgements`] answers with the last one only, so the history it holds back can be asserted
    /// from nowhere else — and that history is not bookkeeping. §13 rates *o triador silencia o que
    /// devia mostrar* a **real** residual risk whose only mitigation is that the pile stays visible
    /// with its reasons, so the row proving a silence happened is the whole of the cure.
    async fn every_judgement(pool: &sqlx::SqlitePool, decision_id: i64) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT verdict FROM map_triage WHERE decision_id = ? ORDER BY id",
        )
        .bind(decision_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// One judgement written past [`triage`] and straight at the table.
    ///
    /// Necessary rather than convenient: [`Judgement`] has two variants and no third, so a test
    /// going through [`triage`] could only ever assert that a Rust enum is a Rust enum. What is
    /// being asked here is what the **table** admits, and that answer has to hold for the caller
    /// who never touches the enum — which is the only caller the CHECK exists for.
    async fn raw_triage(
        pool: &sqlx::SqlitePool,
        decision_id: i64,
        verdict: &str,
        reason: &str,
    ) -> sqlx::Result<sqlx::sqlite::SqliteQueryResult> {
        sqlx::query(
            "INSERT INTO map_triage
               (decision_id, verdict, reason, model, computed_at, inputs_digest)
             VALUES (?, ?, ?, 'local', '2026-08-26T08:00:00+00:00', 'e3b0c442')",
        )
        .bind(decision_id)
        .bind(verdict)
        .bind(reason)
        .execute(pool)
        .await
    }

    #[tokio::test]
    async fn a_triage_verdict_the_model_is_not_allowed_to_reach_is_refused_by_the_table() {
        // §6's table names exactly one thing the triager is forbidden to do — **Aprovar** — and a
        // prohibition written in a comment, a handler and a Rust enum is a prohibition with three
        // ways round it. The column is the one that survives a caller who reaches for none of the
        // three, and this test is that boundary written down where it cannot be forgotten.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        // `approved` is §6's forbidden word verbatim. `settled` is worse, and is the reason this
        // list is not one item long: it is the word that actually turns something green in this
        // codebase, and it is what a caller reaching for `map_stamps`' vocabulary would write —
        // §6.1's *devolvida pela porta da renderização*, arriving through the column instead. The
        // two tables share a column NAME and may never share a VALUE. `FLAGGED` is the same
        // mistake a third way: SQLite compares text case-sensitively, the storage form is lower
        // case, and `Judgement::from_wire` reads nothing else — so such a row would be a judgement
        // no reader can ever return, leaving the decision among the ones nobody looked at while a
        // row in the table insists somebody did.
        for verdict in ["approved", "settled", "partial", "withdrawn", "FLAGGED", ""] {
            assert!(
                raw_triage(&pool, id, verdict, "porque sim").await.is_err(),
                "the table admitted {verdict:?}"
            );
        }
        assert!(
            every_judgement(&pool, id).await.is_empty(),
            "and not one of the six left a row behind"
        );

        for verdict in ["flagged", "silenced"] {
            assert!(
                raw_triage(&pool, id, verdict, "porque sim").await.is_ok(),
                "{verdict:?} is one of the two it is allowed to reach"
            );
        }
        assert_eq!(
            every_judgement(&pool, id).await,
            vec!["flagged".to_string(), "silenced".to_string()]
        );
    }

    #[tokio::test]
    async fn a_silencing_with_no_reason_is_refused() {
        // §6.2 keeps the silenced pile readable *com a razão de cada silenciamento*, because *um
        // triador que silencia o que não devia é um bug do triador, e um bug só é corrigível se
        // for visível*. §13 rates that bug a real residual risk and names this pile as its only
        // mitigation — so a silence carrying nothing to read deletes the mitigation one row at a
        // time, and the table is what refuses to let it.
        //
        // Required on a flag too, and that is not symmetry for its own sake: a flag with no reason
        // is a nag the owner cannot answer, and a nag nobody can answer is one they stop reading —
        // which costs the same trust the silent green costs, from the other side.
        //
        // A tab is whitespace too, and bare `trim` in SQLite strips spaces and nothing else, so
        // `0119` names the characters it must actually see through. The same hole `0118`'s note
        // CHECK already argues, owed here twice over.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        for judgement in [Judgement::Silenced, Judgement::Flagged] {
            for reason in ["", "   ", "\t", "\n \r"] {
                assert!(
                    triage(&pool, "alpha", id, judgement, reason, "local", "e3b0c442")
                        .await
                        .is_err(),
                    "{judgement:?} carrying {reason:?} for a reason"
                );
            }
        }
        assert!(
            every_judgement(&pool, id).await.is_empty(),
            "and none of the eight left a row behind"
        );
    }

    #[tokio::test]
    async fn a_judgement_that_names_no_model_or_looked_at_nothing_is_refused() {
        // The other half of §6.2, which asks for the reason *e o modelo que o produziu*: a pile
        // that cannot say which brain silenced a row is a pile nobody can act on, because the
        // repair for a triager that silences too much is to stop using that triager.
        //
        // `inputs_digest` is refused blank for a sharper reason, and it is the one place in this
        // table where an empty value is not merely unreadable but actively wrong. Staleness is
        // decided by comparing the stored digest against the one computed now; a hash is never
        // empty, so a stored `''` can only have come from a caller that failed to compute one —
        // and if a reader ever computes `''` the same way, the two compare EQUAL and a judgement
        // about a decision that has since changed is presented as current. That is `0118`'s
        // collapse of *could not compute* into *nothing to watch*, reappearing one table over and
        // one slice later, and it is refused here for the reason it is refused there.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        for blank in ["", "   ", "\t"] {
            assert!(
                triage(
                    &pool,
                    "alpha",
                    id,
                    Judgement::Flagged,
                    "porque sim",
                    blank,
                    "e3b0c442"
                )
                .await
                .is_err(),
                "a judgement whose model is {blank:?}"
            );
            assert!(
                triage(
                    &pool,
                    "alpha",
                    id,
                    Judgement::Flagged,
                    "porque sim",
                    "local",
                    blank
                )
                .await
                .is_err(),
                "a judgement whose inputs_digest is {blank:?}"
            );
        }
        assert!(every_judgement(&pool, id).await.is_empty());
    }

    #[tokio::test]
    async fn re_triaging_adds_a_row_and_the_last_one_is_read() {
        // Append-only, as `map_stamps` is, and for a reason of its own: a triager that silenced
        // something it should have shown is a bug, and the row recording that it did so is the
        // only way anybody finds it (§13). A writer that replaced the previous row would erase
        // exactly the evidence the risk table names as that bug's own mitigation.
        //
        // Both halves are asserted, because only the pair discriminates — a table that kept
        // everything and a reader that showed everything would be a map with two answers for one
        // decision.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Flagged,
                "o tipo B deixou de bater com o código",
                "local",
                "d1"
            )
            .await
            .unwrap()
        );
        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Silenced,
                "nada mexeu desde a extracção",
                "local",
                "d2"
            )
            .await
            .unwrap()
        );

        assert_eq!(
            every_judgement(&pool, id).await,
            vec!["flagged".to_string(), "silenced".to_string()],
            "the flag it later took back is still there to be found"
        );
        let latest = judgements(&pool, "alpha").await.unwrap();
        assert_eq!(
            latest.len(),
            1,
            "one answer per decision, not one per judgement"
        );
        assert_eq!(latest[0].judgement, Judgement::Silenced);
        assert_eq!(latest[0].reason, "nada mexeu desde a extracção");
        assert_eq!(latest[0].model, "local");
        assert_eq!(latest[0].inputs_digest, "d2");

        // The tie, and why `MAX(computed_at)` alone would not settle it. This row is given the
        // timestamp of the one before it, which is what two judgements written inside one tick of
        // the clock look like — and a triage run sweeps a project's whole *never seen* pile in a
        // batch, so the collision is the ordinary case here rather than the contrived one. Without
        // the `id DESC` tie-break the winner is whichever row SQLite reached first, and a decision
        // that is silenced on one read and flagged on the next with nothing having happened is the
        // portrait this map refuses to be.
        let when = latest[0].computed_at.clone();
        sqlx::query(
            "INSERT INTO map_triage
               (decision_id, verdict, reason, model, computed_at, inputs_digest)
             VALUES (?, 'flagged', 'e voltou a não bater', 'local', ?, 'd3')",
        )
        .bind(id)
        .bind(&when)
        .execute(&pool)
        .await
        .unwrap();

        let latest = judgements(&pool, "alpha").await.unwrap();
        assert_eq!(
            latest[0].judgement,
            Judgement::Flagged,
            "of two judgements sharing an instant, the one written later is the current one"
        );
    }

    #[tokio::test]
    async fn a_judgement_whose_inputs_moved_still_comes_back_for_the_caller_to_judge() {
        // The split slice 4 already made with `standing`, owed again here. Deciding whether a
        // judgement is stale needs the digest of the inputs **as they are now**, and computing
        // that means reading the repository — which is exactly what this module is kept away from
        // so that its SQL stays testable without one. So the row comes back whatever its digest
        // says, and the caller compares.
        //
        // What this pins is the direction of the omission: `inputs_digest` is ON the returned row.
        // A reader that dropped it would leave the caller unable to tell a current judgement from
        // a stale one, at which point every stale silence reads as a current one — a claim about
        // code nobody has looked at since it changed, which is the silent wrong this map refuses.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Silenced,
                "nada de estranho",
                "local",
                "as it was"
            )
            .await
            .unwrap()
        );

        let latest = judgements(&pool, "alpha").await.unwrap();
        assert_eq!(
            latest[0].inputs_digest, "as it was",
            "the row says what it looked at, and says nothing about whether that is still true"
        );
    }

    #[tokio::test]
    async fn one_project_s_triage_never_reaches_another_project_s_map() {
        // `map_triage` has no `project_id`; it reaches one only through `decision_id`, so the JOIN
        // is the whole of what stands between two owners' piles. The id is a global integer, and a
        // check that lived in the HTTP handler is a check the second caller forgets — the argument
        // `decide` makes, owed by both the reader and the writer here.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;
        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Flagged,
                "vale o olhar",
                "local",
                "d1"
            )
            .await
            .unwrap()
        );

        assert_eq!(judgements(&pool, "alpha").await.unwrap().len(), 1);
        assert!(judgements(&pool, "beta").await.unwrap().is_empty());

        assert!(
            !triage(
                &pool,
                "beta",
                id,
                Judgement::Silenced,
                "nada de estranho",
                "local",
                "d2"
            )
            .await
            .unwrap(),
            "and a judgement aimed at another project's decision lands nowhere"
        );
        assert_eq!(
            every_judgement(&pool, id).await,
            vec!["flagged".to_string()]
        );
    }

    #[tokio::test]
    async fn a_decision_nobody_approved_cannot_be_triaged() {
        // The other two clauses of the same `WHERE`, and neither is ceremony. Triaging a line
        // still waiting in the pile would have the model pass judgement on something the owner
        // never agreed exists — §4 reserves *deciding that a decision exists* to the owner, and
        // this is that authority arriving back through the triager instead. Triaging a retired one
        // would nag about a line somebody explicitly stood down.
        let pool = test_pool().await;
        record(&pool, "alpha", "design", Brain::Local, &two_decisions())
            .await
            .unwrap();
        let waiting = pending(&pool, "alpha").await.unwrap();

        assert!(
            !triage(
                &pool,
                "alpha",
                waiting[0].id,
                Judgement::Flagged,
                "vale o olhar",
                "local",
                "d1"
            )
            .await
            .unwrap(),
            "a line still waiting to be read"
        );
        assert!(decide(&pool, "alpha", waiting[1].id, false).await.unwrap());
        assert!(
            !triage(
                &pool,
                "alpha",
                waiting[1].id,
                Judgement::Silenced,
                "nada de estranho",
                "local",
                "d1"
            )
            .await
            .unwrap(),
            "a line the owner said no to"
        );
        assert!(
            !triage(
                &pool,
                "alpha",
                9_999,
                Judgement::Flagged,
                "vale o olhar",
                "local",
                "d1"
            )
            .await
            .unwrap(),
            "an id that names nothing at all"
        );
        assert!(judgements(&pool, "alpha").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn every_silencing_is_in_the_pile_and_not_only_the_latest_judgement() {
        // **§6.2 asks for *"a razão de CADA silenciamento"*, and `judgements` answers with one row
        // per decision.** A decision silenced last week and flagged today therefore had a silencing
        // in the table and none in the pile — the append-only discipline `map_triage` was given for
        // exactly this, with nothing reading it. §13 rates the triager silencing what it should have
        // shown a **real** residual risk whose only mitigation is the pile, and a mitigation that
        // loses the row the moment the triager changes its mind is not one.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;
        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Silenced,
                "nada de estranho na primeira leitura",
                "local",
                "d1"
            )
            .await
            .unwrap()
        );
        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Silenced,
                "continua a não me saltar nada à vista",
                "cloud",
                "d2"
            )
            .await
            .unwrap()
        );
        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Flagged,
                "afinal isto merece o teu olhar",
                "cloud",
                "d3"
            )
            .await
            .unwrap()
        );

        let pile = silencings(&pool, "alpha").await.unwrap().rows;

        assert_eq!(
            pile.len(),
            2,
            "both silencings survive the flag that overturned them: {pile:?}"
        );
        assert_eq!(
            judgements(&pool, "alpha").await.unwrap()[0].judgement,
            Judgement::Flagged,
            "and the map's own reader still answers with the latest, which is the pair's whole \
             point"
        );
        // Newest first, tie-broken by insertion order — a sweep silences a batch inside one
        // instant, so `computed_at` alone decides almost nothing and the last press is what
        // somebody opens this to read.
        assert_eq!(pile[0].reason, "continua a não me saltar nada à vista");
        assert_eq!(pile[0].model, "cloud");
        assert_eq!(pile[1].reason, "nada de estranho na primeira leitura");
        assert_eq!(pile[1].model, "local");
        // The decision's own words travel with each row, or the pile is a list of ids and the
        // reader has to do the cross-reference §1 says they cannot.
        assert_eq!(pile[0].text, two_decisions()[0].text);
        assert_eq!(pile[0].section, two_decisions()[0].section);
        assert_eq!(pile[0].spec_slug, "design");
        assert!(!pile[0].retired);

        // A flag is not a silencing, and nothing here widens the pile into the whole table.
        assert!(pile.iter().all(|row| !row.reason.contains("merece")));
        assert!(silencings(&pool, "beta").await.unwrap().rows.is_empty());
    }

    #[tokio::test]
    async fn the_silenced_pile_is_capped_and_still_says_how_big_it_is() {
        // **§6.2 says *sempre acessível*, which is not *all at once*.** This table is append-only
        // and grows by up to a batch per press of the button the feature exists to encourage, on a
        // route the window performs every time the map opens — so an uncapped read grows without
        // bound with how often somebody uses the thing. The cap is admissible only because the
        // uncapped size comes back with it: a pile that quietly stopped is the same defect as a
        // batch that quietly truncated, which is the whole reason `TriageReport::left_over` exists.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;

        let written = SILENCED_PAGE + 7;
        for n in 0..written {
            assert!(
                triage(
                    &pool,
                    "alpha",
                    id,
                    Judgement::Silenced,
                    &format!("silenciamento {n}"),
                    "cloud",
                    "d1",
                )
                .await
                .unwrap()
            );
        }

        let pile = silencings(&pool, "alpha").await.unwrap();

        assert_eq!(pile.rows.len(), SILENCED_PAGE, "the cap is the cap");
        assert_eq!(
            pile.total, written,
            "and the number it was cut from is on the answer, or the reader has no way to learn              that anything was cut at all"
        );
        // **The cap drops the OLDEST, which is the end it has to drop.** A sweep writes the newest
        // rows, the current silencings are the newest per decision, and §6.2's pile is opened to
        // read what the triager did lately. Cutting from the other end would take the rows the
        // panel joins against the map and leave the history nobody asked for.
        assert_eq!(
            pile.rows[0].reason,
            format!("silenciamento {}", written - 1)
        );
        assert!(
            pile.rows.iter().all(|row| row.reason != "silenciamento 0"),
            "the oldest is what the cap spends, and it is the one the total speaks for"
        );
    }

    #[tokio::test]
    async fn a_reason_the_daemon_wrote_comes_back_marked_and_a_model_s_does_not() {
        // **`model` names the brain that ANSWERED, and that stays true of an answer nobody could
        // read.** `map_triage::unreadable_flag` records such a failure as a flag rather than
        // dropping it, so the sentence in `reason` is sometimes this daemon's note about a model and
        // not a model's note about the code — and a reader handed the two under one name is being
        // shown a machine's failure as an opinion, which is the attribution §6.2 exists to protect,
        // inverted. Computed here rather than by whoever draws the pile, because a convention spelled
        // in two languages is one that has already stopped working somewhere.
        let pool = test_pool().await;
        let mine = an_approved_decision(&pool, "alpha").await;
        let theirs = pending(&pool, "alpha").await.unwrap()[0].id;
        assert!(decide(&pool, "alpha", theirs, true).await.unwrap());

        assert!(
            triage(
                &pool,
                "alpha",
                mine,
                Judgement::Flagged,
                &format!(
                    "{} o modelo respondeu e ninguém conseguiu ler",
                    crate::map_triage::DAEMON_MARK
                ),
                "cloud",
                "d1",
            )
            .await
            .unwrap()
        );
        assert!(
            triage(
                &pool,
                "alpha",
                theirs,
                Judgement::Flagged,
                "isto contradiz o que o módulo faz",
                "cloud",
                "d2",
            )
            .await
            .unwrap()
        );

        let read = judgements(&pool, "alpha").await.unwrap();
        let mark = |decision_id: i64| {
            read.iter()
                .find(|row| row.decision_id == decision_id)
                .expect("both judgements come back")
                .machine_written
        };

        assert!(
            mark(mine),
            "a sentence opening the daemon's mark is the daemon's"
        );
        assert!(
            !mark(theirs),
            "and everything else is the model's, or the mark means nothing"
        );
    }

    #[tokio::test]
    async fn a_project_nobody_triaged_has_no_last_run_and_one_that_was_has_one() {
        // **The question is whether a run HAPPENED**, which neither of the other two readers can
        // answer: `judgements` returns what still describes the map and `silencings` returns the
        // silences, so a run that flagged everything and whose answers later went stale comes back
        // empty from both — indistinguishable from a project nobody ever pressed the button on. The
        // panel was reduced to hedging over exactly that gap.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;
        let _ = an_approved_decision(&pool, "beta").await;

        assert!(
            last_triaged(&pool, "alpha").await.unwrap().is_none(),
            "never run is a real answer and it is this one"
        );

        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Flagged,
                "vale o olhar",
                "cloud",
                "d1"
            )
            .await
            .unwrap()
        );

        let when = last_triaged(&pool, "alpha")
            .await
            .unwrap()
            .expect("a run happened");
        assert!(!when.is_empty());
        assert!(
            last_triaged(&pool, "beta").await.unwrap().is_none(),
            "and it reaches one project only, through the JOIN that is the whole isolation"
        );

        // **Unfiltered by standing and by retirement, because the run still happened.** Filtering
        // would make `None` mean *nothing it said still stands*, which is a different sentence and
        // one the counts already carry.
        sqlx::query("UPDATE map_decisions SET retired_at = ? WHERE id = ?")
            .bind("2026-08-26T12:00:00+00:00")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            last_triaged(&pool, "alpha").await.unwrap().as_deref(),
            Some(when.as_str()),
            "retiring the decision does not un-run the triager"
        );
        assert!(
            judgements(&pool, "alpha").await.unwrap().is_empty(),
            "while the map's own reader drops it, which is the asymmetry stated from the other side"
        );
    }

    #[tokio::test]
    async fn a_silencing_survives_the_decision_being_retired() {
        // **`retired_at` is deliberately absent from this reader's `WHERE`, and that is the one
        // clause it does not copy from [`approved`].** A silencing of a decision the owner later
        // said no to — or that a re-extraction superseded — is still a thing the triager did, and
        // hiding it deletes the bug report by way of its own subject. §6.2's *sempre acessível* has
        // no exception for a decision somebody withdrew.
        //
        // What IS copied is `approved_at IS NOT NULL`, and the two are different claims: the
        // triager never sees an unapproved line (§4, §6), so a judgement against one is not a
        // record to preserve, it is a defect, and a reader that displayed it is where that defect
        // would go unnoticed.
        let pool = test_pool().await;
        let id = an_approved_decision(&pool, "alpha").await;
        assert!(
            triage(
                &pool,
                "alpha",
                id,
                Judgement::Silenced,
                "nada de estranho",
                "local",
                "d1"
            )
            .await
            .unwrap()
        );

        sqlx::query("UPDATE map_decisions SET retired_at = ? WHERE id = ?")
            .bind("2026-08-26T12:00:00+00:00")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        let pile = silencings(&pool, "alpha").await.unwrap().rows;

        assert_eq!(pile.len(), 1, "{pile:?}");
        assert_eq!(pile[0].reason, "nada de estranho");
        // **Reported and not hidden**, so the pile can say the decision is gone while still showing
        // what was said about it.
        assert!(
            pile[0].retired,
            "a reader that could not tell would present a withdrawn decision as a live one"
        );
        // And the map's own reader drops it, which is the asymmetry stated from the other side: a
        // retired decision is not in the map, so it has no standing there to be counted against.
        assert!(judgements(&pool, "alpha").await.unwrap().is_empty());
    }
}
