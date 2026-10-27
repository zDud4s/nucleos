//! Knowledge proposal write paths and provenance.

use super::*;

/// What somebody is asking the store to learn.
///
/// A struct and not eight positional arguments: the call site of the earlier shape was four string
/// literals in a row, where swapping `title` and `body` compiles, passes, and is found by a person
/// reading a strange prompt a week later.
pub struct Declaration<'a> {
    pub project_id: Option<&'a str>,
    pub origin_run_id: Option<i64>,
    pub kind: Kind,
    pub title: &'a str,
    pub body: &'a str,
    pub reasoning: &'a str,
    /// The row this one replaces — ended if and when THIS one is approved, never before.
    pub supersedes: Option<i64>,
}

/// The two things a declaration can say that the store must refuse.
#[derive(Debug)]
pub enum ProposeError {
    Db(sqlx::Error),
    /// `supersedes` names a row that is not there — a chain nobody could read back.
    UnknownPredecessor(i64),
    /// `supersedes` names a row in another scope. Refused because it is the one column that writes
    /// across the boundary the rest of this module exists to hold.
    ForeignPredecessor(i64),
}

impl std::fmt::Display for ProposeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposeError::Db(error) => write!(formatter, "{error}"),
            ProposeError::UnknownPredecessor(id) => {
                write!(formatter, "there is nothing known with id {id} to replace")
            }
            ProposeError::ForeignPredecessor(id) => {
                write!(
                    formatter,
                    "what is known with id {id} belongs to another scope"
                )
            }
        }
    }
}

impl std::error::Error for ProposeError {}

impl From<sqlx::Error> for ProposeError {
    fn from(error: sqlx::Error) -> Self {
        ProposeError::Db(error)
    }
}

/// A run declaring something it thinks the next run should know.
///
/// Writes the row `proposed` AND the proposal that asks about it, in one transaction — the two are
/// one act, and a crash between them would leave either a lesson nobody can approve or a question
/// about a lesson that is not there.
///
/// The row is written now rather than on approval, unlike `create_calendar_event`'s shape, and the
/// difference is deliberate: a rejected event is nothing, but a rejected LESSON is a record worth
/// keeping — it is how somebody later sees what the agent kept trying to learn and was told no to.
///
/// **The three new columns are derived here and not asked for**, by exactly the translation
/// `0143_knowledge.sql` applies to the rows that came before: the layer from the kind, the scope
/// from the project id, and the source from whether a run is behind the request. It is what keeps
/// this phase a move rather than a change — the door writes what it wrote. Deriving the scope from
/// the run instead of from the caller is a later decision, with its own reasons.
/// The fingerprint is derived from the body with the consolidator's own function, so
/// `consolidate::blocked_outcome` sees a pending, rejected, or reverted proposal.
pub async fn propose(
    pool: &SqlitePool,
    declaration: Declaration<'_>,
) -> Result<(i64, i64), ProposeError> {
    let mut tx = pool.begin().await?;
    let ids = propose_in(&mut tx, declaration).await?;
    tx.commit().await?;
    crate::embed::nudge();
    // After the commit, so a woken reader finds the proposal.
    nucleos_base::wake::bump(nucleos_base::wake::Topic::Proposals);
    Ok(ids)
}

/// `propose`, inside a transaction the caller owns — for a caller whose own writes must land or
/// vanish together with the lesson and its question. Nothing is committed here.
pub async fn propose_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    declaration: Declaration<'_>,
) -> Result<(i64, i64), ProposeError> {
    propose_in_with(tx, declaration, &Provenance::default()).await
}

/// Where a learning came from, when somebody other than a run or the owner is writing it.
///
/// Kept beside [`Declaration`] and not inside it: a struct literal cannot gain fields without
/// editing every literal, and the callers that exist must stay as they are. `None` everywhere is
/// today's behaviour - each field overrides exactly one value the door would otherwise derive.
#[derive(Default)]
pub struct Provenance<'a> {
    /// Replaces the `run`/`owner` derivation (the distiller writes `distiller`).
    pub source: Option<&'a str>,
    /// Which cause queued the distillation this row came out of.
    pub distill_cause: Option<&'a str>,
    /// Tagged evidence, stored as given.
    pub evidence: Option<&'a str>,
    pub points_at: Option<&'a str>,
    /// Replaces the `gate:` signature derived from the body.
    pub fingerprint: Option<&'a str>,
    /// Replaces the layer derived from the kind.
    pub layer: Option<Layer>,
    /// Who the lesson is about; `None` is the code, the work, the project.
    pub about: Option<&'a str>,
    /// Replaces the scope derived from `project_id`; the generalisation pass writes `machine`.
    pub scope: Option<Scope>,
}

