//! The liveness half of the IDE verify box (spec 2026-10-05 section 4.7; F2c-1).
//!
//! An interactive session in a registered worktree can be given a limited MCP box (`verify`,
//! `verify_status`). The box beats this daemon every `BEAT_EVERY`; a worktree is live while its
//! last beat is younger than `LIVE_FOR`, so a closed session stops being live within that window.
//! Liveness is a fact about this process, so it lives in memory and a restart loses it until the
//! next beat. A beat is recorded only when the project's `ide_verify` switch is on: with it off
//! the route refuses and nothing is recorded, which is what "off means as before" rests on.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::Scope;
use crate::state::AppState;
use crate::verify::{Caller, VerifyError, resolve_worktree};
use crate::{autopilot, git_exec};

/// How often a live box beats.
pub const BEAT_EVERY: Duration = Duration::from_secs(30);

/// How long one beat keeps a worktree live: three missed beats before it expires.
pub const LIVE_FOR: Duration = Duration::from_secs(90);

/// How long a beat may wait on git to resolve the worktree.
const RESOLVE_WITHIN: Duration = Duration::from_secs(10);

/// The last beat of each worktree, keyed by its canonical path.
#[derive(Debug, Default)]
pub struct Liveness {
    /// Worktree key -> (project, when it last beat).
    beats: HashMap<String, (String, Instant)>,
}

impl Liveness {
    /// Records a beat and forgets every worktree whose last beat has already expired.
    pub fn beat(&mut self, key: String, project: String, now: Instant) {
        self.beats
            .retain(|_, (_, at)| now.saturating_duration_since(*at) < LIVE_FOR);
        self.beats.insert(key, (project, now));
    }

    /// Whether a worktree beat less than `LIVE_FOR` before `now`. A beat stamped after `now`
    /// counts as live: the clock never runs backwards, so it is simply not older than the ttl.
    pub fn is_live(&self, key: &str, now: Instant) -> bool {
        self.beats
            .get(key)
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) < LIVE_FOR)
    }
}

/// The process-wide registry the route records into.
static LIVE: LazyLock<Mutex<Liveness>> = LazyLock::new(|| Mutex::new(Liveness::default()));

/// Whether a box beat for this worktree (canonical path) recently enough. Read by the hook
/// filter in F2c-3.
#[allow(dead_code)] // F2c-3 gives this its first caller; nothing launches the box before then.
pub(crate) fn is_live(key: &str) -> bool {
    LIVE.lock()
        .map(|live| live.is_live(key, Instant::now()))
        .unwrap_or(false)
}

/// What a recorded beat answers: how long it counts and how often to repeat it.
#[derive(Debug, Serialize)]
pub struct BeatAnswer {
    pub live_for_secs: u64,
    pub beat_every_secs: u64,
}

/// Why a beat was not recorded.
#[derive(Debug)]
pub(crate) enum BeatError {
    /// The project's `ide_verify` switch is off; the message names the project.
    Off(String),
    /// The worktree is not a registered worktree of a known project, or could not be resolved.
    Refused(VerifyError),
}

/// Records a beat for `worktree` at `now`: the worktree must be a registered worktree of a known
/// project, and that project's switch must be on.
pub(crate) async fn record_beat(
    pool: &sqlx::SqlitePool,
    registry: &Mutex<Liveness>,
    worktree: &str,
    now: Instant,
) -> Result<BeatAnswer, BeatError> {
    let (root, project) = resolve_worktree(
        pool,
        Caller::Owner,
        Some(worktree),
        Instant::now() + RESOLVE_WITHIN,
    )
    .await
    .map_err(BeatError::Refused)?;
    let enabled = autopilot::ide_verify_enabled(pool, &project)
        .await
        .map_err(|error| {
            BeatError::Refused(VerifyError::Internal(format!(
                "could not read the IDE verify switch of {project}: {error}"
            )))
        })?;
    if !enabled {
        return Err(BeatError::Off(format!(
            "IDE verify is switched off for project {project}; nothing recorded"
        )));
    }
    let key = git_exec::canonical(&root)
        .await
        .map_err(|error| BeatError::Refused(VerifyError::Unprocessable(error)))?;
    // A poisoned lock only means another beat panicked mid-insert; the map is still usable.
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .beat(key, project, now);
    Ok(BeatAnswer {
        live_for_secs: LIVE_FOR.as_secs(),
        beat_every_secs: BEAT_EVERY.as_secs(),
    })
}

/// The body of `POST /verify/box/beat`.
#[derive(Debug, Deserialize)]
pub struct BeatArgs {
    pub worktree: String,
}

