//! The old name for [`crate::knowledge`], kept alive only until its callers move.
//!
//! Everything this module was is now one store read in four layers, and this file holds no
//! behaviour at all — it exists so the two callers that still say `refine::` keep compiling while
//! the store is being renamed underneath them, and it is deleted in the phase that moves them.
//!
//! A shim and not a second door: nothing here decides anything, nothing here writes anything, and
//! the one function with a body is a translation between the scope 0088 could express and the
//! scope the store now keeps in two columns. A shim that grew an opinion of its own would be the
//! second way in that the store's own door was written to prevent.

pub use crate::knowledge::{
    DecisionError, Declaration, History, Kind, Known as Refinement, ProposeError, approve, history,
    propose, reject, render, revert,
};

use crate::knowledge::{Scope, for_scope};
use sqlx::SqlitePool;

/// What a node of this project is entitled to be told, asked the way 0088's callers ask it.
///
/// `Option<&str>` and not a [`Scope`], because that is the shape the two remaining callers have:
/// `None` is machine-wide. The translation is the migration's own — no project is the house — and
/// it lives here rather than in the store so that the store's signature is the one the design
/// asked for rather than the one history left behind.
pub async fn active_for(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> sqlx::Result<Vec<Refinement>> {
    let scope = match project_id {
        None => Scope::Machine,
        Some(id) => Scope::Project(id.to_owned()),
    };
    for_scope(pool, &scope).await
}
