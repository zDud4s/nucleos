//! Knowledge decision paths and decision errors.

use super::*;

#[derive(Debug, PartialEq, Eq)]
pub enum DecisionError {
    NotFound,
    NotPending,
    Malformed,
}

/// Which row a pending proposal is asking about, or why it is not answerable.
///
/// Shared by both answers deliberately: a yes and a no must agree about what counts as a question,
/// or the pair drifts into a proposal that can be approved and not refused — which is exactly what
/// this layer shipped with, `proposals::reject_proposal` taking `action-approval` alone.
pub(super) async fn pending_knowledge(
    pool: &SqlitePool,
    proposal_id: i64,
) -> Result<i64, DecisionError> {
    let row: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT kind, status, tool_input FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| DecisionError::NotFound)?;
    let (kind, status, tool_input) = row.ok_or(DecisionError::NotFound)?;
    if kind != "refinement" {
        return Err(DecisionError::NotFound);
    }
    if status != "pending" {
        return Err(DecisionError::NotPending);
    }
    tool_input
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| {
            value
                .get("refinement_id")
                .and_then(serde_json::Value::as_i64)
        })
        .ok_or(DecisionError::Malformed)
}

/// Whether the row a pending refinement proposal names was declared in `scope`.
///
/// The refusals are [`approve`]'s: no such refinement proposal is `NotFound`, a decided one is
/// `NotPending`, and one that names no row is `Malformed`.
pub async fn declared_in(
    pool: &SqlitePool,
    proposal_id: i64,
    scope: &Scope,
) -> Result<bool, DecisionError> {
    let knowledge_id = pending_knowledge(pool, proposal_id).await?;
    let (kind, id): (String, Option<String>) =
        sqlx::query_as("SELECT scope_kind, scope_id FROM knowledge WHERE id = ?")
            .bind(knowledge_id)
            .fetch_one(pool)
            .await
            .map_err(|_| DecisionError::Malformed)?;
    let (want_kind, want_id) = scope.columns();
    Ok(kind == want_kind && id == want_id)
}

/// A person said yes, and only now does anything reach a prompt.
///
/// One transaction, like the calendar's: a dropped request must not leave the proposal and the
/// store disagreeing about whether the agent was allowed to learn something.
pub async fn approve(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    approve_as(pool, proposal_id, None).await
}

/// `kind:id` as a human reads a scope in an event note; the machine, which has no id, is bare.
pub(super) fn scope_label(kind: &str, id: Option<&str>) -> String {
    match id {
        Some(id) => format!("{kind}:{id}"),
        None => kind.to_owned(),
    }
}

/// A real move made by [`approve_as`]: the two labels for the event note, and the target's columns.
pub(super) struct Rescope {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) kind: &'static str,
    pub(super) id: Option<String>,
}

