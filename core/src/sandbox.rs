//! The OS sandbox's single door (spec `2026-09-14-sandbox-de-so-design`, D3, D5, D7, D12).
//!
//! Every agent and gate process is meant to start here and nowhere else, so the containment has one
//! place to live. Phase 1 builds the door and nothing behind it: with no backend, [`spawn`] starts
//! the child exactly as the runners do today (own process group, a [`TreeKiller`] that drops before
//! the child) and the [`SandboxStatus`] says so (`NoBackend`), so the shell can show the banner.
//!
//! - [`profile_for`] derives what a unit may write, read and reach from its *mode*, never from the
//!   engine: this module has no per-engine code and must not grow any.
//! - [`spawn`] / [`spawn_prepared`] return a [`Contained`]: the child and its killer together.
//! - [`read_crossing`] / [`write_crossing`] (`sandbox/crossing.rs`) are how the owner touches a path
//!   the sandbox can also write.
//! - The `sandbox: off` switch is a file in the daemon's config directory. No HTTP route reads or
//!   changes it, and a test (`sandbox/guard.rs`) fails if another module names the file.
//!
//! In the no-backend state a profile is computed but not enforced. `Profile::env` is applied to the
//! child (cleared, then exactly that list, never the control token) only when the profile says
//! `env_applied`: today's runners keep the environment they build themselves until their wiring
//! turns it on.

pub mod assistant_routes;
mod crossing;
pub mod empty_dir;
pub mod gate;
#[cfg(test)]
mod guard;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use tokio::process::{Child, Command};

use crate::process_tree::{TreeKiller, spawn_in_own_group};

pub use crossing::{
    CrossingError, CrossingWriter, InProcessWriter, Untrusted, current_writer, read_crossing,
    write_crossing,
};

/// The one environment variable a sandbox never receives: the daemon's control token (D5).
const CONTROL_TOKEN_VAR: &str = "NUCLEOS_DAEMON_TOKEN";

/// Points git's global config at an empty file inside the unit, so the owner's `~/.gitconfig`
/// (credential helpers, aliases, hooks path) never reaches a contained child.
const GIT_CONFIG_GLOBAL_VAR: &str = "GIT_CONFIG_GLOBAL";

/// Name of the switch file inside the daemon's config directory.
const SWITCH_FILE: &str = "sandbox.conf";

/// Which hosts a unit's network may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Egress {
    /// No network at all.
    Closed,
    /// Only these domains (through the daemon's filtering proxy).
    Allow(Vec<String>),
}

/// What a contained unit is granted. Every other path is out of reach (D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub writable: Vec<PathBuf>,
    pub readable: Vec<PathBuf>,
    pub egress: Egress,
    /// The clean environment of the unit (D5), built from an allow-list. A sandbox never inherits.
    pub env: BTreeMap<String, String>,
    /// Whether [`spawn`] / [`spawn_prepared`] give the child exactly `env` and nothing else. Off in
    /// [`profile_for`]: a runner that builds its own environment keeps it until its wiring turns
    /// this on with [`Profile::with_env_applied`].
    pub env_applied: bool,
}

impl Profile {
    /// The same profile, with its `env` applied to the child.
    pub fn with_env_applied(mut self) -> Self {
        self.env_applied = true;
        self
    }
}

/// The kind of unit being started: the row of the D4 table, independent of the engine behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// A job node or item working in its tree folder.
    Worktree,
    Shadow,
    EmailTriage,
    /// Team, council and map-intent turns: an empty folder per run.
    Scratch,
    /// An assistant note turn: the note folder.
    Note,
    /// An assistant project turn: a per-chat repo.
    AssistantProject,
    /// A "real" run: a per-run repo, imported to a branch afterwards.
    Real,
    RunGate,
    QueueGate,
}