/// `propose_in` with the provenance a caller other than a run or the owner brings. Nothing is
/// committed here.
pub async fn propose_in_with(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    declaration: Declaration<'_>,
    provenance: &Provenance<'_>,
) -> Result<(i64, i64), ProposeError> {
    let Declaration {
        project_id,
        origin_run_id,
        kind,
        title,
        body,
        reasoning,
        supersedes,
    } = declaration;

    let scope = provenance
        .scope
        .clone()
        .unwrap_or_else(|| Scope::of_project(project_id));
    let (scope_kind, scope_id) = scope.columns();
    let derived_fingerprint =
        failure_signature(body).map(|signature| format!("gate:{}", signature.fingerprint));
    let fingerprint = provenance
        .fingerprint
        .map(str::to_owned)
        .or(derived_fingerprint);

    // Checked before anything is written, and checked here rather than left to the foreign key:
    // SQLite would accept a link to another scope's row without a word, and the failure would
    // surface as one repository's history quietly containing another's.
    if let Some(predecessor) = supersedes {
        let owner: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT scope_kind, scope_id FROM knowledge WHERE id = ?")
                .bind(predecessor)
                .fetch_optional(&mut **tx)
                .await?;
        let owner = owner.ok_or(ProposeError::UnknownPredecessor(predecessor))?;
        if owner.0 != scope_kind || owner.1.as_deref() != scope_id.as_deref() {
            return Err(ProposeError::ForeignPredecessor(predecessor));
        }
    }

    let now = chrono::Utc::now().to_rfc3339();

    let knowledge_id = sqlx::query(
        "INSERT INTO knowledge
           (layer, scope_kind, scope_id, source, kind, title, body, fingerprint, status, supersedes,
            origin_run_id, created_at, evidence, points_at, distill_cause, about)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'proposed', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(provenance.layer.unwrap_or_else(|| kind.layer()).as_str())
    .bind(scope_kind)
    .bind(scope_id.as_deref())
    // Never read from the body: who knocked at the door is a fact the door has and the text does
    // not.
    .bind(provenance.source.unwrap_or(if origin_run_id.is_some() {
        "run"
    } else {
        "owner"
    }))
    .bind(kind.as_str())
    .bind(title)
    .bind(body)
    .bind(fingerprint.as_deref())
    .bind(supersedes)
    .bind(origin_run_id)
    .bind(&now)
    .bind(provenance.evidence)
    .bind(provenance.points_at)
    .bind(provenance.distill_cause)
    .bind(provenance.about)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'proposed', ?, ?)",
    )
    .bind(knowledge_id)
    .bind(if provenance.source == Some("distiller") {
        "distilled from a job"
    } else if origin_run_id.is_some() {
        "declared by a run"
    } else {
        "declared by the owner"
    })
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    // `tool_input` carries the id and nothing a reader would have to join to understand the
    // question. A proposal a person cannot answer without opening another screen is a proposal that
    // waits until morning and then gets approved unread.
    //
    // **`refinement_id` keeps its name here, and `kind = 'refinement'` below keeps its value.**
    // Both are data already written to disk, in rows this migration does not rewrite — renaming
    // either would orphan every question still waiting for an answer, which is a worse thing than
    // an old word in a JSON payload.
    let tool_input = serde_json::json!({
        "refinement_id": knowledge_id,
        "kind": kind.as_str(),
        "title": title,
        "body": body,
        // Carried so the question reads as what it is. "Approve this note" and "approve this note
        // INSTEAD of the one you approved in March" are different decisions, and only one of them
        // costs you something you already have.
        "supersedes": supersedes,
    })
    .to_string();

    let proposal_id = sqlx::query(
        "INSERT INTO proposals
           (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input,
            created_at, decided_at)
         VALUES ('refinement', 'pending', ?, NULL, ?, NULL, ?, ?, ?, NULL)",
    )
    .bind(origin_run_id)
    .bind(project_id)
    .bind(reasoning)
    .bind(&tool_input)
    .bind(&now)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'pending', 'created', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    sqlx::query("UPDATE knowledge SET proposal_id = ? WHERE id = ?")
        .bind(proposal_id)
        .bind(knowledge_id)
        .execute(&mut **tx)
        .await?;

    Ok((knowledge_id, proposal_id))
}