/// [`approve`], optionally placing the row in a scope other than the one it was declared in.
///
/// `None`, or a target with the row's own columns, is the plain approval exactly. Otherwise the
/// target must be a project, a team or an agent -- never the house, never a job -- and the row must
/// not replace another (`supersedes`), because moving a successor would leave its predecessor's
/// scope answering for a text that now lives elsewhere. Every check is made before anything is
/// written.
///
/// Where the target already holds an `active` row with the same fingerprint, the proposed row is
/// not activated twice: it is archived as a merge into that row, the proposal is approved, and the
/// id that answers is the row that stayed. Otherwise the row is activated in the target scope with
/// one event that names the move. One transaction either way.
pub async fn approve_as(
    pool: &SqlitePool,
    proposal_id: i64,
    target: Option<&Scope>,
) -> Result<i64, DecisionError> {
    let knowledge_id = pending_knowledge(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    // `Some` only for a real move; every refusal happens here, before the first write.
    let mut rescope: Option<Rescope> = None;
    if let Some(target) = target {
        let (kind, id, supersedes, fingerprint): (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT scope_kind, scope_id, supersedes, fingerprint FROM knowledge WHERE id = ?",
        )
        .bind(knowledge_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;
        let columns = target.columns();
        if columns.0 != kind || columns.1 != id {
            if !matches!(target, Scope::Project(_) | Scope::Team(_) | Scope::Agent(_))
                || supersedes.is_some()
            {
                return Err(DecisionError::Malformed);
            }
            let from = scope_label(&kind, id.as_deref());
            let to = scope_label(columns.0, columns.1.as_deref());

            let collision: Option<i64> = match fingerprint {
                Some(fingerprint) => sqlx::query_scalar(
                    "SELECT id FROM knowledge
                      WHERE status = 'active' AND fingerprint = ?
                        AND scope_kind = ? AND scope_id IS ? AND id != ?
                      ORDER BY id LIMIT 1",
                )
                .bind(fingerprint)
                .bind(columns.0)
                .bind(columns.1.clone())
                .bind(knowledge_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| DecisionError::NotFound)?,
                None => None,
            };

            if let Some(kept) = collision {
                let archived = sqlx::query(
                    "UPDATE knowledge SET status = 'archived', ended_at = ?
                      WHERE id = ? AND status = 'proposed'",
                )
                .bind(&now)
                .bind(knowledge_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| DecisionError::NotFound)?;
                if archived.rows_affected() != 1 {
                    return Err(DecisionError::NotPending);
                }
                sqlx::query(
                    "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                     VALUES (?, 'proposed', 'archived', ?, ?)",
                )
                .bind(knowledge_id)
                .bind(format!("rescoped {from} -> {to}; merged into {kept}"))
                .bind(&now)
                .execute(&mut *tx)
                .await
                .map_err(|_| DecisionError::NotFound)?;

                sqlx::query(
                    "UPDATE proposals SET status = 'approved', decided_at = ? WHERE id = ?",
                )
                .bind(&now)
                .bind(proposal_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| DecisionError::NotFound)?;
                sqlx::query(
                    "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
                     VALUES (?, 'pending', 'approved', ?, ?)",
                )
                .bind(proposal_id)
                .bind(format!("refinement merged into {kept}"))
                .bind(&now)
                .execute(&mut *tx)
                .await
                .map_err(|_| DecisionError::NotFound)?;

                tx.commit().await.map_err(|_| DecisionError::NotFound)?;
                return Ok(kept);
            }
            rescope = Some(Rescope {
                from,
                to,
                kind: columns.0,
                id: columns.1,
            });
        }
    }

    // Guarded on `proposed`, so a second approval of the same row is a no-op rather than a second
    // activation stamp over the first.
    let activated = match &rescope {
        None => sqlx::query(
            "UPDATE knowledge SET status = 'active', activated_at = ?
              WHERE id = ? AND status = 'proposed'",
        )
        .bind(&now)
        .bind(knowledge_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?,
        Some(Rescope { kind, id, .. }) => sqlx::query(
            "UPDATE knowledge SET status = 'active', activated_at = ?,
                                  scope_kind = ?, scope_id = ?
              WHERE id = ? AND status = 'proposed'",
        )
        .bind(&now)
        .bind(*kind)
        .bind(id.clone())
        .bind(knowledge_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?,
    };
    if activated.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    // An episode armed while it waited (`arm_trial_in`) starts its trial here: the owner's yes is
    // the confirmation. Any other row keeps what it had.
    sqlx::query(
        "UPDATE knowledge SET last_confirmed_at = ?
          WHERE id = ? AND expires_after_runs IS NOT NULL AND last_confirmed_at IS NULL",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    // One event for the activation, which also names the move when there was one.
    let note = match &rescope {
        None => "approved by the owner".to_owned(),
        Some(Rescope { from, to, .. }) => format!("rescoped {from} -> {to}"),
    };
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'active', ?, ?)",
    )
    .bind(knowledge_id)
    .bind(note)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    // The chain moves here and nowhere else. A successor that ended its predecessor when it was
    // merely *declared* would let a question nobody answered delete the answer already in force,
    // so the old text stands until the moment somebody chooses the new one over it.
    let predecessor: Option<i64> =
        sqlx::query_scalar("SELECT supersedes FROM knowledge WHERE id = ?")
            .bind(knowledge_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| DecisionError::NotFound)?;
    if let Some(predecessor) = predecessor {
        let ended = sqlx::query(
            "UPDATE knowledge SET status = 'superseded', ended_at = ?
              WHERE id = ? AND status = 'active'",
        )
        .bind(&now)
        .bind(predecessor)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;
        // Not an error when it matches nothing: the predecessor may have been reverted while this
        // successor waited for an answer, and what the person just approved is still approved.
        if ended.rows_affected() == 1 {
            sqlx::query(
                "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                 VALUES (?, 'active', 'superseded', ?, ?)",
            )
            .bind(predecessor)
            .bind(format!("replaced by {knowledge_id}"))
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|_| DecisionError::NotFound)?;
        }
    }

    sqlx::query("UPDATE proposals SET status = 'approved', decided_at = ? WHERE id = ?")
        .bind(&now)
        .bind(proposal_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', 'approved', 'refinement activated', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    tx.commit().await.map_err(|_| DecisionError::NotFound)?;
    Ok(knowledge_id)
}

/// A person said no, and the refusal is kept.
///
/// The layer shipped without this and it was not a missing nicety: `proposals::reject_proposal`
/// answers `action-approval` alone, so a refinement proposal had one button. A queue you can only
/// say yes to is a queue where everything is eventually approved — and the thing being approved
/// here is what every later run is told.
///
/// `rejected` and not deleted, for the reason the module's own doc gives: what the agent kept
/// trying to learn and was told no to is a record worth having.
pub async fn reject(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let knowledge_id = pending_knowledge(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    let refused = sqlx::query(
        "UPDATE knowledge SET status = 'rejected', ended_at = ?
          WHERE id = ? AND status = 'proposed'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;
    if refused.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'rejected', 'refused by the owner', ?)",
    )
    .bind(knowledge_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    sqlx::query("UPDATE proposals SET status = 'rejected', decided_at = ? WHERE id = ?")
        .bind(&now)
        .bind(proposal_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', 'rejected', 'refinement refused', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    tx.commit().await.map_err(|_| DecisionError::NotFound)?;
    Ok(knowledge_id)
}

/// Taking one back, which is the half that makes approving safe to do.
///
/// `reverted` and not deleted: the history is the feature. A store somebody can only add to is one
/// nobody dares add to.
pub async fn revert(pool: &SqlitePool, knowledge_id: i64, note: &str) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE knowledge SET status = 'reverted', ended_at = ? WHERE id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'active', 'reverted', ?, ?)",
    )
    .bind(knowledge_id)
    .bind(note)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    // A general lesson gives back the undecided rows it covered (spec 5.5).
    let reopened = super::cover::uncover_in(&mut tx, knowledge_id).await?;
    tx.commit().await?;
    if reopened > 0 {
        nucleos_base::wake::bump(nucleos_base::wake::Topic::Proposals);
    }
    Ok(true)
}