/// Everything `profile_for` needs to know about one run. Built by the caller from the run; it holds
/// no engine, and nothing here is read from the process environment.
#[derive(Debug, Clone)]
pub struct RunScope {
    pub mode: RunMode,
    /// The unit's own folder: tree folder, note folder, per-run repo or empty scratch folder.
    pub unit_dir: PathBuf,
    /// The owner's checkout. Granted to no profile; kept only so `profile_for` can refuse it.
    pub main_checkout: Option<PathBuf>,
    /// The main repo's object store, readable so a worktree can resolve history.
    pub main_repo_objects: Option<PathBuf>,
    /// Target dir and caches of the tree (writable for worktree and gate profiles).
    pub tree_caches: Vec<PathBuf>,
    /// Directories holding the toolchain; become `PATH` and are readable.
    pub toolchain: Vec<PathBuf>,
    /// API domains of the engine, taken from its descriptor (D6).
    pub api_domains: Vec<String>,
    /// Registry domains for gate profiles: crates.io, npm, the Go proxy, github (D6).
    pub registry_domains: Vec<String>,
    /// `NUCLEOS_*` variables of the run. The control token is dropped if it is here.
    pub run_env: BTreeMap<String, String>,
    /// `SystemRoot`, `windir`, `ComSpec` and the like, copied through by the caller.
    pub inherit: BTreeMap<String, String>,
    /// The filtering proxy the unit's `HTTP(S)_PROXY` points at.
    pub proxy: Option<String>,
}

fn under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

fn sorted_unique(mut items: Vec<String>) -> Vec<String> {
    items.sort();
    items.dedup();
    items
}