/// A new pending proposal for a row that lost its question (a covered candidate coming back). The expired
/// proposal stays as it is: its history says what happened. Nothing is committed here.
pub async fn reopen_proposal_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    knowledge_id: i64,
) -> sqlx::Result<i64> {
    let (kind, title, body, supersedes, scope_kind, scope_id): (
        String,
        String,
        String,
        Option<i64>,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT kind, title, body, supersedes, scope_kind, scope_id FROM knowledge WHERE id = ?",
    )
    .bind(knowledge_id)
    .fetch_one(&mut **tx)
    .await?;
    let project_id = if scope_kind == "project" {
        scope_id
    } else {
        None
    };
    let now = chrono::Utc::now().to_rfc3339();
    let tool_input = serde_json::json!({
        "refinement_id": knowledge_id,
        "kind": kind,
        "title": title,
        "body": body,
        "supersedes": supersedes,
    })
    .to_string();
    let proposal_id = sqlx::query(
        "INSERT INTO proposals
           (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input,
            created_at, decided_at)
         VALUES ('refinement', 'pending', NULL, NULL, ?, NULL,
                 'restored: the general lesson that covered it was reverted', ?, ?, NULL)",
    )
    .bind(project_id.as_deref())
    .bind(&tool_input)
    .bind(&now)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'pending', 'created', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE knowledge SET proposal_id = ? WHERE id = ?")
        .bind(proposal_id)
        .bind(knowledge_id)
        .execute(&mut **tx)
        .await?;
    Ok(proposal_id)
}

#[cfg(test)]
mod reopen_tests {
    use super::*;

    #[tokio::test]
    async fn reopening_gives_an_archived_candidate_a_fresh_pending_proposal() {
        let pool = crate::testdb::fresh_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let (id, old) = propose_in_with(
            &mut tx,
            Declaration {
                project_id: Some("alpha"),
                origin_run_id: None,
                kind: Kind::Prompt,
                title: "Use PATH",
                body: "b",
                reasoning: "seeded",
                supersedes: None,
            },
            &Provenance {
                source: Some("distiller"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sqlx::query("UPDATE proposals SET status = 'expired' WHERE id = ?")
            .bind(old)
            .execute(&mut *tx)
            .await
            .unwrap();
        let fresh = reopen_proposal_in(&mut tx, id).await.unwrap();
        tx.commit().await.unwrap();

        assert_ne!(fresh, old);
        let (status, kind, project): (String, String, Option<String>) =
            sqlx::query_as("SELECT status, kind, project_id FROM proposals WHERE id = ?")
                .bind(fresh)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (status.as_str(), kind.as_str(), project.as_deref()),
            ("pending", "refinement", Some("alpha"))
        );
        let linked: i64 = sqlx::query_scalar("SELECT proposal_id FROM knowledge WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(linked, fresh);
        let created: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proposal_events WHERE proposal_id = ? AND note = 'created'",
        )
        .bind(fresh)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(created, 1);
    }
}

/// How many runs a distilled episode may go unconfirmed before it expires - the consolidator's
/// trial, spec `destilador` section 3.
pub const DISTILLED_TRIAL_RUNS: i64 = 50;

/// The note of an event that renews a row without changing its status.
pub const NOTE_RECONFIRMED: &str = "reconfirmed";

/// The identity of a learning for deduplication: its title lowercased, whitespace collapsed and
/// final punctuation stripped. `None` when nothing is left to identify it by.
pub fn title_fingerprint(title: &str) -> Option<String> {
    let lowered = title.to_lowercase();
    let collapsed = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_end_matches(|c: char| ".!?:;,\u{2026}".contains(c) || c.is_whitespace())
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("title:{trimmed}"))
    }
}

