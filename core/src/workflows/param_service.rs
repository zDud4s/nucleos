//! §spec workflow-auto-afinacao
//!
//! The single entry point for changing a declared workflow parameter: the owner door (HTTP) and,
//! in phase 2, the tuner both call `set_value`, `rollback`, `state`, `history` and `set_mode`.
//!
//! The service applies, in this order and before anything is written: the manifest (the name must
//! be declared), the declared bounds, the tier and the auto/fixed mode (for an autonomous actor
//! only), and the path rule (the file must live under the project's state directory, and must not
//! be a symlink). Then, in one `BEGIN IMMEDIATE` transaction, it compares the file's current value
//! with the last recorded one: a value the daemon never wrote is a `baseline` (a starting point
//! only), and a value that differs from the last recorded one was edited by hand, which is
//! recorded and flips the project mode to `fixed`. The daemon never overwrites a hand edit on its
//! own. An owner write also flips the project mode to `fixed`; a tuner write never touches it.
//!
//! The manifest is read from the LIBRARY copy of the pinned version (`Ctx::for_pin`), and this
//! module never names a key of any bundle: it knows the contract, never the meaning.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

use crate::project;
use crate::settings::Scope;
use crate::workflows;
use crate::workflows::param_file;
use crate::workflows::param_manifest::{self, ParamDecl};
use crate::workflows::param_store::{self, Kind, Mode, ModeSource, Row};

/// Which bundle of which project, and where its manifest and its state files are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctx {
    pub project_id: String,
    pub bundle: String,
    /// The library copy of the pinned version: the only place the manifest is read from.
    pub bundle_dir: PathBuf,
    /// The project's state directory; every declared `file` is relative to it.
    pub state_dir: PathBuf,
}

impl Ctx {
    /// The context of one pin, read from the library copy only (an ejected copy in the working
    /// tree is never consulted, so an agent that can edit it cannot widen a bound). `None` when
    /// the pinned version is absent from the library or the id cannot name a state directory.
    pub fn for_pin(
        library: &Path,
        machine_root: &Path,
        project_id: &str,
        pin: &workflows::Pin,
    ) -> Option<Ctx> {
        if !workflows::valid_name(&pin.name) || !plain_segment(&pin.version) {
            return None;
        }
        let bundle_dir = library.join(&pin.name).join(&pin.version);
        if !bundle_dir.join(workflows::MANIFEST).is_file() {
            return None;
        }
        let state_dir = project::state::dir(machine_root, project_id)?;
        Some(Ctx {
            project_id: project_id.to_string(),
            bundle: pin.name.clone(),
            bundle_dir,
            state_dir,
        })
    }
}

/// A version names one directory of the library: no separator, not `.` or `..`.
fn plain_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && !segment.contains(['/', '\\', ':'])
}

/// Who asks for a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// The owner, through the owner door.
    Owner(String),
    /// The phase 2 tuner: an autonomous writer.
    Tuner(String),
}

impl Actor {
    /// `owner:<who>` or `tuner:<id>`, as the history stores it.
    pub fn text(&self) -> String {
        match self {
            Self::Owner(who) => format!("owner:{who}"),
            Self::Tuner(id) => format!("tuner:{id}"),
        }
    }

    /// Whether the actor writes on its own (the tuner), and so is bound by tier and mode.
    pub fn autonomous(&self) -> bool {
        matches!(self, Self::Tuner(_))
    }
}

/// Why a request was refused. Each refusal happens before anything is written, except
/// `HandEdited`, which keeps the observation of the owner's edit.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    UnknownParam,
    OutOfBounds(String),
    FixedByOwner,
    TierNotAutonomous(u8),
    HandEdited { file_value: Value, recorded: Value },
    FileUnsafe(String),
    FileAbsent,
    NotAScalar(String),
    NoSuchVersion(i64),
    Db(String),
}

impl Refusal {
    /// The stable machine-readable code of the refusal.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownParam => "unknown_param",
            Self::OutOfBounds(_) => "out_of_bounds",
            Self::FixedByOwner => "fixed_by_owner",
            Self::TierNotAutonomous(_) => "tier_not_autonomous",
            Self::HandEdited { .. } => "hand_edited",
            Self::FileUnsafe(_) => "file_unsafe",
            Self::FileAbsent => "file_absent",
            Self::NotAScalar(_) => "not_a_scalar",
            Self::NoSuchVersion(_) => "no_such_version",
            Self::Db(_) => "db",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownParam => write!(f, "the bundle's manifest declares no such parameter"),
            Self::OutOfBounds(why) => write!(f, "out of bounds: {why}"),
            Self::FixedByOwner => write!(f, "the mode is fixed: only the owner may change it"),
            Self::TierNotAutonomous(tier) => write!(f, "a tier {tier} parameter is owner-only"),
            Self::HandEdited {
                file_value,
                recorded,
            } => write!(
                f,
                "the file holds {file_value}, not the last recorded {recorded}: it was edited by \
                 hand, and the parameter's mode is now fixed"
            ),
            Self::FileUnsafe(why) => write!(f, "unsafe file: {why}"),
            Self::FileAbsent => write!(f, "the parameter's file does not exist"),
            Self::NotAScalar(why) => write!(f, "{why}"),
            Self::NoSuchVersion(version) => write!(f, "the parameter has no version {version}"),
            Self::Db(why) => write!(f, "database: {why}"),
        }
    }
}

impl std::error::Error for Refusal {}

fn db(error: sqlx::Error) -> Refusal {
    Refusal::Db(error.to_string())
}