/// The clean environment of D5: an allow-list, with the home and temp folders inside the unit.
fn clean_env(scope: &RunScope) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (key, value) in &scope.inherit {
        env.insert(key.clone(), value.clone());
    }
    if !scope.toolchain.is_empty() {
        let joined: Option<OsString> = std::env::join_paths(&scope.toolchain).ok();
        if let Some(path) = joined.and_then(|p| p.into_string().ok()) {
            env.insert("PATH".to_owned(), path);
        }
    }
    let unit = &scope.unit_dir;
    let inside = |name: &str| unit.join(name).display().to_string();
    for (key, folder) in [
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("APPDATA", "appdata"),
        ("LOCALAPPDATA", "localappdata"),
        ("TEMP", "tmp"),
        ("TMP", "tmp"),
    ] {
        env.insert(key.to_owned(), inside(folder));
    }
    // macOS/Linux read `TMPDIR`; Windows keeps `TEMP`/`TMP` above. Same folder, same contract.
    env.insert("TMPDIR".to_owned(), inside("tmp"));
    env.insert(GIT_CONFIG_GLOBAL_VAR.to_owned(), inside("home/.gitconfig"));
    // The unit's cache and toolchain folders, named by what the tree granted as caches.
    for cache in &scope.tree_caches {
        let var = match cache.file_name().and_then(|name| name.to_str()) {
            Some("target") => "CARGO_TARGET_DIR",
            Some("cargo-home") => "CARGO_HOME",
            Some("rustup") => "RUSTUP_HOME",
            Some("gomodcache") => "GOMODCACHE",
            Some("npm-cache") => "npm_config_cache",
            _ => continue,
        };
        env.insert(var.to_owned(), cache.display().to_string());
    }
    env.insert("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned());
    let git_config = [
        ("gc.auto", "0".to_owned()),
        ("maintenance.auto", "false".to_owned()),
        ("safe.directory", unit.display().to_string()),
    ];
    env.insert("GIT_CONFIG_COUNT".to_owned(), git_config.len().to_string());
    for (index, (key, value)) in git_config.into_iter().enumerate() {
        env.insert(format!("GIT_CONFIG_KEY_{index}"), key.to_owned());
        env.insert(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    if let Some(proxy) = &scope.proxy {
        env.insert("HTTPS_PROXY".to_owned(), proxy.clone());
        env.insert("HTTP_PROXY".to_owned(), proxy.clone());
    }
    for (key, value) in &scope.run_env {
        if key.starts_with("NUCLEOS_") && key != CONTROL_TOKEN_VAR {
            env.insert(key.clone(), value.clone());
        }
    }
    env
}

/// The profile of a run, from its mode and provision (D3, D4). No engine is consulted.
pub fn profile_for(scope: &RunScope) -> Profile {
    let mut writable = vec![scope.unit_dir.clone()];
    let mut readable = Vec::new();
    let mut domains = Vec::new();
    let objects = scope.main_repo_objects.iter().cloned();
    let toolchain = scope.toolchain.iter().cloned();
    match scope.mode {
        RunMode::Worktree | RunMode::RunGate => {
            writable.extend(scope.tree_caches.iter().cloned());
            readable.extend(objects.chain(toolchain));
            domains.extend(scope.api_domains.iter().cloned());
            if scope.mode == RunMode::RunGate {
                domains.extend(scope.registry_domains.iter().cloned());
            }
        }
        RunMode::Shadow | RunMode::AssistantProject | RunMode::Real => {
            readable.extend(objects.chain(toolchain));
            domains.extend(scope.api_domains.iter().cloned());
        }
        RunMode::QueueGate => {
            writable.extend(scope.tree_caches.iter().cloned());
            readable.extend(objects.chain(toolchain));
            domains.extend(scope.registry_domains.iter().cloned());
        }
        RunMode::EmailTriage | RunMode::Scratch => {
            domains.extend(scope.api_domains.iter().cloned());
        }
        RunMode::Note => {
            readable.push(scope.unit_dir.clone());
            domains.extend(scope.api_domains.iter().cloned());
        }
    }
    // No profile touches an owner checkout: whatever the caller put there is dropped.
    if let Some(checkout) = &scope.main_checkout {
        writable.retain(|path| !under(path, checkout));
        // The object store sits under the checkout's `.git` and is the one granted read there.
        readable.retain(|path| {
            !under(path, checkout) || scope.main_repo_objects.as_ref() == Some(path)
        });
    }
    let domains = sorted_unique(domains);
    Profile {
        writable,
        readable,
        egress: if domains.is_empty() {
            Egress::Closed
        } else {
            Egress::Allow(domains)
        },
        env: clean_env(scope),
        env_applied: false,
    }
}

/// How the sandbox stands, for the shell to show (D7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxStatus {
    /// Phases 1-3a: the door exists, nothing contains yet.
    NoBackend,
    /// The owner switched it off in the config file.
    Off,
    /// A backend is enforcing the profiles.
    Armed,
    /// A backend exists but cannot start; runs are refused rather than started uncontained.
    Refused { reason: String },
}

impl SandboxStatus {
    /// The banner text the shell shows, if any.
    pub fn banner(&self) -> Option<String> {
        match self {
            Self::NoBackend => Some("sandbox por implementar".to_owned()),
            Self::Off => Some("sandbox desligada".to_owned()),
            Self::Armed => None,
            Self::Refused { reason } => Some(reason.clone()),
        }
    }

    /// Whether a run must not start. Only a refused backend stops runs: `NoBackend` and `Off`
    /// both run as today.
    pub fn refuses_runs(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

/// Whether the switch file in `dir` holds a `sandbox: off` line. Comments (`#`), case and quotes
/// around the value are ignored; a missing or unreadable file means "not off".
pub fn switch_is_off(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(SWITCH_FILE)) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.split('#').next().unwrap_or("").trim();
        line.split_once(':').is_some_and(|(key, value)| {
            key.trim().eq_ignore_ascii_case("sandbox")
                && value
                    .trim()
                    .trim_matches(['"', '\''])
                    .trim()
                    .eq_ignore_ascii_case("off")
        })
    })
}

/// The daemon's config directory, where the switch lives. Never the repo, never an env override.
pub fn switch_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// The status given the directory holding the switch file.
pub fn status_with(config_dir: Option<&Path>) -> SandboxStatus {
    if config_dir.is_some_and(switch_is_off) {
        SandboxStatus::Off
    } else {
        SandboxStatus::NoBackend
    }
}

/// The status as the daemon sees it now.
pub fn status() -> SandboxStatus {
    status_with(switch_dir().as_deref())
}