/// Record an episode a job taught: believed from the start, on trial.
///
/// Active, episodic and the distiller's, expiring unless somebody reconfirms it. `observations`
/// and `generator` stay NULL - they are the consolidator's measurement and a distilled row has
/// none. Nothing is committed here.
pub async fn record_distilled(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    declaration: &Declaration<'_>,
    provenance: &Provenance<'_>,
) -> sqlx::Result<i64> {
    let scope = Scope::of_project(declaration.project_id);
    let (scope_kind, scope_id) = scope.columns();
    let now = chrono::Utc::now().to_rfc3339();

    let id = sqlx::query(
        "INSERT INTO knowledge
           (layer, scope_kind, scope_id, source, kind, title, body, fingerprint, evidence,
            points_at, distill_cause, expires_after_runs, last_confirmed_at, status,
            created_at, activated_at)
         VALUES ('episodic', ?, ?, 'distiller', ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(scope_kind)
    .bind(scope_id.as_deref())
    .bind(declaration.kind.as_str())
    .bind(declaration.title)
    .bind(declaration.body)
    .bind(provenance.fingerprint)
    .bind(provenance.evidence)
    .bind(provenance.points_at)
    .bind(provenance.distill_cause)
    .bind(DISTILLED_TRIAL_RUNS)
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'active', 'created', ?)",
    )
    .bind(id)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    Ok(id)
}

/// A learning seen again: merge the new evidence into the project's newest live row with the same
/// fingerprint, renew it when it is an active episode, and say so in a same-status event.
///
/// `None` when the project has no such row. Project-scoped on purpose - the same title in another
/// project is another learning. Nothing is committed here.
pub async fn reconfirm_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    project_id: &str,
    fingerprint: &str,
    evidence: &str,
) -> sqlx::Result<Option<i64>> {
    let row: Option<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, layer, status, evidence FROM knowledge
         WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint = ?
           AND status IN ('active', 'proposed')
         ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(fingerprint)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((id, layer, status, stored)) = row else {
        return Ok(None);
    };
    reconfirm_row_in(tx, id, &layer, &status, stored, evidence).await?;
    Ok(Some(id))
}

/// `reconfirm_in` for a row already identified by its id (a near-identical learning found by
/// similarity rather than by fingerprint). `false` when no live row has that id.
pub async fn reconfirm_id_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    evidence: &str,
) -> sqlx::Result<bool> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT layer, status, evidence FROM knowledge
         WHERE id = ? AND status IN ('active', 'proposed')",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((layer, status, stored)) = row else {
        return Ok(false);
    };
    reconfirm_row_in(tx, id, &layer, &status, stored, evidence).await?;
    Ok(true)
}

/// The note of an event that says a new learning resembles an older one without being it.
pub const NOTE_NEAR_DUPLICATE: &str = "near_duplicate:";

/// Write a same-status event on `knowledge_id` carrying `note`.
pub(super) async fn same_status_event_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    knowledge_id: i64,
    note: String,
) -> sqlx::Result<()> {
    let status: String = sqlx::query_scalar("SELECT status FROM knowledge WHERE id = ?")
        .bind(knowledge_id)
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(knowledge_id)
    .bind(&status)
    .bind(&status)
    .bind(note)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Say, in a same-status event, that `knowledge_id` is a near-duplicate of `of_id`.
pub async fn note_near_duplicate_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    knowledge_id: i64,
    of_id: i64,
) -> sqlx::Result<()> {
    same_status_event_in(tx, knowledge_id, format!("{NOTE_NEAR_DUPLICATE}{of_id}")).await
}

/// The note of an event that says a distilled learning quoted the owner's text, so it was proposed
/// rather than recorded (`.ai/specs/2026-10-07-fronteira-prompts-design.md` section 4.3).
pub const NOTE_QUOTED_OWNER_TEXT: &str = "quoted_owner_text";