/// One parameter as the service sees it: declared or orphaned, its mode, the file's value and the
/// last recorded row.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamState {
    /// `None` when the parameter has history but the manifest no longer declares it.
    pub decl: Option<ParamDecl>,
    pub name: String,
    pub mode: Mode,
    pub mode_source: ModeSource,
    /// The value at the key in the file; `None` when the key, the file or the path is not usable.
    pub file_value: Option<Value>,
    pub recorded: Option<Row>,
    /// The file (or the declared default, for an absent key) differs from the last recorded value.
    pub drift: bool,
    pub orphaned: bool,
}

/// The declarations of the context's bundle. A manifest that does not read or validate declares
/// nothing (logged): every name is then unknown and every row orphaned.
fn declarations(ctx: &Ctx) -> Vec<ParamDecl> {
    match param_manifest::manifest_params(&ctx.bundle_dir) {
        Ok(decls) => decls,
        Err(error) => {
            tracing::warn!(bundle = %ctx.bundle, %error, "a bundle's params could not be read");
            Vec::new()
        }
    }
}

fn declared(ctx: &Ctx, name: &str) -> Result<ParamDecl, Refusal> {
    declarations(ctx)
        .into_iter()
        .find(|decl| decl.name == name)
        .ok_or(Refusal::UnknownParam)
}

/// The one place the path rule is applied at write time: the declared file again passes
/// `safe_relative`, its canonical parent lies under the canonical state directory, and the target
/// itself is a regular file, not a symlink.
fn guarded_target(ctx: &Ctx, decl: &ParamDecl) -> Result<PathBuf, Refusal> {
    if !param_manifest::safe_relative(&decl.file) {
        return Err(Refusal::FileUnsafe(format!(
            "`{}` is not a relative path inside the state directory",
            decl.file
        )));
    }
    let target = ctx.state_dir.join(&decl.file);
    let Ok(root) = ctx.state_dir.canonicalize() else {
        return Err(Refusal::FileAbsent);
    };
    let Some(parent) = target.parent() else {
        return Err(Refusal::FileAbsent);
    };
    let Ok(parent) = parent.canonicalize() else {
        return Err(Refusal::FileAbsent);
    };
    if !parent.starts_with(&root) {
        return Err(Refusal::FileUnsafe(format!(
            "`{}` resolves outside the state directory",
            decl.file
        )));
    }
    let meta = match std::fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Refusal::FileAbsent);
        }
        Err(error) => return Err(Refusal::FileUnsafe(error.to_string())),
    };
    if meta.file_type().is_symlink() {
        return Err(Refusal::FileUnsafe(format!("`{}` is a symlink", decl.file)));
    }
    if !meta.is_file() {
        return Err(Refusal::FileUnsafe(format!(
            "`{}` is not a file",
            decl.file
        )));
    }
    Ok(target)
}

fn read_text(target: &Path) -> Result<String, Refusal> {
    std::fs::read_to_string(target).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Refusal::FileAbsent,
        _ => Refusal::FileUnsafe(format!("cannot read: {error}")),
    })
}

fn not_a_scalar(error: param_file::FileError) -> Refusal {
    Refusal::NotAScalar(error.to_string())
}

/// Two recorded values are the same value: numbers compare by value (`4` and `4.0` are one).
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if a.is_number() && b.is_number() => x == y,
        _ => a == b,
    }
}

/// What the file says the parameter is: the value at the key, else the declared default.
fn observed(file_value: &Option<Value>, decl: &ParamDecl) -> Option<Value> {
    file_value.clone().or_else(|| decl.default.clone())
}

async fn latest_value_on(
    conn: &mut SqliteConnection,
    ctx: &Ctx,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value_json FROM workflow_param_history
         WHERE project_id = ? AND bundle = ? AND param = ? ORDER BY version DESC LIMIT 1",
    )
    .bind(&ctx.project_id)
    .bind(&ctx.bundle)
    .bind(name)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?;
    match raw {
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| Refusal::Db(e.to_string())),
        None => Ok(None),
    }
}

async fn written_at_on(
    conn: &mut SqliteConnection,
    ctx: &Ctx,
    name: &str,
    version: i64,
) -> Result<String, Refusal> {
    sqlx::query_scalar(
        "SELECT written_at FROM workflow_param_history
         WHERE project_id = ? AND bundle = ? AND param = ? AND version = ?",
    )
    .bind(&ctx.project_id)
    .bind(&ctx.bundle)
    .bind(name)
    .bind(version)
    .fetch_one(&mut *conn)
    .await
    .map_err(db)
}

/// Write `value` as the new value of a declared parameter.
pub async fn set_value(
    pool: &SqlitePool,
    ctx: &Ctx,
    name: &str,
    value: &Value,
    actor: &Actor,
    reason: &str,
) -> Result<Row, Refusal> {
    let decl = declared(ctx, name)?;
    apply(pool, ctx, &decl, value, actor, reason, Kind::Set, None).await
}

