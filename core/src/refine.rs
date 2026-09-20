//! The old name for [`crate::knowledge`], kept alive only until its last caller moves.
//!
//! Everything this module was is now one store read in four layers, and this file holds no
//! behaviour at all — it exists so `job.rs`, which is the one place still saying `refine::`, keeps
//! compiling while the store is renamed underneath it. It is deleted in the phase that moves it.
//!
//! A shim and not a second door: nothing here decides anything, nothing here writes anything, and
//! the one function with a body is a translation between the scope 0088 could express and the
//! scope the store now keeps in two columns. A shim that grew an opinion of its own would be the
//! second way in that the store's own door was written to prevent.
//!
//! **Two names and not twelve.** The first draft re-exported the whole of the old surface, and
//! `-D warnings` refused it the moment the HTTP door started naming the store directly. That is
//! the right refusal: a shim wide enough to cover callers that no longer exist is a shim nobody
//! can tell is nearly empty, and being nearly empty is the only argument for keeping it at all.

pub use crate::knowledge::render;

use crate::knowledge::{Known, Scope, for_scope};
use sqlx::SqlitePool;

/// What a node of this project is entitled to be told, asked the way 0088's caller asks it.
///
/// `Option<&str>` and not a [`Scope`], because that is the shape the one remaining caller has:
/// `None` is machine-wide. The translation is the migration's own — no project is the house — and
/// it lives here rather than in the store so that the store's signature is the one the design
/// asked for rather than the one history left behind.
pub async fn active_for(pool: &SqlitePool, project_id: Option<&str>) -> sqlx::Result<Vec<Known>> {
    let scope = match project_id {
        None => Scope::Machine,
        Some(id) => Scope::Project(id.to_owned()),
    };
    for_scope(pool, &scope).await
}