/// A started child and the killer of its whole tree.
///
/// **Field order is load-bearing**: fields drop in declaration order and the killer has to run
/// before the child is dropped, so it stays first.
pub struct Contained {
    pub killer: Option<TreeKiller>,
    pub child: Child,
}

impl Contained {
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Kills the child and everything it started, now.
    pub fn kill_now(&mut self) {
        if let Some(killer) = self.killer.as_mut() {
            killer.kill_now();
        }
    }

    /// Lets the child outlive this value: the tree is no longer killed on drop.
    pub fn disarm(&mut self) {
        if let Some(killer) = self.killer.as_mut() {
            killer.disarm();
        }
    }
}

/// Gives `command` exactly the profile's environment: everything else is cleared, and the control
/// token is dropped even if the caller had set it. Folders the env names (home, temp) and the empty
/// global git config are created here, best effort, because the scope itself creates nothing (D13).
fn apply_profile_env(command: &mut Command, profile: &Profile) {
    for key in ["HOME", "TEMP", "TMPDIR"] {
        if let Some(dir) = profile.env.get(key) {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    if let Some(file) = profile.env.get(GIT_CONFIG_GLOBAL_VAR)
        && !Path::new(file).exists()
    {
        if let Some(parent) = Path::new(file).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(file, "");
    }
    // `env_clear` also discards what the caller set on the command, so the list is re-applied whole.
    command.env_clear();
    for (key, value) in &profile.env {
        if key != CONTROL_TOKEN_VAR {
            command.env(key, value);
        }
    }
}

/// The one launch every spawn goes through. With no backend the profile is not enforced; its env is
/// applied only when the profile asks for it ([`Profile::env_applied`]).
fn launch(mut command: Command, profile: &Profile) -> io::Result<Contained> {
    if profile.env_applied {
        apply_profile_env(&mut command, profile);
    }
    spawn_in_own_group(&mut command);
    command.kill_on_drop(true);
    let child = command.spawn()?;
    let killer = child.id().map(TreeKiller::new);
    Ok(Contained { killer, child })
}

/// Starts `argv` in `cwd` with piped stdio, under `profile`.
pub fn spawn(argv: &[String], cwd: &Path, profile: &Profile) -> io::Result<Contained> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    launch(command, profile)
}

/// Starts a command the caller already configured (arguments, environment, stdio), under
/// `profile`. For runners that build a richer `Command` than `spawn` takes.
pub fn spawn_prepared(command: Command, profile: &Profile) -> io::Result<Contained> {
    launch(command, profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(mode: RunMode) -> RunScope {
        RunScope {
            mode,
            unit_dir: PathBuf::from("/units/u1"),
            main_checkout: Some(PathBuf::from("/owner/repo")),
            main_repo_objects: Some(PathBuf::from("/owner/repo/.git/objects")),
            tree_caches: vec![PathBuf::from("/units/u1/target")],
            toolchain: vec![PathBuf::from("/tools/bin")],
            api_domains: vec!["api.anthropic.com".to_owned()],
            registry_domains: vec!["crates.io".to_owned(), "github.com".to_owned()],
            run_env: BTreeMap::new(),
            inherit: BTreeMap::new(),
            proxy: Some("http://127.0.0.1:9999".to_owned()),
        }
    }

    const ALL_MODES: [RunMode; 9] = [
        RunMode::Worktree,
        RunMode::Shadow,
        RunMode::EmailTriage,
        RunMode::Scratch,
        RunMode::Note,
        RunMode::AssistantProject,
        RunMode::Real,
        RunMode::RunGate,
        RunMode::QueueGate,
    ];

    #[test]
    fn no_profile_writes_or_reads_the_owner_checkout() {
        for mode in ALL_MODES {
            let mut s = scope(mode);
            // Even a caller that hands the checkout over by mistake does not get it granted.
            s.tree_caches.push(PathBuf::from("/owner/repo/target"));
            s.toolchain.push(PathBuf::from("/owner/repo/tools"));
            let p = profile_for(&s);
            let checkout = Path::new("/owner/repo");
            assert!(
                !p.writable.iter().any(|w| w.starts_with(checkout)),
                "{mode:?}: {:?}",
                p.writable
            );
            // The object store lives under the checkout's `.git`; it is the one allowed read.
            assert!(
                !p.readable
                    .iter()
                    .any(|r| r == checkout || r == Path::new("/owner/repo/tools")),
                "{mode:?}: {:?}",
                p.readable
            );
        }
    }

    #[test]
    fn a_worktree_writes_its_folder_and_caches_and_reads_the_objects() {
        let p = profile_for(&scope(RunMode::Worktree));
        assert_eq!(
            p.writable,
            vec![
                PathBuf::from("/units/u1"),
                PathBuf::from("/units/u1/target")
            ]
        );
        assert!(
            p.readable
                .contains(&PathBuf::from("/owner/repo/.git/objects"))
        );
        assert_eq!(
            p.egress,
            Egress::Allow(vec!["api.anthropic.com".to_owned()])
        );
    }

    #[test]
    fn triage_and_scratch_runs_read_nothing_and_write_only_their_folder() {
        for mode in [RunMode::EmailTriage, RunMode::Scratch] {
            let p = profile_for(&scope(mode));
            assert_eq!(p.writable, vec![PathBuf::from("/units/u1")], "{mode:?}");
            assert!(p.readable.is_empty(), "{mode:?}");
        }
    }

    #[test]
    fn a_note_turn_reads_the_folder_it_writes() {
        let p = profile_for(&scope(RunMode::Note));
        assert_eq!(p.writable, vec![PathBuf::from("/units/u1")]);
        assert_eq!(p.readable, vec![PathBuf::from("/units/u1")]);
    }

    #[test]
    fn gates_reach_the_registries_and_only_the_run_gate_also_reaches_the_api() {
        let run = profile_for(&scope(RunMode::RunGate));
        assert_eq!(
            run.egress,
            Egress::Allow(vec![
                "api.anthropic.com".to_owned(),
                "crates.io".to_owned(),
                "github.com".to_owned(),
            ])
        );
        let queue = profile_for(&scope(RunMode::QueueGate));
        assert_eq!(
            queue.egress,
            Egress::Allow(vec!["crates.io".to_owned(), "github.com".to_owned()])
        );
    }

    #[test]
    fn a_unit_with_no_domains_has_its_network_closed() {
        let mut s = scope(RunMode::Scratch);
        s.api_domains.clear();
        assert_eq!(profile_for(&s).egress, Egress::Closed);
    }

    #[test]
    fn the_profile_is_the_same_whatever_engine_the_domains_came_from() {
        // The scope carries domains, not an engine: the same scope gives the same profile.
        assert_eq!(
            profile_for(&scope(RunMode::Real)),
            profile_for(&scope(RunMode::Real))
        );
    }

    #[test]
    fn the_env_is_an_allow_list_with_home_and_temp_inside_the_unit() {
        let mut s = scope(RunMode::Worktree);
        s.inherit
            .insert("SystemRoot".to_owned(), "C:\\Windows".to_owned());
        s.run_env
            .insert("NUCLEOS_RUN_ID".to_owned(), "r1".to_owned());
        s.run_env
            .insert("AWS_SECRET_ACCESS_KEY".to_owned(), "leak".to_owned());
        let env = profile_for(&s).env;
        assert_eq!(env["SystemRoot"], "C:\\Windows");
        assert_eq!(env["NUCLEOS_RUN_ID"], "r1");
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
        for key in [
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
        ] {
            assert!(
                Path::new(&env[key]).starts_with("/units/u1"),
                "{key} = {}",
                env[key]
            );
        }
        assert_eq!(env["GIT_CONFIG_NOSYSTEM"], "1");
        assert_eq!(env["GIT_CONFIG_COUNT"], "3");
        assert_eq!(env["GIT_CONFIG_KEY_0"], "gc.auto");
        assert_eq!(env["GIT_CONFIG_VALUE_0"], "0");
        assert_eq!(env["GIT_CONFIG_KEY_1"], "maintenance.auto");
        assert_eq!(env["GIT_CONFIG_VALUE_1"], "false");
        assert_eq!(env["GIT_CONFIG_KEY_2"], "safe.directory");
        assert_eq!(env["HTTPS_PROXY"], "http://127.0.0.1:9999");
        assert_eq!(env["HTTP_PROXY"], "http://127.0.0.1:9999");
        assert!(env["PATH"].contains("tools"));
    }

    #[test]
    fn the_control_token_never_enters_a_sandbox_env() {
        let mut s = scope(RunMode::Worktree);
        s.run_env
            .insert(CONTROL_TOKEN_VAR.to_owned(), "secret".to_owned());
        s.inherit.insert("PATH_ELSE".to_owned(), "x".to_owned());
        let env = profile_for(&s).env;
        assert!(!env.contains_key(CONTROL_TOKEN_VAR));
        assert!(!env.values().any(|v| v == "secret"));
    }

    #[test]
    fn the_switch_reads_an_off_line_and_ignores_comments_case_and_quotes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!switch_is_off(dir.path()), "no file means not off");
        let write = |text: &str| std::fs::write(dir.path().join(SWITCH_FILE), text).unwrap();
        write("sandbox: off\n");
        assert!(switch_is_off(dir.path()));
        write("# sandbox: off\n");
        assert!(!switch_is_off(dir.path()));
        write("Sandbox:  \"OFF\"  # temporarily\n");
        assert!(switch_is_off(dir.path()));
        write("sandbox: on\n");
        assert!(!switch_is_off(dir.path()));
        write("other: off\n");
        assert!(!switch_is_off(dir.path()));
    }

    #[test]
    fn the_status_is_no_backend_until_the_switch_says_off() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(status_with(Some(dir.path())), SandboxStatus::NoBackend);
        assert_eq!(status_with(None), SandboxStatus::NoBackend);
        std::fs::write(dir.path().join(SWITCH_FILE), "sandbox: off").unwrap();
        assert_eq!(status_with(Some(dir.path())), SandboxStatus::Off);
    }

    #[test]
    fn only_a_refused_backend_stops_runs_and_the_banner_says_so() {
        assert!(!SandboxStatus::NoBackend.refuses_runs());
        assert!(!SandboxStatus::Off.refuses_runs());
        assert!(!SandboxStatus::Armed.refuses_runs());
        let refused = SandboxStatus::Refused {
            reason: "no account".to_owned(),
        };
        assert!(refused.refuses_runs());
        assert_eq!(
            SandboxStatus::NoBackend.banner().as_deref(),
            Some("sandbox por implementar")
        );
        assert_eq!(SandboxStatus::Armed.banner(), None);
        assert_eq!(refused.banner().as_deref(), Some("no account"));
    }

    fn exit_with(code: u8) -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), format!("exit {code}")]
        } else {
            vec!["sh".into(), "-c".into(), format!("exit {code}")]
        }
    }

    fn sleeper() -> Vec<String> {
        if cfg!(windows) {
            ["ping", "-n", "30", "127.0.0.1"].map(String::from).to_vec()
        } else {
            ["sleep", "30"].map(String::from).to_vec()
        }
    }

    #[tokio::test]
    async fn spawn_runs_the_command_in_cwd_and_returns_the_child_with_its_killer() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_for(&scope(RunMode::Scratch));
        let mut contained = spawn(&exit_with(3), dir.path(), &profile).unwrap();
        assert!(contained.killer.is_some());
        assert!(contained.pid().is_some());
        let status = contained.child.wait().await.unwrap();
        assert_eq!(status.code(), Some(3));
    }

    #[tokio::test]
    async fn kill_now_ends_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_for(&scope(RunMode::Scratch));
        let mut contained = spawn(&sleeper(), dir.path(), &profile).unwrap();
        contained.kill_now();
        let status = contained.child.wait().await.unwrap();
        assert!(!status.success());
    }

    #[tokio::test]
    async fn an_empty_argv_is_refused_before_anything_starts() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_for(&scope(RunMode::Scratch));
        let err = spawn(&[], dir.path(), &profile)
            .err()
            .expect("empty argv fails");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn a_missing_program_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_for(&scope(RunMode::Scratch));
        let argv = vec!["nucleos-no-such-program-xyz".to_owned()];
        assert!(spawn(&argv, dir.path(), &profile).is_err());
    }

    /// Argv that prints variable `name` (empty when unset), on either platform.
    fn print_var(name: &str) -> Vec<String> {
        if cfg!(windows) {
            ["cmd", "/C", &format!("echo [%{name}%]")]
                .map(String::from)
                .to_vec()
        } else {
            ["sh", "-c", &format!("echo \"[${{{name}}}]\"")]
                .map(String::from)
                .to_vec()
        }
    }

    async fn output_of(profile: &Profile, argv: &[String], set: &[(&str, &str)]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::piped());
        for (key, value) in set {
            command.env(key, value);
        }
        let contained = spawn_prepared(command, profile).unwrap();
        let out = contained.child.wait_with_output().await.unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[tokio::test]
    async fn launch_applies_the_profile_env_and_clears_the_rest() {
        let unit = tempfile::tempdir().unwrap();
        let mut s = scope(RunMode::Worktree);
        s.unit_dir = unit.path().to_path_buf();
        s.tree_caches = vec![unit.path().join("target")];
        // The shell the test starts needs its own lookup path and Windows root, which the caller
        // hands over through `inherit` and the toolchain, as every real scope does.
        s.toolchain.clear();
        for key in ["PATH", "SystemRoot", "windir", "ComSpec"] {
            if let Ok(value) = std::env::var(key) {
                s.inherit.insert(key.to_owned(), value);
            }
        }
        let profile = profile_for(&s).with_env_applied();

        let home = output_of(&profile, &print_var("HOME"), &[]).await;
        assert!(home.contains("home"), "HOME is the unit's: {home}");
        let target = output_of(&profile, &print_var("CARGO_TARGET_DIR"), &[]).await;
        assert!(target.contains("target"), "{target}");
        let git = output_of(&profile, &print_var("GIT_CONFIG_GLOBAL"), &[]).await;
        assert!(git.contains(".gitconfig"), "{git}");
        assert_eq!(
            std::fs::read_to_string(unit.path().join("home/.gitconfig")).unwrap(),
            "",
            "the global git config is an empty file"
        );
        // What the caller set on the command, and the daemon's own environment, is cleared.
        let stray = output_of(&profile, &print_var("STRAY_VAR"), &[("STRAY_VAR", "x")]).await;
        assert!(
            !stray.contains('x'),
            "a caller-set variable survived: {stray}"
        );
    }

    #[tokio::test]
    async fn a_child_never_sees_the_control_token_even_if_the_daemon_has_it() {
        let unit = tempfile::tempdir().unwrap();
        let mut s = scope(RunMode::Worktree);
        s.unit_dir = unit.path().to_path_buf();
        for key in ["PATH", "SystemRoot", "windir", "ComSpec"] {
            if let Ok(value) = std::env::var(key) {
                s.inherit.insert(key.to_owned(), value);
            }
        }
        s.toolchain.clear();
        // Even a profile whose env carries the token (a caller bug) does not hand it over.
        let mut profile = profile_for(&s).with_env_applied();
        profile
            .env
            .insert(CONTROL_TOKEN_VAR.to_owned(), "secret".to_owned());
        let seen = output_of(
            &profile,
            &print_var(CONTROL_TOKEN_VAR),
            &[(CONTROL_TOKEN_VAR, "secret")],
        )
        .await;
        assert!(
            !seen.contains("secret"),
            "the token reached the child: {seen}"
        );
    }

    #[tokio::test]
    async fn spawn_prepared_keeps_the_callers_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_for(&scope(RunMode::Scratch));
        let argv = exit_with(4);
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).current_dir(dir.path());
        let mut contained = spawn_prepared(command, &profile).unwrap();
        assert_eq!(contained.child.wait().await.unwrap().code(), Some(4));
    }
}
