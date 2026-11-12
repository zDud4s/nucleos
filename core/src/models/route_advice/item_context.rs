//! What a routed run knows about the item it works on: its files, attempt, last gate output
//! and every model that already failed it. Re-exported from the parent, so the public paths stay
//! `route_advice::{ItemContext, ItemRef, failed_label, item_context}`.

use sqlx::SqlitePool;

/// The item a run works on, as the route request needs it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemContext {
    pub files: Vec<String>,
    /// `gate_attempts + 1`: which attempt at the item this run is.
    pub attempt: Option<u32>,
    pub gate_output: Option<String>,
    /// `model[@effort]` of every attempt that already failed the item.
    pub failed: Vec<String>,
}

/// How a run names its item: a team item by its id, a sequential job's by its place in the queue.
#[derive(Debug, Clone, Copy)]
pub enum ItemRef {
    Id(i64),
    Ordinal { job_id: i64, ordinal: i64 },
}

/// `model[@effort]`, as the router's `failed` matches it.
pub fn failed_label(model: &str, effort: Option<&str>) -> String {
    match effort {
        Some(effort) if !effort.is_empty() => format!("{model}@{effort}"),
        _ => model.to_owned(),
    }
}

/// The item's files, attempt, last gate output, and every model that already failed it.
///
/// `failed` is a chain: the previous run recorded, in `route_failed`, what had failed before it,
/// so the whole history is that list plus the previous run's own model when its gate failed. A
/// previous run launched with routing off recorded no model and adds nothing — nothing is guessed.
/// Every read is best-effort: a missing row is an item with no history, never a failed run.
pub async fn item_context(pool: &SqlitePool, item: ItemRef, current_run: i64) -> ItemContext {
    type Row = (
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
    );
    let row: Option<Row> = match item {
        ItemRef::Id(id) => sqlx::query_as(
            "SELECT files, gate_attempts, gate_output, gate_status, run_id FROM job_nodes WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await,
        ItemRef::Ordinal { job_id, ordinal } => sqlx::query_as(
            "SELECT files, gate_attempts, gate_output, gate_status, run_id
               FROM job_nodes WHERE job_id = ? AND ordinal = ?",
        )
        .bind(job_id)
        .bind(ordinal)
        .fetch_optional(pool)
        .await,
    }
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "could not read a routed run's item");
        None
    });
    let Some((files, gate_attempts, gate_output, gate_status, previous)) = row else {
        return ItemContext::default();
    };
    let mut failed: Vec<String> = Vec::new();
    if gate_status.as_deref() == Some("failed")
        && let Some(previous) = previous.filter(|previous| *previous != current_run)
    {
        let prior: Option<(Option<String>, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT model, effort, route_failed FROM runs WHERE id = ?")
                .bind(previous)
                .fetch_optional(pool)
                .await
                .unwrap_or_default();
        if let Some((model, effort, route_failed)) = prior {
            for label in route_failed
                .as_deref()
                .and_then(|text| serde_json::from_str::<Vec<String>>(text).ok())
                .unwrap_or_default()
            {
                if !failed.contains(&label) {
                    failed.push(label);
                }
            }
            if let Some(model) = model.filter(|model| !model.is_empty()) {
                let label = failed_label(&model, effort.as_deref());
                if !failed.contains(&label) {
                    failed.push(label);
                }
            }
        }
    }
    ItemContext {
        files: files
            .as_deref()
            .and_then(|text| serde_json::from_str::<Vec<String>>(text).ok())
            .unwrap_or_default(),
        attempt: u32::try_from(gate_attempts + 1).ok(),
        gate_output,
        failed,
    }
}