/// Say, in a same-status event, that `knowledge_id` quoted the owner's text.
pub async fn note_quoted_owner_text_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    knowledge_id: i64,
) -> sqlx::Result<()> {
    same_status_event_in(tx, knowledge_id, NOTE_QUOTED_OWNER_TEXT.to_owned()).await
}

/// Arm a proposed episode for the trial `record_distilled` gives a recorded one, without the
/// confirmation that starts it: the expiry sweep needs `active` and a `last_confirmed_at`, so the
/// row waits untouched, and `approve` starts the trial (spec section 4.4).
pub async fn arm_trial_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    knowledge_id: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE knowledge SET expires_after_runs = ?
          WHERE id = ? AND status = 'proposed' AND layer = 'episodic'",
    )
    .bind(DISTILLED_TRIAL_RUNS)
    .bind(knowledge_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Merge evidence into a live row, renew it when it is an active episode, and log the event.
pub(super) async fn reconfirm_row_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: i64,
    layer: &str,
    status: &str,
    stored: Option<String>,
    evidence: &str,
) -> sqlx::Result<()> {
    let elements = |raw: Option<&str>| -> Vec<serde_json::Value> {
        raw.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(known_element)
            .collect()
    };
    let mut merged = elements(stored.as_deref());
    for element in elements(Some(evidence)) {
        let key = |value: &serde_json::Value| (value.get("t").cloned(), value.get("id").cloned());
        if !merged.iter().any(|kept| key(kept) == key(&element)) {
            merged.push(element);
        }
    }
    let merged = tagged_evidence(&serde_json::Value::Array(merged));

    let now = chrono::Utc::now().to_rfc3339();
    let renews = layer == "episodic" && status == "active";
    sqlx::query(
        "UPDATE knowledge SET evidence = ?,
           last_confirmed_at = CASE WHEN ? THEN ? ELSE last_confirmed_at END
         WHERE id = ?",
    )
    .bind(merged.as_deref())
    .bind(renews)
    .bind(&now)
    .bind(id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(status)
    .bind(status)
    .bind(NOTE_RECONFIRMED)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

#[cfg(test)]
mod wake_tests {
    use super::*;

    /// Declaring a learning wakes the proposals topic once the transaction has committed.
    ///
    /// The wake bus is process-global, so the assertion is `>= 1`, never an exact delta.
    #[tokio::test]
    async fn wake_signal_knowledge_propose_bumps_proposals() {
        let pool = crate::testdb::fresh_pool().await;
        let rx = nucleos_base::wake::subscribe(nucleos_base::wake::Topic::Proposals);
        let before = *rx.borrow();

        propose(
            &pool,
            Declaration {
                project_id: None,
                origin_run_id: None,
                kind: Kind::Memory,
                title: "Wake lesson",
                body: "Signals the proposals topic.",
                reasoning: "wake signal test",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        assert!(rx.borrow().wrapping_sub(before) >= 1);
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    async fn propose_with(
        pool: &sqlx::SqlitePool,
        provenance: Provenance<'_>,
    ) -> (String, Option<String>, Option<String>, String) {
        let mut tx = pool.begin().await.unwrap();
        let (id, _) = propose_in_with(
            &mut tx,
            Declaration {
                project_id: Some("alpha"),
                origin_run_id: None,
                kind: Kind::Memory,
                title: "A general lesson",
                body: "It holds everywhere.",
                reasoning: "seen three times",
                supersedes: None,
            },
            &provenance,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        sqlx::query_as("SELECT scope_kind, scope_id, about, status FROM knowledge WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn provenance_carries_about_and_overrides_the_derived_scope() {
        let pool = crate::testdb::fresh_pool().await;
        let got = propose_with(
            &pool,
            Provenance {
                source: Some("distiller"),
                about: Some("owner"),
                scope: Some(Scope::Machine),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            got,
            ("machine".into(), None, Some("owner".into()), "proposed".into())
        );
    }

    #[tokio::test]
    async fn without_about_or_scope_the_door_behaves_as_before() {
        let pool = crate::testdb::fresh_pool().await;
        let got = propose_with(&pool, Provenance::default()).await;
        assert_eq!(
            got,
            ("project".into(), Some("alpha".into()), None, "proposed".into())
        );
    }
}