/// `POST /verify/box/beat`: a box announces that its session is still there. Control or admin key
/// only; a run key must not be able to keep a worktree alive.
pub async fn post_box_beat(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(args): Json<BeatArgs>,
) -> Result<Json<BeatAnswer>, (StatusCode, String)> {
    if Caller::from_scope(&scope) != Some(Caller::Owner) {
        return Err((
            StatusCode::FORBIDDEN,
            "only the owner's key can keep a verify box alive".to_owned(),
        ));
    }
    match record_beat(&state.pool, &LIVE, &args.worktree, Instant::now()).await {
        Ok(answer) => Ok(Json(answer)),
        Err(BeatError::Off(message)) => Err((StatusCode::CONFLICT, message)),
        Err(BeatError::Refused(error)) => Err((error.status(), error.message().to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Runs git in `dir`, asserts it succeeded, and returns its trimmed stdout.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A one-commit git repository.
    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git_in(repo.path(), &["init", "-q", "-b", "main"]);
        git_in(repo.path(), &["add", "-A"]);
        git_in(repo.path(), &["commit", "-q", "-m", "one"]);
        repo
    }

    /// A pool with `repo` rostered as project `alpha`, its IDE verify switch as given.
    async fn pool_with(repo: &Path, ide_verify: bool) -> SqlitePool {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, ide_verify) \
             VALUES ('alpha', 'active', ?, ?)",
        )
        .bind(repo.to_string_lossy().into_owned())
        .bind(i64::from(ide_verify))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn key_of(repo: &Path) -> String {
        crate::git_exec::canonical(repo).await.unwrap()
    }

    #[test]
    fn a_box_is_live_until_its_beat_is_older_than_the_ttl() {
        assert_eq!(BEAT_EVERY, Duration::from_secs(30));
        assert_eq!(LIVE_FOR, Duration::from_secs(90));

        let at = Instant::now();
        let mut live = Liveness::default();
        assert!(!live.is_live("/w/a", at), "nothing was recorded yet");

        live.beat("/w/a".to_owned(), "alpha".to_owned(), at);
        assert!(live.is_live("/w/a", at));
        assert!(live.is_live("/w/a", at + LIVE_FOR - Duration::from_secs(1)));
        assert!(
            !live.is_live("/w/a", at + LIVE_FOR),
            "a beat exactly as old as the ttl is already expired"
        );
        assert!(!live.is_live("/w/a", at + LIVE_FOR + Duration::from_secs(1)));
        assert!(!live.is_live("/w/other", at), "liveness is per worktree");

        // A later beat renews it, and an earlier-than-now clock never panics.
        live.beat(
            "/w/a".to_owned(),
            "alpha".to_owned(),
            at + Duration::from_secs(80),
        );
        assert!(live.is_live("/w/a", at + Duration::from_secs(150)));
        assert!(live.is_live("/w/a", at));
    }

    #[tokio::test]
    async fn a_beat_with_the_switch_off_records_nothing() {
        let repo = repo();
        let pool = pool_with(repo.path(), false).await;
        let registry = Mutex::new(Liveness::default());
        let now = Instant::now();

        let answer = record_beat(&pool, &registry, &repo.path().to_string_lossy(), now).await;

        assert!(
            matches!(answer, Err(BeatError::Off(_))),
            "a beat with the switch off must be refused as off"
        );
        let key = key_of(repo.path()).await;
        assert!(!registry.lock().unwrap().is_live(&key, now));
        assert!(
            !registry
                .lock()
                .unwrap()
                .is_live(&key, now + Duration::from_secs(1))
        );
    }

    #[tokio::test]
    async fn a_beat_with_the_switch_on_makes_the_worktree_live() {
        let repo = repo();
        let pool = pool_with(repo.path(), true).await;
        let registry = Mutex::new(Liveness::default());
        let now = Instant::now();

        let answer = record_beat(&pool, &registry, &repo.path().to_string_lossy(), now).await;

        let Ok(answer) = answer else {
            panic!("a beat for a registered worktree with the switch on was refused");
        };
        assert_eq!(answer.live_for_secs, 90);
        assert_eq!(answer.beat_every_secs, 30);
        let key = key_of(repo.path()).await;
        assert!(registry.lock().unwrap().is_live(&key, now));
        assert!(
            !registry.lock().unwrap().is_live(&key, now + LIVE_FOR),
            "the registry honours the ttl"
        );
    }

    #[tokio::test]
    async fn a_beat_for_a_path_outside_any_project_is_refused() {
        let rostered = repo();
        let pool = pool_with(rostered.path(), true).await;
        let registry = Mutex::new(Liveness::default());
        let now = Instant::now();

        // A real git repository the roster has never heard of, and a plain directory.
        let stranger = repo();
        let plain = tempfile::tempdir().unwrap();
        for path in [stranger.path(), plain.path()] {
            let answer = record_beat(&pool, &registry, &path.to_string_lossy(), now).await;
            assert!(
                matches!(answer, Err(BeatError::Refused(_))),
                "{} is outside every project and must be refused",
                path.display()
            );
            let key = crate::git_exec::canonical(path).await.unwrap();
            assert!(!registry.lock().unwrap().is_live(&key, now));
        }
        let key = key_of(rostered.path()).await;
        assert!(
            !registry.lock().unwrap().is_live(&key, now),
            "a refused beat must not make an unrelated worktree live"
        );
    }
}