/// Write the value of an old version as a NEW version (kind `rollback`); history never rewinds.
/// The old value is checked against today's manifest.
pub async fn rollback(
    pool: &SqlitePool,
    ctx: &Ctx,
    name: &str,
    to_version: i64,
    actor: &Actor,
    reason: &str,
) -> Result<Row, Refusal> {
    let decl = declared(ctx, name)?;
    let old = param_store::history(pool, &ctx.project_id, &ctx.bundle, name)
        .await
        .map_err(db)?
        .into_iter()
        .find(|row| row.version == to_version)
        .ok_or(Refusal::NoSuchVersion(to_version))?;
    apply(
        pool,
        ctx,
        &decl,
        &old.value,
        actor,
        reason,
        Kind::Rollback,
        Some(to_version),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply(
    pool: &SqlitePool,
    ctx: &Ctx,
    decl: &ParamDecl,
    value: &Value,
    actor: &Actor,
    reason: &str,
    kind: Kind,
    rolled_back_to: Option<i64>,
) -> Result<Row, Refusal> {
    let name = decl.name.as_str();
    let value = decl
        .check(value)
        .map_err(|violation| Refusal::OutOfBounds(violation.to_string()))?;
    if actor.autonomous() {
        if decl.tier > 1 {
            return Err(Refusal::TierNotAutonomous(decl.tier));
        }
        let (mode, _) = param_store::effective_mode(pool, &ctx.project_id, &ctx.bundle, name)
            .await
            .map_err(db)?;
        if mode == Mode::Fixed {
            return Err(Refusal::FixedByOwner);
        }
    }
    let target = guarded_target(ctx, decl)?;

    let tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db)?;
    write_locked(
        tx,
        ctx,
        decl,
        &value,
        actor,
        reason,
        kind,
        rolled_back_to,
        &target,
    )
    .await
}

/// The part of `apply` that runs under the write lock. The effective mode is read again here, on
/// the transaction's own connection: the pre-check above ran against the pool and the owner may
/// have flipped the mode to `fixed` since. This check is the authoritative one.
#[allow(clippy::too_many_arguments)]
async fn write_locked(
    mut tx: sqlx::Transaction<'_, sqlx::Sqlite>,
    ctx: &Ctx,
    decl: &ParamDecl,
    value: &Value,
    actor: &Actor,
    reason: &str,
    kind: Kind,
    rolled_back_to: Option<i64>,
    target: &Path,
) -> Result<Row, Refusal> {
    let name = decl.name.as_str();
    if actor.autonomous() {
        let (mode, _) = param_store::effective_mode_on(&mut tx, &ctx.project_id, &ctx.bundle, name)
            .await
            .map_err(db)?;
        if mode == Mode::Fixed {
            return Err(Refusal::FixedByOwner);
        }
    }
    let before = read_text(target)?;
    let file_value = param_file::read(&before, &decl.key).map_err(not_a_scalar)?;
    let seen = observed(&file_value, decl);
    match latest_value_on(&mut tx, ctx, name).await? {
        None => {
            // A value the daemon never wrote: only a starting point, it fixes nothing.
            if let Some(start) = &seen {
                param_store::append(
                    &mut tx,
                    &ctx.project_id,
                    &ctx.bundle,
                    name,
                    start,
                    Kind::Baseline,
                    "daemon",
                    "value found before the daemon's first write",
                    None,
                )
                .await
                .map_err(db)?;
            }
        }
        Some(recorded) => {
            let file_now = seen.unwrap_or(Value::Null);
            if !same(&file_now, &recorded) {
                param_store::append(
                    &mut tx,
                    &ctx.project_id,
                    &ctx.bundle,
                    name,
                    &file_now,
                    Kind::HandEdit,
                    "daemon",
                    "file value differs from the last recorded value",
                    None,
                )
                .await
                .map_err(db)?;
                let scope = Scope::project(&ctx.project_id);
                param_store::set_mode_on(
                    &mut tx,
                    &scope,
                    &ctx.bundle,
                    name,
                    Some(Mode::Fixed),
                    "daemon",
                )
                .await
                .map_err(db)?;
                if actor.autonomous() {
                    // The observation is kept; the owner's file value is never overwritten.
                    tx.commit().await.map_err(db)?;
                    return Err(Refusal::HandEdited {
                        file_value: file_now,
                        recorded,
                    });
                }
            }
        }
    }

    let actor_text = actor.text();
    let version = param_store::append(
        &mut tx,
        &ctx.project_id,
        &ctx.bundle,
        name,
        value,
        kind,
        &actor_text,
        reason,
        rolled_back_to,
    )
    .await
    .map_err(db)?;
    if !actor.autonomous() {
        // The owner's value overrides the tuner until the owner sets the mode back to `auto`.
        let scope = Scope::project(&ctx.project_id);
        param_store::set_mode_on(
            &mut tx,
            &scope,
            &ctx.bundle,
            name,
            Some(Mode::Fixed),
            &actor_text,
        )
        .await
        .map_err(db)?;
    }
    let after = param_file::write(&before, &decl.key, value).map_err(not_a_scalar)?;
    let written_at = written_at_on(&mut tx, ctx, name, version).await?;
    project::state::write_atomically(target, &after)
        .map_err(|error| Refusal::FileUnsafe(format!("cannot write: {error}")))?;
    if let Err(error) = tx.commit().await {
        if let Err(restore) = project::state::write_atomically(target, &before) {
            tracing::error!(bundle = %ctx.bundle, param = name, %restore,
                "a parameter file could not be restored after a failed commit");
        }
        return Err(db(error));
    }
    Ok(Row {
        version,
        value: value.clone(),
        kind,
        actor: actor_text,
        reason: reason.to_string(),
        rolled_back_to,
        written_at,
    })
}

/// The value the file holds at the key, read without writing anything.
fn file_value_of(ctx: &Ctx, decl: &ParamDecl) -> Result<Option<Value>, Refusal> {
    let target = guarded_target(ctx, decl)?;
    let text = read_text(&target)?;
    param_file::read(&text, &decl.key).map_err(not_a_scalar)
}

/// Every declared parameter, then every orphaned one (history, but no longer declared). Read-only:
/// it never writes the history, the mode or the file.
pub async fn state(pool: &SqlitePool, ctx: &Ctx) -> Result<Vec<ParamState>, Refusal> {
    let decls = declarations(ctx);
    let mut out = Vec::new();
    for decl in &decls {
        let (mode, mode_source) =
            param_store::effective_mode(pool, &ctx.project_id, &ctx.bundle, &decl.name)
                .await
                .map_err(db)?;
        let recorded = param_store::latest(pool, &ctx.project_id, &ctx.bundle, &decl.name)
            .await
            .map_err(db)?;
        let readable = file_value_of(ctx, decl);
        let drift = match (&readable, &recorded) {
            (Ok(file_value), Some(row)) => {
                let seen = observed(file_value, decl).unwrap_or(Value::Null);
                !same(&seen, &row.value)
            }
            _ => false,
        };
        out.push(ParamState {
            decl: Some(decl.clone()),
            name: decl.name.clone(),
            mode,
            mode_source,
            file_value: readable.ok().flatten(),
            recorded,
            drift,
            orphaned: false,
        });
    }
    let names = param_store::params_with_history(pool, &ctx.project_id, &ctx.bundle)
        .await
        .map_err(db)?;
    for name in names {
        if decls.iter().any(|decl| decl.name == name) {
            continue;
        }
        let (mode, mode_source) =
            param_store::effective_mode(pool, &ctx.project_id, &ctx.bundle, &name)
                .await
                .map_err(db)?;
        let recorded = param_store::latest(pool, &ctx.project_id, &ctx.bundle, &name)
            .await
            .map_err(db)?;
        out.push(ParamState {
            decl: None,
            name,
            mode,
            mode_source,
            file_value: None,
            recorded,
            drift: false,
            orphaned: true,
        });
    }
    Ok(out)
}

/// Every recorded row of a parameter, newest first; orphaned names included.
pub async fn history(pool: &SqlitePool, ctx: &Ctx, name: &str) -> Result<Vec<Row>, Refusal> {
    param_store::history(pool, &ctx.project_id, &ctx.bundle, name)
        .await
        .map_err(db)
}

/// Set (or, with `None`, clear) a parameter's mode in one scope. The owner door only.
pub async fn set_mode(
    pool: &SqlitePool,
    ctx_bundle: &str,
    scope: &Scope,
    name: &str,
    mode: Option<Mode>,
    by: &str,
) -> Result<(), Refusal> {
    param_store::set_mode(pool, scope, ctx_bundle, name, mode, by)
        .await
        .map_err(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BUNDLE: &str = "synth";
    const PROJECT: &str = "proj-1";

    /// A synthetic bundle: no key of any real bundle appears here.
    const MANIFEST: &str = "description: synthetic
params:
  - name: alpha.size
    type: int
    min: 1
    max: 10
    tier: 1
    file: p.yaml
    key: alpha.size
  - name: alpha.mode
    type: enum
    values: [fast, slow]
    default: fast
    tier: 1
    file: p.yaml
    key: alpha.mode
  - name: beta.big
    type: int
    min: 1
    max: 100
    tier: 2
    file: p.yaml
    key: beta.big
";

    const STATE: &str = "# Synthetic state file.
alpha:
  # how many at once
  size: 3   # an inline comment

  other: keep
beta:
  big: 50
";

    struct Fixture {
        temp: tempfile::TempDir,
        pool: SqlitePool,
        ctx: Ctx,
        library: PathBuf,
        machine: PathBuf,
    }

    impl Fixture {
        async fn new() -> Self {
            Self::with_state(Some(STATE)).await
        }

        async fn with_state(state: Option<&str>) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let library = temp.path().join("library");
            let machine = temp.path().join("machine");
            write_manifest(&library, "1.0", MANIFEST);
            let state_dir = project::state::dir(&machine, PROJECT).unwrap();
            std::fs::create_dir_all(&state_dir).unwrap();
            if let Some(text) = state {
                std::fs::write(state_dir.join("p.yaml"), text).unwrap();
            }
            let ctx = Ctx::for_pin(&library, &machine, PROJECT, &pin("1.0")).unwrap();
            Fixture {
                temp,
                pool: crate::testdb::fresh_pool().await,
                ctx,
                library,
                machine,
            }
        }

        /// A second library version of the same bundle, with its own manifest.
        fn version(&self, version: &str, manifest: &str) -> Ctx {
            write_manifest(&self.library, version, manifest);
            Ctx::for_pin(&self.library, &self.machine, PROJECT, &pin(version)).unwrap()
        }

        fn file(&self) -> PathBuf {
            self.ctx.state_dir.join("p.yaml")
        }

        fn text(&self) -> String {
            std::fs::read_to_string(self.file()).unwrap()
        }

        fn value(&self, key: &str) -> Option<Value> {
            param_file::read(&self.text(), key).unwrap()
        }

        async fn set(&self, name: &str, value: Value, actor: &Actor) -> Result<Row, Refusal> {
            set_value(&self.pool, &self.ctx, name, &value, actor, "test").await
        }

        async fn back(&self, name: &str, to: i64, actor: &Actor) -> Result<Row, Refusal> {
            rollback(&self.pool, &self.ctx, name, to, actor, "test").await
        }

        async fn history(&self, name: &str) -> Vec<Row> {
            history(&self.pool, &self.ctx, name).await.unwrap()
        }

        async fn mode(&self, name: &str) -> (Mode, ModeSource) {
            param_store::effective_mode(&self.pool, PROJECT, BUNDLE, name)
                .await
                .unwrap()
        }

        async fn set_mode(&self, scope: &Scope, name: &str, mode: Option<Mode>) {
            set_mode(&self.pool, BUNDLE, scope, name, mode, "owner:me")
                .await
                .unwrap();
        }

        /// The tuner writes 5, then the owner edits the file to 7 by hand.
        async fn tuner_five_then_hand_edit_seven(&self) {
            self.set("alpha.size", json!(5), &tuner()).await.unwrap();
            let edited = self.text().replace("size: 5", "size: 7");
            std::fs::write(self.file(), edited).unwrap();
        }
    }

    fn write_manifest(library: &Path, version: &str, manifest: &str) {
        let dir = library.join(BUNDLE).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(workflows::MANIFEST), manifest).unwrap();
    }

    fn pin(version: &str) -> workflows::Pin {
        workflows::Pin {
            name: BUNDLE.into(),
            version: version.into(),
            origin: format!("library:{BUNDLE}@{version}"),
            hash: String::new(),
            ejected_at: None,
            path: None,
            nodes: Default::default(),
        }
    }

    fn tuner() -> Actor {
        Actor::Tuner("t1".into())
    }

    fn owner() -> Actor {
        Actor::Owner("me".into())
    }

    fn kinds(rows: &[Row]) -> Vec<(i64, Kind, Value)> {
        rows.iter()
            .map(|r| (r.version, r.kind, r.value.clone()))
            .collect()
    }

    #[tokio::test]
    async fn set_value_writes_the_file_and_one_history_row() {
        let f = Fixture::with_state(Some("alpha:\n  other: keep\n")).await;
        let row = f.set("alpha.size", json!(5), &tuner()).await.unwrap();
        assert_eq!(row.version, 1);
        assert_eq!(row.kind, Kind::Set);
        assert_eq!(row.actor, "tuner:t1");
        assert_eq!(f.value("alpha.size"), Some(json!(5)));
        let history = f.history("alpha.size").await;
        assert_eq!(history, vec![row]);
    }

    #[tokio::test]
    async fn the_first_write_records_the_file_value_as_a_baseline_then_the_new_value() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(5), &tuner()).await.unwrap();
        let history = f.history("alpha.size").await;
        assert_eq!(
            kinds(&history),
            vec![(2, Kind::Set, json!(5)), (1, Kind::Baseline, json!(3))]
        );
        assert_eq!(history[1].actor, "daemon");
    }

    #[tokio::test]
    async fn a_write_keeps_every_comment_and_unrelated_line_byte_for_byte() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(5), &tuner()).await.unwrap();
        assert_eq!(f.text(), STATE.replace("size: 3", "size: 5"));
    }

    #[tokio::test]
    async fn an_out_of_bounds_value_writes_nothing_and_adds_no_history_row() {
        let f = Fixture::new().await;
        for bad in [json!(0), json!(11), json!(4.5), json!(true), json!("4")] {
            for actor in [tuner(), owner()] {
                let refusal = f.set("alpha.size", bad.clone(), &actor).await.unwrap_err();
                assert_eq!(refusal.code(), "out_of_bounds", "{bad}");
            }
        }
        let refusal = f
            .set("alpha.mode", json!("mid"), &owner())
            .await
            .unwrap_err();
        assert_eq!(refusal.code(), "out_of_bounds");
        assert_eq!(f.text(), STATE);
        assert!(f.history("alpha.size").await.is_empty());
        assert!(f.history("alpha.mode").await.is_empty());
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Default)
        );
    }

    #[tokio::test]
    async fn a_hand_edit_blocks_an_autonomous_write_records_a_hand_edit_row_and_flips_the_project_mode_to_fixed()
     {
        let f = Fixture::new().await;
        f.tuner_five_then_hand_edit_seven().await;
        let edited = f.text();
        let refusal = f.set("alpha.size", json!(6), &tuner()).await.unwrap_err();
        assert_eq!(
            refusal,
            Refusal::HandEdited {
                file_value: json!(7),
                recorded: json!(5)
            }
        );
        assert_eq!(refusal.code(), "hand_edited");
        assert_eq!(f.text(), edited);
        let history = f.history("alpha.size").await;
        assert_eq!(history[0].kind, Kind::HandEdit);
        assert_eq!(history[0].value, json!(7));
        assert_eq!(history[0].actor, "daemon");
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Fixed, ModeSource::Project)
        );
        let again = f.set("alpha.size", json!(6), &tuner()).await.unwrap_err();
        assert_eq!(again, Refusal::FixedByOwner);
        assert_eq!(f.text(), edited);
        assert_eq!(f.history("alpha.size").await.len(), history.len());
    }

    #[tokio::test]
    async fn setting_the_mode_back_to_auto_lets_the_tuner_write_again_from_the_hand_edited_value() {
        let f = Fixture::new().await;
        f.tuner_five_then_hand_edit_seven().await;
        f.set("alpha.size", json!(6), &tuner()).await.unwrap_err();
        f.set_mode(&Scope::project(PROJECT), "alpha.size", Some(Mode::Auto))
            .await;
        f.set("alpha.size", json!(8), &tuner()).await.unwrap();
        assert_eq!(f.value("alpha.size"), Some(json!(8)));
        let history = f.history("alpha.size").await;
        assert_eq!(history[0].kind, Kind::Set);
        assert_eq!(history[0].value, json!(8));
        assert_eq!(history[1].kind, Kind::HandEdit);
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Project)
        );
    }

    #[tokio::test]
    async fn a_preexisting_value_the_daemon_never_wrote_is_a_baseline_and_the_tuner_may_change_it()
    {
        let f = Fixture::with_state(Some("alpha:\n  size: 6\n")).await;
        f.set("alpha.size", json!(5), &tuner()).await.unwrap();
        assert_eq!(
            kinds(&f.history("alpha.size").await),
            vec![(2, Kind::Set, json!(5)), (1, Kind::Baseline, json!(6))]
        );
        assert_eq!(f.value("alpha.size"), Some(json!(5)));
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Default)
        );
    }

    #[tokio::test]
    async fn an_owner_set_flips_the_project_mode_to_fixed_and_the_tuner_is_then_refused() {
        let f = Fixture::new().await;
        let row = f.set("alpha.size", json!(7), &owner()).await.unwrap();
        assert_eq!(row.actor, "owner:me");
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Fixed, ModeSource::Project)
        );
        let refusal = f.set("alpha.size", json!(8), &tuner()).await.unwrap_err();
        assert_eq!(refusal, Refusal::FixedByOwner);
        assert_eq!(refusal.code(), "fixed_by_owner");
        assert_eq!(f.value("alpha.size"), Some(json!(7)));

        // A tuner write never changes the mode.
        let g = Fixture::new().await;
        g.set("alpha.size", json!(4), &tuner()).await.unwrap();
        assert_eq!(
            g.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Default)
        );
    }

    #[tokio::test]
    async fn an_owner_rollback_flips_the_project_mode_to_fixed() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(4), &tuner()).await.unwrap();
        f.set("alpha.size", json!(5), &tuner()).await.unwrap();
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Default)
        );
        let row = f.back("alpha.size", 2, &owner()).await.unwrap();
        assert_eq!(row.kind, Kind::Rollback);
        assert_eq!(row.value, json!(4));
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Fixed, ModeSource::Project)
        );
    }

    #[tokio::test]
    async fn an_owner_write_is_allowed_over_a_hand_edit_and_records_it_first() {
        let f = Fixture::new().await;
        f.tuner_five_then_hand_edit_seven().await;
        let row = f.set("alpha.size", json!(9), &owner()).await.unwrap();
        assert_eq!(row.kind, Kind::Set);
        assert_eq!(f.value("alpha.size"), Some(json!(9)));
        let history = f.history("alpha.size").await;
        assert_eq!(
            kinds(&history[..2]),
            vec![(4, Kind::Set, json!(9)), (3, Kind::HandEdit, json!(7))]
        );
        assert_eq!(history[0].actor, "owner:me");
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Fixed, ModeSource::Project)
        );
    }

    #[tokio::test]
    async fn a_fixed_parameter_refuses_the_tuner_including_its_rollback_but_not_the_owner() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(4), &tuner()).await.unwrap();
        f.set_mode(&Scope::project(PROJECT), "alpha.size", Some(Mode::Fixed))
            .await;
        let before = f.text();
        assert_eq!(
            f.set("alpha.size", json!(5), &tuner()).await.unwrap_err(),
            Refusal::FixedByOwner
        );
        assert_eq!(
            f.back("alpha.size", 1, &tuner()).await.unwrap_err(),
            Refusal::FixedByOwner
        );
        assert_eq!(f.text(), before);
        assert_eq!(f.history("alpha.size").await.len(), 2);
        f.set("alpha.size", json!(5), &owner()).await.unwrap();
        assert_eq!(f.value("alpha.size"), Some(json!(5)));
        f.back("alpha.size", 1, &owner()).await.unwrap();
        assert_eq!(f.value("alpha.size"), Some(json!(3)));
    }

    #[tokio::test]
    async fn the_machine_default_fixed_applies_until_a_project_override_says_auto() {
        let f = Fixture::new().await;
        f.set_mode(&Scope::Machine, "alpha.size", Some(Mode::Fixed))
            .await;
        assert_eq!(
            f.set("alpha.size", json!(4), &tuner()).await.unwrap_err(),
            Refusal::FixedByOwner
        );
        assert_eq!(f.text(), STATE);
        f.set_mode(&Scope::project(PROJECT), "alpha.size", Some(Mode::Auto))
            .await;
        f.set("alpha.size", json!(4), &tuner()).await.unwrap();
        assert_eq!(f.value("alpha.size"), Some(json!(4)));
    }

    #[tokio::test]
    async fn a_detected_hand_edit_leaves_the_machine_mode_untouched_and_only_writes_the_project_row()
     {
        let f = Fixture::new().await;
        f.set_mode(&Scope::Machine, "alpha.size", Some(Mode::Auto))
            .await;
        f.tuner_five_then_hand_edit_seven().await;
        f.set("alpha.size", json!(6), &tuner()).await.unwrap_err();
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT scope, mode, updated_by FROM workflow_param_modes
             WHERE bundle = ? AND param = ? ORDER BY scope",
        )
        .bind(BUNDLE)
        .bind("alpha.size")
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    Scope::Machine.to_string(),
                    "auto".to_string(),
                    "owner:me".to_string()
                ),
                (
                    Scope::project(PROJECT).to_string(),
                    "fixed".to_string(),
                    "daemon".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_tier_two_parameter_refuses_the_tuner_and_accepts_the_owner() {
        let f = Fixture::new().await;
        let refusal = f.set("beta.big", json!(60), &tuner()).await.unwrap_err();
        assert_eq!(refusal, Refusal::TierNotAutonomous(2));
        assert_eq!(refusal.code(), "tier_not_autonomous");
        assert_eq!(f.text(), STATE);
        assert!(f.history("beta.big").await.is_empty());
        f.set("beta.big", json!(60), &owner()).await.unwrap();
        assert_eq!(f.value("beta.big"), Some(json!(60)));
    }

    #[tokio::test]
    async fn rollback_writes_the_old_value_as_a_new_version_and_never_rewinds_history() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(4), &tuner()).await.unwrap();
        f.set("alpha.size", json!(5), &tuner()).await.unwrap();
        let row = f.back("alpha.size", 2, &tuner()).await.unwrap();
        assert_eq!(row.version, 4);
        assert_eq!(row.kind, Kind::Rollback);
        assert_eq!(row.value, json!(4));
        assert_eq!(row.rolled_back_to, Some(2));
        let history = f.history("alpha.size").await;
        let versions: Vec<i64> = history.iter().map(|r| r.version).collect();
        assert_eq!(versions, vec![4, 3, 2, 1]);
        assert_eq!(f.value("alpha.size"), Some(json!(4)));
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Default)
        );
    }

    #[tokio::test]
    async fn rollback_to_a_value_the_manifest_no_longer_allows_is_refused() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(9), &tuner()).await.unwrap();
        f.set("alpha.size", json!(4), &tuner()).await.unwrap();
        let narrowed = f.version("2.0", &MANIFEST.replacen("max: 10", "max: 8", 1));
        let before = f.text();
        let refusal = rollback(&f.pool, &narrowed, "alpha.size", 2, &owner(), "")
            .await
            .unwrap_err();
        assert_eq!(refusal.code(), "out_of_bounds");
        assert_eq!(f.text(), before);
        assert_eq!(f.history("alpha.size").await.len(), 3);
    }

    #[tokio::test]
    async fn rollback_to_a_missing_version_is_no_such_version() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(4), &tuner()).await.unwrap();
        let refusal = f.back("alpha.size", 42, &owner()).await.unwrap_err();
        assert_eq!(refusal, Refusal::NoSuchVersion(42));
        assert_eq!(refusal.code(), "no_such_version");
        assert_eq!(f.history("alpha.size").await.len(), 2);
    }

    #[tokio::test]
    async fn a_parameter_removed_from_the_manifest_is_orphaned_readable_and_not_writable() {
        let f = Fixture::new().await;
        f.set("alpha.size", json!(5), &tuner()).await.unwrap();
        let start = MANIFEST.find("  - name: alpha.mode").unwrap();
        let without = format!("description: synthetic\nparams:\n{}", &MANIFEST[start..]);
        let newer = f.version("2.0", &without);
        let states = state(&f.pool, &newer).await.unwrap();
        let orphan = states.iter().find(|s| s.name == "alpha.size").unwrap();
        assert!(orphan.orphaned);
        assert!(orphan.decl.is_none());
        assert_eq!(orphan.recorded.as_ref().unwrap().value, json!(5));
        let rows = history(&f.pool, &newer, "alpha.size").await.unwrap();
        assert_eq!(rows.len(), 2);
        let before = f.text();
        let set = set_value(&f.pool, &newer, "alpha.size", &json!(6), &owner(), "")
            .await
            .unwrap_err();
        assert_eq!(set.code(), "unknown_param");
        let back = rollback(&f.pool, &newer, "alpha.size", 1, &owner(), "")
            .await
            .unwrap_err();
        assert_eq!(back, Refusal::UnknownParam);
        assert_eq!(f.text(), before);
        assert_eq!(f.history("alpha.size").await.len(), 2);
    }

    #[tokio::test]
    async fn an_absent_key_reads_as_the_declared_default_and_is_inserted_on_write() {
        let f = Fixture::new().await;
        let states = state(&f.pool, &f.ctx).await.unwrap();
        let mode = states.iter().find(|s| s.name == "alpha.mode").unwrap();
        assert_eq!(mode.file_value, None);
        assert_eq!(mode.recorded, None);
        assert!(!mode.drift);
        f.set("alpha.mode", json!("slow"), &tuner()).await.unwrap();
        assert_eq!(
            kinds(&f.history("alpha.mode").await),
            vec![
                (2, Kind::Set, json!("slow")),
                (1, Kind::Baseline, json!("fast"))
            ]
        );
        assert_eq!(f.value("alpha.mode"), Some(json!("slow")));
        assert_eq!(f.value("alpha.size"), Some(json!(3)));
    }

    #[tokio::test]
    async fn a_file_that_does_not_exist_is_file_absent_and_is_not_created() {
        let f = Fixture::with_state(None).await;
        let refusal = f.set("alpha.size", json!(4), &owner()).await.unwrap_err();
        assert_eq!(refusal, Refusal::FileAbsent);
        assert_eq!(refusal.code(), "file_absent");
        assert!(!f.file().exists());
        assert!(f.history("alpha.size").await.is_empty());
    }

    /// Links `link` to `original`; false (after a note) when the platform refuses, e.g. Windows
    /// without the symlink privilege.
    fn try_symlink(original: &Path, link: &Path, dir: bool) -> bool {
        #[cfg(unix)]
        let made = {
            let _ = dir;
            std::os::unix::fs::symlink(original, link)
        };
        #[cfg(windows)]
        let made = if dir {
            std::os::windows::fs::symlink_dir(original, link)
        } else {
            std::os::windows::fs::symlink_file(original, link)
        };
        if let Err(error) = &made {
            eprintln!("skipped: cannot create a symlink here: {error}");
        }
        made.is_ok()
    }

    /// Links the directory `link` to `original`: a symlink on unix, a junction on Windows, which
    /// needs no privilege. False (after a note) when the platform refuses.
    fn try_dir_link(original: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            try_symlink(original, link, true)
        }
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link.as_os_str())
                .arg(original.as_os_str())
                .output();
            match status {
                Ok(out) if out.status.success() => true,
                other => {
                    eprintln!("skipped: cannot create a junction here: {other:?}");
                    false
                }
            }
        }
    }

    #[tokio::test]
    async fn a_mode_flipped_to_fixed_after_the_precheck_still_refuses_the_tuner() {
        let f = Fixture::new().await;
        let decl = declared(&f.ctx, "alpha.size").unwrap();
        let target = guarded_target(&f.ctx, &decl).unwrap();
        // The pre-check in `apply` has passed (mode auto); the owner flips the mode before the
        // transaction takes the write lock, which is the state `write_locked` starts from.
        let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        param_store::set_mode_on(
            &mut tx,
            &Scope::project(PROJECT),
            BUNDLE,
            "alpha.size",
            Some(Mode::Fixed),
            "owner:me",
        )
        .await
        .unwrap();
        let refusal = write_locked(
            tx,
            &f.ctx,
            &decl,
            &json!(5),
            &tuner(),
            "test",
            Kind::Set,
            None,
            &target,
        )
        .await
        .unwrap_err();
        assert_eq!(refusal, Refusal::FixedByOwner);
        assert_eq!(f.text(), STATE);
        assert!(f.history("alpha.size").await.is_empty());
    }

    #[tokio::test]
    async fn a_symlinked_parent_directory_outside_the_state_directory_is_file_unsafe() {
        let f = Fixture::new().await;
        let manifest = "description: synthetic
params:
  - name: gamma.size
    type: int
    min: 1
    max: 10
    tier: 1
    file: sub/q.yaml
    key: gamma.size
";
        let ctx = f.version("2.0", manifest);
        let outside = f.temp.path().join("outside-dir");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("q.yaml"), "gamma:\n  size: 3\n").unwrap();
        if !try_dir_link(&outside, &ctx.state_dir.join("sub")) {
            return;
        }
        let refusal = set_value(&f.pool, &ctx, "gamma.size", &json!(4), &owner(), "test")
            .await
            .unwrap_err();
        assert_eq!(refusal.code(), "file_unsafe");
        assert_eq!(
            std::fs::read_to_string(outside.join("q.yaml")).unwrap(),
            "gamma:\n  size: 3\n"
        );
        assert!(
            history(&f.pool, &ctx, "gamma.size")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_symlinked_target_outside_the_state_directory_is_file_unsafe() {
        let f = Fixture::with_state(None).await;
        let outside = f.temp.path().join("outside.yaml");
        std::fs::write(&outside, STATE).unwrap();
        if !try_symlink(&outside, &f.file(), false) {
            return;
        }
        let refusal = f.set("alpha.size", json!(4), &owner()).await.unwrap_err();
        assert_eq!(refusal.code(), "file_unsafe");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), STATE);
        assert!(f.history("alpha.size").await.is_empty());
    }

    #[tokio::test]
    async fn a_non_scalar_value_in_the_file_is_not_a_scalar_and_nothing_is_written() {
        let text = "alpha:\n  size: [1, 2]\n";
        let f = Fixture::with_state(Some(text)).await;
        let refusal = f.set("alpha.size", json!(4), &owner()).await.unwrap_err();
        assert_eq!(refusal.code(), "not_a_scalar");
        assert_eq!(f.text(), text);
        assert!(f.history("alpha.size").await.is_empty());
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Default)
        );
    }

    #[tokio::test]
    async fn state_is_read_only_it_reports_drift_without_writing_a_row() {
        let f = Fixture::new().await;
        f.tuner_five_then_hand_edit_seven().await;
        let edited = f.text();
        let states = state(&f.pool, &f.ctx).await.unwrap();
        let size = states.iter().find(|s| s.name == "alpha.size").unwrap();
        assert!(size.drift);
        assert!(!size.orphaned);
        assert_eq!(size.file_value, Some(json!(7)));
        assert_eq!(size.recorded.as_ref().unwrap().value, json!(5));
        assert_eq!(
            (size.mode, size.mode_source),
            (Mode::Auto, ModeSource::Default)
        );
        let big = states.iter().find(|s| s.name == "beta.big").unwrap();
        assert!(!big.drift);
        assert_eq!(big.file_value, Some(json!(50)));
        assert_eq!(f.history("alpha.size").await.len(), 2);
        assert_eq!(
            f.mode("alpha.size").await,
            (Mode::Auto, ModeSource::Default)
        );
        assert_eq!(f.text(), edited);
    }

    #[tokio::test]
    async fn ctx_for_pin_reads_the_library_copy_and_ignores_an_ejected_copy_in_the_project() {
        let f = Fixture::new().await;
        // An ejected copy in the project's working tree that widens the bound.
        let project_root = f.temp.path().join("project");
        let ejected = project_root.join(".nucleos/workflows").join(BUNDLE);
        std::fs::create_dir_all(&ejected).unwrap();
        std::fs::write(
            ejected.join(workflows::MANIFEST),
            MANIFEST.replacen("max: 10", "max: 99", 1),
        )
        .unwrap();
        let mut ejected_pin = pin("1.0");
        ejected_pin.ejected_at = Some("2026-11-08T00:00:00Z".into());
        ejected_pin.path = Some(".nucleos/workflows/synth".into());
        let ctx = Ctx::for_pin(&f.library, &f.machine, PROJECT, &ejected_pin).unwrap();
        assert_eq!(ctx.bundle_dir, f.library.join(BUNDLE).join("1.0"));
        assert_eq!(ctx.bundle, BUNDLE);
        assert_eq!(
            ctx.state_dir,
            project::state::dir(&f.machine, PROJECT).unwrap()
        );
        let refusal = set_value(&f.pool, &ctx, "alpha.size", &json!(50), &owner(), "")
            .await
            .unwrap_err();
        assert_eq!(refusal.code(), "out_of_bounds");

        assert!(Ctx::for_pin(&f.library, &f.machine, PROJECT, &pin("9.9")).is_none());
        assert!(Ctx::for_pin(&f.library, &f.machine, PROJECT, &pin("..")).is_none());
        assert!(Ctx::for_pin(&f.library, &f.machine, "", &pin("1.0")).is_none());
    }
}
